use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::supervisor::backend::{Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

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

    async fn kill_child(child: &mut tokio::process::Child) {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    async fn drain(stream: &mut Option<impl AsyncReadExt + Unpin>) -> String {
        let mut out = String::new();
        let mut buf = vec![0u8; 4096];
        if let Some(s) = stream.as_mut() {
            loop {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        }
        out
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

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let timeout_fut = async {
            if timeout_secs == 0 {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_secs(timeout_secs)).await;
            }
        };
        tokio::pin!(timeout_fut);

        let timed_out;
        let exit_code;
        tokio::select! {
            status = child.wait() => {
                timed_out = false;
                exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
            }
            _ = &mut timeout_fut => {
                timed_out = true;
                exit_code = -1;
                Self::kill_child(&mut child).await;
            }
        }

        let stdout = Self::drain(&mut stdout_pipe).await;
        let stderr = Self::drain(&mut stderr_pipe).await;

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
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(matches!(out.status, JobStatus::Failed));
        assert!(out.errors.iter().any(|e| e.contains("timed out")));
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
