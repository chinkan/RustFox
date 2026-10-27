//! ADR-0043 / #277 anti cold-intro on shared Active `AgenticLoop` final-reply path.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustfox::cold_intro::{
    is_capability_cold_intro, COLD_INTRO_CLARIFY_REWRITE, COLD_INTRO_REGEN_NUDGE,
};
use rustfox::llm::{ChatCompletion, ChatMessage, LlmClient, MessageContent, ToolDefinition};
use rustfox::loop_runner::{AgenticLoop, LoopConfig, LoopOutcome, MessageContainer};
use rustfox::mcp::McpManager;
use rustfox::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
use rustfox::provider::{Provider, ProviderConfig, ProviderRegistry};
use rustfox::tool_registry::{ToolContext, ToolRegistry, ToolUiMode};

type Seen = Arc<Mutex<Vec<Vec<String>>>>;

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
            extra_headers: vec![],
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
            .unwrap_or_else(|| assistant("no-more-steps"));
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

fn assistant(t: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: Some(MessageContent::from_text(t.to_string())),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

fn user(t: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        content: Some(MessageContent::from_text(t.to_string())),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

fn system(t: &str) -> ChatMessage {
    ChatMessage {
        role: "system".into(),
        content: Some(MessageContent::from_text(t.to_string())),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

fn cantonese_cold_intro() -> String {
    // Live FN shape (tip 29e4461 / #280 W1): greeting + ≥2 menu, no help-offer.
    "嗨 Fox 喺度 👋 有咩想做？\n\
     - 📰 出帖 (Threads / 廣東話)、fetch 新聞\n\
     - 📁 Obsidian vault 筆記整理、inbox sweep、每日筆記\n\
     - 💰 財務：信用卡月結單 parse、記帳、Google Sheet 同步"
        .to_string()
}

fn english_cold_intro() -> String {
    // W1: greeting + ≥2 menu, help-offer not required (#280).
    "Hello! I'm Fox. What would you like to do?\n\
     - Fetching news and posting to Threads\n\
     - Organizing your Obsidian vault\n\
     - Coding and debugging in the sandbox"
        .to_string()
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
        loop_detection_threshold: 3,
        interactive_loop_callback: false,
        allowed_tools: None,
        langsmith_project: None,
        model: None,
        tool_event_tx: None,
        stream_token_tx: None,
        recovery_nudge: None,
        session_key: None,
    }
}

async fn run_loop(
    history: Vec<ChatMessage>,
    steps: Vec<ChatMessage>,
    max_iterations: u32,
) -> (LoopOutcome, Vec<Vec<String>>, Vec<ChatMessage>) {
    let seen: Seen = Arc::default();
    let llm = client(steps, seen.clone());
    let tools = ToolRegistry::new();
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
    let mut messages = MessageContainer::Plain(history);
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
    .run(&mut messages, "u", "c")
    .await
    .unwrap();
    let MessageContainer::Plain(msgs) = messages else {
        unreachable!()
    };
    let seen = seen.lock().unwrap().clone();
    (outcome, seen, msgs)
}

fn history_with_prior_and_current() -> Vec<ChatMessage> {
    vec![
        system("you are fox"),
        user("earlier request about cron"),
        assistant("scheduled for 09:00"),
        user("跟進一下"),
    ]
}

#[tokio::test]
async fn guard_rejects_cold_intro_then_accepts_regen() {
    let cold = cantonese_cold_intro();
    assert!(is_capability_cold_intro(&cold));
    let (outcome, seen, _) = run_loop(
        history_with_prior_and_current(),
        vec![assistant(&cold), assistant("收到，我而家跟進 cron。")],
        5,
    )
    .await;
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, "收到，我而家跟進 cron。");
            assert!(!is_capability_cold_intro(&text));
            assert_eq!(iterations, 2);
        }
        other => panic!("expected FinalResponse, got {other:?}"),
    }
    assert_eq!(seen.len(), 2);
    assert!(
        seen[1].iter().any(|u| u.contains("capability cold-intro")),
        "regen call must see the nudge: {:?}",
        seen[1]
    );
    assert_eq!(
        seen[1].last().map(String::as_str),
        Some(COLD_INTRO_REGEN_NUDGE)
    );
}

#[tokio::test]
async fn guard_rewrites_when_regen_still_cold() {
    let cold = english_cold_intro();
    assert!(is_capability_cold_intro(&cold));
    let (outcome, seen, _) = run_loop(
        history_with_prior_and_current(),
        vec![assistant(&cold), assistant(&cold)],
        5,
    )
    .await;
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, COLD_INTRO_CLARIFY_REWRITE);
            assert!(!is_capability_cold_intro(&text));
            assert_eq!(iterations, 2);
        }
        other => panic!("expected FinalResponse, got {other:?}"),
    }
    assert_eq!(seen.len(), 2);
}

#[tokio::test]
async fn first_turn_allows_cold_intro() {
    let cold = cantonese_cold_intro();
    let history = vec![system("you are fox"), user("hi")];
    let (outcome, seen, _) = run_loop(history, vec![assistant(&cold)], 3).await;
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, cold);
            assert!(is_capability_cold_intro(&text));
            assert_eq!(iterations, 1);
        }
        other => panic!("expected FinalResponse, got {other:?}"),
    }
    assert_eq!(seen.len(), 1, "no regenerate on first turn");
}

#[tokio::test]
async fn normal_short_reply_not_rejected() {
    let (outcome, seen, _) = run_loop(
        history_with_prior_and_current(),
        vec![assistant("Got it — I'll pick up the cron item.")],
        3,
    )
    .await;
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, "Got it — I'll pick up the cron item.");
            assert_eq!(iterations, 1);
        }
        other => panic!("expected FinalResponse, got {other:?}"),
    }
    assert_eq!(seen.len(), 1);
}
