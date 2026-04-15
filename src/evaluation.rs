use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::hooks::{AgentHook, HookContext};

// ── Trajectory ──────────────────────────────────────────────────────────

/// The kind of event recorded in a trajectory entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryEventKind {
    /// The initial user message that started this request.
    UserMessage,
    /// An LLM API call was made.
    LlmCall,
    /// An LLM response was received.
    LlmResponse,
    /// A tool was invoked.
    ToolCall,
    /// A tool returned a result.
    ToolResult,
    /// A subagent was started.
    SubagentStart,
    /// A subagent returned a result.
    SubagentResult,
    /// The final text response was produced.
    FinalResponse,
    /// An error occurred during processing.
    Error,
}

/// A single recorded event in the agent trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryEntry {
    /// ISO-8601 timestamp of the event.
    pub timestamp: String,
    /// The kind of event.
    pub event: TrajectoryEventKind,
    /// Event-specific data (tool name, arguments, result, etc.).
    pub data: serde_json::Value,
    /// The agentic-loop iteration when this event occurred.
    pub iteration: u32,
}

/// A complete trajectory for one `process_message` invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// Unique identifier for this request (typically a UUID).
    pub request_id: String,
    /// The user who initiated the request.
    pub user_id: String,
    /// The chat/conversation this request belongs to.
    pub chat_id: String,
    /// All recorded events, in chronological order.
    pub entries: Vec<TrajectoryEntry>,
    /// ISO-8601 timestamp when the request started.
    pub start_time: String,
    /// ISO-8601 timestamp when the request completed (set at the end).
    pub end_time: Option<String>,
}

impl Trajectory {
    /// Create a new trajectory for a request.
    pub fn new(request_id: &str, user_id: &str, chat_id: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            user_id: user_id.to_string(),
            chat_id: chat_id.to_string(),
            entries: Vec::new(),
            start_time: now_iso8601(),
            end_time: None,
        }
    }

    /// Append an entry to the trajectory.
    pub fn record(&mut self, event: TrajectoryEventKind, data: serde_json::Value, iteration: u32) {
        self.entries.push(TrajectoryEntry {
            timestamp: now_iso8601(),
            event,
            data,
            iteration,
        });
    }

    /// Mark the trajectory as complete.
    pub fn finish(&mut self) {
        self.end_time = Some(now_iso8601());
    }

    /// Count how many entries match a given event kind.
    pub fn count_events(&self, kind: &TrajectoryEventKind) -> usize {
        self.entries.iter().filter(|e| &e.event == kind).count()
    }

    /// Return total number of tool calls recorded.
    pub fn tool_call_count(&self) -> usize {
        self.count_events(&TrajectoryEventKind::ToolCall)
    }

    /// Return true if the trajectory contains any error events.
    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.event == TrajectoryEventKind::Error)
    }

    /// Return true if the trajectory ends with a final response.
    pub fn has_final_response(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.event == TrajectoryEventKind::FinalResponse)
    }
}

// ── Evaluator trait ─────────────────────────────────────────────────────

/// The outcome of running one evaluator against a trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// Name of the evaluator that produced this result.
    pub evaluator_name: String,
    /// Whether the evaluation passed.
    pub passed: bool,
    /// Optional numeric score (0.0–1.0 scale recommended).
    pub score: Option<f64>,
    /// Human-readable explanation of the evaluation outcome.
    pub reason: String,
    /// Additional evaluator-specific data.
    pub metadata: serde_json::Value,
}

/// Trait for implementing custom evaluation logic that inspects a completed
/// trajectory and produces an [`EvaluationResult`].
#[async_trait::async_trait]
pub trait Evaluator: Send + Sync {
    /// A short identifier for this evaluator.
    fn name(&self) -> &str;

    /// Evaluate the given trajectory and return a result.
    async fn evaluate(&self, trajectory: &Trajectory) -> Result<EvaluationResult>;
}

// ── EvaluationManager ───────────────────────────────────────────────────

/// Holds registered evaluators and runs them against completed trajectories.
pub struct EvaluationManager {
    evaluators: Vec<Box<dyn Evaluator>>,
}

