//! Scheduled-task auto-resume (ADR-0067 / #412).
//!
//! Incomplete scheduled work continues from a transcript checkpoint on
//! process start and on a dedicated 60-second restore tick. That tick is
//! not the builtin liveness cron named `heartbeat`, and it is not the
//! hourly dead-letter watchdog. Interactive Retry and `TurnGate` are
//! unchanged. Unverified results stay a human gate.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::checkpoint::{CheckpointStopKind, CheckpointStore, TranscriptCheckpoint};
use super::reminders::ScheduledTaskStore;
use super::reruns::{RerunQueue, RerunState};

/// Dedicated restore cadence. Not 3600 (hourly watchdog) and not the
/// builtin `heartbeat` cron (`0 0 * * * *`).
pub const RESTORE_TICK_SECS: u64 = 60;

/// Task ids whose scheduled worker is inside the job runner.
#[derive(Clone, Debug)]
pub struct LiveScheduledRuns {
    tasks: Arc<Mutex<HashSet<String>>>,
}

/// Held for the duration of one scheduled job. Drop releases the task id.
#[must_use = "dropping the guard releases the scheduled task immediately"]
pub struct LiveHold {
    tasks: Arc<Mutex<HashSet<String>>>,
    task_id: String,
}

impl Default for LiveScheduledRuns {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveScheduledRuns {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn hold(&self, task_id: &str) -> LiveHold {
        self.tasks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(task_id.to_string());
        LiveHold {
            tasks: Arc::clone(&self.tasks),
            task_id: task_id.to_string(),
        }
    }

    pub fn snapshot(&self) -> HashSet<String> {
        self.tasks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }
}

impl Drop for LiveHold {
    fn drop(&mut self) {
        self.tasks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&self.task_id);
    }
}

/// What `bind_stop_for_auto_resume` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// Continue key is this `pending_reruns` id. Dispatch it.
    Ready(String),
    /// Human gate, another running row, or nothing to continue.
    Skip,
}

/// One informational line. No Retry button and no `/retry` instruction.
pub fn auto_resume_notice(kind: CheckpointStopKind, detail: &str) -> Option<String> {
    match kind {
        CheckpointStopKind::Timeout => Some(format!(
            "Scheduled task timed out ({detail}) and will auto-continue."
        )),
        CheckpointStopKind::MaxIterations => Some(format!(
            "Scheduled task hit max iterations ({detail}) and will auto-continue."
        )),
        _ => None,
    }
}

/// A fresh cron must not start a second run while this task is busy or gated.
pub async fn fresh_cron_blocked(
    queue: &RerunQueue,
    store: &ScheduledTaskStore,
    live: &HashSet<String>,
    task_id: &str,
) -> Result<bool> {
    if live.contains(task_id) || store.has_running_for_task(task_id).await? {
        return Ok(true);
    }
    queue.has_live_control_row(task_id).await
}

/// Human Retry must not start a second copy while auto-resume is in flight.
pub async fn human_retry_blocked(
    store: &ScheduledTaskStore,
    live: &HashSet<String>,
    task_id: &str,
) -> Result<bool> {
    if live.contains(task_id) {
        return Ok(true);
    }
    store.has_running_for_task(task_id).await
}

/// Bind a timeout or max-iterations stop to an immediately eligible queue row.
/// Does not create an `awaiting_user` gate. An open human gate wins.
#[allow(clippy::too_many_arguments)]
pub async fn bind_stop_for_auto_resume(
    queue: &RerunQueue,
    checkpoints: &CheckpointStore,
    store: &ScheduledTaskStore,
    task_id: &str,
    run_id: &str,
    existing_queue_id: Option<&str>,
    checkpoint: &TranscriptCheckpoint,
    detail: &str,
) -> Result<BindOutcome> {
    if !checkpoint.stop_kind.is_scheduled_auto_resume() {
        return Ok(BindOutcome::Skip);
    }
    if queue.open_human_gate(task_id).await?.is_some() {
        return Ok(BindOutcome::Skip);
    }
    if store.has_other_running(task_id, run_id).await? {
        return Ok(BindOutcome::Skip);
    }
    let queue_id = if let Some(id) = existing_queue_id {
        match queue.get(id).await? {
            Some(row) if row.state == RerunState::AwaitingUser => return Ok(BindOutcome::Skip),
            Some(row) => {
                queue.requeue_auto_resume(&row.id, detail).await?;
                row.id
            }
            None => queue.enqueue_auto_resume(task_id, run_id, detail).await?,
        }
    } else if let Some(row) = queue.queued_for_task(task_id).await? {
        queue.requeue_auto_resume(&row.id, detail).await?;
        row.id
    } else {
        queue.enqueue_auto_resume(task_id, run_id, detail).await?
    };
    checkpoints
        .save_with_task(&queue_id, Some(run_id), Some(task_id), checkpoint)
        .await?;
    checkpoints.retain_only(task_id, &queue_id).await?;
    Ok(BindOutcome::Ready(queue_id))
}

