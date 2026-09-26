//! Dead-letter re-run queue for scheduled tasks (ADR-0013).
//!
//! When a scheduled run dies on a *transient* LLM failure (429/5xx — after
//! the ADR-0009 provider budget and the ADR-0012 fallback chain were both
//! spent), the job is worth re-firing *later*, not resending now: upstream
//! shared-pool congestion runs in tens of minutes.
//!
//! Two-strike semantics (Kan's grill answer Q3):
//!   first death  → `queued`, the watchdog auto re-fires it once (attempts=1);
//!   second death → `awaiting_user`, a DM asks Kan — never auto again.
//! Re-fire is a full agent-loop replay (LLM state is not resumable), which is
//! why the second strike hands over to a human: side-effect judgement
//! ("did it half-publish?") is not something the queue can make.
//!
//! `pending_reruns` is *control state*. `scheduled_task_runs` stays
//! append-only audit evidence and is never rewritten by this module.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Default wait before a queued re-fire becomes eligible.
pub const ELIGIBILITY_DELAY_MINUTES: i64 = 30;
/// Awaiting-user rows abandoned (with a one-line DM) after this many days.
pub const AWAITING_USER_EXPIRY_DAYS: i64 = 7;
/// Terminal rows (done/abandoned/superseded) are purged after this many days.
pub const TERMINAL_RETENTION_DAYS: i64 = 30;

/// Queue row states. Mirrors CHECK constraint in the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunState {
    Queued,
    AwaitingUser,
    Abandoned,
    Done,
    Superseded,
}

impl RerunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::AwaitingUser => "awaiting_user",
            Self::Abandoned => "abandoned",
            Self::Done => "done",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingRerun {
    pub id: String,
    pub task_id: String,
    pub original_run_id: String,
    pub fail_reason: String,
    pub attempts: i64,
    pub state: RerunState,
    pub next_eligible_at: String,
    pub created_at: String,
}

/// Shared-connection handle, same pattern as [`ScheduledTaskStore`].
#[derive(Clone)]
pub struct RerunQueue {
    conn: Arc<Mutex<Connection>>,
}

impl RerunQueue {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Enqueue a dead run for later auto re-fire. Any live row for the same
    /// task is superseded first (latest definition wins — prompts are edited
    /// via portal/tools between strikes). Returns the new row id.
    pub async fn enqueue(
        &self,
        task_id: &str,
        original_run_id: &str,
        fail_reason: &str,
    ) -> Result<String> {
        self.insert_row(task_id, original_run_id, fail_reason, "queued", 0)
            .await
    }

    /// Enqueue a run that died on a *non-transient* stop (max-iterations) as
    /// a manual gate: `awaiting_user` from birth, so the watchdog NEVER
    /// auto re-fires it (replaying half-finished work risks duplicated side
    /// effects), but `/continue` can resume it and `/continue cancel` can
    /// drop it. The run is still recorded — the point is that the human
    /// learns about the death, not that we stay silent.
    pub async fn enqueue_manual(
        &self,
        task_id: &str,
        original_run_id: &str,
        fail_reason: &str,
    ) -> Result<String> {
        self.insert_row(task_id, original_run_id, fail_reason, "awaiting_user", 1)
            .await
    }

