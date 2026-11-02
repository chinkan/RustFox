//! Scheduler transcript checkpoints (ADR-0039, amended by ADR-0054 / #360).
//!
//! Written on the wall-clock timeout human-gate, and also on the
//! max-iterations and guard-rewrite gates (#331). Retry with a checkpoint
//! continues the agent loop from the saved transcript; Cancel clears it.
//! No checkpoint → ADR-0013 full replay with a full iteration budget.
//!
//! The row stores *why* it was written (`stop_kind`). Human Retry of a
//! max-iterations / IterationStall checkpoint gets a fresh full
//! `max_iterations`. Timeout, guard-rewrite, and other checkpoints keep
//! `max(cap − used, 1)` (#360, ADR-0054 Decision 4).

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::llm::{ChatMessage, MessageContent};

/// Operator-facing / persisted interrupted-tool stub (ADR-0039 Q6/Q7).
pub const INTERRUPTED_TOOL_RESULT: &str =
    "Interrupted: tool call cancelled by run timeout (not re-executed).";

/// Why a transcript checkpoint was written. Continue budget reads this
/// instead of guessing from `iteration >= cap` (#360).
///
/// Older rows and unknown stored values are [`CheckpointStopKind::Other`]
/// and keep the remaining-budget formula.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStopKind {
    /// Agent loop returned [`crate::loop_runner::LoopOutcome::MaxIterations`].
    MaxIterations,
    /// Wall-clock timeout (scheduled cancel snapshot).
    Timeout,
    /// Guard replaced the final reply and the run was not verified.
    GuardRewrite,
    /// Legacy row, or a stop that is not one of the three gates above.
    #[default]
    Other,
}

impl CheckpointStopKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxIterations => "max_iterations",
            Self::Timeout => "timeout",
            Self::GuardRewrite => "guard_rewrite",
            Self::Other => "other",
        }
    }

    /// Unknown text stays [`Self::Other`] so a typo does not grant a full budget.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "max_iterations" => Self::MaxIterations,
            "timeout" => Self::Timeout,
            "guard_rewrite" => Self::GuardRewrite,
            _ => Self::Other,
        }
    }
}

/// Map a scheduled loop stop onto the persisted continue-budget kind.
pub fn stop_kind_from_scheduled(stop: ScheduledLoopStop) -> CheckpointStopKind {
    match stop {
        ScheduledLoopStop::MaxIterations { .. } => CheckpointStopKind::MaxIterations,
        ScheduledLoopStop::Cancelled { .. } => CheckpointStopKind::Timeout,
        ScheduledLoopStop::Final {
            rewritten: true, ..
        } => CheckpointStopKind::GuardRewrite,
        ScheduledLoopStop::Final {
            rewritten: false, ..
        } => CheckpointStopKind::Other,
    }
}

/// Iteration budget for one transcript continue (#360 / ADR-0054 D4).
///
/// `MaxIterations` is a fresh full `cap` (the configured `max_iterations`).
/// Every other kind keeps `max(cap − used, 1)`.
pub fn continue_iteration_budget(cap: u32, used: u32, kind: CheckpointStopKind) -> u32 {
    match kind {
        CheckpointStopKind::MaxIterations => cap,
        CheckpointStopKind::Timeout
        | CheckpointStopKind::GuardRewrite
        | CheckpointStopKind::Other => cap.saturating_sub(used).max(1),
    }
}

/// `None` is a missing checkpoint: full replay with the full configured cap
/// (ADR-0013). `Some` uses [`continue_iteration_budget`].
pub fn continue_iteration_budget_or_full(
    cap: u32,
    checkpoint: Option<&TranscriptCheckpoint>,
) -> u32 {
    match checkpoint {
        Some(cp) => continue_iteration_budget(cap, cp.iteration, cp.stop_kind),
        None => cap,
    }
}

/// Used / remaining / cap for a run that just stopped on max iterations.
///
/// `prior` is the checkpoint this run continued from. `None` is a fresh run
/// or a full replay. `used` is the loop budget that was granted (and then
/// exhausted). `remaining` is 0 because the loop returned MaxIterations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IterationStallAccounting {
    pub used: u32,
    pub remaining: u32,
    pub cap: u32,
}

