use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CodeChange,
    BugFix,
    Refactor,
    Research,
    Writing,
    OpsAutomation,
    WorkflowAutomation,
    DataTransformation,
    DecisionSupport,
    GeneralAssistant,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    Fast,
    Standard,
    Rigorous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaskStatus {
    Intake,
    Classify,
    Route,
    Clarify,
    Plan,
    PrepareWorkspace,
    Execute,
    Review,
    Verify,
    Report,
    Archive,
    Paused,
    Failed,
    Cancelled,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub user_request: String,
    pub task_type: TaskType,
    pub priority: u8,
    pub risk_level: RiskLevel,
    pub execution_mode: ExecutionMode,
    pub status: TaskStatus,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub constraints: serde_json::Value,
    #[serde(default)]
    pub inputs: serde_json::Value,
    #[serde(default)]
    pub expected_outputs: serde_json::Value,
}

impl Task {
    pub fn new(title: &str, user_request: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.to_string(),
            user_request: user_request.to_string(),
            task_type: TaskType::Unknown,
            priority: 5,
            risk_level: RiskLevel::Low,
            execution_mode: ExecutionMode::Standard,
            status: TaskStatus::Intake,
            required_capabilities: Vec::new(),
            constraints: serde_json::Value::Null,
            inputs: serde_json::Value::Null,
            expected_outputs: serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_serializes_round_trip() {
        let t = Task::new("Summarize CHANGELOG", "summarize the changelog file");
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.title, "Summarize CHANGELOG");
        assert_eq!(back.task_type, TaskType::Unknown);
        assert_eq!(back.risk_level, RiskLevel::Low);
        assert_eq!(back.status, TaskStatus::Intake);
    }
}
