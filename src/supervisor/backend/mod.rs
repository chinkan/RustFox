use crate::cancel_registry::CancelRegistry;
use crate::supervisor::job::{Job, JobOutput, JobType};
use anyhow::Result;
use std::future;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

pub mod claude_code;
pub mod codex;
pub mod mcp;
pub mod reasoning;
pub mod script;
pub mod shell;

/// Cap for retained CLI stdout/stderr on success, timeout, and cancel paths.
/// Mirrors `command_tool`'s `MAX_BUFFER_CHARS`.
pub const OUTPUT_BUFFER_CHARS: usize = 100_000;

/// Job-scoped cancel registry for CLI children spawned by [`run_cli_process`].
/// Keyed by `Job.id`. Shared by claude_code / codex / script backends.
static CLI_CANCEL_REGISTRY: LazyLock<CancelRegistry> = LazyLock::new(CancelRegistry::new);

/// Signal a running CLI job (registered by [`run_cli_process`]) to be killed.
/// Idempotent: unknown / already-finished job_ids are a no-op.
pub async fn cancel_cli_job(job_id: &str) -> Result<()> {
    let _ = CLI_CANCEL_REGISTRY.cancel(job_id).await;
    Ok(())
}

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

enum StopReason {
    Completed(std::process::ExitStatus),
    TimedOut,
    Cancelled,
}

/// Kill the CLI child process group (Unix) with `child.kill()` as fallback.
async fn kill_cli_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill().await;
}

/// Drain a pipe into a capped string (same cap as chat `command_tool`).
async fn drain_pipe(stream: Option<impl AsyncReadExt + Unpin>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut out = String::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.chars().count() > OUTPUT_BUFFER_CHARS {
                    out = crate::utils::strings::truncate_tail(&out, OUTPUT_BUFFER_CHARS);
                }
            }
        }
    }
    out
}

fn cap_output(s: String) -> String {
    if s.chars().count() > OUTPUT_BUFFER_CHARS {
        crate::utils::strings::truncate_tail(&s, OUTPUT_BUFFER_CHARS)
    } else {
        s
    }
}

