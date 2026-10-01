//! Narrow, injectable seam over the scheduler *arming* operations that the
//! scheduling tools depend on (issue #109).
//!
//! The tools used to call `Scheduler::add_cron_job` / `add_one_shot_job`
//! directly and throw away the returned job id, so a tool-created task was
//! never dis-armed (its row kept `scheduler_job_id = NULL`) and a "cancelled"
//! recurring task kept firing forever. Routing every arm through
//! [`Agent::arm_task`] — which persists the id and is the *same* path the
//! portal already uses — closes both holes.
//!
//! The trait exists so a regression test can exercise the full tool surface
//! (create → cancel) with a fake, without constructing a whole `Agent`
//! (mirrors `portal::AgentOps`). `AgentOpsScheduling` is the production impl.

use anyhow::Result;
use std::sync::Arc;
use uuid::Uuid;

use crate::agent::Agent;
use crate::scheduler::reminders::ScheduledTask;

/// The subset of `Agent` the scheduling tools need: arm a task (register the
/// live job **and** persist its id) and disarm one (remove the live job).
#[async_trait::async_trait]
pub trait SchedulingOps: Send + Sync {
    async fn arm_task(&self, task: &ScheduledTask) -> Result<Uuid>;
    async fn disarm_task(&self, task: &ScheduledTask) -> bool;
}

/// Production implementation — delegates straight to [`Agent`].
pub struct AgentOpsScheduling {
    agent: Arc<Agent>,
}

impl AgentOpsScheduling {
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent }
    }
}

#[async_trait::async_trait]
impl SchedulingOps for AgentOpsScheduling {
    async fn arm_task(&self, task: &ScheduledTask) -> Result<Uuid> {
        self.agent.arm_task(task).await
    }

    async fn disarm_task(&self, task: &ScheduledTask) -> bool {
        self.agent.disarm_task(task).await
    }
}

/// Late-bound holder for the production [`SchedulingOps`].
///
/// `main()` builds the tool registry *before* the `Agent` exists (Agent takes
/// the registry by value), so the tool cannot hold an `Arc<Agent>` yet. This
/// holder is registered up-front and filled in with `AgentOpsScheduling` the
/// moment the agent is constructed. Until then, `arm` errors loudly and
/// `disarm` is a no-op — but no tool can run before the agent exists anyway.
pub struct OnceScheduling {
    inner: std::sync::OnceLock<Arc<dyn SchedulingOps>>,
}

impl Default for OnceScheduling {
    fn default() -> Self {
        Self::new()
    }
}

impl OnceScheduling {
    pub fn new() -> Self {
        Self {
            inner: std::sync::OnceLock::new(),
        }
    }

    /// Install the real ops. Idempotent: the first value wins.
    pub fn set(&self, ops: Arc<dyn SchedulingOps>) {
        let _ = self.inner.set(ops);
    }
}

#[async_trait::async_trait]
impl SchedulingOps for OnceScheduling {
    async fn arm_task(&self, task: &ScheduledTask) -> Result<Uuid> {
        let ops = self
            .inner
            .get()
            .ok_or_else(|| anyhow::anyhow!("scheduling ops not initialised yet"))?;
        ops.arm_task(task).await
    }

    async fn disarm_task(&self, task: &ScheduledTask) -> bool {
        match self.inner.get() {
            Some(ops) => ops.disarm_task(task).await,
            None => true,
        }
    }
}
