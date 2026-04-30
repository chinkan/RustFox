use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::supervisor::backend::{Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct ScriptBackend {
    bin: String,
    args: Vec<String>,
    workdir: PathBuf,
}

impl ScriptBackend {
    pub fn new(bin: String, args: Vec<String>, workdir: PathBuf) -> Self {
        Self { bin, args, workdir }
    }
}

#[async_trait::async_trait]
impl Backend for ScriptBackend {
    fn name(&self) -> &str {
        "script"
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
        let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        let timeout_secs = job.timeout_secs;
        job.status = JobStatus::Running;

        let mut cmd = Command::new(&self.bin);
        cmd.args(&self.args)
            .current_dir(&self.workdir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
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
            evidence: vec![Evidence::ExitCode(exit)],
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
    async fn script_backend_runs_stub_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("script-stub.sh");
        tokio::fs::write(&stub, "#!/bin/sh\necho 'script output'\n")
            .await
            .unwrap();
        let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&stub, perms).await.unwrap();

        let b = ScriptBackend::new(
            stub.to_string_lossy().into_owned(),
            vec![],
            dir.path().into(),
        );
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ShellJob,
            "script",
            "run script",
        );
        job.prompt = Some("input".into());
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(out.summary.contains("script output"));
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Succeeded
        ));
    }
}
