//! Regression test: compaction must never lose the user's request, even when
//! the summarizer fails (ADR 0003 Q7). Uses an in-memory store and an LLM
//! client whose provider always fails (empty base_url → relative URL → no
//! network traffic).

use std::collections::HashMap;
use std::sync::Arc;

use rustfox::config::ProviderType;
use rustfox::conversation::{CompactionContext, ConversationManager};
use rustfox::llm::{ChatMessage, LlmClient, MessageContent};
use rustfox::memory::MemoryStore;
use rustfox::provider::{OpenRouterProvider, ProviderConfig, ProviderRegistry};

fn failing_llm() -> LlmClient {
    let config = ProviderConfig {
        name: "test".to_string(),
        provider_type: ProviderType::OpenRouter,
        base_url: String::new(),
        api_key: None,
        default_model: "test-model".to_string(),
        supports_vision: false,
        max_tokens: 100,
        discover_models: false,
        context_window: 4096,
        context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
        parse_retry_limit: 0,
        rate_limit_retry_limit: 0,
    };
    let provider: Arc<dyn rustfox::provider::Provider> = Arc::new(OpenRouterProvider::new(config));
    let mut providers = HashMap::new();
    providers.insert("test".to_string(), provider);
    LlmClient::new(Arc::new(ProviderRegistry::new(
        providers,
        "test".to_string(),
    )))
}

fn msg(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(MessageContent::from_text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

#[tokio::test]
async fn compaction_never_loses_user_request() {
    let store = MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("telegram", rustfox::platform::DEFAULT_BOT_ID, "intent_u1")
        .await
        .unwrap();

    // Seed history: long initial request A + 15 tool exchanges + follow-up B.
    let mut history: Vec<ChatMessage> = vec![msg(
        "user",
        &format!("UNIQUE_KEYWORD_A initial request {}", "x".repeat(900)),
    )];
    for i in 0..15 {
        history.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![rustfox::llm::ToolCall {
                id: format!("call_{i}"),
                call_type: "function".to_string(),
                function: rustfox::llm::FunctionCall {
                    name: "search".to_string(),
                    arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                },
            }]),
            tool_call_id: None,
        });
        history.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::from_text(format!(
                "tool result {}",
                "z".repeat(200)
            ))),
            tool_calls: None,
            tool_call_id: Some(format!("call_{i}")),
        });
    }
    for m in &history {
        store.save_message(&conv, m).await.unwrap();
    }

    // Load via the real conversation path, then add the live follow-up.
    let mut cmgr = ConversationManager::new(
        &store,
        "telegram",
        rustfox::platform::DEFAULT_BOT_ID,
        "intent_u1",
        "system prompt".to_string(),
        &rustfox::skills::SkillRegistry::new(),
        &minimal_config(),
    )
    .await
    .unwrap();
    cmgr.add_user_turn(msg("user", "UNIQUE_KEYWORD_B follow-up request"));
    let original_len = cmgr.messages().len();

    let llm = failing_llm();
    let window = rustfox::agent_prompt::estimate_tokens(cmgr.messages());
    let ctx = CompactionContext {
        llm: &llm,
        context_window: window,
        compaction_model: None,
        user_model_path: None,
    };

    // Two passes: both must defer (LLM failure), never truncate.
    for pass in 0..2 {
        let compacted = cmgr.compact_messages(&ctx).await.unwrap();
        assert!(!compacted, "pass {pass}: must defer on summarizer failure");
        assert_eq!(
            cmgr.messages().len(),
            original_len,
            "pass {pass}: messages unchanged"
        );
    }

    let texts: Vec<String> = cmgr
        .messages()
        .iter()
        .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("UNIQUE_KEYWORD_A")),
        "initial request preserved verbatim"
    );
    assert!(
        texts.last().unwrap().contains("UNIQUE_KEYWORD_B"),
        "latest user intent preserved verbatim and last"
    );
}

fn minimal_config() -> rustfox::config::Config {
    // keep(): the temp dir must outlive the loaded config file.
    let dir = tempfile::tempdir().unwrap().keep();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        r#"
[telegram]
bot_token = "test"
allowed_user_ids = [1]

[openrouter]
api_key = "test"

[sandbox]
allowed_directory = "."
"#,
    )
    .unwrap();
    rustfox::config::Config::load(&path).unwrap()
}

/// Stub provider that returns a fixed summary (no network).
struct StubSummaryProvider {
    config: rustfox::provider::ProviderConfig,
    reply: String,
}

impl StubSummaryProvider {
    fn new(reply: impl Into<String>) -> Self {
        Self {
            config: rustfox::provider::ProviderConfig {
                name: "stub".to_string(),
                provider_type: ProviderType::OpenRouter,
                base_url: "http://stub.invalid/v1".to_string(),
                api_key: None,
                default_model: "stub-model".to_string(),
                supports_vision: false,
                max_tokens: 256,
                discover_models: false,
                context_window: 4096,
                context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
                parse_retry_limit: 0,
                rate_limit_retry_limit: 0,
            },
            reply: reply.into(),
        }
    }

