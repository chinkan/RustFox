//! ADR-0020 steer drain points A and B in `AgenticLoop` (no network).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustfox::llm::{
    ChatCompletion, ChatMessage, FunctionCall, LlmClient, MessageContent, ToolCall, ToolDefinition,
};
use rustfox::loop_runner::{AgenticLoop, LoopConfig, LoopOutcome, MessageContainer, SteerFn};
use rustfox::mcp::McpManager;
use rustfox::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
use rustfox::provider::{Provider, ProviderConfig, ProviderRegistry};
use rustfox::tool_registry::{ToolContext, ToolHandler, ToolRegistry, ToolUiMode};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<Vec<String>>>>;

/// Replies from a script and records the user texts it was shown per call.
struct ScriptLlm {
    config: ProviderConfig,
    steps: Mutex<VecDeque<ChatMessage>>,
    seen: Seen,
}

fn text_of(m: &ChatMessage) -> String {
    m.content.as_ref().map(|c| c.as_text()).unwrap_or_default()
}

fn client(steps: Vec<ChatMessage>, seen: Seen) -> LlmClient {
    let provider = Arc::new(ScriptLlm {
        config: ProviderConfig {
            name: "fixture".into(),
            provider_type: rustfox::config::ProviderType::OpenRouter,
            base_url: "http://fixture.invalid/v1".into(),
            api_key: None,
            default_model: "stub".into(),
            supports_vision: false,
            max_tokens: 64,
            discover_models: false,
            context_window: 4096,
            context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
        },
        steps: Mutex::new(VecDeque::from(steps)),
        seen,
    });
    let mut providers = HashMap::new();
    providers.insert("fixture".to_string(), provider as Arc<dyn Provider>);
    LlmClient::new(Arc::new(ProviderRegistry::new(providers, "fixture".into())))
}

#[async_trait]
impl Provider for ScriptLlm {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        false
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }
    async fn chat_completion(
        &self,
        _client: &reqwest::Client,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<ChatCompletion> {
        self.seen.lock().unwrap().push(
            messages
                .iter()
                .filter(|m| m.role == "user")
                .map(text_of)
                .collect(),
        );
        let message = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| text("no-more-steps"));
        Ok(ChatCompletion {
            message,
            finish_reason: Some("stop".into()),
            model: model.to_string(),
        })
    }
    async fn list_models(&self, _client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        Ok(vec!["stub".into()])
    }
}

fn text(t: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: Some(MessageContent::from_text(t.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn user(t: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        ..text(t)
    }
}

fn tool_step() -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "t1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "ok_tool".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
    }
}

struct OkTool;

#[async_trait]
impl ToolHandler for OkTool {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            tool_type: "function".into(),
            function: rustfox::llm::FunctionDefinition {
                name: "ok_tool".into(),
                description: "test tool".into(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        }]
    }
    async fn execute(&self, _: &str, _: Value, _: ToolContext) -> anyhow::Result<String> {
        Ok("done".into())
    }
}

struct NullSender;