    async fn insert_row(
        &self,
        task_id: &str,
        original_run_id: &str,
        fail_reason: &str,
        state: &str,
        attempts: i64,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_reruns
             SET state = 'superseded', updated_at = datetime('now')
             WHERE task_id = ?1 AND state IN ('queued', 'awaiting_user')",
            rusqlite::params![task_id],
        )
        .context("Failed to supersede live rerun rows")?;
        conn.execute(
            "INSERT INTO pending_reruns
             (id, task_id, original_run_id, fail_reason, attempts, state, next_eligible_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                    datetime('now', ?7))",
            rusqlite::params![
                id,
                task_id,
                original_run_id,
                fail_reason,
                attempts,
                state,
                format!("+{ELIGIBILITY_DELAY_MINUTES} minutes")
            ],
        )
        .context("Failed to insert rerun row")?;
        Ok(id)
    }

    /// Rows eligible to fire now (state=queued, next_eligible_at in the past).
    pub async fn due(&self) -> Result<Vec<PendingRerun>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, original_run_id, fail_reason, attempts, state,
                        next_eligible_at, created_at
                 FROM pending_reruns
                 WHERE state = 'queued' AND next_eligible_at <= datetime('now')
                 ORDER BY next_eligible_at",
            )
            .context("Failed to prepare due query")?;
        let rows = stmt
            .query_map([], Self::row_to_pending)
            .context("Failed to query due reruns")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("Failed to deserialize due reruns")
    }

    /// Watchdog dispatches a row: bump attempts + push eligibility forward.
    /// Done *before* the send so a crash mid-dispatch can't strand an
    /// attempt counter at 0 → see `reset_inflight_on_boot` for the flip side.
    pub async fn mark_dispatched(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_reruns
             SET attempts = attempts + 1,
                 next_eligible_at = datetime('now', ?2),
                 updated_at = datetime('now')
             WHERE id = ?1 AND state = 'queued'",
            rusqlite::params![id, format!("+{ELIGIBILITY_DELAY_MINUTES} minutes")],
        )
        .context("Failed to bump rerun attempts")?;
        Ok(())
    }

    pub async fn mark_done(&self, id: &str) -> Result<()> {
        self.transition(id, RerunState::Done).await
    }

    pub async fn mark_awaiting_user(&self, id: &str, reason: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_reruns
             SET state = 'awaiting_user', fail_reason = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id, reason],
        )
        .context("Failed to move rerun to awaiting_user")?;
        Ok(())
    }

    pub async fn mark_abandoned(&self, id: &str) -> Result<()> {
        self.transition(id, RerunState::Abandoned).await
    }

    async fn transition(&self, id: &str, state: RerunState) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_reruns SET state = ?2, updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id, state.as_str()],
        )
        .with_context(|| format!("Failed to set rerun state {}", state.as_str()))?;
        Ok(())
    }

    /// Human gate `retry`: reset to queued with a fresh attempt budget.
    pub async fn retry(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE pending_reruns
                 SET state = 'queued', attempts = 0,
                     next_eligible_at = datetime('now', ?2), updated_at = datetime('now')
                 WHERE id = ?1 AND state IN ('awaiting_user', 'abandoned')",
                rusqlite::params![id, format!("+{ELIGIBILITY_DELAY_MINUTES} minutes")],
            )
            .context("Failed to retry rerun")?;
        Ok(n > 0)
    }

    /// Human gate `cancel`: abandon explicitly.
    pub async fn cancel(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE pending_reruns
                 SET state = 'abandoned', updated_at = datetime('now')
                 WHERE id = ?1 AND state IN ('queued', 'awaiting_user')",
                rusqlite::params![id],
            )
            .context("Failed to cancel rerun")?;
        Ok(n > 0)
    }

    pub async fn get(&self, id: &str) -> Result<Option<PendingRerun>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, original_run_id, fail_reason, attempts, state,
                        next_eligible_at, created_at
                 FROM pending_reruns WHERE id = ?1",
            )
            .context("Failed to prepare get query")?;
        let mut rows = stmt
            .query_map(rusqlite::params![id], Self::row_to_pending)
            .context("Failed to query rerun")?;
        match rows.next() {
            Some(Ok(r)) => Ok(Some(r)),
            Some(Err(e)) => Err(e).context("Failed to deserialize rerun"),
            None => Ok(None),
        }
    }

    pub async fn list_active(&self) -> Result<Vec<PendingRerun>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, original_run_id, fail_reason, attempts, state,
                        next_eligible_at, created_at
                 FROM pending_reruns
                 WHERE state IN ('queued', 'awaiting_user')
                 ORDER BY updated_at DESC",
            )
            .context("Failed to prepare list query")?;
        let rows = stmt
            .query_map([], Self::row_to_pending)
            .context("Failed to list reruns")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("Failed to deserialize reruns")
    }

    /// `awaiting_user` rows nobody answered within the expiry window →
    /// abandoned (returns ids so the caller can DM one-liners).
    pub async fn expire_awaiting(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id FROM pending_reruns
                     WHERE state = 'awaiting_user'
                       AND updated_at <= datetime('now', '-{AWAITING_USER_EXPIRY_DAYS} days')"
            ))
            .context("Failed to prepare expire query")?;
        let ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .context("Failed to scan expiring reruns")?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        for id in &ids {
            conn.execute(
                "UPDATE pending_reruns SET state = 'abandoned', updated_at = datetime('now')
                 WHERE id = ?1",
                rusqlite::params![id],
            )
            .context("Failed to abandon expired rerun")?;
        }
        Ok(ids)
    }

    /// Purge terminal rows older than the retention window.
    pub async fn purge_terminal(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                &format!(
                    "DELETE FROM pending_reruns
                     WHERE state IN ('done', 'abandoned', 'superseded')
                       AND updated_at <= datetime('now', '-{TERMINAL_RETENTION_DAYS} days')"
                ),
                [],
            )
            .context("Failed to purge terminal reruns")?;
        Ok(n)
    }

    /// Crash safety at boot: a queued row with attempts>0 means we dispatched
    /// a re-fire that never got its outcome persisted (process died mid-run).
    /// Reset the counter so the row keeps *auto*-attempt semantics — without
    /// this, a phantom consumed attempt would upgrade a later failure to
    /// "ask the human" for a re-fire that never actually happened.
    pub async fn reset_inflight_on_boot(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE pending_reruns
                 SET attempts = 0, next_eligible_at = datetime('now', ?1), updated_at = datetime('now')
                 WHERE state = 'queued' AND attempts > 0",
                rusqlite::params![format!("+{ELIGIBILITY_DELAY_MINUTES} minutes")],
            )
            .context("Failed to reset inflight reruns")?;
        Ok(n)
    }

    fn row_to_pending(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingRerun> {
        let state_str: String = row.get(5)?;
        let state = match state_str.as_str() {
            "queued" => RerunState::Queued,
            "awaiting_user" => RerunState::AwaitingUser,
            "abandoned" => RerunState::Abandoned,
            "done" => RerunState::Done,
            "superseded" => RerunState::Superseded,
            other => {
                tracing::warn!("Unknown rerun state '{other}', treating as queued");
                RerunState::Queued
            }
        };
        Ok(PendingRerun {
            id: row.get(0)?,
            task_id: row.get(1)?,
            original_run_id: row.get(2)?,
            fail_reason: row.get(3)?,
            attempts: row.get(4)?,
            state,
            next_eligible_at: row.get(6)?,
            created_at: row.get(7)?,
        })
    }
}

