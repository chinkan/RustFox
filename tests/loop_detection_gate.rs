//! #231: AgenticLoop must break exact tool+args repetition (rewired LoopDetector).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustfox::llm::{
    ChatCompletion, ChatMessage, FunctionCall, LlmClient, MessageContent, ToolCall, ToolDefinition,
};
use rustfox::loop_runner::{AgenticLoop, LoopConfig, LoopOutcome, MessageContainer};
use rustfox::mcp::McpManager;
use rustfox::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
use rustfox::provider::{Provider, ProviderConfig, ProviderRegistry};
use rustfox::tool_registry::{ToolContext, ToolHandler, ToolRegistry, ToolUiMode};
use serde_json::{json, Value};

struct ScriptLlm {
    config: ProviderConfig,
    steps: Mutex<VecDeque<ChatMessage>>,
    calls: Arc<Mutex<usize>>,
}

fn client(steps: Vec<ChatMessage>, calls: Arc<Mutex<usize>>) -> LlmClient {
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
        calls,
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
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<ChatCompletion> {
        *self.calls.lock().unwrap() += 1;
        let message = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::from_text("fallback-text")),
                tool_calls: None,
                tool_call_id: None,
            });
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

fn same_tool_step(id: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "spin_tool".into(),
                arguments: r#"{"x":1}"#.into(),
            },
        }]),
        tool_call_id: None,
    }
}

struct SpinTool {
    execs: Arc<Mutex<usize>>,
}

#[async_trait]
impl ToolHandler for SpinTool {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            tool_type: "function".into(),
            function: rustfox::llm::FunctionDefinition {
                name: "spin_tool".into(),
                description: "repeats".into(),
                parameters: json!({"type": "object", "properties": {"x": {"type": "integer"}}}),
            },
        }]
    }
    async fn execute(&self, _: &str, _: Value, _: ToolContext) -> anyhow::Result<String> {
        *self.execs.lock().unwrap() += 1;
        Ok("same".into())
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

#[tokio::test]
async fn repeated_identical_tool_calls_stop_after_recoveries() {
    // threshold=3 → detect on 3rd identical record. Then nudge (no exec), clear.
    // After 2 recoveries, 3rd detection → FinalResponse stop.
    // Script enough identical tool steps to exceed recoveries without answering.
    let steps: Vec<_> = (0..12).map(|i| same_tool_step(&format!("t{i}"))).collect();
    let calls = Arc::new(Mutex::new(0usize));
    let execs = Arc::new(Mutex::new(0usize));
    let llm = client(steps, calls.clone());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(SpinTool {
        execs: execs.clone(),
    }));
    let mcp = McpManager::new();
    let cfg = LoopConfig {
        max_iterations: 40,
        empty_response_retry_limit: 1,
        context_window: 4096,
        loop_detection_enabled: true,
        loop_detection_threshold: 3,
        interactive_loop_callback: false,
        allowed_tools: None,
        langsmith_project: None,
        model: None,
        tool_event_tx: None,
        stream_token_tx: None,
        recovery_nudge: None,
    };
    let sender = NullSender;
    let loop_runner = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        None,
        None,
        &sender,
        Box::new(|uid, cid| ToolContext {
            sandbox_dir: std::path::PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(NullSender),
            cancel_registry: Arc::new(rustfox::cancel_registry::CancelRegistry::new()),
            user_id: uid.into(),
            chat_id: cid.into(),
            bot_id: "main".into(),
            tool_ui_mode: ToolUiMode::Silent,
        }),
        None,
    );
    let mut msgs = MessageContainer::Plain(vec![ChatMessage {
        role: "user".into(),
        content: Some(MessageContent::from_text("spin")),
        tool_calls: None,
        tool_call_id: None,
    }]);
    let outcome = loop_runner
        .run(&mut msgs, "u", "c")
        .await
        .expect("loop should return Ok");
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert!(
                text.contains("repeatedly") || text.contains("stopped"),
                "expected loop-stop message, got: {text}"
            );
            assert!(
                iterations < 40,
                "must stop before burning max_iterations; got {iterations}"
            );
        }
        other => panic!("expected FinalResponse stop, got {other:?}"),
    }
    let n_exec = *execs.lock().unwrap();
    let n_llm = *calls.lock().unwrap();
    // Executions happen only before each detection window fills; recoveries skip exec.
    assert!(
        n_exec < 20,
        "duplicate tools must not keep executing unbound; execs={n_exec} llm={n_llm}"
    );
    assert!(n_llm < 40, "LLM rounds must be bounded; llm={n_llm}");
}
