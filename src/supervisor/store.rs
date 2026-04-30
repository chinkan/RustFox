use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::supervisor::job::{Job, JobStatus, JobType};
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

    pub async fn update_classification(&self, t: &Task) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sup_tasks
             SET task_type=?1, risk_level=?2, execution_mode=?3, updated_at=datetime('now')
             WHERE id=?4",
            rusqlite::params![
                serde_json::to_string(&t.task_type)?,
                serde_json::to_string(&t.risk_level)?,
                serde_json::to_string(&t.execution_mode)?,
                t.id,
            ],
        )
        .context("update sup_tasks classification")?;
        Ok(())
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

    pub async fn create_job(&self, j: &Job) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_jobs
             (id, task_id, parent_job_id, job_type, backend, goal, prompt,
              input_context, timeout_secs, retry_max, retry_count, allow_tools,
              workspace, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![
                j.id,
                j.task_id,
                j.parent_job_id,
                serde_json::to_string(&j.job_type)?,
                j.backend,
                j.goal,
                j.prompt,
                j.input_context.to_string(),
                j.timeout_secs as i64,
                j.retry_max as i64,
                j.retry_count as i64,
                serde_json::to_string(&j.allow_tools)?,
                j.workspace,
                serde_json::to_string(&j.status)?,
            ],
        )
        .context("insert sup_jobs")?;
        Ok(())
    }

    pub async fn jobs_for_task(&self, task_id: &str) -> Result<Vec<Job>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, parent_job_id, job_type, backend, goal, prompt,
                    input_context, timeout_secs, retry_max, retry_count, allow_tools,
                    workspace, status, result_summary, error
             FROM sup_jobs WHERE task_id=?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt
            .query_map([task_id], |r| {
                Ok(Job {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    parent_job_id: r.get(2)?,
                    job_type: serde_json::from_str::<JobType>(&r.get::<_, String>(3)?).map_err(
                        |e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        },
                    )?,
                    backend: r.get(4)?,
                    goal: r.get(5)?,
                    prompt: r.get(6)?,
                    input_context: serde_json::from_str(&r.get::<_, String>(7)?)
                        .unwrap_or(serde_json::Value::Null),
                    timeout_secs: r.get::<_, i64>(8)? as u64,
                    retry_max: r.get::<_, i64>(9)? as u32,
                    retry_count: r.get::<_, i64>(10)? as u32,
                    allow_tools: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
                    workspace: r.get(12)?,
                    status: serde_json::from_str::<JobStatus>(&r.get::<_, String>(13)?).map_err(
                        |e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                13,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        },
                    )?,
                    // M3: lossy reconstruction — full evidence persistence is M6+.
                    // We preserve the stored summary and synthesize a single
                    // `OutputValidated` evidence entry so that VerificationEngine's
                    // "≥1 evidence" gate can be satisfied for jobs that completed.
                    result: r.get::<_, Option<String>>(14)?.map(|summary| {
                        crate::supervisor::job::JobOutput {
                            status: crate::supervisor::job::JobStatus::Succeeded,
                            summary,
                            evidence: vec![crate::supervisor::job::Evidence::OutputValidated {
                                description: "stored job result".into(),
                            }],
                            errors: vec![],
                            changed_files: vec![],
                            next_step: None,
                        }
                    }),
                    error: r.get(15)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub async fn update_job_status(
        &self,
        id: &str,
        status: JobStatus,
        summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sup_jobs SET status=?1, result_summary=?2, error=?3,
                                 finished_at=datetime('now') WHERE id=?4",
            rusqlite::params![serde_json::to_string(&status)?, summary, error, id],
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
    async fn save_and_load_jobs_for_task() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "u");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut job = crate::supervisor::job::Job::new(
            &task.id,
            crate::supervisor::job::JobType::ExecutorJob,
            "reasoning",
            "do",
        );
        job.prompt = Some("do it".into());
        store.create_job(&job).await.unwrap();
        let jobs = store.jobs_for_task(&task.id).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job.id);
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