    fn into_llm(self) -> LlmClient {
        let mut providers = HashMap::new();
        providers.insert(
            "stub".to_string(),
            Arc::new(self) as Arc<dyn rustfox::provider::Provider>,
        );
        LlmClient::new(Arc::new(ProviderRegistry::new(
            providers,
            "stub".to_string(),
        )))
    }
}

#[async_trait::async_trait]
impl rustfox::provider::Provider for StubSummaryProvider {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &rustfox::provider::ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        _client: &reqwest::Client,
        _messages: &[ChatMessage],
        _tools: &[rustfox::llm::ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<rustfox::llm::ChatCompletion> {
        Ok(rustfox::llm::ChatCompletion {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::from_text(self.reply.clone())),
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: Some("stop".to_string()),
            model: model.to_string(),
        })
    }

    async fn list_models(&self, _client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        Ok(vec![self.config.default_model.clone()])
    }
}

#[tokio::test]
async fn over_watermark_compacts_and_keeps_skills_soul_system() {
    let store = MemoryStore::open_in_memory().unwrap();
    let system = concat!(
        "SYSTEM_PROMPT_MARKER
",
        "SOUL_BODY_MARKER full soul content never trim
",
        "SKILL_BODY_MARKER full skill bodies never trim
",
    );
    let mut cmgr = ConversationManager::new(
        &store,
        "telegram",
        rustfox::platform::DEFAULT_BOT_ID,
        "compact_ok_u1",
        system.to_string(),
        &rustfox::skills::SkillRegistry::new(),
        &minimal_config(),
    )
    .await
    .unwrap();

    // Seed oversized history after the system message ConversationManager built.
    cmgr.add_user_turn(msg(
        "user",
        &format!("UNIQUE_KEYWORD_A initial {}", "x".repeat(900)),
    ));
    for i in 0..15 {
        cmgr.add_assistant_turn(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![rustfox::llm::ToolCall {
                id: format!("call_{i}"),
                call_type: "function".to_string(),
                function: rustfox::llm::FunctionCall {
                    name: "search".to_string(),
                    arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                },
            }]),
            tool_call_id: None,
        });
        cmgr.add_tool_result(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::from_text(format!(
                "tool result {}",
                "z".repeat(200)
            ))),
            tool_calls: None,
            tool_call_id: Some(format!("call_{i}")),
        });
    }
    cmgr.add_user_turn(msg("user", "UNIQUE_KEYWORD_B follow-up"));

    let tokens_before = rustfox::agent_prompt::estimate_tokens(cmgr.messages());
    let llm = StubSummaryProvider::new("INTEGRATION_LAYER summary of older turns").into_llm();
    let ctx = CompactionContext {
        llm: &llm,
        context_window: tokens_before,
        compaction_model: None,
        user_model_path: None,
    };

    assert!(
        cmgr.compact_messages(&ctx).await.unwrap(),
        "over watermark must compact"
    );

    let sys = cmgr.messages()[0].content.as_ref().unwrap().as_text();
    assert!(sys.contains("SYSTEM_PROMPT_MARKER"), "system preserved");
    assert!(
        sys.contains("SOUL_BODY_MARKER full soul content never trim"),
        "soul preserved"
    );
    assert!(
        sys.contains("SKILL_BODY_MARKER full skill bodies never trim"),
        "skills preserved"
    );
    assert_eq!(cmgr.messages()[1].role, "system");
    assert!(
        cmgr.messages()[1]
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("INTEGRATION_LAYER"),
        "summary as system message"
    );
    assert!(
        cmgr.messages()
            .last()
            .unwrap()
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("UNIQUE_KEYWORD_B"),
        "latest intent verbatim"
    );
    assert!(
        rustfox::agent_prompt::estimate_tokens(cmgr.messages()) < tokens_before,
        "token estimate must drop"
    );
}

#[tokio::test]
async fn below_watermark_skips_compaction() {
    let store = MemoryStore::open_in_memory().unwrap();
    let mut cmgr = ConversationManager::new(
        &store,
        "telegram",
        rustfox::platform::DEFAULT_BOT_ID,
        "compact_below_u1",
        "tiny system".to_string(),
        &rustfox::skills::SkillRegistry::new(),
        &minimal_config(),
    )
    .await
    .unwrap();
    cmgr.add_user_turn(msg("user", "hi"));
    cmgr.add_user_turn(msg("user", "hello again"));
    let before_len = cmgr.messages().len();

    let llm = StubSummaryProvider::new("should not be called").into_llm();
    let ctx = CompactionContext {
        llm: &llm,
        context_window: 100_000,
        compaction_model: None,
        user_model_path: None,
    };
    assert!(!cmgr.compact_messages(&ctx).await.unwrap());
    assert_eq!(cmgr.messages().len(), before_len);
}