/// Compact JSON view for tool output (`rerun_queue list`).
#[derive(Serialize)]
pub struct RerunSummary {
    pub id: String,
    pub task_id: String,
    pub attempts: i64,
    pub state: &'static str,
    pub next_eligible_at: String,
    pub fail_reason: String,
}

impl From<&PendingRerun> for RerunSummary {
    fn from(r: &PendingRerun) -> Self {
        Self {
            id: r.id.clone(),
            task_id: r.task_id.clone(),
            attempts: r.attempts,
            state: r.state.as_str(),
            next_eligible_at: r.next_eligible_at.clone(),
            fail_reason: r.fail_reason.chars().take(200).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn queue_with_tasks() -> (RerunQueue, Arc<Mutex<Connection>>) {
        let conn: Arc<Mutex<Connection>> =
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        {
            let c = conn.lock().await;
            c.execute_batch(
                "CREATE TABLE scheduled_tasks (
                    id TEXT PRIMARY KEY, scheduler_job_id TEXT, user_id TEXT NOT NULL,
                    chat_id TEXT NOT NULL, platform TEXT NOT NULL, trigger_type TEXT NOT NULL,
                    trigger_value TEXT NOT NULL, prompt TEXT NOT NULL, description TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'active',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')), next_run_at TEXT, deleted_at TEXT);
                 CREATE TABLE scheduled_task_runs (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, run_at TEXT NOT NULL,
                    response TEXT, error TEXT, status TEXT NOT NULL DEFAULT 'completed',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')));
                 CREATE TABLE pending_reruns (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, original_run_id TEXT NOT NULL,
                    fail_reason TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL DEFAULT 'queued', next_eligible_at TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')));",
            )
            .unwrap();
        }
        (RerunQueue::new(Arc::clone(&conn)), conn)
    }

    async fn seed_task(conn: &Arc<Mutex<Connection>>, id: &str, status: &str, deleted: bool) {
        let c = conn.lock().await;
        c.execute(
            "INSERT INTO scheduled_tasks (id,user_id,chat_id,platform,trigger_type,trigger_value,prompt,description,status,deleted_at)
             VALUES (?1,'42','100','telegram','recurring','0 0 * * *','p','desc',?2,?3)",
            rusqlite::params![id, status, if deleted { Some("x".to_string()) } else { None }],
        )
        .unwrap();
        c.execute(
            "INSERT INTO scheduled_task_runs (id,task_id,run_at,status,error) VALUES (?1,?2,'2026-01-01T00:00:00','failed','boom')",
            rusqlite::params![format!("run-{id}"), id],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn enqueue_creates_queued_row_30min_out() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "429 too many").await.unwrap();
        let r = q.get(&id).await.unwrap().unwrap();
        assert_eq!(r.state, RerunState::Queued);
        assert_eq!(r.attempts, 0);
        assert_eq!(r.fail_reason, "429 too many");
        let c = conn.lock().await;
        let mins: f64 = c
            .query_row(
                "SELECT (julianday(next_eligible_at)-julianday('now'))*24*60 FROM pending_reruns WHERE id=?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (15.0..45.0).contains(&mins),
            "eligibility ~30m out, got {mins}m"
        );
    }

    #[tokio::test]
    async fn enqueue_supersedes_previous_live_row() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let first = q.enqueue("t1", "run-t1", "e1").await.unwrap();
        let second = q.enqueue("t1", "run-t1", "e2").await.unwrap();
        assert_ne!(first, second);
        assert_eq!(
            q.get(&first).await.unwrap().unwrap().state,
            RerunState::Superseded
        );
        assert_eq!(
            q.get(&second).await.unwrap().unwrap().state,
            RerunState::Queued
        );
        assert_eq!(
            q.list_active().await.unwrap().len(),
            1,
            "only latest stays live"
        );
    }

    #[tokio::test]
    async fn due_respects_eligibility_boundary() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        assert!(
            q.due().await.unwrap().is_empty(),
            "30m in the future → not due"
        );
        {
            let c = conn.lock().await;
            c.execute("UPDATE pending_reruns SET next_eligible_at = datetime('now','-1 minute') WHERE id=?1", rusqlite::params![id]).unwrap();
        }
        let due = q.due().await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
    }

    #[tokio::test]
    async fn two_strike_lifecycle() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "first 429").await.unwrap();
        q.mark_dispatched(&id).await.unwrap();
        assert_eq!(q.get(&id).await.unwrap().unwrap().attempts, 1);
        // Second strike → ask the human, stays terminal-ish until they answer.
        q.mark_awaiting_user(&id, "429 again").await.unwrap();
        let r = q.get(&id).await.unwrap().unwrap();
        assert_eq!(r.state, RerunState::AwaitingUser);
        assert_eq!(r.fail_reason, "429 again");
        // awaiting_user rows never auto re-fire.
        {
            let c = conn.lock().await;
            c.execute("UPDATE pending_reruns SET next_eligible_at = datetime('now','-1 minute') WHERE id=?1", rusqlite::params![id]).unwrap();
        }
        assert!(
            q.due().await.unwrap().is_empty(),
            "awaiting_user must not be due"
        );
    }

