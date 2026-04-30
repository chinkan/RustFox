use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::supervisor::task::{ExecutionMode, RiskLevel, Task, TaskStatus, TaskType};

#[derive(Clone)]
pub struct TaskStore {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub struct TransitionRow {
    pub from: TaskStatus,
    pub to: TaskStatus,
    pub actor: String,
    pub reason: Option<String>,
    pub occurred_at: String,
}

impl TaskStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub async fn create(
        &self,
        t: &Task,
        platform: &str,
        user_id: &str,
        chat_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_tasks
             (id, title, user_request, task_type, priority, risk_level, execution_mode,
              workflow, state, inputs, constraints, expected_outputs, approval_policy,
              platform, user_id, chat_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            rusqlite::params![
                t.id,
                t.title,
                t.user_request,
                serde_json::to_string(&t.task_type)?,
                t.priority,
                serde_json::to_string(&t.risk_level)?,
                serde_json::to_string(&t.execution_mode)?,
                "general",
                serde_json::to_string(&t.status)?,
                serde_json::to_string(&t.inputs)?,
                serde_json::to_string(&t.constraints)?,
                serde_json::to_string(&t.expected_outputs)?,
                serde_json::Value::Null.to_string(),
                platform,
                user_id,
                chat_id,
            ],
        )
        .context("insert sup_tasks")?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id,title,user_request,task_type,priority,risk_level,execution_mode,state
             FROM sup_tasks WHERE id=?1",
        )?;
        let mut rows = stmt.query_map([id], |r| {
            Ok(Task {
                id: r.get(0)?,
                title: r.get(1)?,
                user_request: r.get(2)?,
                task_type: serde_json::from_str::<TaskType>(&r.get::<_, String>(3)?).map_err(
                    |e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    },
                )?,
                priority: r.get(4)?,
                risk_level: serde_json::from_str::<RiskLevel>(&r.get::<_, String>(5)?).map_err(
                    |e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    },
                )?,
                execution_mode: serde_json::from_str::<ExecutionMode>(&r.get::<_, String>(6)?)
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                status: serde_json::from_str::<TaskStatus>(&r.get::<_, String>(7)?).map_err(
                    |e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    },
                )?,
                required_capabilities: vec![],
                constraints: serde_json::Value::Null,
                inputs: serde_json::Value::Null,
                expected_outputs: serde_json::Value::Null,
            })
        })?;
        Ok(match rows.next() {
            Some(Ok(t)) => Some(t),
            _ => None,
        })
    }

    pub async fn record_transition(
        &self,
        task_id: &str,
        from: TaskStatus,
        to: TaskStatus,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_transitions (task_id, from_state, to_state, reason, actor)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                task_id,
                serde_json::to_string(&from)?,
                serde_json::to_string(&to)?,
                reason,
                actor
            ],
        )?;
        conn.execute(
            "UPDATE sup_tasks SET state=?1, updated_at=datetime('now') WHERE id=?2",
            rusqlite::params![serde_json::to_string(&to)?, task_id],
        )?;
        Ok(())
    }

    pub async fn transitions(&self, task_id: &str) -> Result<Vec<TransitionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT from_state, to_state, actor, reason, occurred_at
             FROM sup_transitions WHERE task_id=?1 ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([task_id], |r| {
                Ok(TransitionRow {
                    from: serde_json::from_str(&r.get::<_, String>(0)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    to: serde_json::from_str(&r.get::<_, String>(1)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    actor: r.get(2)?,
                    reason: r.get(3)?,
                    occurred_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_task_then_load_back() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = TaskStore::new(memory.connection());
        let mut t = crate::supervisor::task::Task::new("T", "do thing");
        t.task_type = crate::supervisor::task::TaskType::Research;
        store
            .create(&t, "telegram", "u1", Some("c1"))
            .await
            .unwrap();
        let loaded = store.get(&t.id).await.unwrap().unwrap();
        assert_eq!(loaded.title, "T");
        assert_eq!(
            loaded.task_type,
            crate::supervisor::task::TaskType::Research
        );
    }

    #[tokio::test]
    async fn record_transition_appends_audit_row() {
        use crate::supervisor::task::TaskStatus;
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = TaskStore::new(memory.connection());
        let t = crate::supervisor::task::Task::new("T", "u");
        store.create(&t, "telegram", "u1", None).await.unwrap();
        store
            .record_transition(
                &t.id,
                TaskStatus::Intake,
                TaskStatus::Classify,
                "supervisor",
                Some("auto"),
            )
            .await
            .unwrap();
        let history = store.transitions(&t.id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to, TaskStatus::Classify);
    }
}