impl EvaluationManager {
    /// Create an empty evaluation manager.
    pub fn new() -> Self {
        Self {
            evaluators: Vec::new(),
        }
    }

    /// Register a new evaluator.
    pub fn register(&mut self, evaluator: Box<dyn Evaluator>) {
        self.evaluators.push(evaluator);
    }

    /// Run all registered evaluators against the trajectory and return results.
    pub async fn evaluate_all(&self, trajectory: &Trajectory) -> Vec<EvaluationResult> {
        let mut results = Vec::with_capacity(self.evaluators.len());
        for evaluator in &self.evaluators {
            match evaluator.evaluate(trajectory).await {
                Ok(result) => {
                    if result.passed {
                        debug!(
                            evaluator = %result.evaluator_name,
                            score = ?result.score,
                            "Evaluation passed: {}",
                            result.reason
                        );
                    } else {
                        info!(
                            evaluator = %result.evaluator_name,
                            score = ?result.score,
                            "Evaluation failed: {}",
                            result.reason
                        );
                    }
                    results.push(result);
                }
                Err(e) => {
                    warn!(
                        evaluator = %evaluator.name(),
                        "Evaluator error: {:#}",
                        e
                    );
                    results.push(EvaluationResult {
                        evaluator_name: evaluator.name().to_string(),
                        passed: false,
                        score: None,
                        reason: format!("Evaluator error: {:#}", e),
                        metadata: serde_json::Value::Null,
                    });
                }
            }
        }
        results
    }
}

// ── Built-in evaluator: SuccessCriteriaEvaluator ────────────────────────

/// Built-in evaluator that checks basic success criteria:
/// - Did the trajectory produce a final response?
/// - Were there no errors?
/// - Did it stay within the iteration budget?
pub struct SuccessCriteriaEvaluator {
    /// Maximum allowed iterations before considering the task failed.
    pub max_iterations: u32,
}

impl SuccessCriteriaEvaluator {
    pub fn new(max_iterations: u32) -> Self {
        Self { max_iterations }
    }
}

#[async_trait::async_trait]
impl Evaluator for SuccessCriteriaEvaluator {
    fn name(&self) -> &str {
        "success_criteria"
    }

    async fn evaluate(&self, trajectory: &Trajectory) -> Result<EvaluationResult> {
        let has_final = trajectory.has_final_response();
        let has_errors = trajectory.has_errors();
        let total_llm_calls = trajectory.count_events(&TrajectoryEventKind::LlmResponse);
        let within_budget = (total_llm_calls as u32) <= self.max_iterations;

        let passed = has_final && !has_errors && within_budget;

        let mut reasons = Vec::new();
        if !has_final {
            reasons.push("no final response produced".to_string());
        }
        if has_errors {
            reasons.push("errors occurred during processing".to_string());
        }
        if !within_budget {
            reasons.push(format!(
                "exceeded iteration budget ({} > {})",
                total_llm_calls, self.max_iterations
            ));
        }
        let reason = if passed {
            "all success criteria met".to_string()
        } else {
            reasons.join("; ")
        };

        let score = if passed { Some(1.0) } else { Some(0.0) };

        Ok(EvaluationResult {
            evaluator_name: self.name().to_string(),
            passed,
            score,
            reason,
            metadata: serde_json::json!({
                "has_final_response": has_final,
                "has_errors": has_errors,
                "llm_calls": total_llm_calls,
                "tool_calls": trajectory.tool_call_count(),
                "within_budget": within_budget,
            }),
        })
    }
}

// ── TrajectoryHook: bridge between hooks and trajectory ─────────────────

/// An [`AgentHook`] implementation that automatically records trajectory entries
/// for every lifecycle event. The recorded trajectory can later be retrieved
/// and passed to evaluators.
///
/// Because the hook is invoked via shared `&self`, the internal trajectory is
/// wrapped in a `tokio::sync::Mutex`.
pub struct TrajectoryHook {
    trajectory: tokio::sync::Mutex<Trajectory>,
}

