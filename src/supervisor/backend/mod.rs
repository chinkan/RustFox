use crate::supervisor::job::{Job, JobOutput, JobType};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

pub mod claude_code;
pub mod codex;
pub mod mcp;
pub mod reasoning;
pub mod script;
pub mod shell;

/// Per-job execution context handed to `Backend::run`. Today it carries an
/// optional channel used by backends to spawn child jobs that the orchestrator
/// will execute after the parent finishes.
#[derive(Clone, Default)]
pub struct RunContext {
    subjob_tx: Option<UnboundedSender<Job>>,
}

impl RunContext {
    pub fn new() -> Self {
        Self { subjob_tx: None }
    }

    pub fn with_subjob_channel(tx: UnboundedSender<Job>) -> Self {
        Self {
            subjob_tx: Some(tx),
        }
    }

    /// Queue a child job to run after the current job completes. If no channel
    /// is wired (e.g. when the backend is invoked outside the orchestrator)
    /// the call is a no-op.
    pub fn spawn_subjob(&self, job: Job) {
        if let Some(tx) = &self.subjob_tx {
            let _ = tx.send(job);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub reasoning: bool,
    pub coding: bool,
    pub shell: bool,
    pub research: bool,
    pub document: bool,
    pub long_running: bool,
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> BackendCapabilities;
    fn can_handle(&self, job_type: &JobType) -> bool;

    // Spec §10 required methods. `run` is the only one most backends override.
    async fn prepare(&self, _job: &mut Job) -> Result<()> {
        Ok(())
    }
    async fn run(&self, job: &mut Job, _ctx: &RunContext) -> Result<JobOutput>;
    async fn collect_result(&self, _job: &Job) -> Result<Option<JobOutput>> {
        Ok(None)
    }
    async fn verify_result(&self, _job: &Job, out: &JobOutput) -> Result<bool> {
        Ok(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Succeeded
        ))
    }
    async fn cancel(&self, _job_id: &str) -> Result<()> {
        Ok(())
    }
    async fn resume(&self, _job_id: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct Registry {
    backends: Vec<Arc<dyn Backend>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, b: Arc<dyn Backend>) {
        self.backends.push(b);
    }

    /// Select first backend that satisfies all required capabilities.
    pub fn select_for(&self, required: &[String]) -> Option<Arc<dyn Backend>> {
        self.backends
            .iter()
            .find(|b| {
                let c = b.capabilities();
                required.iter().all(|r| match r.as_str() {
                    "reasoning" => c.reasoning,
                    "coding" => c.coding,
                    "shell" => c.shell,
                    "research" => c.research,
                    "document" => c.document,
                    _ => false,
                })
            })
            .cloned()
    }

    pub fn select_by_name(&self, name: &str) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.name() == name).cloned()
    }

    pub fn names(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.name()).collect()
    }
}

/// Shared helper that spawns a child process, pipes `prompt` to its stdin,
/// applies a per-job `timeout_secs` deadline, and returns a [`JobOutput`].
/// Used by [`claude_code::ClaudeCodeCliBackend`], [`codex::CodexCliBackend`],
/// and [`script::ScriptBackend`] to eliminate duplicated spawn/timeout/capture
/// boilerplate.
pub async fn run_cli_process(
    job: &mut Job,
    bin: &str,
    args: &[String],
    workdir: &PathBuf,
) -> Result<crate::supervisor::job::JobOutput> {
    use crate::supervisor::job::{Evidence, JobOutput, JobStatus};
    let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
    let timeout_secs = job.timeout_secs;
    job.status = JobStatus::Running;

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Ignore write errors: the process may exit before reading all stdin.
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let output =
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(res) => res?,
            Err(_) => {
                job.status = JobStatus::Failed;
                return Ok(JobOutput {
                    status: JobStatus::Failed,
                    summary: String::new(),
                    evidence: vec![],
                    errors: vec![format!("CLI timed out after {timeout_secs}s")],
                    changed_files: vec![],
                    next_step: None,
                });
            }
        };
    let exit = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let status = if output.status.success() {
        JobStatus::Succeeded
    } else {
        JobStatus::Failed
    };
    job.status = status.clone();
    Ok(JobOutput {
        status,
        summary: stdout.trim().into(),
        evidence: vec![Evidence::ExitCode { code: exit }],
        errors: if stderr.is_empty() {
            vec![]
        } else {
            vec![stderr]
        },
        changed_files: vec![],
        next_step: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyReasoning;
    #[async_trait::async_trait]
    impl Backend for DummyReasoning {
        fn name(&self) -> &str {
            "dummy-reasoning"
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                reasoning: true,
                ..Default::default()
            }
        }
        fn can_handle(&self, _: &crate::supervisor::job::JobType) -> bool {
            true
        }
        async fn run(
            &self,
            _: &mut crate::supervisor::job::Job,
            _: &RunContext,
        ) -> anyhow::Result<crate::supervisor::job::JobOutput> {
            Ok(crate::supervisor::job::JobOutput {
                status: crate::supervisor::job::JobStatus::Succeeded,
                summary: "ok".into(),
                evidence: vec![],
                errors: vec![],
                changed_files: vec![],
                next_step: None,
            })
        }
    }

    #[tokio::test]
    async fn registry_finds_backend_by_capability() {
        let mut reg = Registry::new();
        reg.register(Arc::new(DummyReasoning));
        let chosen = reg.select_for(&["reasoning".into()]).unwrap();
        assert_eq!(chosen.name(), "dummy-reasoning");
    }
}