/// Shared helper that spawns a child process, pipes `prompt` to its stdin,
/// applies a per-job `timeout_secs` deadline (`0` = no wall-clock deadline),
/// listens for [`Backend::cancel`] via a job-scoped oneshot, and returns a
/// [`JobOutput`]. Used by [`claude_code::ClaudeCodeCliBackend`],
/// [`codex::CodexCliBackend`], and [`script::ScriptBackend`].
///
/// On timeout or cancel the child process group is killed and any capped
/// partial stdout/stderr already drained is retained (not discarded).
pub async fn run_cli_process(
    job: &mut Job,
    bin: &str,
    args: &[String],
    workdir: &PathBuf,
) -> Result<crate::supervisor::job::JobOutput> {
    use crate::supervisor::job::{Evidence, JobOutput, JobStatus};
    let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
    let timeout_secs = job.timeout_secs;
    let job_id = job.id.clone();
    job.status = JobStatus::Running;

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Ignore write errors: the process may exit before reading all stdin.
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move { drain_pipe(stdout).await });
    let stderr_task = tokio::spawn(async move { drain_pipe(stderr).await });

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    CLI_CANCEL_REGISTRY
        .register(job_id.clone(), cancel_tx)
        .await;
    tokio::pin!(cancel_rx);

    // Deadline future: timeout_secs==0 means no wall-clock bound (still
    // cancellable). Must register cancel *before* opening this unbound wait.
    let deadline = async {
        if timeout_secs == 0 {
            future::pending::<()>().await;
        } else {
            tokio::time::sleep(Duration::from_secs(timeout_secs)).await;
        }
    };

    let reason = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(s) => StopReason::Completed(s),
                Err(e) => {
                    CLI_CANCEL_REGISTRY.unregister(&job_id).await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    return Err(e.into());
                }
            }
        }
        _ = &mut cancel_rx => {
            kill_cli_child(&mut child).await;
            let _ = child.wait().await;
            StopReason::Cancelled
        }
        _ = deadline => {
            kill_cli_child(&mut child).await;
            let _ = child.wait().await;
            StopReason::TimedOut
        }
    };

    CLI_CANCEL_REGISTRY.unregister(&job_id).await;

    let stdout_s = cap_output(stdout_task.await.unwrap_or_default());
    let stderr_s = cap_output(stderr_task.await.unwrap_or_default());

    match reason {
        StopReason::TimedOut => {
            job.status = JobStatus::Failed;
            let mut errors = vec![format!("CLI timed out after {timeout_secs}s")];
            if !stderr_s.is_empty() {
                errors.push(stderr_s);
            }
            Ok(JobOutput {
                status: JobStatus::Failed,
                summary: stdout_s.trim().into(),
                evidence: vec![],
                errors,
                changed_files: vec![],
                next_step: None,
            })
        }
        StopReason::Cancelled => {
            job.status = JobStatus::Cancelled;
            let mut errors = vec!["CLI job cancelled".into()];
            if !stderr_s.is_empty() {
                errors.push(stderr_s);
            }
            Ok(JobOutput {
                status: JobStatus::Cancelled,
                summary: stdout_s.trim().into(),
                evidence: vec![],
                errors,
                changed_files: vec![],
                next_step: None,
            })
        }
        StopReason::Completed(exit_status) => {
            let exit = exit_status.code().unwrap_or(-1);
            let status = if exit_status.success() {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed
            };
            job.status = status.clone();
            Ok(JobOutput {
                status,
                summary: stdout_s.trim().into(),
                evidence: vec![Evidence::ExitCode { code: exit }],
                errors: if stderr_s.is_empty() {
                    vec![]
                } else {
                    vec![stderr_s]
                },
                changed_files: vec![],
                next_step: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::supervisor::job::JobStatus;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::time::Instant;

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

    #[cfg(unix)]
    async fn write_stub(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let stub = dir.join(name);
        tokio::fs::write(&stub, body).await.unwrap();
        let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&stub, perms).await.unwrap();
        stub
    }

    #[tokio::test]
    #[cfg(unix)] // `#!/bin/sh` stub
    async fn cli_timeout_kills_and_retains_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "partial-hang.sh",
            "#!/bin/sh\necho 'partial-before-hang'\nsleep 30\n",
        )
        .await;

        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "script",
            "hang",
        );
        job.prompt = Some("x".into());
        job.timeout_secs = 1;

        let started = Instant::now();
        let out = run_cli_process(&mut job, &stub.to_string_lossy(), &[], &dir.path().into())
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            matches!(out.status, JobStatus::Failed),
            "timeout should fail, got {:?}",
            out.status
        );
        assert!(
            out.errors.iter().any(|e| e.contains("timed out")),
            "errors={:?}",
            out.errors
        );
        assert!(
            out.summary.contains("partial-before-hang"),
            "must retain partial stdout, got summary={:?}",
            out.summary
        );
        assert!(
            elapsed.as_secs() < 5,
            "should have killed child within seconds, elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)] // `#!/bin/sh` stub
    async fn cli_cancel_kills_running_job() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "slow.sh",
            "#!/bin/sh\necho 'started-ok'\nsleep 120\n",
        )
        .await;

        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "script",
            "slow",
        );
        job.prompt = Some("x".into());
        // Large but non-zero so we exercise the cancel branch, not timeout=0.
        job.timeout_secs = 60;
        let job_id = job.id.clone();
        let bin = stub.to_string_lossy().into_owned();
        let workdir: PathBuf = dir.path().into();

        let handle =
            tokio::spawn(async move { run_cli_process(&mut job, &bin, &[], &workdir).await });

        // Give the child time to start and print.
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel_cli_job(&job_id).await.unwrap();

        let out = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("cancel must complete within 5s (process must die)")
            .expect("join")
            .expect("run_cli_process");

        assert!(
            matches!(out.status, JobStatus::Cancelled),
            "expected Cancelled, got {:?} errors={:?}",
            out.status,
            out.errors
        );
        assert!(
            out.summary.contains("started-ok") || out.summary.is_empty(),
            "partial summary ok; got {:?}",
            out.summary
        );
        assert!(out.errors.iter().any(|e| e.contains("cancelled")));
    }

    #[tokio::test]
    #[cfg(unix)] // `#!/bin/sh` stub
    async fn cli_timeout_zero_cancel_does_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "forever.sh",
            "#!/bin/sh\necho 'running-forever'\nsleep 300\n",
        )
        .await;

        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "script",
            "forever",
        );
        job.prompt = Some("x".into());
        job.timeout_secs = 0; // no wall-clock deadline — cancel is the only bound
        let job_id = job.id.clone();
        let bin = stub.to_string_lossy().into_owned();
        let workdir: PathBuf = dir.path().into();

        let handle =
            tokio::spawn(async move { run_cli_process(&mut job, &bin, &[], &workdir).await });

        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel_cli_job(&job_id).await.unwrap();

        let out = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("timeout_secs=0 + cancel must not hang forever")
            .expect("join")
            .expect("run_cli_process");

        assert!(
            matches!(out.status, JobStatus::Cancelled),
            "expected Cancelled, got {:?}",
            out.status
        );
    }

    #[tokio::test]
    #[cfg(unix)] // `#!/bin/sh` stub
    async fn backend_cancel_delegates_to_cli_registry() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "backend-cancel.sh",
            "#!/bin/sh\necho 'via-backend'\nsleep 120\n",
        )
        .await;

        let backend = script::ScriptBackend::new(
            stub.to_string_lossy().into_owned(),
            vec![],
            dir.path().into(),
        );
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "script",
            "x",
        );
        job.prompt = Some("x".into());
        job.timeout_secs = 0;
        let job_id = job.id.clone();

        let handle = tokio::spawn(async move {
            let b = script::ScriptBackend::new(
                stub.to_string_lossy().into_owned(),
                vec![],
                dir.path().into(),
            );
            b.run(&mut job, &RunContext::new()).await
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        // Any ScriptBackend instance shares the module registry.
        backend.cancel(&job_id).await.unwrap();

        let out = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("Backend::cancel must stop the job")
            .expect("join")
            .expect("run");

        assert!(matches!(out.status, JobStatus::Cancelled));
    }
}