/// Queue ids the startup path or the 60-second tick should dispatch.
///
/// `heal_dead_claims` is the process-start path only. It re-arms an
/// auto-resume row that was claimed (`attempts > 0`) and then killed with
/// the process. The tick does not heal those rows, so an in-flight continue
/// is not started twice. Neither path waits 30 minutes, and neither
/// full-replays when no checkpoint exists.
pub async fn queue_ids_to_restore(
    queue: &RerunQueue,
    checkpoints: &CheckpointStore,
    store: &ScheduledTaskStore,
    live: &HashSet<String>,
    heal_dead_claims: bool,
) -> Result<Vec<String>> {
    let mut ready = Vec::new();
    let mut seen = HashSet::new();

    for row in queue.list_auto_resume_queued().await? {
        if !seen.insert(row.task_id.clone()) {
            continue;
        }
        // An in-flight claim stays with its worker. Touching the checkpoint
        // here would delete the mid-run row the live run just saved.
        if row.attempts > 0 && !heal_dead_claims {
            continue;
        }
        if !task_can_restore(queue, store, live, &row.task_id).await? {
            continue;
        }
        let attached =
            attach_checkpoint(checkpoints, &row.task_id, &row.id, &row.original_run_id).await?;
        if !attached {
            continue;
        }
        if row.attempts > 0 {
            queue.requeue_auto_resume(&row.id, &row.fail_reason).await?;
        }
        ready.push(row.id);
    }

    for found in checkpoints.resumable_latest_per_task().await? {
        if seen.contains(&found.task_id) {
            continue;
        }
        if !task_can_restore(queue, store, live, &found.task_id).await? {
            continue;
        }
        // A transient queued row or any other live control row keeps its own path.
        if queue.has_live_control_row(&found.task_id).await? {
            continue;
        }
        let run_id = found.run_id.clone().unwrap_or_default();
        let detail = format!("process restart ({})", found.checkpoint.stop_kind.as_str());
        let queue_id = queue
            .enqueue_auto_resume(&found.task_id, &run_id, &detail)
            .await?;
        checkpoints
            .save_with_task(
                &queue_id,
                found.run_id.as_deref(),
                Some(&found.task_id),
                &found.checkpoint,
            )
            .await?;
        checkpoints.retain_only(&found.task_id, &queue_id).await?;
        seen.insert(found.task_id);
        ready.push(queue_id);
    }

    Ok(ready)
}

async fn task_can_restore(
    queue: &RerunQueue,
    store: &ScheduledTaskStore,
    live: &HashSet<String>,
    task_id: &str,
) -> Result<bool> {
    if live.contains(task_id) {
        return Ok(false);
    }
    if store.has_running_for_task(task_id).await? {
        return Ok(false);
    }
    if queue.open_human_gate(task_id).await?.is_some() {
        return Ok(false);
    }
    Ok(true)
}