    #[tokio::test]
    async fn success_marks_done() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_dispatched(&id).await.unwrap();
        q.mark_done(&id).await.unwrap();
        assert_eq!(q.get(&id).await.unwrap().unwrap().state, RerunState::Done);
        assert!(q.list_active().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn human_retry_resets_attempts_and_requeues() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_dispatched(&id).await.unwrap();
        q.mark_awaiting_user(&id, "again").await.unwrap();
        assert!(q.retry(&id).await.unwrap());
        let r = q.get(&id).await.unwrap().unwrap();
        assert_eq!(r.state, RerunState::Queued);
        assert_eq!(
            r.attempts, 0,
            "fresh auto-attempt budget after human says retry"
        );
    }

    #[tokio::test]
    async fn human_retry_and_cancel_on_done_rows_are_noops() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_done(&id).await.unwrap();
        assert!(!q.retry(&id).await.unwrap(), "done is not retryable");
        assert!(!q.cancel(&id).await.unwrap(), "done is not cancellable");
    }

    #[tokio::test]
    async fn cancel_from_queued_abandons() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        assert!(q.cancel(&id).await.unwrap());
        assert_eq!(
            q.get(&id).await.unwrap().unwrap().state,
            RerunState::Abandoned
        );
    }

    #[tokio::test]
    async fn expire_awaiting_after_seven_days() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let id = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_awaiting_user(&id, "again").await.unwrap();
        assert!(
            q.expire_awaiting().await.unwrap().is_empty(),
            "not yet expired"
        );
        {
            let c = conn.lock().await;
            c.execute(
                "UPDATE pending_reruns SET updated_at = datetime('now','-8 days') WHERE id=?1",
                rusqlite::params![id],
            )
            .unwrap();
        }
        let expired = q.expire_awaiting().await.unwrap();
        assert_eq!(expired, vec![id.clone()]);
        assert_eq!(
            q.get(&id).await.unwrap().unwrap().state,
            RerunState::Abandoned
        );
    }

    #[tokio::test]
    async fn purge_terminal_removes_old_dead_rows_only() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let old_done = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_done(&old_done).await.unwrap();
        let live = q.enqueue("t1", "run-t1", "e2").await.unwrap();
        {
            let c = conn.lock().await;
            c.execute(
                "UPDATE pending_reruns SET updated_at = datetime('now','-40 days') WHERE id=?1",
                rusqlite::params![old_done],
            )
            .unwrap();
        }
        let n = q.purge_terminal().await.unwrap();
        assert_eq!(n, 1);
        assert!(q.get(&old_done).await.unwrap().is_none());
        assert!(
            q.get(&live).await.unwrap().is_some(),
            "live rows untouched by purge"
        );
    }

    #[tokio::test]
    async fn crash_reset_clears_inflight_attempts() {
        let (q, conn) = queue_with_tasks().await;
        seed_task(&conn, "t1", "active", false).await;
        let dispatched = q.enqueue("t1", "run-t1", "e").await.unwrap();
        q.mark_dispatched(&dispatched).await.unwrap(); // attempts=1, process died before outcome
        let queued = q.enqueue("t1", "run-t1", "e2").await.unwrap(); // supersedes dispatched!
        _ = queued;
        // Fresh scenario: dispatched row is now superseded, so reset a new one:
        let id = q.enqueue("t1", "run-t1", "e3").await.unwrap();
        q.mark_dispatched(&id).await.unwrap();
        let n = q.reset_inflight_on_boot().await.unwrap();
        assert!(n >= 1);
        let r = q.get(&id).await.unwrap().unwrap();
        assert_eq!(r.attempts, 0, "phantom attempt must not survive boot reset");
        assert_eq!(r.state, RerunState::Queued);
    }

    #[tokio::test]
    async fn get_by_id_on_missing_returns_none_and_store_still_works() {
        let (q, _conn) = queue_with_tasks().await;
        assert!(q.get("nope").await.unwrap().is_none());
        // FK sanity: enqueue against a task that has a run row is fine even
        // if the task is inactive — eligibility filters at dispatch time.
        let id = q.enqueue("ghost", "run-x", "e").await.unwrap();
        assert!(q.get(&id).await.unwrap().is_some());
    }
}
