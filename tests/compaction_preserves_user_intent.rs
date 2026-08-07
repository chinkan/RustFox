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
        .get_or_create_conversation("telegram", "intent_u1")
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