/// Point `queue_id` at a resumable checkpoint. `false` means there is nothing
/// to continue, so the caller must not full-replay.
async fn attach_checkpoint(
    checkpoints: &CheckpointStore,
    task_id: &str,
    queue_id: &str,
    original_run_id: &str,
) -> Result<bool> {
    if let Some(cp) = checkpoints.get(queue_id).await? {
        if cp.stop_kind.is_scheduled_auto_resume() {
            return Ok(true);
        }
    }
    // When the interrupted run id is known, continue that run's checkpoint.
    let run_filter = if original_run_id.is_empty() {
        None
    } else {
        Some(original_run_id)
    };
    let Some((src_id, cp)) = checkpoints.latest_resumable(task_id, run_filter).await? else {
        return Ok(false);
    };
    if src_id == queue_id {
        return Ok(cp.stop_kind.is_scheduled_auto_resume());
    }
    checkpoints
        .save_with_task(queue_id, Some(original_run_id), Some(task_id), &cp)
        .await?;
    checkpoints.retain_only(task_id, queue_id).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, MessageContent};
    use crate::scheduler::checkpoint::CheckpointStopKind;
    use rusqlite::Connection;
    use tokio::sync::Mutex as AsyncMutex;

    async fn fixture() -> (RerunQueue, CheckpointStore, ScheduledTaskStore) {
        let conn = Arc::new(AsyncMutex::new(Connection::open_in_memory().unwrap()));
        {
            let c = conn.lock().await;
            c.execute_batch(
                "CREATE TABLE pending_reruns (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, original_run_id TEXT NOT NULL,
                    fail_reason TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL DEFAULT 'queued', next_eligible_at TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')));
                 CREATE TABLE scheduled_task_runs (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, run_at TEXT NOT NULL,
                    response TEXT, error TEXT, status TEXT NOT NULL DEFAULT 'completed',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')));
                 CREATE TABLE scheduled_tasks (
                    id TEXT PRIMARY KEY, user_id TEXT NOT NULL, chat_id TEXT NOT NULL,
                    platform TEXT NOT NULL, trigger_type TEXT NOT NULL, trigger_value TEXT NOT NULL,
                    prompt TEXT NOT NULL, description TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active',
                    bot_id TEXT NOT NULL);",
            )
            .unwrap();
        }
        let queue = RerunQueue::new(Arc::clone(&conn));
        let checkpoints = CheckpointStore::new(Arc::clone(&conn));
        checkpoints.ensure_schema_async().await.unwrap();
        let store = ScheduledTaskStore::new(conn);
        (queue, checkpoints, store)
    }

    fn cp(iteration: u32, kind: CheckpointStopKind) -> TranscriptCheckpoint {
        TranscriptCheckpoint::from_messages(
            vec![ChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("partial".into())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            iteration,
            kind,
        )
    }

    async fn eligible_minutes(queue: &RerunQueue, id: &str) -> f64 {
        let conn = queue.connection();
        let c = conn.lock().await;
        c.query_row(
            "SELECT (julianday(next_eligible_at) - julianday('now')) * 24 * 60
             FROM pending_reruns WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn restore_tick_is_sixty_seconds_not_the_hourly_watchdog() {
        assert_eq!(RESTORE_TICK_SECS, 60);
        assert_ne!(RESTORE_TICK_SECS, 3600);
    }

    #[test]
    fn notice_is_informational_and_has_no_retry_control() {
        for kind in [
            CheckpointStopKind::Timeout,
            CheckpointStopKind::MaxIterations,
        ] {
            let note = auto_resume_notice(kind, "15 minutes").expect("notice");
            let lower = note.to_lowercase();
            assert!(lower.contains("auto-continue"), "{note}");
            assert!(!lower.contains("retry"), "{note}");
            assert!(!note.contains("/retry"), "{note}");
        }
        assert!(auto_resume_notice(CheckpointStopKind::GuardRewrite, "x").is_none());
        assert!(auto_resume_notice(CheckpointStopKind::MidRun, "x").is_none());
    }

    #[tokio::test]
    async fn timeout_bind_is_queued_now_and_not_a_human_gate() {
        let (queue, checkpoints, store) = fixture().await;
        let snap = cp(4, CheckpointStopKind::Timeout);
        let outcome = bind_stop_for_auto_resume(
            &queue,
            &checkpoints,
            &store,
            "task-1",
            "run-1",
            None,
            &snap,
            "15 minutes",
        )
        .await
        .unwrap();
        let BindOutcome::Ready(id) = outcome else {
            panic!("expected ready, got {outcome:?}");
        };
        let row = queue.get(&id).await.unwrap().unwrap();
        assert_eq!(row.state, RerunState::Queued);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.task_id, "task-1");
        assert!(row.fail_reason.starts_with("auto-resume: "));
        let mins = eligible_minutes(&queue, &id).await;
        assert!(mins < 1.0, "must not wait 30 minutes, got {mins}");
        assert!(
            queue.due().await.unwrap().is_empty(),
            "hourly due() must skip auto-resume"
        );
        let loaded = checkpoints.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.stop_kind, CheckpointStopKind::Timeout);
        assert_eq!(loaded.iteration, 4);
        assert_eq!(
            crate::scheduler::checkpoint::continue_iteration_budget(
                25,
                loaded.iteration,
                loaded.stop_kind
            ),
            21,
            "timeout auto-resume keeps remaining budget, not a full cap"
        );
    }

    #[tokio::test]
    async fn max_iterations_bind_keeps_full_cap_kind() {
        let (queue, checkpoints, store) = fixture().await;
        let snap = cp(25, CheckpointStopKind::MaxIterations);
        let BindOutcome::Ready(id) = bind_stop_for_auto_resume(
            &queue,
            &checkpoints,
            &store,
            "task-1",
            "run-1",
            None,
            &snap,
            "used 25 / remaining 0 / cap 25",
        )
        .await
        .unwrap() else {
            panic!("ready");
        };
        let loaded = checkpoints.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.stop_kind, CheckpointStopKind::MaxIterations);
        assert_eq!(
            crate::scheduler::checkpoint::continue_iteration_budget(
                25,
                loaded.iteration,
                loaded.stop_kind
            ),
            25,
            "IterationStall auto-resume gets the full configured cap"
        );
        let row = queue.get(&id).await.unwrap().unwrap();
        assert_eq!(row.state, RerunState::Queued);
        assert_ne!(row.state, RerunState::AwaitingUser);
    }

    #[tokio::test]
    async fn unverified_gate_blocks_auto_resume_even_with_a_checkpoint() {
        let (queue, checkpoints, store) = fixture().await;
        let gate = queue
            .enqueue_manual("task-1", "run-1", "result not verified")
            .await
            .unwrap();
        checkpoints
            .save_midrun("task-1", "run-1", &cp(3, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        let outcome = bind_stop_for_auto_resume(
            &queue,
            &checkpoints,
            &store,
            "task-1",
            "run-9",
            None,
            &cp(3, CheckpointStopKind::Timeout),
            "timeout",
        )
        .await
        .unwrap();
        assert_eq!(outcome, BindOutcome::Skip);
        let gate_row = queue.get(&gate).await.unwrap().unwrap();
        assert_eq!(gate_row.state, RerunState::AwaitingUser);
        let live = HashSet::new();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &live, true)
            .await
            .unwrap();
        assert!(
            ids.is_empty(),
            "unverified wins over an incomplete checkpoint"
        );
    }

    #[tokio::test]
    async fn process_restart_creates_a_queue_row_for_the_midrun_checkpoint() {
        let (queue, checkpoints, store) = fixture().await;
        let mut snap = cp(7, CheckpointStopKind::MidRun);
        snap.dangling_tool_ids.push("tool-inflight".into());
        checkpoints
            .save_midrun("task-1", "run-7", &snap)
            .await
            .unwrap();
        let live = HashSet::new();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &live, true)
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
        let row = queue.get(&ids[0]).await.unwrap().unwrap();
        assert_eq!(row.state, RerunState::Queued);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.task_id, "task-1");
        let mins = eligible_minutes(&queue, &ids[0]).await;
        assert!(
            mins < 1.0,
            "boot resume must not wait 30 minutes, got {mins}"
        );
        let loaded = checkpoints
            .get(&ids[0])
            .await
            .unwrap()
            .expect("keyed to queue id");
        assert_eq!(loaded.stop_kind, CheckpointStopKind::MidRun);
        assert_eq!(loaded.iteration, 7);
        assert!(loaded
            .dangling_tool_ids
            .iter()
            .any(|id| id == "tool-inflight"));
        assert_eq!(
            crate::scheduler::checkpoint::continue_iteration_budget(
                25,
                loaded.iteration,
                loaded.stop_kind
            ),
            18,
            "process-restart continue does not get a full cap"
        );
        assert!(
            checkpoints.get("midrun:run-7").await.unwrap().is_none(),
            "continue loads the queue id, not a second copy"
        );
    }

    #[tokio::test]
    async fn no_checkpoint_and_guard_rewrite_do_not_restore() {
        let (queue, checkpoints, store) = fixture().await;
        checkpoints
            .save_with_task(
                "gate-1",
                Some("run-g"),
                Some("task-guard"),
                &cp(2, CheckpointStopKind::GuardRewrite),
            )
            .await
            .unwrap();
        let live = HashSet::new();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &live, true)
            .await
            .unwrap();
        assert!(
            ids.is_empty(),
            "no full-replay and no unverified auto-resume"
        );
    }

    #[tokio::test]
    async fn reset_inflight_and_due_leave_auto_resume_alone() {
        let (queue, _checkpoints, _store) = fixture().await;
        let auto_id = queue
            .enqueue_auto_resume("task-1", "run-1", "timeout")
            .await
            .unwrap();
        let transient = queue.enqueue("task-2", "run-2", "429").await.unwrap();
        {
            let conn = queue.connection();
            let c = conn.lock().await;
            c.execute(
                "UPDATE pending_reruns SET next_eligible_at = datetime('now', '-1 minute') WHERE id = ?1",
                rusqlite::params![transient],
            )
            .unwrap();
        }
        let due = queue.due().await.unwrap();
        assert!(due.iter().any(|row| row.id == transient));
        assert!(
            due.iter().all(|row| row.id != auto_id),
            "hourly due() must not dispatch auto-resume"
        );
        {
            let conn = queue.connection();
            let c = conn.lock().await;
            c.execute(
                "UPDATE pending_reruns SET attempts = 2 WHERE id IN (?1, ?2)",
                rusqlite::params![auto_id, transient],
            )
            .unwrap();
        }
        let n = queue.reset_inflight_on_boot().await.unwrap();
        assert_eq!(n, 1, "only the transient claim is reset");
        let auto = queue.get(&auto_id).await.unwrap().unwrap();
        assert_eq!(
            auto.attempts, 2,
            "auto-resume must not inherit the +30 minute reset"
        );
        let transient_row = queue.get(&transient).await.unwrap().unwrap();
        assert_eq!(transient_row.attempts, 0);
        let mins = eligible_minutes(&queue, &transient).await;
        assert!(
            (mins - 30.0).abs() < 1.0,
            "transient reset keeps the +30 minute delay, got {mins}"
        );
    }

    #[tokio::test]
    async fn cron_and_human_retry_do_not_start_a_second_run() {
        let (queue, checkpoints, store) = fixture().await;
        let BindOutcome::Ready(_) = bind_stop_for_auto_resume(
            &queue,
            &checkpoints,
            &store,
            "task-1",
            "run-1",
            None,
            &cp(1, CheckpointStopKind::Timeout),
            "timeout",
        )
        .await
        .unwrap() else {
            panic!("ready");
        };
        let live = HashSet::new();
        assert!(fresh_cron_blocked(&queue, &store, &live, "task-1")
            .await
            .unwrap());
        assert!(!fresh_cron_blocked(&queue, &store, &live, "task-2")
            .await
            .unwrap());

        queue
            .enqueue_manual("task-3", "run-3", "unverified")
            .await
            .unwrap();
        assert!(fresh_cron_blocked(&queue, &store, &live, "task-3")
            .await
            .unwrap());

        {
            let conn = store.connection();
            let c = conn.lock().await;
            c.execute(
                "INSERT INTO scheduled_task_runs (id, task_id, run_at, status)
                 VALUES ('run-live', 'task-4', '2026-11-15T00:00:00', 'running')",
                [],
            )
            .unwrap();
        }
        assert!(fresh_cron_blocked(&queue, &store, &live, "task-4")
            .await
            .unwrap());
        assert!(human_retry_blocked(&store, &live, "task-4").await.unwrap());
        let mut holding = HashSet::new();
        holding.insert("task-5".into());
        assert!(human_retry_blocked(&store, &holding, "task-5")
            .await
            .unwrap());
        assert!(!human_retry_blocked(&store, &live, "task-5").await.unwrap());
    }

    #[tokio::test]
    async fn tick_does_not_heal_an_inflight_claim_startup_does() {
        let (queue, checkpoints, store) = fixture().await;
        let id = queue
            .enqueue_auto_resume("task-1", "run-1", "timeout")
            .await
            .unwrap();
        checkpoints
            .save_with_task(
                &id,
                Some("run-1"),
                Some("task-1"),
                &cp(2, CheckpointStopKind::Timeout),
            )
            .await
            .unwrap();
        queue.mark_dispatched(&id).await.unwrap();
        let live = HashSet::new();
        let tick = queue_ids_to_restore(&queue, &checkpoints, &store, &live, false)
            .await
            .unwrap();
        assert!(
            tick.is_empty(),
            "in-flight auto-resume is not dispatched again"
        );
        let row = queue.get(&id).await.unwrap().unwrap();
        assert_eq!(row.attempts, 1);
        let startup = queue_ids_to_restore(&queue, &checkpoints, &store, &live, true)
            .await
            .unwrap();
        assert_eq!(startup, vec![id.clone()]);
        let healed = queue.get(&id).await.unwrap().unwrap();
        assert_eq!(healed.attempts, 0);
        let mins = eligible_minutes(&queue, &id).await;
        assert!(
            mins < 1.0,
            "startup heal must not wait 30 minutes, got {mins}"
        );
    }

    #[tokio::test]
    async fn tick_does_not_rewrite_an_inflight_midrun_checkpoint() {
        let (queue, checkpoints, store) = fixture().await;
        let id = queue
            .enqueue_auto_resume("task-1", "run-1", "timeout")
            .await
            .unwrap();
        checkpoints
            .save_with_task(
                &id,
                Some("run-1"),
                Some("task-1"),
                &cp(2, CheckpointStopKind::Timeout),
            )
            .await
            .unwrap();
        queue.mark_dispatched(&id).await.unwrap();
        checkpoints
            .save_midrun("task-1", "run-2", &cp(3, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        let tick = queue_ids_to_restore(&queue, &checkpoints, &store, &HashSet::new(), false)
            .await
            .unwrap();
        assert!(tick.is_empty());
        assert!(checkpoints.get("midrun:run-2").await.unwrap().is_some());
        assert_eq!(queue.get(&id).await.unwrap().unwrap().attempts, 1);
    }

    #[tokio::test]
    async fn restore_uses_the_interrupted_run_when_the_queue_row_names_it() {
        let (queue, checkpoints, store) = fixture().await;
        let id = queue
            .enqueue_auto_resume("task-1", "run-old", "process restart")
            .await
            .unwrap();
        checkpoints
            .save_midrun("task-1", "run-old", &cp(4, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        checkpoints
            .save_midrun("task-1", "run-other", &cp(9, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &HashSet::new(), false)
            .await
            .unwrap();
        assert_eq!(ids, vec![id.clone()]);
        let loaded = checkpoints.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.iteration, 4);
        assert_eq!(loaded.stop_kind, CheckpointStopKind::MidRun);
    }

    #[tokio::test]
    async fn live_worker_and_running_row_block_restore_without_a_checkpoint_replay() {
        let (queue, checkpoints, store) = fixture().await;
        checkpoints
            .save_midrun("task-1", "run-1", &cp(2, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        let runs = LiveScheduledRuns::new();
        let hold = runs.hold("task-1");
        let live = runs.snapshot();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &live, true)
            .await
            .unwrap();
        assert!(ids.is_empty());
        drop(hold);
        {
            let conn = store.connection();
            let c = conn.lock().await;
            c.execute(
                "INSERT INTO scheduled_task_runs (id, task_id, run_at, status)
                 VALUES ('run-1', 'task-1', '2026-11-15T00:00:00', 'running')",
                [],
            )
            .unwrap();
        }
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &HashSet::new(), true)
            .await
            .unwrap();
        assert!(ids.is_empty(), "a live running row is not a second resume");
    }

    #[tokio::test]
    async fn clear_on_cancel_removes_the_checkpoint_so_restore_is_empty() {
        let (queue, checkpoints, store) = fixture().await;
        checkpoints
            .save_midrun("task-1", "run-1", &cp(2, CheckpointStopKind::MidRun))
            .await
            .unwrap();
        checkpoints.clear_task("task-1").await.unwrap();
        let ids = queue_ids_to_restore(&queue, &checkpoints, &store, &HashSet::new(), true)
            .await
            .unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn existing_queue_id_is_reused_for_continue() {
        let (queue, checkpoints, store) = fixture().await;
        let existing = queue
            .enqueue_auto_resume("task-1", "run-1", "first")
            .await
            .unwrap();
        queue.mark_dispatched(&existing).await.unwrap();
        let BindOutcome::Ready(id) = bind_stop_for_auto_resume(
            &queue,
            &checkpoints,
            &store,
            "task-1",
            "run-2",
            Some(&existing),
            &cp(9, CheckpointStopKind::MaxIterations),
            "used 9 / remaining 0 / cap 25",
        )
        .await
        .unwrap() else {
            panic!("ready");
        };
        assert_eq!(id, existing, "continue stays on the same queue id");
        let row = queue.get(&id).await.unwrap().unwrap();
        assert_eq!(row.attempts, 0);
        assert_eq!(row.state, RerunState::Queued);
        let loaded = checkpoints.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.iteration, 9);
        assert_eq!(loaded.stop_kind, CheckpointStopKind::MaxIterations);
    }
}