impl TrajectoryHook {
    /// Create a new trajectory hook for the given request.
    pub fn new(request_id: &str, user_id: &str, chat_id: &str) -> Self {
        Self {
            trajectory: tokio::sync::Mutex::new(Trajectory::new(request_id, user_id, chat_id)),
        }
    }

    /// Take a snapshot of the current trajectory.
    #[allow(dead_code)]
    pub async fn snapshot(&self) -> Trajectory {
        self.trajectory.lock().await.clone()
    }

    /// Finalize the trajectory and return the completed copy.
    pub async fn finalize(&self) -> Trajectory {
        let mut t = self.trajectory.lock().await;
        t.finish();
        t.clone()
    }

    /// Record an event directly into the trajectory.
    pub async fn record(&self, event: TrajectoryEventKind, data: serde_json::Value, iteration: u32) {
        self.trajectory.lock().await.record(event, data, iteration);
    }
}

#[async_trait::async_trait]
impl AgentHook for TrajectoryHook {
    async fn pre_process(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::UserMessage,
            serde_json::json!({ "message": ctx.message }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn pre_llm_call(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::LlmCall,
            serde_json::json!({ "iteration": ctx.iteration }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn post_llm_call(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::LlmResponse,
            serde_json::json!({ "iteration": ctx.iteration }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn pre_tool_call(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::ToolCall,
            serde_json::json!({
                "tool": ctx.tool_name,
                "args": ctx.tool_args,
            }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn post_tool_call(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::ToolResult,
            serde_json::json!({
                "tool": ctx.tool_name,
                "result_length": ctx.tool_result.as_ref().map(|r| r.len()),
            }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn pre_subagent(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::SubagentStart,
            serde_json::json!({ "agent": ctx.tool_name }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn post_subagent(&self, ctx: &HookContext) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::SubagentResult,
            serde_json::json!({
                "agent": ctx.tool_name,
                "result_length": ctx.tool_result.as_ref().map(|r| r.len()),
            }),
            ctx.iteration,
        );
        Ok(())
    }

    async fn on_error(&self, ctx: &HookContext, error: &str) -> Result<()> {
        let mut t = self.trajectory.lock().await;
        t.record(
            TrajectoryEventKind::Error,
            serde_json::json!({ "error": error }),
            ctx.iteration,
        );
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trajectory_new_has_start_time() {
        let t = Trajectory::new("req-1", "user-1", "chat-1");
        assert_eq!(t.request_id, "req-1");
        assert!(!t.start_time.is_empty());
        assert!(t.end_time.is_none());
        assert!(t.entries.is_empty());
    }

    #[test]
    fn test_trajectory_record_and_counts() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(
            TrajectoryEventKind::UserMessage,
            serde_json::json!({}),
            0,
        );
        t.record(TrajectoryEventKind::LlmCall, serde_json::json!({}), 0);
        t.record(TrajectoryEventKind::LlmResponse, serde_json::json!({}), 0);
        t.record(TrajectoryEventKind::ToolCall, serde_json::json!({}), 0);
        t.record(TrajectoryEventKind::ToolResult, serde_json::json!({}), 0);
        t.record(
            TrajectoryEventKind::FinalResponse,
            serde_json::json!({}),
            1,
        );

        assert_eq!(t.entries.len(), 6);
        assert_eq!(t.tool_call_count(), 1);
        assert!(t.has_final_response());
        assert!(!t.has_errors());
    }

    #[test]
    fn test_trajectory_finish_sets_end_time() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        assert!(t.end_time.is_none());
        t.finish();
        assert!(t.end_time.is_some());
    }

    #[test]
    fn test_trajectory_has_errors() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        assert!(!t.has_errors());
        t.record(
            TrajectoryEventKind::Error,
            serde_json::json!({"error": "boom"}),
            0,
        );
        assert!(t.has_errors());
    }

    #[tokio::test]
    async fn test_success_criteria_evaluator_pass() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(TrajectoryEventKind::LlmCall, serde_json::json!({}), 0);
        t.record(TrajectoryEventKind::LlmResponse, serde_json::json!({}), 0);
        t.record(
            TrajectoryEventKind::FinalResponse,
            serde_json::json!({}),
            0,
        );

        let evaluator = SuccessCriteriaEvaluator::new(25);
        let result = evaluator.evaluate(&t).await.unwrap();
        assert!(result.passed);
        assert_eq!(result.score, Some(1.0));
    }

    #[tokio::test]
    async fn test_success_criteria_evaluator_fail_no_final() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(TrajectoryEventKind::LlmCall, serde_json::json!({}), 0);
        t.record(TrajectoryEventKind::LlmResponse, serde_json::json!({}), 0);

        let evaluator = SuccessCriteriaEvaluator::new(25);
        let result = evaluator.evaluate(&t).await.unwrap();
        assert!(!result.passed);
        assert!(result.reason.contains("no final response"));
    }

    #[tokio::test]
    async fn test_success_criteria_evaluator_fail_with_errors() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(
            TrajectoryEventKind::Error,
            serde_json::json!({"error": "fail"}),
            0,
        );
        t.record(
            TrajectoryEventKind::FinalResponse,
            serde_json::json!({}),
            0,
        );

        let evaluator = SuccessCriteriaEvaluator::new(25);
        let result = evaluator.evaluate(&t).await.unwrap();
        assert!(!result.passed);
        assert!(result.reason.contains("errors occurred"));
    }

    #[tokio::test]
    async fn test_success_criteria_evaluator_fail_budget() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        for _ in 0..5 {
            t.record(TrajectoryEventKind::LlmResponse, serde_json::json!({}), 0);
        }
        t.record(
            TrajectoryEventKind::FinalResponse,
            serde_json::json!({}),
            0,
        );

        let evaluator = SuccessCriteriaEvaluator::new(3); // budget = 3, but 5 calls made
        let result = evaluator.evaluate(&t).await.unwrap();
        assert!(!result.passed);
        assert!(result.reason.contains("exceeded iteration budget"));
    }

    #[tokio::test]
    async fn test_evaluation_manager_runs_all() {
        let mut manager = EvaluationManager::new();
        manager.register(Box::new(SuccessCriteriaEvaluator::new(25)));

        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(TrajectoryEventKind::LlmResponse, serde_json::json!({}), 0);
        t.record(
            TrajectoryEventKind::FinalResponse,
            serde_json::json!({}),
            0,
        );

        let results = manager.evaluate_all(&t).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
    }

    #[tokio::test]
    async fn test_trajectory_hook_records_events() {
        let hook = TrajectoryHook::new("req-1", "user-1", "chat-1");
        let ctx = HookContext::new("user-1", "chat-1")
            .with_message("hello")
            .with_iteration(0);

        hook.pre_process(&ctx).await.unwrap();
        hook.pre_llm_call(&ctx).await.unwrap();
        hook.post_llm_call(&ctx).await.unwrap();

        let tool_ctx = HookContext::new("user-1", "chat-1")
            .with_tool_name("read_file")
            .with_iteration(0);
        hook.pre_tool_call(&tool_ctx).await.unwrap();
        hook.post_tool_call(&tool_ctx).await.unwrap();

        let snapshot = hook.snapshot().await;
        assert_eq!(snapshot.entries.len(), 5);
        assert_eq!(snapshot.entries[0].event, TrajectoryEventKind::UserMessage);
        assert_eq!(snapshot.entries[1].event, TrajectoryEventKind::LlmCall);
        assert_eq!(snapshot.entries[3].event, TrajectoryEventKind::ToolCall);
    }

    #[tokio::test]
    async fn test_trajectory_hook_finalize() {
        let hook = TrajectoryHook::new("req-1", "user-1", "chat-1");
        let trajectory = hook.finalize().await;
        assert!(trajectory.end_time.is_some());
    }

    #[test]
    fn test_trajectory_serialization() {
        let mut t = Trajectory::new("req-1", "user-1", "chat-1");
        t.record(
            TrajectoryEventKind::UserMessage,
            serde_json::json!({"text": "hello"}),
            0,
        );
        t.finish();

        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("user_message"));
        assert!(json.contains("req-1"));

        let deserialized: Trajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.request_id, "req-1");
        assert_eq!(deserialized.entries.len(), 1);
    }
}