#[async_trait]
impl PlatformSender for NullSender {
    async fn send_message(
        &self,
        _: &str,
        _: &str,
        _: MessageFormat,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
    }
    async fn send_file(
        &self,
        _: &str,
        _: &std::path::Path,
        _: Option<&str>,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
    }
    async fn show_cancel_button(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
    }
    async fn edit_message(&self, _: &str, _: &PlatformMessageId, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_message(&self, _: &str, _: &PlatformMessageId) -> anyhow::Result<()> {
        Ok(())
    }
    async fn notify_shutdown(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn cfg(max_iterations: u32) -> LoopConfig {
    LoopConfig {
        max_iterations,
        empty_response_retry_limit: 1,
        context_window: 4096,
        loop_detection_enabled: false,
        interactive_loop_callback: false,
        allowed_tools: None,
        langsmith_project: None,
        model: None,
        tool_event_tx: None,
        stream_token_tx: None,
        recovery_nudge: None,
    }
}

/// Steer source that hands out one scripted batch per call.
fn scripted_steer<'a>(batches: Vec<Vec<&'static str>>) -> SteerFn<'a> {
    let q = Arc::new(Mutex::new(VecDeque::from(batches)));
    Box::new(move || {
        let batch = q.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(async move { batch.into_iter().map(user).collect() })
    })
}

async fn run(
    steps: Vec<ChatMessage>,
    batches: Vec<Vec<&'static str>>,
    max_iterations: u32,
) -> (LoopOutcome, Vec<Vec<String>>, Vec<ChatMessage>) {
    let seen: Seen = Arc::default();
    let llm = client(steps, seen.clone());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(OkTool));
    let mcp = McpManager::new();
    let cfg = cfg(max_iterations);
    let sender = Arc::new(NullSender);
    let s2 = sender.clone();
    let make_ctx = move |uid: &str, cid: &str| ToolContext {
        sandbox_dir: std::path::PathBuf::from("/tmp"),
        home_dir: None,
        sender: s2.clone(),
        cancel_registry: Arc::new(rustfox::cancel_registry::CancelRegistry::new()),
        user_id: uid.into(),
        chat_id: cid.into(),
        bot_id: "main".into(),
        tool_ui_mode: ToolUiMode::Silent,
    };
    let mut messages = MessageContainer::Plain(vec![user("first")]);
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        None,
        None,
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .with_steer(scripted_steer(batches))
    .run(&mut messages, "u", "c")
    .await
    .unwrap();
    let MessageContainer::Plain(msgs) = messages else {
        unreachable!()
    };
    let seen = seen.lock().unwrap().clone();
    (outcome, seen, msgs)
}

#[tokio::test]
async fn drain_a_injects_before_next_llm_call() {
    // Call 1 runs a tool; "also do Y" arrives meanwhile and is seen on call 2.
    let (outcome, seen, _) = run(
        vec![tool_step(), text("did X and Y")],
        vec![vec![], vec!["[Steer] also do Y"]],
        5,
    )
    .await;
    assert!(
        matches!(outcome, LoopOutcome::FinalResponse { ref text, iterations: 2 } if text == "did X and Y")
    );
    assert_eq!(seen[0], ["first"]);
    assert_eq!(seen[1], ["first", "[Steer] also do Y"]);
}

#[tokio::test]
async fn drain_b_reruns_instead_of_returning_the_stale_draft() {
    // Drain A empty, then a message lands while the final draft was produced.
    let (outcome, seen, msgs) = run(
        vec![text("draft"), text("updated answer")],
        vec![vec![], vec!["[Steer] wait, use Z"]],
        5,
    )
    .await;
    assert!(
        matches!(outcome, LoopOutcome::FinalResponse { ref text, iterations: 2 } if text == "updated answer"),
        "{outcome:?}"
    );
    assert_eq!(seen[1], ["first", "[Steer] wait, use Z"]);
    let roles: Vec<_> = msgs.iter().map(|m| (m.role.as_str(), text_of(m))).collect();
    assert_eq!(
        roles,
        [
            ("user", "first".to_string()),
            ("assistant", "draft".to_string()),
            ("user", "[Steer] wait, use Z".to_string()),
        ],
        "draft kept as context before the steer message"
    );
}

#[tokio::test]
async fn steer_drains_count_toward_max_iterations() {
    // Every draft is superseded by a new message: the budget still ends it.
    let (outcome, seen, _) = run(
        vec![text("d1"), text("d2")],
        vec![vec![], vec!["a"], vec![], vec!["b"]],
        2,
    )
    .await;
    assert!(matches!(outcome, LoopOutcome::MaxIterations), "{outcome:?}");
    assert_eq!(seen.len(), 2);
}

#[tokio::test]
async fn no_steer_source_behaves_as_before() {
    let (outcome, seen, _) = run(vec![text("hi")], vec![], 3).await;
    assert!(
        matches!(outcome, LoopOutcome::FinalResponse { ref text, iterations: 1 } if text == "hi")
    );
    assert_eq!(seen.len(), 1);
}
