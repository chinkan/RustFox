use anyhow::Result;
use std::path::PathBuf;

use crate::supervisor::backend::{run_cli_process, Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Job, JobOutput, JobType};

pub struct ClaudeCodeCliBackend {
    bin: String,
    args: Vec<String>,
    workdir: PathBuf,
}

impl ClaudeCodeCliBackend {
    pub fn new(bin: String, args: Vec<String>, workdir: PathBuf) -> Self {
        Self { bin, args, workdir }
    }
}

#[async_trait::async_trait]
impl Backend for ClaudeCodeCliBackend {
    fn name(&self) -> &str {
        "claude_code_cli"
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            coding: true,
            reasoning: true,
            long_running: true,
            ..Default::default()
        }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(
            jt,
            JobType::ExecutorJob | JobType::ReviewerJob | JobType::PlannerJob
        )
    }
    async fn run(&self, job: &mut Job, _ctx: &RunContext) -> Result<JobOutput> {
        run_cli_process(job, &self.bin, &self.args, &self.workdir).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claude_code_backend_runs_stub_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("claude-stub.sh");
        tokio::fs::write(&stub, "#!/bin/sh\necho 'pretend output'\n")
            .await
            .unwrap();
        let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&stub, perms).await.unwrap();

        let b = ClaudeCodeCliBackend::new(
            stub.to_string_lossy().into_owned(),
            vec!["--print".into()],
            dir.path().into(),
        );
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ExecutorJob,
            "claude_code_cli",
            "do x",
        );
        job.prompt = Some("do x".into());
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(out.summary.contains("pretend output"));
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Succeeded
        ));
    }

    #[tokio::test]
    async fn claude_code_backend_times_out_when_cli_hangs() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hang-stub.sh");
        tokio::fs::write(&stub, "#!/bin/sh\nsleep 30\n")
            .await
            .unwrap();
        let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&stub, perms).await.unwrap();

        let b = ClaudeCodeCliBackend::new(
            stub.to_string_lossy().into_owned(),
            vec![],
            dir.path().into(),
        );
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ExecutorJob,
            "claude_code_cli",
            "x",
        );
        job.prompt = Some("x".into());
        job.timeout_secs = 1;
        let started = std::time::Instant::now();
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        let elapsed = started.elapsed();
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Failed
        ));
        assert!(out.errors.iter().any(|e| e.contains("timed out")));
        assert!(
            elapsed.as_secs() < 5,
            "should have killed child within seconds"
        );
    }
}
