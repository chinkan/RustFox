//! Scheduler human-gate Telegram copy + inline Retry/Cancel (ADR-0038 Slice A).
//!
//! Copy skeleton and operator UX only — queue semantics stay ADR-0013
//! (`RerunQueue::retry` / `cancel` = full replay). Slice B transcript continue
//! is out.

use super::reruns::RerunQueue;
use crate::scheduler::reminders::ScheduledTaskStore;
use anyhow::Result;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

/// Operator-facing gate reasons that share the ADR-0038 skeleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HumanGateKind {
    /// First-run wall-clock timeout (ADR-0024 → `enqueue_manual`).
    FirstRunTimeout,
    /// Timeout while an existing `rerun_id` is set.
    RerunTimeout,
    /// Max-iterations → human-gate.
    IterationStall,
    /// Second death → ask human.
    FailedTwice,
}

/// Parsed Retry / Cancel intent (callback or slash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAction {
    Retry(String),
    Cancel(String),
}

impl GateAction {
    pub fn queue_id(&self) -> &str {
        match self {
            Self::Retry(id) | Self::Cancel(id) => id,
        }
    }
}

/// Outcome shown after answering the gate (ADR-0038 Q8/Q9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    Retried,
    Cancelled,
    AlreadyDone,
}

impl GateOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retried => "Retried",
            Self::Cancelled => "Cancelled",
            Self::AlreadyDone => "Already done",
        }
    }
}

/// Readable duration for operator copy (ADR-0038 Q2).
/// Whole minutes when `secs % 60 == 0`; otherwise a clear seconds form.
pub fn format_readable_duration(secs: u64) -> String {
    if secs > 0 && secs.is_multiple_of(60) {
        let mins = secs / 60;
        if mins == 1 {
            "1 minute".to_string()
        } else {
            format!("{mins} minutes")
        }
    } else if secs == 1 {
        "1 second".to_string()
    } else {
        format!("{secs} seconds")
    }
}

fn title_for(kind: HumanGateKind) -> &'static str {
    match kind {
        HumanGateKind::FirstRunTimeout => "Scheduled task stopped — time limit reached",
        HumanGateKind::RerunTimeout => "Scheduled task stopped — re-run time limit reached",
        HumanGateKind::IterationStall => "Scheduled task stopped — iteration limit reached",
        HumanGateKind::FailedTwice => "Scheduled task stopped — failed twice",
    }
}

fn reason_for(kind: HumanGateKind, detail: &str) -> String {
    match kind {
        HumanGateKind::FirstRunTimeout => format!(
            "It ran for {detail} and didn't finish. I did **not** retry automatically, because some work may already have happened."
        ),
        HumanGateKind::RerunTimeout => format!(
            "The re-run hit the wall-clock limit ({detail}) and didn't finish. I did **not** retry automatically, because some work may already have happened."
        ),
        HumanGateKind::IterationStall => format!(
            "It hit the iteration cap ({detail}) without finishing. I did **not** retry automatically, because some work may already have happened."
        ),
        HumanGateKind::FailedTwice => format!(
            "It failed again on re-run. I did **not** retry automatically, because some work may already have happened.\n\nLast error: {detail}"
        ),
    }
}

/// PO skeleton (ADR-0038 Q1–Q5) with `/retry` `/cancel` text backup (Q10).
pub fn format_human_gate_message(kind: HumanGateKind, detail: &str, queue_id: &str) -> String {
    let title = title_for(kind);
    let reason = reason_for(kind, detail);
    format!(
        "**{title}**\n\n\
{reason}\n\n\
Reply:\n\
• `/retry {queue_id}` — run it once more\n\
• `/cancel {queue_id}` — leave it stopped\n\n\
(id: `{queue_id}`)"
    )
}

/// Callback data `rerun:retry:<uuid>` / `rerun:cancel:<uuid>` (≤64 bytes).
pub fn callback_data(action: &GateAction) -> String {
    let data = match action {
        GateAction::Retry(id) => format!("rerun:retry:{id}"),
        GateAction::Cancel(id) => format!("rerun:cancel:{id}"),
    };
    debug_assert!(
        data.len() <= 64,
        "Telegram callback_data must be ≤64 bytes, got {}",
        data.len()
    );
    data
}

/// Parse `rerun:retry:<id>` / `rerun:cancel:<id>` (reject if >64 bytes).
pub fn parse_rerun_callback(data: &str) -> Option<GateAction> {
    if data.len() > 64 {
        return None;
    }
    if let Some(id) = data.strip_prefix("rerun:retry:") {
        if !id.is_empty() {
            return Some(GateAction::Retry(id.to_string()));
        }
    }
    if let Some(id) = data.strip_prefix("rerun:cancel:") {
        if !id.is_empty() {
            return Some(GateAction::Cancel(id.to_string()));
        }
    }
    None
}