pub fn accounting_after_max_iterations(
    cap: u32,
    prior: Option<&TranscriptCheckpoint>,
) -> IterationStallAccounting {
    let used = continue_iteration_budget_or_full(cap, prior);
    IterationStallAccounting {
        used,
        remaining: 0,
        cap,
    }
}

/// Shared operator phrase. Never a bare config cap by itself.
pub fn format_iteration_stall_detail(used: u32, remaining: u32, cap: u32) -> String {
    format!("used {used} / remaining {remaining} / cap {cap}")
}

/// Chat and schedule sentence when the loop exhausts its budget.
pub fn max_iterations_user_text(used: u32, remaining: u32, cap: u32) -> String {
    format!(
        "I've reached the maximum number of tool call iterations ({}). Please try rephrasing your request.",
        format_iteration_stall_detail(used, remaining, cap)
    )
}

/// Run-row / queue `fail_reason` for an IterationStall gate.
pub fn max_iterations_run_reason(used: u32, remaining: u32, cap: u32) -> String {
    format!(
        "Reached max iterations ({}) without a final response",
        format_iteration_stall_detail(used, remaining, cap)
    )
}

/// Transcript snapshot for restart-safe continue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptCheckpoint {
    pub messages: Vec<ChatMessage>,
    pub iteration: u32,
    pub dangling_tool_ids: Vec<String>,
    /// Explicit gate kind. Not derived from `iteration` vs the cap.
    /// Missing on an older serialized value stays [`CheckpointStopKind::Other`].
    #[serde(default)]
    pub stop_kind: CheckpointStopKind,
}

impl TranscriptCheckpoint {
    /// Build from in-flight loop messages; compute dangling tool ids.
    pub fn from_messages(
        messages: Vec<ChatMessage>,
        iteration: u32,
        stop_kind: CheckpointStopKind,
    ) -> Self {
        let dangling_tool_ids = find_dangling_tool_ids(&messages);
        Self {
            messages,
            iteration,
            dangling_tool_ids,
            stop_kind,
        }
    }

    /// Messages ready for the next model turn: dangling ids get a safe
    /// interrupted tool-result so the list stays well-formed.
    pub fn messages_for_continue(&self) -> Vec<ChatMessage> {
        let mut out = self.messages.clone();
        synthesize_interrupted_results(&mut out, &self.dangling_tool_ids);
        out
    }
}

/// Tool-call ids that have an assistant `tool_calls` entry but no tool result.
pub fn find_dangling_tool_ids(messages: &[ChatMessage]) -> Vec<String> {
    let answered: HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    let mut dangling = Vec::new();
    let mut seen = HashSet::new();
    for msg in messages {
        if let Some(calls) = &msg.tool_calls {
            for tc in calls {
                if !answered.contains(tc.id.as_str()) && seen.insert(tc.id.clone()) {
                    dangling.push(tc.id.clone());
                }
            }
        }
    }
    dangling
}

/// Append interrupted stubs for dangling ids that still lack a tool result.
pub fn synthesize_interrupted_results(messages: &mut Vec<ChatMessage>, dangling: &[String]) {
    let answered: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    for id in dangling {
        if answered.contains(id) {
            continue;
        }
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(INTERRUPTED_TOOL_RESULT.to_string())),
            tool_calls: None,
            tool_call_id: Some(id.clone()),
            reasoning_content: None,
        });
    }
}

/// Which scheduled loop stop this is, for transcript capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledLoopStop {
    /// `rewritten` is set when a guard replaced the model text.
    Final {
        rewritten: bool,
        iterations: u32,
    },
    Cancelled {
        iterations: u32,
    },
    MaxIterations {
        iterations: u32,
    },
}

/// Iteration to store, if this stop should capture a transcript.
///
/// Chat (`is_scheduled == false`) and a clean final reply return `None`.
/// Timeout / cancel, max-iterations, and a guard rewrite return the
/// iteration the runner should persist (ADR-0039 / ADR-0054).
pub fn scheduled_transcript_iteration(is_scheduled: bool, stop: ScheduledLoopStop) -> Option<u32> {
    if !is_scheduled {
        return None;
    }
    match stop {
        ScheduledLoopStop::Final {
            rewritten,
            iterations,
        } => rewritten.then_some(iterations),
        ScheduledLoopStop::Cancelled { iterations }
        | ScheduledLoopStop::MaxIterations { iterations } => Some(iterations),
    }
}

