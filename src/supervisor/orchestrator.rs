use anyhow::Result;
use std::collections::HashMap;

use crate::supervisor::backend::{Registry, RunContext};
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

    /// Register fallback backends per primary-backend name. When the named
    /// primary backend fails (returns `Err` or a `Failed` `JobOutput`), the
    /// orchestrator retries the job with each fallback name in order before
    /// declaring the job failed.
    pub fn set_fallbacks(&mut self, m: HashMap<String, Vec<String>>) {
        self.fallbacks = m;
    }

    pub async fn execute_plan(&self, _task: &Task, plan: Plan) -> Result<OrchestratorOutcome> {
        let mut processed: std::collections::HashSet<usize> = Default::default();

        for idx in 0..plan.jobs.len() {
            if processed.contains(&idx) {
                continue;
            }
            if let Some(group) = plan.parallel_groups.iter().find(|g| g.contains(&idx)) {
                let futs: Vec<_> =
                    group
                        .iter()
                        .map(|&gi| {
                            let job = plan.jobs[gi].clone();
                            let store = self.store.clone();
                            let reg = self.reg.clone();
                            let fb = self.fallbacks.clone();
                            async move {
                                Self::execute_one_job_with_subjobs(&reg, &store, &fb, job).await
                            }
                        })
                        .collect();
                let results = futures::future::join_all(futs).await;
                for r in results {
                    match r? {
                        JobOutcome::Failed(id) => return Ok(OrchestratorOutcome::FailedAt(id)),
                        JobOutcome::Succeeded => {}
                    }
                }
                for &gi in group {
                    processed.insert(gi);
                }
            } else {
                let job = plan.jobs[idx].clone();
                match Self::execute_one_job_with_subjobs(
                    &self.reg,
                    &self.store,
                    &self.fallbacks,
                    job,
                )
                .await?
                {
                    JobOutcome::Failed(id) => return Ok(OrchestratorOutcome::FailedAt(id)),
                    JobOutcome::Succeeded => {}
                }
                processed.insert(idx);
            }
        }
        Ok(OrchestratorOutcome::AllSucceeded)
    }

    /// Run a single job with fallback support. The provided `ctx` is forwarded
    /// to each backend invocation (including fallbacks) so backends may
    /// `spawn_subjob` regardless of which fallback ultimately handles the job.
    async fn execute_one_job(
        reg: &Registry,
        store: &TaskStore,
        fallbacks: &HashMap<String, Vec<String>>,
        mut job: Job,
        ctx: &RunContext,
    ) -> Result<JobOutcome> {
        store.create_job(&job).await?;
        let primary_name = job.backend.clone();
        let mut backends: Vec<String> = vec![primary_name.clone()];
        if let Some(fb) = fallbacks.get(&primary_name) {
            for n in fb {
                backends.push(n.clone());
            }
        }

        let mut last_err: Option<String> = None;
        for name in &backends {
            let backend = reg
                .select_by_name(name)
                .or_else(|| reg.select_for(std::slice::from_ref(name)))
                .or_else(|| reg.select_by_name("reasoning"));
            let Some(backend) = backend else {
                last_err = Some(format!("backend not found: {name}"));
                continue;
            };
            match backend.run(&mut job, ctx).await {
                Ok(out) if matches!(out.status, JobStatus::Succeeded) => {
                    store
                        .update_job_status(&job.id, JobStatus::Succeeded, Some(&out.summary), None)
                        .await?;
                    return Ok(JobOutcome::Succeeded);
                }
                Ok(out) => {
                    last_err = Some(
                        out.errors
                            .first()
                            .cloned()
                            .unwrap_or_else(|| out.summary.clone()),
                    );
                }
                Err(e) => {
                    last_err = Some(format!("{e:#}"));
                }
            }
        }
        store
            .update_job_status(
                &job.id,
                JobStatus::Failed,
                None,
                last_err.as_deref().or(Some("all backends failed")),
            )
            .await?;
        Ok(JobOutcome::Failed(job.id))
    }

    /// Run a parent job, then drain and execute any subjobs the backend
    /// queued via `RunContext::spawn_subjob`. Subjobs run sequentially with a
    /// fresh `RunContext` (no nested spawning supported in M6) and their
    /// `parent_job_id` is set to the parent. Subjob failures are recorded but
    /// do **not** propagate up — the parent's outcome still determines whether
    /// the plan continues.
    async fn execute_one_job_with_subjobs(
        reg: &Registry,
        store: &TaskStore,
        fallbacks: &HashMap<String, Vec<String>>,
        job: Job,
    ) -> Result<JobOutcome> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = RunContext::with_subjob_channel(tx);
        let parent_id = job.id.clone();
        let outcome = Self::execute_one_job(reg, store, fallbacks, job, &ctx).await?;
        // Dropping `ctx` closes the sender so try_recv won't block forever
        // even if a backend cloned the channel internally.
        drop(ctx);
        while let Ok(mut subjob) = rx.try_recv() {
            subjob.parent_job_id = Some(parent_id.clone());
            let _ =
                Self::execute_one_job(reg, store, fallbacks, subjob, &RunContext::new()).await?;
        }
        Ok(outcome)
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

        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut reg = crate::supervisor::backend::Registry::new();
        let c1 = counter.clone();
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(move |_| {
                let c = c1.clone();
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("done".into())
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
        orch.execute_plan(&task, plan).await.unwrap();
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "all three parallel jobs must have run"
        );
    }

    #[tokio::test]
    async fn orchestrator_runs_non_contiguous_parallel_group_without_skipping_serial_jobs() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "x");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut reg = crate::supervisor::backend::Registry::new();
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
                |p| async move { Ok(format!("ran:{p}")) },
            ),
        ));

        // 4 jobs: indices 0 and 3 in parallel; 1 and 2 sequential.
        let mut plan = crate::supervisor::planner::Plan {
            jobs: vec![],
            parallel_groups: vec![vec![0, 3]],
        };
        for i in 0..4 {
            let mut j = crate::supervisor::job::Job::new(
                &task.id,
                crate::supervisor::job::JobType::ExecutorJob,
                "reasoning",
                &format!("g{i}"),
            );
            j.prompt = Some(format!("p{i}"));
            plan.jobs.push(j);
        }

        let orch = crate::supervisor::orchestrator::Orchestrator::new(reg, store.clone());
        orch.execute_plan(&task, plan).await.unwrap();

        let jobs = store.jobs_for_task(&task.id).await.unwrap();
        assert_eq!(jobs.len(), 4, "all four jobs must be persisted");
        for j in &jobs {
            assert_eq!(
                j.status,
                crate::supervisor::job::JobStatus::Succeeded,
                "job {} should have run, got {:?}",
                j.id,
                j.status
            );
        }
    }

    struct FailoverEcho;
    #[async_trait::async_trait]
    impl crate::supervisor::backend::Backend for FailoverEcho {
        fn name(&self) -> &str {
            "failover-echo"
        }
        fn capabilities(&self) -> crate::supervisor::backend::BackendCapabilities {
            crate::supervisor::backend::BackendCapabilities {
                reasoning: true,
                ..Default::default()
            }
        }
        fn can_handle(&self, _: &crate::supervisor::job::JobType) -> bool {
            true
        }
        async fn run(
            &self,
            j: &mut crate::supervisor::job::Job,
            _ctx: &crate::supervisor::backend::RunContext,
        ) -> anyhow::Result<crate::supervisor::job::JobOutput> {
            Ok(crate::supervisor::job::JobOutput {
                status: crate::supervisor::job::JobStatus::Succeeded,
                summary: format!("fallback handled {}", j.prompt.clone().unwrap_or_default()),
                evidence: vec![crate::supervisor::job::Evidence::OutputValidated {
                    description: "fallback".into(),
                }],
                errors: vec![],
                changed_files: vec![],
                next_step: None,
            })
        }
    }

    #[tokio::test]
    async fn orchestrator_falls_back_when_primary_fails() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "x");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut reg = crate::supervisor::backend::Registry::new();
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
                |_| async move { Err(anyhow::anyhow!("primary boom")) },
            ),
        ));
        reg.register(std::sync::Arc::new(FailoverEcho));

        let mut fallbacks = std::collections::HashMap::new();
        fallbacks.insert("reasoning".into(), vec!["failover-echo".into()]);

        let mut plan = crate::supervisor::planner::Plan {
            jobs: vec![],
            parallel_groups: vec![],
        };
        let mut j = crate::supervisor::job::Job::new(
            &task.id,
            crate::supervisor::job::JobType::ExecutorJob,
            "reasoning",
            "g",
        );
        j.prompt = Some("hi".into());
        plan.jobs.push(j);

        let mut orch = Orchestrator::new(reg, store.clone());
        orch.set_fallbacks(fallbacks);
        let res = orch.execute_plan(&task, plan).await.unwrap();
        assert!(matches!(res, OrchestratorOutcome::AllSucceeded));
    }

    /// Backend that queues exactly one subjob during `run` to exercise the
    /// orchestrator's subjob drain.
    struct SpawningBackend;
    #[async_trait::async_trait]
    impl crate::supervisor::backend::Backend for SpawningBackend {
        fn name(&self) -> &str {
            "spawner"
        }
        fn capabilities(&self) -> crate::supervisor::backend::BackendCapabilities {
            crate::supervisor::backend::BackendCapabilities {
                reasoning: true,
                ..Default::default()
            }
        }
        fn can_handle(&self, _: &crate::supervisor::job::JobType) -> bool {
            true
        }
        async fn run(
            &self,
            job: &mut crate::supervisor::job::Job,
            ctx: &crate::supervisor::backend::RunContext,
        ) -> anyhow::Result<crate::supervisor::job::JobOutput> {
            let mut sub = crate::supervisor::job::Job::new(
                &job.task_id,
                crate::supervisor::job::JobType::ExecutorJob,
                "reasoning",
                "child",
            );
            sub.prompt = Some("child task".into());
            ctx.spawn_subjob(sub);
            Ok(crate::supervisor::job::JobOutput {
                status: crate::supervisor::job::JobStatus::Succeeded,
                summary: "parent done".into(),
                evidence: vec![crate::supervisor::job::Evidence::OutputValidated {
                    description: "ok".into(),
                }],
                errors: vec![],
                changed_files: vec![],
                next_step: None,
            })
        }
    }

    #[tokio::test]
    async fn orchestrator_executes_spawned_subjob_after_parent() {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "x");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let mut reg = crate::supervisor::backend::Registry::new();
        reg.register(std::sync::Arc::new(SpawningBackend));
        reg.register(std::sync::Arc::new(
            crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
                |p| async move { Ok(format!("echo:{p}")) },
            ),
        ));

        let plan = crate::supervisor::planner::Plan {
            jobs: vec![{
                let mut j = crate::supervisor::job::Job::new(
                    &task.id,
                    crate::supervisor::job::JobType::ExecutorJob,
                    "spawner",
                    "g",
                );
                j.prompt = Some("p".into());
                j
            }],
            parallel_groups: vec![],
        };

        let orch = Orchestrator::new(reg, store.clone());
        let res = orch.execute_plan(&task, plan).await.unwrap();
        assert!(matches!(res, OrchestratorOutcome::AllSucceeded));

        let jobs = store.jobs_for_task(&task.id).await.unwrap();
        assert_eq!(jobs.len(), 2, "parent + child should both be persisted");
        let parent = jobs
            .iter()
            .find(|j| j.parent_job_id.is_none())
            .expect("parent job present");
        let child = jobs
            .iter()
            .find(|j| j.parent_job_id.is_some())
            .expect("child job present");
        assert_eq!(child.parent_job_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.status, crate::supervisor::job::JobStatus::Succeeded);
    }
}
