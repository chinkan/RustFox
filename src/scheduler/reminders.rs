use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Marker used to exclude non-user rows (e.g. system/builtin identities)
/// from owner-scoped listings. Kept as a constant so future callers can
/// extend the convention without re-deriving it.
pub const SYSTEM_USER_PREFIX: &str = "__system";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScheduledTask {
    pub id: String,
    pub scheduler_job_id: Option<String>,
    pub user_id: String,
    pub chat_id: String,
    pub platform: String,
    pub trigger_type: String,
    pub trigger_value: String,
    pub prompt: String,
    pub description: String,
    pub status: String,
    pub created_at: String,
    pub next_run_at: Option<String>,
    /// Soft-delete stamp (T3 / ADR-0011a R6). `None` = live row. A portal
    /// DELETE sets this instead of removing the row, so run history and the
    /// definition survive as evidence.
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScheduledTaskRun {
    pub id: String,
    pub task_id: String,
    pub run_at: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct ScheduledTaskStore {
    conn: Arc<Mutex<Connection>>,
}

#[allow(dead_code)]
impl ScheduledTaskStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub async fn create(&self, task: &ScheduledTask) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO scheduled_tasks
             (id, scheduler_job_id, user_id, chat_id, platform, trigger_type,
              trigger_value, prompt, description, status, created_at, next_run_at,
              deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                task.id,
                task.scheduler_job_id,
                task.user_id,
                task.chat_id,
                task.platform,
                task.trigger_type,
                task.trigger_value,
                task.prompt,
                task.description,
                task.status,
                task.created_at,
                task.next_run_at,
                task.deleted_at,
            ],
        )
        .context("Failed to insert scheduled task")?;
        Ok(())
    }

    pub async fn list_active_for_user(&self, user_id: &str) -> Result<Vec<ScheduledTask>> {
        let conn = self.conn.lock().await;
        self.query_tasks(
            &conn,
            "WHERE user_id = ?1 AND status = 'active' AND deleted_at IS NULL",
            rusqlite::params![user_id],
        )
    }

    /// Owner-scoped listing that survives the *composite* run-scoped id used
    /// when a scheduled task executes itself (`user_id = "{owner}:{task_id}"`,
    /// see `Agent::build_fire_closure`).
    ///
    /// The naive `list_active_for_user` scopes on a literal `user_id`, so a
    /// scheduled run's self-listing matches nothing (it searches for
    /// `"owner:task_id"`) while a portal-created task is keyed on a different
    /// id entirely (`"web"` vs the Telegram owner). This predicate matches:
    ///
    /// 1. the row whose `id` equals the queried composite task id (the task
    ///    listing itself — `sankey` is `` when the caller passed a base id);
    /// 2. every row whose owner is the base id (covers a scheduled run asking
    ///    for its siblings, and the plain Telegram caller);
    /// 3. every row whose owner is the portal identity (`"web"…`) or an
    ///    owner-less/system identity (`"system"`, `__system…`) — the "bot-wide
    ///    view" that management tools need.
    ///
    /// A telegram owner that merely *starts with* `"web"` (e.g. `"webmaster"`)
    /// does not match (3) because the predicate anchors at the prefix.
    pub async fn list_active_for_owner_scope(
        &self,
        queried_user_id: &str,
    ) -> Result<Vec<ScheduledTask>> {
        // A scheduled run carries "{owner}:{task_id}", so enumerating it from
        // inside its own task must still resolve back to the owner's rows and
        // to the task itself.
        let (base_owner, composite_task_id) = match queried_user_id.split_once(':') {
            Some((owner, task_id)) => (owner, task_id),
            None => (queried_user_id, ""),
        };
        let conn = self.conn.lock().await;
        self.query_tasks(
            &conn,
            "WHERE status = 'active' AND deleted_at IS NULL
               AND ( id = ?1
                     OR user_id = ?2
                     OR substr(user_id, 1, 6) = 'web'
                     OR substr(user_id, 1, 6) = 'system'
                     OR substr(user_id, 1, 8) = ?3 )",
            rusqlite::params![composite_task_id, base_owner, SYSTEM_USER_PREFIX],
        )
    }

    pub async fn list_all_active(&self) -> Result<Vec<ScheduledTask>> {
        let conn = self.conn.lock().await;
        self.query_tasks(
            &conn,
            "WHERE status = 'active' AND deleted_at IS NULL",
            rusqlite::params![],
        )
    }

    /// Active + paused tasks — what the portal Tasks page should show so a
    /// paused task can be re-enabled from the UI instead of vanishing.
    pub async fn list_browsable(&self) -> Result<Vec<ScheduledTask>> {
        let conn = self.conn.lock().await;
        self.query_tasks(
            &conn,
            "WHERE status IN ('active', 'paused') AND deleted_at IS NULL",
            rusqlite::params![],
        )
    }

    pub async fn set_status(&self, id: &str, status: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, id],
        )
        .context("Failed to update task status")?;
        Ok(())
    }

    pub async fn update_scheduler_job_id(&self, id: &str, job_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE scheduled_tasks SET scheduler_job_id = ?1 WHERE id = ?2",
            rusqlite::params![job_id, id],
        )
        .context("Failed to update scheduler_job_id")?;
        Ok(())
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Option<ScheduledTask>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, scheduler_job_id, user_id, chat_id, platform, trigger_type,
                        trigger_value, prompt, description, status, created_at, next_run_at, deleted_at
                 FROM scheduled_tasks WHERE id = ?1",
            )
            .context("Failed to prepare get_by_id query")?;
        let mut rows = stmt
            .query_map(rusqlite::params![id], |row| {
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    scheduler_job_id: row.get(1)?,
                    user_id: row.get(2)?,
                    chat_id: row.get(3)?,
                    platform: row.get(4)?,
                    trigger_type: row.get(5)?,
                    trigger_value: row.get(6)?,
                    prompt: row.get(7)?,
                    description: row.get(8)?,
                    status: row.get(9)?,
                    created_at: row.get(10)?,
                    next_run_at: row.get(11)?,
                    deleted_at: row.get(12)?,
                })
            })
            .context("Failed to query task by id")?;
        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(e).context("Failed to deserialize task"),
            None => Ok(None),
        }
    }

    /// Partial update for the portal task editor (T3). `None` arguments are
    /// left untouched (COALESCE); `next_run = Some(x)` writes `x` (which may
    /// be NULL to clear it for recurring edits). Returns rows affected
    /// (0 = id not found or already soft-deleted).
    pub async fn update_task_fields(
        &self,
        id: &str,
        prompt: Option<&str>,
        trigger_value: Option<&str>,
        description: Option<&str>,
        next_run: Option<Option<&str>>,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE scheduled_tasks
                 SET prompt        = COALESCE(?1, prompt),
                     trigger_value = COALESCE(?2, trigger_value),
                     description   = COALESCE(?3, description),
                     next_run_at   = CASE WHEN ?4 THEN ?5 ELSE next_run_at END
                 WHERE id = ?6 AND deleted_at IS NULL",
                rusqlite::params![
                    prompt,
                    trigger_value,
                    description,
                    next_run.is_some(),
                    next_run.flatten(),
                    id
                ],
            )
            .context("Failed to update task fields")?;
        Ok(n)
    }

    /// Soft delete (T3 / ADR-0011a R6): stamp `deleted_at`, keep the row and
    /// its entire `scheduled_task_runs` history. Returns rows affected.
    pub async fn soft_delete(&self, id: &str) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE scheduled_tasks SET deleted_at = datetime('now'), status = 'cancelled'
                 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
            )
            .context("Failed to soft-delete task")?;
        Ok(n)
    }

    /// Retire a successfully-fired one-shot task (issue #109, related
    /// observation). Without this, a one-shot that ran stays `active` forever,
    /// inflating listings and risking mis-restore. Idempotent and a no-op for
    /// recurring rows: returns `true` only when an active one-shot row was
    /// actually transitioned to `completed`.
    pub async fn retire_completed_one_shot(&self, id: &str, trigger_type: &str) -> Result<bool> {
        if trigger_type != "one_shot" {
            return Ok(false);
        }
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE scheduled_tasks SET status = 'completed'
                 WHERE id = ?1 AND status = 'active' AND trigger_type = 'one_shot'",
                rusqlite::params![id],
            )
            .context("Failed to retire one-shot task")?;
        Ok(n > 0)
    }

    pub async fn update_next_run_at(&self, id: &str, next_run_at: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
            rusqlite::params![next_run_at, id],
        )
        .context("Failed to update next_run_at")?;
        Ok(())
    }

    pub async fn insert_run(
        &self,
        id: &str,
        task_id: &str,
        run_at: &str,
        response: Option<&str>,
        error: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO scheduled_task_runs (id, task_id, run_at, response, error, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, task_id, run_at, response, error, status],
        )
        .context("Failed to insert scheduled task run")?;
        Ok(())
    }

    pub async fn update_run(
        &self,
        id: &str,
        response: Option<&str>,
        error: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE scheduled_task_runs SET response = ?1, error = ?2, status = ?3 WHERE id = ?4",
            rusqlite::params![response, error, status, id],
        )
        .context("Failed to update scheduled task run")?;
        Ok(())
    }

    pub async fn get_task_runs(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<ScheduledTaskRun>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, run_at, response, error, status, created_at
                 FROM scheduled_task_runs
                 WHERE task_id = ?1
                 ORDER BY run_at DESC
                 LIMIT ?2",
            )
            .context("Failed to prepare get_task_runs query")?;
        let runs = stmt
            .query_map(rusqlite::params![task_id, limit as i64], |row| {
                Ok(ScheduledTaskRun {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    run_at: row.get(2)?,
                    response: row.get(3)?,
                    error: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .context("Failed to map rows")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect rows")?;
        Ok(runs)
    }

    // Private helper — executes SELECT with a WHERE clause fragment.
    // Takes &Connection directly (caller already holds the lock).
    fn query_tasks(
        &self,
        conn: &Connection,
        where_clause: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<ScheduledTask>> {
        let sql = format!(
            "SELECT id, scheduler_job_id, user_id, chat_id, platform, trigger_type,
                    trigger_value, prompt, description, status, created_at, next_run_at, deleted_at
             FROM scheduled_tasks {}
             ORDER BY created_at ASC",
            where_clause
        );
        let mut stmt = conn.prepare(&sql).context("Failed to prepare query")?;
        let tasks = stmt
            .query_map(params, |row| {
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    scheduler_job_id: row.get(1)?,
                    user_id: row.get(2)?,
                    chat_id: row.get(3)?,
                    platform: row.get(4)?,
                    trigger_type: row.get(5)?,
                    trigger_value: row.get(6)?,
                    prompt: row.get(7)?,
                    description: row.get(8)?,
                    status: row.get(9)?,
                    created_at: row.get(10)?,
                    next_run_at: row.get(11)?,
                    deleted_at: row.get(12)?,
                })
            })
            .context("Failed to map rows")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect rows")?;
        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    fn make_task(id: &str, user_id: &str, trigger_type: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.to_string(),
            scheduler_job_id: None,
            user_id: user_id.to_string(),
            chat_id: "123456".to_string(),
            platform: "telegram".to_string(),
            trigger_type: trigger_type.to_string(),
            trigger_value: "2099-01-01T09:00:00".to_string(),
            prompt: "Say hello!".to_string(),
            description: "Test task".to_string(),
            status: "active".to_string(),
            created_at: "2026-01-01T00:00:00".to_string(),
            next_run_at: Some("2099-01-01T09:00:00".to_string()),
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        let task = make_task("task-1", "user-1", "one_shot");
        store.create(&task).await.unwrap();

        let tasks = store.list_active_for_user("user-1").await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-1");
    }

    #[tokio::test]
    async fn test_list_only_returns_active() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("task-a", "user-2", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("task-b", "user-2", "one_shot"))
            .await
            .unwrap();
        store.set_status("task-b", "cancelled").await.unwrap();

        let tasks = store.list_active_for_user("user-2").await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-a");
    }

    #[tokio::test]
    async fn test_list_all_active_excludes_completed() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("t1", "user-a", "recurring"))
            .await
            .unwrap();
        store
            .create(&make_task("t2", "user-b", "one_shot"))
            .await
            .unwrap();
        store.set_status("t2", "completed").await.unwrap();

        let all = store.list_all_active().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "t1");
    }

    #[tokio::test]
    async fn test_update_scheduler_job_id() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("task-x", "user-3", "one_shot"))
            .await
            .unwrap();
        store
            .update_scheduler_job_id("task-x", "sched-uuid-123")
            .await
            .unwrap();

        let tasks = store.list_all_active().await.unwrap();
        assert_eq!(tasks[0].scheduler_job_id.as_deref(), Some("sched-uuid-123"));
    }

    // ---- Regression: issue #109, Bug 1 (owner-scoped listing) ----

    /// A portal-created task (`user_id = "web"`, `platform = "portal"`) must be
    /// visible to the Telegram owner's listing. Before the fix, the tool
    /// scoped on a literal `user_id` and omitted it (20 vs 21).
    #[tokio::test]
    async fn test_owner_scope_includes_portal_tasks() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        let tg = make_task("tg-1", "80180742", "one_shot");
        store.create(&tg).await.unwrap();
        let mut portal = make_task("portal-1", "web", "recurring");
        portal.platform = "portal".to_string();
        store.create(&portal).await.unwrap();

        // Ordinary Telegram caller: base id == queried id.
        let tasks = store.list_active_for_owner_scope("80180742").await.unwrap();
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"tg-1"), "own task must be listed: {ids:?}");
        assert!(
            ids.contains(&"portal-1"),
            "portal-created task must not be invisible: {ids:?}"
        );
        assert_eq!(tasks.len(), 2);
    }

    /// A scheduled run's own execution context carries a composite
    /// `user_id = "{owner}:{task_id}"`. Its self-listing must be non-empty
    /// (previously the literal-`user_id` predicate matched nothing and the
    /// tool answered "No active scheduled tasks.").
    #[tokio::test]
    async fn test_owner_scope_scheduled_run_self_listing_is_non_empty() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        let task = make_task("debe2062-aaaa", "80180742", "recurring");
        store.create(&task).await.unwrap();

        let self_ctx = "80180742:debe2062-aaaa".to_string();
        let tasks = store.list_active_for_owner_scope(&self_ctx).await.unwrap();
        assert!(
            !tasks.is_empty(),
            "a scheduled run must be able to see its own task"
        );
        assert!(tasks.iter().any(|t| t.id == "debe2062-aaaa"));
    }

    /// The bot-wide listing must not leak a task belonging to an *unrelated*
    /// Telegram user (isolation between base owners is preserved), while still
    /// surfacing portal + system rows.
    #[tokio::test]
    async fn test_owner_scope_excludes_other_telegram_users() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("mine", "80180742", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("someone-else", "99999999", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("portal", "web", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("sys", "system:watchdog", "one_shot"))
            .await
            .unwrap();

        let tasks = store.list_active_for_owner_scope("80180742").await.unwrap();
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"mine"));
        assert!(ids.contains(&"portal"));
        assert!(ids.contains(&"sys"));
        assert!(
            !ids.contains(&"someone-else"),
            "another Telegram user's tasks must stay hidden: {ids:?}"
        );
    }

    /// A Telegram owner whose id merely starts with `"web"` must not be treated
    /// as the portal identity (the predicate is anchored, not a bare LIKE).
    #[tokio::test]
    async fn test_owner_scope_web_prefix_is_anchored_not_greedy() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("webmaster-task", "webmaster", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("portal-task", "web", "one_shot"))
            .await
            .unwrap();

        let tasks = store.list_active_for_owner_scope("80180742").await.unwrap();
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"portal-task"), "portal identity matches");
        assert!(
            !ids.contains(&"webmaster-task"),
            "an unrelated 'webmaster' owner must not be swept in: {ids:?}"
        );
    }

    #[tokio::test]
    async fn test_retire_completed_one_shot_is_idempotent_and_recurring_safe() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .create(&make_task("os", "u", "one_shot"))
            .await
            .unwrap();
        store
            .create(&make_task("rec", "u", "recurring"))
            .await
            .unwrap();

        // Recurring rows are never retired.
        assert!(!store
            .retire_completed_one_shot("rec", "recurring")
            .await
            .unwrap());
        // One-shot: first call retires, second is a no-op.
        assert!(store
            .retire_completed_one_shot("os", "one_shot")
            .await
            .unwrap());
        assert!(!store
            .retire_completed_one_shot("os", "one_shot")
            .await
            .unwrap());

        let os = store.get_by_id("os").await.unwrap().unwrap();
        assert_eq!(os.status, "completed");
        let rec = store.get_by_id("rec").await.unwrap().unwrap();
        assert_eq!(rec.status, "active");
    }

    #[tokio::test]
    async fn test_insert_and_get_task_runs() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        // Create parent task first for FOREIGN KEY
        let task = make_task("task-1", "user-1", "one_shot");
        store.create(&task).await.unwrap();

        store
            .insert_run(
                "run-1",
                "task-1",
                "2026-07-13T10:00:00",
                Some("hello"),
                None,
                "completed",
            )
            .await
            .unwrap();
        store
            .insert_run(
                "run-2",
                "task-1",
                "2026-07-13T11:00:00",
                None,
                Some("error"),
                "failed",
            )
            .await
            .unwrap();

        let runs = store.get_task_runs("task-1", 10).await.unwrap();
        assert_eq!(runs.len(), 2);
        // Most recent first
        assert_eq!(runs[0].id, "run-2");
        assert_eq!(runs[1].id, "run-1");
        assert_eq!(runs[0].response, None);
        assert_eq!(runs[0].error.as_deref(), Some("error"));
        assert_eq!(runs[1].response.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn test_get_task_runs_empty() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        let runs = store.get_task_runs("nonexistent", 10).await.unwrap();
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn test_update_run() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        // Create parent task first for FOREIGN KEY
        let task = make_task("task-x", "user-1", "one_shot");
        store.create(&task).await.unwrap();

        store
            .insert_run(
                "run-x",
                "task-x",
                "2026-07-13T12:00:00",
                None,
                None,
                "running",
            )
            .await
            .unwrap();

        store
            .update_run("run-x", Some("result"), None, "completed")
            .await
            .unwrap();

        let runs = store.get_task_runs("task-x", 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].response.as_deref(), Some("result"));
        assert_eq!(runs[0].status, "completed");
    }
}
