use anyhow::Result;
use std::path::PathBuf;

use crate::supervisor::backend::{run_cli_process, Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Job, JobOutput, JobType};

pub struct CodexCliBackend {
    bin: String,
    args: Vec<String>,
    workdir: PathBuf,
}

impl CodexCliBackend {
    pub fn new(bin: String, args: Vec<String>, workdir: PathBuf) -> Self {
        Self { bin, args, workdir }
    }
}

#[async_trait::async_trait]
impl Backend for CodexCliBackend {
    fn name(&self) -> &str {
        "codex_cli"
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
    async fn codex_cli_backend_runs_stub_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("codex-stub.sh");
        tokio::fs::write(&stub, "#!/bin/sh\necho 'codex output'\n")
            .await
            .unwrap();
        let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&stub, perms).await.unwrap();

        let b = CodexCliBackend::new(
            stub.to_string_lossy().into_owned(),
            vec![],
            dir.path().into(),
        );
        let mut job = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ExecutorJob,
            "codex_cli",
            "do y",
        );
        job.prompt = Some("do y".into());
        let out = b.run(&mut job, &RunContext::new()).await.unwrap();
        assert!(out.summary.contains("codex output"));
        assert!(matches!(
            out.status,
            crate::supervisor::job::JobStatus::Succeeded
        ));
    }
}
