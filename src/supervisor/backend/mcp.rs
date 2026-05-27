use anyhow::Result;
use std::sync::Arc;

use crate::mcp::McpManager;
use crate::supervisor::backend::{Backend, BackendCapabilities, RunContext};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct McpBackend {
    mcp: Arc<McpManager>,
}

impl McpBackend {
    pub fn new(mcp: Arc<McpManager>) -> Self {
        Self { mcp }
    }
}

#[async_trait::async_trait]
impl Backend for McpBackend {
    fn name(&self) -> &str {
        "mcp"
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            research: true,
            document: true,
            ..Default::default()
        }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(jt, JobType::ResearchJob | JobType::DocumentJob)
    }
    async fn run(&self, job: &mut Job, _ctx: &RunContext) -> Result<JobOutput> {
        // input_context = {"tool": "mcp_<server>_<tool>", "args": {...}}
        let tool_name = job
            .input_context
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing tool name"))?
            .to_string();
        let args = job
            .input_context
            .get("args")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        job.status = JobStatus::Running;
        let result = self.mcp.call_tool(&tool_name, &args).await;
        match result {
            Ok(text) => {
                job.status = JobStatus::Succeeded;
                Ok(JobOutput {
                    status: JobStatus::Succeeded,
                    summary: text,
                    evidence: vec![Evidence::OutputValidated {
                        description: format!("mcp tool {tool_name} returned non-error"),
                    }],
                    errors: vec![],
                    changed_files: vec![],
                    next_step: None,
                })
            }
            Err(e) => {
                job.status = JobStatus::Failed;
                Ok(JobOutput {
                    status: JobStatus::Failed,
                    summary: String::new(),
                    evidence: vec![],
                    errors: vec![format!("{e:#}")],
                    changed_files: vec![],
                    next_step: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_backend_advertises_research_and_document() {
        let mgr = std::sync::Arc::new(crate::mcp::McpManager::new());
        let b = McpBackend::new(mgr);
        let c = b.capabilities();
        assert!(c.research && c.document);
    }
}
