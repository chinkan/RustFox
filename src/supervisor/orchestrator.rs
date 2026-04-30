use anyhow::Result;
use std::collections::HashMap;

use crate::supervisor::backend::Registry;
use crate::supervisor::job::{Job, JobStatus};
use crate::supervisor::planner::Plan;
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::Task;

pub enum OrchestratorOutcome {
    AllSucceeded,
    FailedAt(String),
}

enum JobOutcome {
    Succeeded,
    Failed(String),
}

pub struct Orchestrator {
    reg: Registry,
    store: TaskStore,
    fallbacks: HashMap<String, Vec<String>>,
}

impl Orchestrator {
    pub fn new(reg: Registry, store: TaskStore) -> Self {
        Self {
            reg,
            store,
            fallbacks: HashMap::new(),
        }
    }

    pub async fn execute_plan(&self, _task: &Task, plan: Plan) -> Result<OrchestratorOutcome> {
        let mut grouped: std::collections::HashSet<usize> = Default::default();
        for g in &plan.parallel_groups {
            for i in g {
                grouped.insert(*i);
            }
        }

        let mut idx = 0;
        while idx < plan.jobs.len() {
            if let Some(group) = plan.parallel_groups.iter().find(|g| g.contains(&idx)) {
                let futs = group.iter().map(|&gi| {
                    let job = plan.jobs[gi].clone();
                    let store = self.store.clone();
                    let reg = self.reg.clone();
                    let fb = self.fallbacks.clone();
                    async move { Self::execute_one_job(&reg, &store, &fb, job).await }
                });
                let results = futures::future::join_all(futs).await;
                for r in results {
                    match r? {
                        JobOutcome::Failed(id) => return Ok(OrchestratorOutcome::FailedAt(id)),
                        JobOutcome::Succeeded => {}
                    }
                }
                idx = group.iter().max().copied().unwrap() + 1;
            } else if grouped.contains(&idx) {
                // Already processed by an earlier group iteration; skip.
                idx += 1;
            } else {
                let job = plan.jobs[idx].clone();
                match Self::execute_one_job(&self.reg, &self.store, &self.fallbacks, job).await? {
                    JobOutcome::Failed(id) => return Ok(OrchestratorOutcome::FailedAt(id)),
                    JobOutcome::Succeeded => {}
                }
                idx += 1;
            }
        }
        Ok(OrchestratorOutcome::AllSucceeded)
    }

    async fn execute_one_job(
        reg: &Registry,
        store: &TaskStore,
        _fallbacks: &HashMap<String, Vec<String>>,
        mut job: Job,
    ) -> Result<JobOutcome> {
        store.create_job(&job).await?;
        let backend = reg
            .select_by_name(&job.backend)
            .or_else(|| reg.select_for(&[job.backend.clone()]));
        let Some(backend) = backend else {
            store
                .update_job_status(&job.id, JobStatus::Failed, None, Some("no backend matched"))
                .await?;
            return Ok(JobOutcome::Failed(job.id));
        };
        let out = backend.run(&mut job).await;
        match out {
            Ok(out) if matches!(out.status, JobStatus::Succeeded) => {
                store
                    .update_job_status(&job.id, JobStatus::Succeeded, Some(&out.summary), None)
                    .await?;
                Ok(JobOutcome::Succeeded)
            }
            Ok(out) => {
                store
                    .update_job_status(
                        &job.id,
                        JobStatus::Failed,
                        Some(&out.summary),
                        out.errors.first().map(String::as_str),
                    )
                    .await?;
                Ok(JobOutcome::Failed(job.id))
            }
            Err(e) => {
                store
                    .update_job_status(&job.id, JobStatus::Failed, None, Some(&format!("{e:#}")))
                    .await?;
                Ok(JobOutcome::Failed(job.id))
            }
        }
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

    #[tokio::test]
    async fn orchestrator_runs_parallel_group_concurrently() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "x");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut reg = crate::supervisor::backend::Registry::new();
        let counter = std::sync::Arc::new(tokio::sync::Mutex::new(0));
        let c1 = counter.clone();
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(move |_| {
                let c = c1.clone();
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let mut g = c.lock().await;
                    *g += 1;
                    Ok(format!("done-{}", *g))
                }
            }),
        ));

        let mut plan = crate::supervisor::planner::Plan {
            jobs: vec![],
            parallel_groups: vec![],
        };
        for _ in 0..3 {
            let mut j = crate::supervisor::job::Job::new(
                &task.id,
                crate::supervisor::job::JobType::ExecutorJob,
                "reasoning",
                "g",
            );
            j.prompt = Some("x".into());
            plan.jobs.push(j);
        }
        plan.parallel_groups = vec![vec![0, 1, 2]];

        let orch = Orchestrator::new(reg, store.clone());
        let started = std::time::Instant::now();
        orch.execute_plan(&task, plan).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed.as_millis() < 130,
            "expected concurrent execution, took {}ms",
            elapsed.as_millis()
        );
    }
}