/// Deterministic `/retry <id>` / `/cancel <id>` only (no NL).
pub fn parse_gate_slash(cmd: &str, arg: &str) -> Option<GateAction> {
    let id = arg.trim();
    if id.is_empty() {
        return None;
    }
    match cmd {
        "retry" => Some(GateAction::Retry(id.to_string())),
        "cancel" => Some(GateAction::Cancel(id.to_string())),
        _ => None,
    }
}

/// Inline Retry / Cancel keyboard for a queue row.
pub fn human_gate_keyboard(queue_id: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            "Retry",
            callback_data(&GateAction::Retry(queue_id.to_string())),
        ),
        InlineKeyboardButton::callback(
            "Cancel",
            callback_data(&GateAction::Cancel(queue_id.to_string())),
        ),
    ]])
}

/// Empty markup to strip buttons on edit.
pub fn empty_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(Vec::<Vec<InlineKeyboardButton>>::new())
}

/// Apply Retry/Cancel via existing queue APIs, with bot ownership check.
///
/// Already-handled / unknown / other-bot rows → [`GateOutcome::AlreadyDone`]
/// (idempotent; no double-dispatch).
pub async fn apply_gate_action(
    queue: &RerunQueue,
    task_store: &ScheduledTaskStore,
    bot_id: &str,
    action: &GateAction,
) -> Result<GateOutcome> {
    let id = action.queue_id();
    let Some(row) = queue.get(id).await? else {
        return Ok(GateOutcome::AlreadyDone);
    };
    match task_store.get_by_id(&row.task_id).await? {
        Some(task) if task.bot_id == bot_id => {}
        _ => return Ok(GateOutcome::AlreadyDone),
    }
    match action {
        GateAction::Retry(_) => {
            if queue.retry(id).await? {
                Ok(GateOutcome::Retried)
            } else {
                Ok(GateOutcome::AlreadyDone)
            }
        }
        GateAction::Cancel(_) => {
            if queue.cancel(id).await? {
                Ok(GateOutcome::Cancelled)
            } else {
                Ok(GateOutcome::AlreadyDone)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::reruns::RerunQueue;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn readable_duration_whole_minutes() {
        assert_eq!(format_readable_duration(900), "15 minutes");
        assert_eq!(format_readable_duration(60), "1 minute");
        assert_eq!(format_readable_duration(120), "2 minutes");
    }

    #[test]
    fn readable_duration_seconds_when_not_divisible() {
        assert_eq!(format_readable_duration(90), "90 seconds");
        assert_eq!(format_readable_duration(1), "1 second");
        assert_eq!(format_readable_duration(0), "0 seconds");
    }

    #[test]
    fn first_run_timeout_copy_matches_po_skeleton() {
        let msg = format_human_gate_message(
            HumanGateKind::FirstRunTimeout,
            &format_readable_duration(900),
            "abc-123",
        );
        assert!(msg.contains("**Scheduled task stopped — time limit reached**"));
        assert!(msg.contains("15 minutes"));
        assert!(msg.contains("did **not** retry automatically"));
        assert!(msg.contains("run it once more"));
        assert!(!msg.contains("try once more automatically"));
        assert!(msg.contains("`/retry abc-123`"));
        assert!(msg.contains("`/cancel abc-123`"));
        assert!(msg.contains("(id: `abc-123`)"));
        assert!(!msg.contains("900s"));
        assert!(!msg.contains("half-done"));
    }

    #[test]
    fn siblings_swap_title_only_keep_skeleton() {
        let stall = format_human_gate_message(HumanGateKind::IterationStall, "25", "qid");
        assert!(stall.contains("iteration limit reached"));
        assert!(stall.contains("did **not** retry automatically"));
        assert!(stall.contains("run it once more"));

        let twice = format_human_gate_message(HumanGateKind::FailedTwice, "boom", "qid");
        assert!(twice.contains("failed twice"));
        assert!(twice.contains("Last error: boom"));
        assert!(twice.contains("run it once more"));

        let rerun = format_human_gate_message(
            HumanGateKind::RerunTimeout,
            &format_readable_duration(900),
            "qid",
        );
        assert!(rerun.contains("re-run time limit reached"));
        assert!(rerun.contains("15 minutes"));
    }

    #[test]
    fn callback_parse_roundtrip_and_size() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let retry = GateAction::Retry(id.to_string());
        let cancel = GateAction::Cancel(id.to_string());
        let rd = callback_data(&retry);
        let cd = callback_data(&cancel);
        assert!(rd.len() <= 64, "retry callback {rd} len {}", rd.len());
        assert!(cd.len() <= 64, "cancel callback {cd} len {}", cd.len());
        assert_eq!(parse_rerun_callback(&rd), Some(retry));
        assert_eq!(parse_rerun_callback(&cd), Some(cancel));
        assert_eq!(parse_rerun_callback("model_select:foo"), None);
        assert_eq!(
            parse_rerun_callback(&format!("rerun:retry:{}", "x".repeat(60))),
            None
        );
    }

    #[test]
    fn slash_parse_only_retry_cancel_with_id() {
        assert_eq!(
            parse_gate_slash("retry", "abc"),
            Some(GateAction::Retry("abc".into()))
        );
        assert_eq!(
            parse_gate_slash("cancel", " abc "),
            Some(GateAction::Cancel("abc".into()))
        );
        assert_eq!(parse_gate_slash("retry", ""), None);
        assert_eq!(parse_gate_slash("cancel", ""), None);
        assert_eq!(parse_gate_slash("stop", "abc"), None);
    }

    async fn queue_and_store() -> (RerunQueue, ScheduledTaskStore, String) {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        {
            let c = conn.lock().await;
            c.execute_batch(
                "CREATE TABLE pending_reruns (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, original_run_id TEXT NOT NULL,
                    fail_reason TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL, next_eligible_at TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE TABLE scheduled_tasks (
                    id TEXT PRIMARY KEY, scheduler_job_id TEXT, user_id TEXT NOT NULL,
                    chat_id TEXT NOT NULL, platform TEXT NOT NULL, trigger_type TEXT NOT NULL,
                    trigger_value TEXT NOT NULL, prompt TEXT NOT NULL, description TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'active',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')), next_run_at TEXT,
                    deleted_at TEXT, bot_id TEXT NOT NULL, timezone TEXT NOT NULL DEFAULT 'UTC'
                );",
            )
            .unwrap();
            c.execute(
                "INSERT INTO scheduled_tasks
                 (id, user_id, chat_id, platform, trigger_type, trigger_value, prompt, description, bot_id)
                 VALUES ('t1', '42', '1', 'telegram', 'one_shot', '2026-01-01T00:00:00Z', 'p', 'd', 'bot-a')",
                [],
            )
            .unwrap();
        }
        let q = RerunQueue::new(Arc::clone(&conn));
        let store = ScheduledTaskStore::new(conn);
        let id = q.enqueue_manual("t1", "run-1", "test gate").await.unwrap();
        (q, store, id)
    }

    #[tokio::test]
    async fn apply_retry_then_idempotent_already_done() {
        let (q, store, id) = queue_and_store().await;
        let action = GateAction::Retry(id.clone());
        let first = apply_gate_action(&q, &store, "bot-a", &action)
            .await
            .unwrap();
        assert_eq!(first, GateOutcome::Retried);
        // ADR-0040: apply_gate_action Retry makes the row due immediately
        // (kick is wired in Telegram handlers / Agent::kick_human_retry).
        let due = q.due().await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
        let second = apply_gate_action(&q, &store, "bot-a", &action)
            .await
            .unwrap();
        assert_eq!(second, GateOutcome::AlreadyDone);
    }

    #[tokio::test]
    async fn apply_cancel_does_not_make_row_due() {
        let (q, store, id) = queue_and_store().await;
        let action = GateAction::Cancel(id.clone());
        assert_eq!(
            apply_gate_action(&q, &store, "bot-a", &action)
                .await
                .unwrap(),
            GateOutcome::Cancelled
        );
        assert!(
            q.due().await.unwrap().is_empty(),
            "cancel must not kick / must not leave a due queued row"
        );
    }

    #[tokio::test]
    async fn apply_cancel_then_idempotent_already_done() {
        let (q, store, id) = queue_and_store().await;
        let action = GateAction::Cancel(id.clone());
        assert_eq!(
            apply_gate_action(&q, &store, "bot-a", &action)
                .await
                .unwrap(),
            GateOutcome::Cancelled
        );
        assert_eq!(
            apply_gate_action(&q, &store, "bot-a", &action)
                .await
                .unwrap(),
            GateOutcome::AlreadyDone
        );
    }

    #[tokio::test]
    async fn other_bot_is_already_done_no_dispatch() {
        let (q, store, id) = queue_and_store().await;
        let action = GateAction::Retry(id.clone());
        assert_eq!(
            apply_gate_action(&q, &store, "other-bot", &action)
                .await
                .unwrap(),
            GateOutcome::AlreadyDone
        );
        let row = q.get(&id).await.unwrap().unwrap();
        assert_eq!(row.state.as_str(), "awaiting_user");
    }

    #[tokio::test]
    async fn missing_row_is_already_done() {
        let (q, store, _) = queue_and_store().await;
        let action = GateAction::Cancel("no-such-id".into());
        assert_eq!(
            apply_gate_action(&q, &store, "bot-a", &action)
                .await
                .unwrap(),
            GateOutcome::AlreadyDone
        );
    }
}
