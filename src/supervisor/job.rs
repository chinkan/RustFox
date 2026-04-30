use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    PlannerJob,
    ExecutorJob,
    ReviewerJob,
    VerifierJob,
    ResearchJob,
    ShellJob,
    DocumentJob,
    ApprovalJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    ExitCode(i32),
    FileCreated {
        path: String,
        sha256: Option<String>,
    },
    TestPassed {
        name: String,
    },
    OutputValidated {
        description: String,
    },
    LogStored {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutput {
    pub status: JobStatus,
    pub summary: String,
    pub evidence: Vec<Evidence>,
    pub errors: Vec<String>,
    pub changed_files: Vec<String>,
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub task_id: String,
    pub parent_job_id: Option<String>,
    pub job_type: JobType,
    pub backend: String,
    pub goal: String,
    pub prompt: Option<String>,
    pub input_context: serde_json::Value,
    pub timeout_secs: u64,
    pub retry_max: u32,
    pub retry_count: u32,
    pub allow_tools: Vec<String>,
    pub workspace: Option<String>,
    pub status: JobStatus,
    pub result: Option<JobOutput>,
    pub error: Option<String>,
}

impl Job {
    pub fn new(task_id: &str, job_type: JobType, backend: &str, goal: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            parent_job_id: None,
            job_type,
            backend: backend.to_string(),
            goal: goal.to_string(),
            prompt: None,
            input_context: serde_json::Value::Null,
            timeout_secs: 600,
            retry_max: 0,
            retry_count: 0,
            allow_tools: Vec::new(),
            workspace: None,
            status: JobStatus::Pending,
            result: None,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_output_contract_required_fields() {
        let out = JobOutput {
            status: JobStatus::Succeeded,
            summary: "ok".into(),
            evidence: vec![Evidence::ExitCode(0)],
            errors: vec![],
            changed_files: vec![],
            next_step: None,
        };
        assert!(matches!(out.status, JobStatus::Succeeded));
    }
}
