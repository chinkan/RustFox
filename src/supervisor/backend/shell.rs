use anyhow::Result;
use std::path::PathBuf;
use tokio::process::Command;

use crate::supervisor::backend::{Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct ShellBackend {
    sandbox: PathBuf,
}

impl ShellBackend {
    pub fn new(sandbox: PathBuf) -> Self {
        Self { sandbox }
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
            return Ok(JobOutput {
                status: JobStatus::Failed,
                summary: String::new(),
                evidence: vec![],
                errors: vec!["sandbox-violation: cd outside sandbox".into()],
                changed_files: vec![],
                next_step: None,
            });
        }
        let output = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&self.sandbox)
            .output()
            .await?;
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
            summary: stdout.trim().to_string(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_backend_runs_echo_in_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let b = ShellBackend::new(dir.path().into());
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
        let b = ShellBackend::new(dir.path().into());
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
}
