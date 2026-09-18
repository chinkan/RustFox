use anyhow::Result;
use std::path::PathBuf;
use tokio::process::Command;

use crate::supervisor::backend::{Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};
use crate::utils::process::{
    finish_drains, kill_child, optional_timeout, DrainBuf, DRAIN_JOIN_TIMEOUT,
};

pub struct ShellBackend {
    sandbox: PathBuf,
    /// From `sandbox.execute_timeout_secs`. 0 = no sandbox cap (job.timeout_secs only).
    execute_timeout_secs: u64,
}

impl ShellBackend {
    pub fn new(sandbox: PathBuf, execute_timeout_secs: u64) -> Self {
        Self {
            sandbox,
            execute_timeout_secs,
        }
    }

    /// Tighter of job deadline and sandbox wall-clock. 0 sandbox = job only.
    fn effective_timeout_secs(&self, job_timeout_secs: u64) -> u64 {
        match self.execute_timeout_secs {
            0 => job_timeout_secs,
            n if job_timeout_secs == 0 => n,
            n => n.min(job_timeout_secs),
        }
    }

    // TODO(security, M2.5): naive validation — only catches obvious `cd /…`,
    // `cd ..`, and `../` patterns. Determined callers can still escape via
    // `bash -c`, command substitution `$(...)`, or `pushd`. Replace with full
    // path canonicalization (see `validate_sandbox_path` in src/tools.rs) before
    // exposing ShellBackend through any user-facing entrypoint.
    fn validate(&self, cmd: &str) -> bool {
        let lower = cmd.trim_start();
        if lower.starts_with("cd /") || lower.contains("cd ..") {
            return false;
        }
        if lower.contains("../") {
            return false;
        }
        true
    }
}

#[async_trait::async_trait]
impl Backend for ShellBackend {
    fn name(&self) -> &str {
        "shell"
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            shell: true,
            ..Default::default()
        }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(jt, JobType::ShellJob)
    }
    async fn run(&self, job: &mut Job, _ctx: &RunContext) -> Result<JobOutput> {
        let cmd = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        if !self.validate(&cmd) {
            job.status = JobStatus::Failed;
            return Ok(JobOutput::failed(vec![
                "sandbox-violation: cd outside sandbox".into(),
            ]));
        }

        let timeout_secs = self.effective_timeout_secs(job.timeout_secs);
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&cmd)
            .current_dir(&self.sandbox)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                job.status = JobStatus::Failed;
                return Ok(JobOutput::failed(vec![format!("command failed: {e}")]));
            }
        };

        let stdout_buf = DrainBuf::new();
        let stderr_buf = DrainBuf::new();
        let mut drain_handles = Vec::new();
        if let Some(s) = child.stdout.take() {
            drain_handles.push(stdout_buf.spawn_reader(s));
        }
        if let Some(s) = child.stderr.take() {
            drain_handles.push(stderr_buf.spawn_reader(s));
        }

        let timeout_fut = optional_timeout(timeout_secs);
        tokio::pin!(timeout_fut);

        let timed_out;
        let exit_code;
        tokio::select! {
            status = child.wait() => {
                timed_out = false;
                exit_code = match status {
                    Ok(s) => s.code().unwrap_or(-1),
                    Err(_) => -1,
                };
            }
            _ = &mut timeout_fut => {
                timed_out = true;
                exit_code = -1;
                kill_child(&mut child).await;
            }
        }

        // Parallel drain join; partials already in DrainBuf if we abort.
        finish_drains(drain_handles, DRAIN_JOIN_TIMEOUT).await;
        let stdout = stdout_buf.snapshot().await;
        let stderr = stderr_buf.snapshot().await;

        if timed_out {
            job.status = JobStatus::Failed;
            let mut errors = vec![format!("timed out after {timeout_secs}s")];
            if !stderr.is_empty() {
                errors.push(stderr);
            }
            let mut out = JobOutput::failed(errors);
            out.summary = stdout.trim().to_string();
            return Ok(out);
        }

        let status = if exit_code == 0 {
            JobStatus::Succeeded
        } else {
            JobStatus::Failed
        };
        job.status = status.clone();
        Ok(JobOutput {
            status,
            summary: stdout.trim().to_string(),
            evidence: vec![Evidence::ExitCode { code: exit_code }],
            errors: if stderr.is_empty() {
                vec![]
            } else {
                vec![stderr]
            },
            changed_files: vec![],
            next_step: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn shell_backend_runs_echo_in_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into(), 90);
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "shell",
            "echo hi",
        );
        job.prompt = Some("echo hi".into());
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Succeeded
        ));
        assert!(out.summary.contains("hi"));
        assert!(matches!(
            out.evidence[0],
            crate::supervisor::job::Evidence::ExitCode { code: 0 }
        ));
    }

    #[tokio::test]
    async fn shell_backend_rejects_command_escaping_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into(), 90);
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "shell",
            "cd /etc && cat passwd",
        );
        job.prompt = Some("cd /etc && cat passwd".into());
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Failed
        ));
    }

    #[tokio::test]
    async fn shell_backend_timeout_kills_and_respects_config() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into(), 1);
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "shell",
            "sleep 30",
        );
        job.prompt = Some("sleep 30".into());
        job.timeout_secs = 600;
        let start = std::time::Instant::now();
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "timeout path took {:?}",
            start.elapsed()
        );
        assert!(matches!(out.status, JobStatus::Failed));
        assert!(out.errors.iter().any(|e| e.contains("timed out")));
    }

    #[tokio::test]
    async fn shell_backend_timeout_keeps_partial_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into(), 2);
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "shell",
            "partial",
        );
        // Emit then sleep past timeout — partial must survive.
        job.prompt = Some("echo hi; sleep 30".into());
        job.timeout_secs = 600;
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(matches!(out.status, JobStatus::Failed));
        assert!(
            out.summary.contains("hi"),
            "expected partial stdout, got {:?}",
            out.summary
        );
    }

    #[tokio::test]
    async fn shell_backend_large_stdout_does_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into(), 15);
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "shell",
            "large",
        );
        // POSIX dd — no python3 dependency.
        job.prompt =
            Some("dd if=/dev/zero bs=1024 count=200 2>/dev/null | tr '\\0' 'x'; echo".into());
        job.timeout_secs = 15;
        let start = std::time::Instant::now();
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "large stdout took too long (possible pipe deadlock): {:?}",
            start.elapsed()
        );
        assert!(matches!(out.status, JobStatus::Succeeded));
        assert!(out.summary.len() >= 100_000);
    }

    #[test]
    fn effective_timeout_picks_tighter_cap() {
        let b = ShellBackend::new(PathBuf::from("/tmp"), 90);
        assert_eq!(b.effective_timeout_secs(600), 90);
        assert_eq!(b.effective_timeout_secs(30), 30);
        let unlimited = ShellBackend::new(PathBuf::from("/tmp"), 0);
        assert_eq!(unlimited.effective_timeout_secs(600), 600);
    }
}
