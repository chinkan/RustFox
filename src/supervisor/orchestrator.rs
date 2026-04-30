use anyhow::Result;

use crate::supervisor::backend::Registry;
use crate::supervisor::job::JobStatus;
use crate::supervisor::planner::Plan;
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::Task;

pub enum OrchestratorOutcome {
    AllSucceeded,
    FailedAt(String),
}

pub struct Orchestrator {
    reg: Registry,
    store: TaskStore,
}

impl Orchestrator {
    pub fn new(reg: Registry, store: TaskStore) -> Self {
        Self { reg, store }
    }

    pub async fn execute_plan(&self, _task: &Task, plan: Plan) -> Result<OrchestratorOutcome> {
        for mut job in plan.jobs {
            self.store.create_job(&job).await?;
            let backend = self
                .reg
                .select_by_name(&job.backend)
                .or_else(|| self.reg.select_for(&[job.backend.clone()]));
            let Some(backend) = backend else {
                self.store
                    .update_job_status(&job.id, JobStatus::Failed, None, Some("no backend matched"))
                    .await?;
                return Ok(OrchestratorOutcome::FailedAt(job.id));
            };
            let out = backend.run(&mut job).await;
            match out {
                Ok(out) if matches!(out.status, JobStatus::Succeeded) => {
                    self.store
                        .update_job_status(
                            &job.id,
                            JobStatus::Succeeded,
                            Some(&out.summary),
                            None,
                        )
                        .await?;
                }
                Ok(out) => {
                    self.store
                        .update_job_status(
                            &job.id,
                            JobStatus::Failed,
                            Some(&out.summary),
                            out.errors.first().map(String::as_str),
                        )
                        .await?;
                    return Ok(OrchestratorOutcome::FailedAt(job.id));
                }
                Err(e) => {
                    self.store
                        .update_job_status(
                            &job.id,
                            JobStatus::Failed,
                            None,
                            Some(&format!("{e:#}")),
                        )
                        .await?;
                    return Ok(OrchestratorOutcome::FailedAt(job.id));
                }
            }
        }
        Ok(OrchestratorOutcome::AllSucceeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn orchestrator_runs_plan_and_persists_results() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());

        let task = crate::supervisor::task::Task::new("T", "summarize");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut reg = crate::supervisor::backend::Registry::new();
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
                |p| async move { Ok(format!("answered: {p}")) },
            ),
        ));

        let plan = crate::supervisor::planner::Planner::new().plan(&task);
        let orch = Orchestrator::new(reg, store.clone());
        let outcome = orch.execute_plan(&task, plan).await.unwrap();
        assert!(matches!(outcome, OrchestratorOutcome::AllSucceeded));

        let jobs = store.jobs_for_task(&task.id).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, crate::supervisor::job::JobStatus::Succeeded);
    }
}