/// SQLite-backed store keyed by pending_reruns / queue id.
#[derive(Clone)]
pub struct CheckpointStore {
    conn: Arc<Mutex<Connection>>,
}

impl CheckpointStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Idempotent schema for production boot and in-memory tests.
    pub fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduler_transcript_checkpoints (
                queue_id TEXT PRIMARY KEY,
                run_id TEXT,
                messages_json TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                dangling_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                stop_kind TEXT NOT NULL DEFAULT 'other'
            );",
        )
        .context("create scheduler_transcript_checkpoints")?;
        // Databases created before #360 have the table and no stop_kind column.
        // CREATE IF NOT EXISTS does not add it. Unknown / missing values load
        // as `other` and keep the remaining-budget formula.
        let mut stmt = conn
            .prepare("PRAGMA table_info(scheduler_transcript_checkpoints)")
            .context("checkpoint schema")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("read checkpoint columns")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect checkpoint columns")?;
        if !cols.iter().any(|c| c == "stop_kind") {
            conn.execute_batch(
                "ALTER TABLE scheduler_transcript_checkpoints
                 ADD COLUMN stop_kind TEXT NOT NULL DEFAULT 'other';",
            )
            .context("add scheduler_transcript_checkpoints.stop_kind")?;
        }
        Ok(())
    }

    pub async fn ensure_schema_async(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)
    }

    /// Upsert checkpoint for a human-gate queue row.
    ///
    /// Callers: wall-clock timeout (ADR-0039), max-iterations, and guard
    /// rewrite (ADR-0054 / #331).
    pub async fn save(
        &self,
        queue_id: &str,
        run_id: Option<&str>,
        checkpoint: &TranscriptCheckpoint,
    ) -> Result<()> {
        let messages_json =
            serde_json::to_string(&checkpoint.messages).context("serialize checkpoint messages")?;
        let dangling_json = serde_json::to_string(&checkpoint.dangling_tool_ids)
            .context("serialize dangling tool ids")?;
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        conn.execute(
            "INSERT INTO scheduler_transcript_checkpoints
             (queue_id, run_id, messages_json, iteration, dangling_json, stop_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(queue_id) DO UPDATE SET
               run_id = excluded.run_id,
               messages_json = excluded.messages_json,
               iteration = excluded.iteration,
               dangling_json = excluded.dangling_json,
               stop_kind = excluded.stop_kind,
               created_at = datetime('now')",
            rusqlite::params![
                queue_id,
                run_id,
                messages_json,
                checkpoint.iteration as i64,
                dangling_json,
                checkpoint.stop_kind.as_str()
            ],
        )
        .context("upsert transcript checkpoint")?;
        Ok(())
    }

    /// [`Self::save`] when a snapshot exists. `Ok(false)` when there is nothing
    /// to resume (clean final reply, chat cancel, missing snapshot).
    pub async fn save_if_present(
        &self,
        queue_id: &str,
        run_id: Option<&str>,
        checkpoint: Option<&TranscriptCheckpoint>,
    ) -> Result<bool> {
        let Some(checkpoint) = checkpoint else {
            return Ok(false);
        };
        self.save(queue_id, run_id, checkpoint).await?;
        Ok(true)
    }

    pub async fn get(&self, queue_id: &str) -> Result<Option<TranscriptCheckpoint>> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        let mut stmt = conn.prepare(
            "SELECT messages_json, iteration, dangling_json, stop_kind
             FROM scheduler_transcript_checkpoints WHERE queue_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![queue_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let messages_json: String = row.get(0)?;
        let iteration: i64 = row.get(1)?;
        let dangling_json: String = row.get(2)?;
        let stop_kind_raw: String = row.get(3)?;
        let messages: Vec<ChatMessage> =
            serde_json::from_str(&messages_json).context("deserialize checkpoint messages")?;
        let dangling_tool_ids: Vec<String> =
            serde_json::from_str(&dangling_json).context("deserialize dangling ids")?;
        Ok(Some(TranscriptCheckpoint {
            messages,
            iteration: iteration as u32,
            dangling_tool_ids,
            stop_kind: CheckpointStopKind::parse(&stop_kind_raw),
        }))
    }

    /// Load and delete — used when Retry dispatches continue (consume).
    pub async fn take(&self, queue_id: &str) -> Result<Option<TranscriptCheckpoint>> {
        let cp = self.get(queue_id).await?;
        if cp.is_some() {
            self.clear(queue_id).await?;
        }
        Ok(cp)
    }

    pub async fn clear(&self, queue_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        let n = conn
            .execute(
                "DELETE FROM scheduler_transcript_checkpoints WHERE queue_id = ?1",
                rusqlite::params![queue_id],
            )
            .context("delete transcript checkpoint")?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "x".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn find_dangling_and_synthesize() {
        let msgs = vec![
            ChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::Text("hi".into())),
                tool_calls: Some(vec![tool_call("a"), tool_call("b")]),
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(MessageContent::Text("ok".into())),
                tool_calls: None,
                tool_call_id: Some("a".into()),
                reasoning_content: None,
            },
        ];
        let dangling = find_dangling_tool_ids(&msgs);
        assert_eq!(dangling, vec!["b".to_string()]);
        let cp = TranscriptCheckpoint::from_messages(msgs, 3, CheckpointStopKind::Timeout);
        assert_eq!(cp.iteration, 3);
        assert_eq!(cp.dangling_tool_ids, vec!["b".to_string()]);
        let cont = cp.messages_for_continue();
        assert_eq!(cont.len(), 3);
        assert_eq!(cont[2].tool_call_id.as_deref(), Some("b"));
        assert!(cont[2]
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("not re-executed"));
        assert!(find_dangling_tool_ids(&cont).is_empty());
    }

    #[tokio::test]
    async fn save_get_take_clear() {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let store = CheckpointStore::new(conn);
        store.ensure_schema_async().await.unwrap();
        let cp = TranscriptCheckpoint {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("p".into())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            iteration: 2,
            dangling_tool_ids: vec!["d1".into()],
            stop_kind: CheckpointStopKind::Timeout,
        };
        store.save("qid-1", Some("run-1"), &cp).await.unwrap();
        let loaded = store.get("qid-1").await.unwrap().unwrap();
        assert_eq!(loaded.iteration, cp.iteration);
        assert_eq!(loaded.dangling_tool_ids, cp.dangling_tool_ids);
        assert_eq!(loaded.stop_kind, CheckpointStopKind::Timeout);
        assert_eq!(loaded.messages.len(), 1);
        let taken = store.take("qid-1").await.unwrap().unwrap();
        assert_eq!(taken.iteration, cp.iteration);
        assert_eq!(taken.dangling_tool_ids, cp.dangling_tool_ids);
        assert!(store.get("qid-1").await.unwrap().is_none());

        store.save("qid-2", None, &cp).await.unwrap();
        assert!(store.clear("qid-2").await.unwrap());
        assert!(store.get("qid-2").await.unwrap().is_none());
        assert!(!store.clear("qid-2").await.unwrap());
    }

    #[tokio::test]
    async fn save_if_present_skips_missing_snapshot() {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let store = CheckpointStore::new(conn);
        assert!(!store
            .save_if_present("qid", Some("run"), None)
            .await
            .unwrap());
        assert!(store.get("qid").await.unwrap().is_none());
        let cp = TranscriptCheckpoint {
            messages: vec![],
            iteration: 4,
            dangling_tool_ids: vec![],
            stop_kind: CheckpointStopKind::GuardRewrite,
        };
        assert!(store
            .save_if_present("qid", Some("run"), Some(&cp))
            .await
            .unwrap());
        assert_eq!(store.get("qid").await.unwrap().unwrap().iteration, 4);
        assert_eq!(
            store.get("qid").await.unwrap().unwrap().stop_kind,
            CheckpointStopKind::GuardRewrite
        );
    }

    fn checkpoint(iteration: u32, kind: CheckpointStopKind) -> TranscriptCheckpoint {
        TranscriptCheckpoint {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("partial".into())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            iteration,
            dangling_tool_ids: vec!["dangling".into()],
            stop_kind: kind,
        }
    }

    #[test]
    fn max_iterations_retry_budget_is_the_full_configured_cap() {
        // Live shape: deploy cap 100, checkpoint already at the cap.
        // Old D4 would resume with 1. #360 resumes with the full cap.
        let cap = 100u32;
        let prior = checkpoint(cap, CheckpointStopKind::MaxIterations);
        assert_eq!(
            continue_iteration_budget(cap, prior.iteration, prior.stop_kind),
            cap
        );
        // used just under the cap must not fall through to remaining = 1.
        assert_eq!(
            continue_iteration_budget(25, 24, CheckpointStopKind::MaxIterations),
            25
        );
        // A second stall stores a cumulative iteration past the cap.
        // Retry is still a fresh full budget, not max(cap - 50, 1).
        assert_eq!(
            continue_iteration_budget(25, 50, CheckpointStopKind::MaxIterations),
            25
        );
        let accounting = accounting_after_max_iterations(cap, Some(&prior));
        assert_eq!(
            accounting,
            IterationStallAccounting {
                used: cap,
                remaining: 0,
                cap,
            }
        );
        // Transcript continue still closes dangling tool ids. Only the budget changes.
        let continued = prior.messages_for_continue();
        assert!(find_dangling_tool_ids(&continued).is_empty());
        assert!(continued
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("dangling")));
    }

    #[test]
    fn timeout_and_guard_continue_keep_remaining_budget() {
        let cap = 25u32;
        // used == cap must NOT look like a max-iterations reset.
        assert_eq!(
            continue_iteration_budget(cap, cap, CheckpointStopKind::Timeout),
            1
        );
        assert_eq!(
            continue_iteration_budget(cap, cap, CheckpointStopKind::GuardRewrite),
            1
        );
        assert_eq!(
            continue_iteration_budget(cap, cap, CheckpointStopKind::Other),
            1
        );
        assert_eq!(
            continue_iteration_budget(cap, 10, CheckpointStopKind::Timeout),
            15
        );
        assert_eq!(
            continue_iteration_budget(cap, 10, CheckpointStopKind::GuardRewrite),
            15
        );
        assert_eq!(
            continue_iteration_budget(cap, 24, CheckpointStopKind::Timeout),
            1
        );
        let prior = checkpoint(24, CheckpointStopKind::Timeout);
        assert_eq!(
            accounting_after_max_iterations(cap, Some(&prior)),
            IterationStallAccounting {
                used: 1,
                remaining: 0,
                cap,
            }
        );
    }

    #[test]
    fn missing_checkpoint_is_a_full_replay_budget() {
        assert_eq!(continue_iteration_budget_or_full(25, None), 25);
        assert_eq!(continue_iteration_budget_or_full(100, None), 100);
        assert_eq!(
            accounting_after_max_iterations(25, None),
            IterationStallAccounting {
                used: 25,
                remaining: 0,
                cap: 25,
            }
        );
    }

    #[test]
    fn iteration_stall_copy_states_used_remaining_and_cap() {
        let detail = format_iteration_stall_detail(1, 0, 100);
        assert!(detail.contains("used 1"), "{detail}");
        assert!(detail.contains("remaining 0"), "{detail}");
        assert!(detail.contains("cap 100"), "{detail}");
        assert_ne!(detail, "100");
        assert!(!detail.contains("(100)"), "{detail}");

        let reason = max_iterations_run_reason(1, 0, 100);
        let reply = max_iterations_user_text(1, 0, 100);
        assert!(reason.contains(&detail), "{reason}");
        assert!(reply.contains(&detail), "{reply}");
        assert!(reply.contains("maximum number of tool call iterations"));
        assert!(!reason.contains("(100)"), "{reason}");
        assert!(!reply.contains("cap (100)"), "{reply}");
    }

    #[test]
    fn adr_0054_decision_4_grants_a_full_max_iterations_retry() {
        let adr = include_str!("../../docs/adr/0054-scheduler-cross-run-progress.md");
        assert!(
            adr.contains("sets the next loop's `max_iterations` to the full configured cap"),
            "Decision 4 must grant a full cap on max-iterations Retry"
        );
        assert!(adr.contains("does not raise the default `max_iterations` (25)"));
        assert!(
            adr.contains("Timeout, guard-rewrite, and other checkpoints keep"),
            "other continues keep the remaining formula"
        );
        assert!(
            !adr.contains("resumes with one model turn"),
            "Decision 4 must not keep the old one-turn resume"
        );
        assert!(
            !adr.contains("still get only the remaining iteration budget"),
            "Consequences must not say max-iterations Retry is remaining-only"
        );
    }

    #[test]
    fn stop_kind_parse_rejects_unknown_as_other() {
        assert_eq!(
            CheckpointStopKind::parse("max_iterations"),
            CheckpointStopKind::MaxIterations
        );
        assert_eq!(
            CheckpointStopKind::parse("timeout"),
            CheckpointStopKind::Timeout
        );
        assert_eq!(
            CheckpointStopKind::parse("guard_rewrite"),
            CheckpointStopKind::GuardRewrite
        );
        assert_eq!(
            CheckpointStopKind::parse("other"),
            CheckpointStopKind::Other
        );
        assert_eq!(
            CheckpointStopKind::parse("MaxIterations"),
            CheckpointStopKind::Other
        );
        assert_eq!(CheckpointStopKind::parse(""), CheckpointStopKind::Other);
    }

    #[test]
    fn scheduled_stop_maps_to_explicit_kind() {
        assert_eq!(
            stop_kind_from_scheduled(ScheduledLoopStop::MaxIterations { iterations: 25 }),
            CheckpointStopKind::MaxIterations
        );
        assert_eq!(
            stop_kind_from_scheduled(ScheduledLoopStop::Cancelled { iterations: 3 }),
            CheckpointStopKind::Timeout
        );
        assert_eq!(
            stop_kind_from_scheduled(ScheduledLoopStop::Final {
                rewritten: true,
                iterations: 2
            }),
            CheckpointStopKind::GuardRewrite
        );
        assert_eq!(
            stop_kind_from_scheduled(ScheduledLoopStop::Final {
                rewritten: false,
                iterations: 2
            }),
            CheckpointStopKind::Other
        );
    }

    #[tokio::test]
    async fn loaded_max_iterations_checkpoint_grants_full_budget() {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let store = CheckpointStore::new(conn);
        let cap = 100u32;
        let cp = checkpoint(cap, CheckpointStopKind::MaxIterations);
        store.save("qid", Some("run"), &cp).await.unwrap();
        let loaded = store.get("qid").await.unwrap().unwrap();
        assert_eq!(loaded.stop_kind, CheckpointStopKind::MaxIterations);
        assert_eq!(loaded.iteration, cap);
        assert_eq!(
            continue_iteration_budget(cap, loaded.iteration, loaded.stop_kind),
            cap,
            "Retry after IterationStall must set the loop budget to config.max_iterations()"
        );
        assert_eq!(loaded.messages.len(), cp.messages.len());
        assert_eq!(loaded.dangling_tool_ids, cp.dangling_tool_ids);
    }

    #[tokio::test]
    async fn legacy_checkpoint_without_stop_kind_keeps_remaining_budget() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scheduler_transcript_checkpoints (
                queue_id TEXT PRIMARY KEY,
                run_id TEXT,
                messages_json TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                dangling_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scheduler_transcript_checkpoints
             (queue_id, messages_json, iteration, dangling_json)
             VALUES ('q', '[]', 25, '[]')",
            [],
        )
        .unwrap();
        let store = CheckpointStore::new(Arc::new(Mutex::new(conn)));
        let loaded = store.get("q").await.unwrap().unwrap();
        assert_eq!(loaded.stop_kind, CheckpointStopKind::Other);
        assert_eq!(loaded.iteration, 25);
        assert_eq!(
            continue_iteration_budget(25, loaded.iteration, loaded.stop_kind),
            1
        );
    }

    #[test]
    fn transcript_capture_is_timeout_max_iterations_and_guard_only() {
        use ScheduledLoopStop::*;
        assert_eq!(
            scheduled_transcript_iteration(false, MaxIterations { iterations: 25 }),
            None
        );
        assert_eq!(
            scheduled_transcript_iteration(true, MaxIterations { iterations: 25 }),
            Some(25)
        );
        assert_eq!(
            scheduled_transcript_iteration(true, Cancelled { iterations: 3 }),
            Some(3)
        );
        assert_eq!(
            scheduled_transcript_iteration(
                true,
                Final {
                    rewritten: false,
                    iterations: 2
                }
            ),
            None
        );
        assert_eq!(
            scheduled_transcript_iteration(
                true,
                Final {
                    rewritten: true,
                    iterations: 2
                }
            ),
            Some(2)
        );
        assert_eq!(
            scheduled_transcript_iteration(
                false,
                Final {
                    rewritten: true,
                    iterations: 2
                }
            ),
            None
        );
    }
}
