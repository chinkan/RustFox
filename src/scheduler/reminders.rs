use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

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
              trigger_value, prompt, description, status, created_at, next_run_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
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
            ],
        )
        .context("Failed to insert scheduled task")?;
        Ok(())
    }

    pub async fn list_active_for_user(&self, user_id: &str) -> Result<Vec<ScheduledTask>> {
        let conn = self.conn.lock().await;
        self.query_tasks(
            &conn,
            "WHERE user_id = ?1 AND status = 'active'",
            rusqlite::params![user_id],
        )
    }

    pub async fn list_all_active(&self) -> Result<Vec<ScheduledTask>> {
        let conn = self.conn.lock().await;
        self.query_tasks(&conn, "WHERE status = 'active'", rusqlite::params![])
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
                        trigger_value, prompt, description, status, created_at, next_run_at
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
                })
            })
            .context("Failed to query task by id")?;
        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(e).context("Failed to deserialize task"),
            None => Ok(None),
        }
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
                    trigger_value, prompt, description, status, created_at, next_run_at
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
