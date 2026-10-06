use anyhow::Result;

use crate::agent_prompt::prepare_messages_for_llm;
use crate::config::Config;
use crate::llm::{ChatMessage, ContentPart, LlmClient, MessageContent};
use crate::memory::MemoryStore;
use crate::platform::IncomingMessage;
use crate::skills::SkillRegistry;

/// Inputs for one compaction pass (ADR 0003).
pub struct CompactionContext<'a> {
    pub llm: &'a LlmClient,
    /// Provider window in tokens (from `registry.effective_context_window`).
    pub context_window: usize,
    /// Optional cheaper model for summary + flush turns (Q9).
    pub compaction_model: Option<&'a str>,
    /// USER.md path for the durable-memory flush (Q5); `None` disables flush.
    pub user_model_path: Option<&'a std::path::Path>,
}

pub struct ConversationManager {
    messages: Vec<ChatMessage>,
    system_prompt: String,
    memory: MemoryStore,
    conversation_id: String,
    /// Running summary of compacted history (ADR 0003 Q2) — layered,
    /// persisted as `[SUMMARY]` rows (Q8), injected as a system message.
    summary: Option<String>,
    /// Highest message index whose user turn was already flushed to USER.md (Q6).
    last_flush_turn: Option<usize>,
}

impl ConversationManager {
    pub async fn new(
        memory: &MemoryStore,
        platform: &str,
        bot_id: &str,
        user_id: &str,
        system_prompt: String,
        _skills: &SkillRegistry,
        config: &Config,
    ) -> Result<Self> {
        let claim_legacy = Config::bot_claims_legacy_default(&config.bots, bot_id);
        let conversation_id = memory
            .get_or_create_conversation_with_claim(platform, bot_id, user_id, claim_legacy)
            .await?;
        let history = memory
            .load_messages(&conversation_id)
            .await
            .unwrap_or_default();

        let mut folded_summary: Vec<String> = Vec::new();
        let mut raw: Vec<ChatMessage> = Vec::new();
        for m in history {
            if m.role == "system" {
                if let Some(text) = m.content.as_ref().map(|c| c.as_text()) {
                    if let Some(rest) = text.strip_prefix("[SUMMARY]") {
                        folded_summary.push(rest.trim().to_string());
                        continue;
                    }
                }
            }
            if m.role == "user" && m.tool_call_id.as_deref() == Some("summary") {
                continue; // legacy marker-style summary entries are superseded
            }
            raw.push(m);
        }
        let summary = (!folded_summary.is_empty()).then(|| folded_summary.join("\n\n"));

        let now = chrono::Local::now();
        let context_prompt = format!(
            "\n\nCurrent date and time: {} ({})",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.format("%A")
        );

        let system_msg = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!(
                "{system_prompt}{context_prompt}"
            ))),
            tool_calls: None,
            tool_call_id: None,
        };

        let mut messages = vec![system_msg];
        if let Some(s) = &summary {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(format!(
                    "Previously compacted context:\n{s}"
                ))),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        messages.extend(raw);

        Ok(Self {
            messages,
            system_prompt,
            memory: memory.clone(),
            conversation_id,
            summary,
            last_flush_turn: None,
        })
    }

    /// Text content the model sees for a user turn (and that we persist).
    /// Typed user text plus attachment/OCR extraction when present, joined by a blank line.
    pub(crate) fn combine_user_and_attachment_text(
        user_text: &str,
        attachment_text: &str,
    ) -> String {
        if attachment_text.is_empty() {
            user_text.to_string()
        } else if user_text.is_empty() {
            attachment_text.to_string()
        } else {
            format!("{user_text}\n\n{attachment_text}")
        }
    }

    pub async fn add_incoming(
        &mut self,
        incoming: &IncomingMessage,
        config: &Config,
        supports_vision: bool,
    ) -> Result<(String, Vec<ContentPart>)> {
        let (attachment_text, image_parts) = crate::file_processor::process_attachments(
            &incoming.attachments,
            &incoming.text,
            config,
            &self.memory,
            supports_vision,
        )
        .await;

        // Persist the same text the model will see (user text + attachment/OCR),
        // not the bare process_attachments return (empty when there is no attachment).
        let combined = Self::combine_user_and_attachment_text(&incoming.text, &attachment_text);

        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(combined.clone())),
            tool_calls: None,
            tool_call_id: None,
        };
        // Scheduled runs persist the prompt and the result together, once,
        // from the job runner (failure / cancel / max-iterations included).
        if incoming.schedule_id.is_none() {
            self.memory
                .save_message(&self.conversation_id, &user_msg)
                .await?;
        }

        Ok((combined, image_parts))
    }

    pub fn add_user_turn(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    /// ADR 0003 Q6: flush only when the range contains a user-authored
    /// message newer than the last flushed one.
    pub(crate) fn should_flush(
        range_user_max: Option<usize>,
        last_flush_turn: Option<usize>,
    ) -> bool {
        match (range_user_max, last_flush_turn) {
            (Some(max), Some(last)) => max > last,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Apply a new summary layer (ADR 0003 Q2/Q8): fold into the running
    /// summary, rebuild the message list as [system, summary block,
    /// protected tail], and persist the layer as a `[SUMMARY]` system
    /// message. Persistence failures are logged and ignored — the in-memory
    /// state wins.
    pub(crate) async fn apply_summary_layer(
        &mut self,
        layer: &str,
        tail_start: usize,
    ) -> Result<()> {
        let layer = layer.trim();
        if layer.is_empty() {
            anyhow::bail!("empty summary layer");
        }
        self.summary = Some(match self.summary.take() {
            Some(prev) => format!("{prev}\n\n{layer}"),
            None => layer.to_string(),
        });

        let mut new_msgs = Vec::with_capacity(2 + self.messages.len().saturating_sub(tail_start));
        if let Some(system) = self.messages.first().cloned() {
            new_msgs.push(system);
        }
        new_msgs.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!(
                "Previously compacted context:\n{}",
                self.summary.as_deref().unwrap_or_default()
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        new_msgs.extend(self.messages.iter().skip(tail_start).cloned());
        self.messages = new_msgs;

        let persisted = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!("[SUMMARY]\n{layer}"))),
            tool_calls: None,
            tool_call_id: None,
        };
        if let Err(e) = self
            .memory
            .save_message(&self.conversation_id, &persisted)
            .await
        {
            tracing::warn!(error = %format!("{e:#}"), "Failed to persist summary layer");
        }
        Ok(())
    }

    pub fn add_assistant_turn(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn add_tool_result(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn inject_rag_context(&mut self, rag_block: &str) {
        if !rag_block.is_empty() {
            self.system_prompt
                .push_str(&format!("\n\n# Retrieved Context\n{rag_block}"));
        }
    }

    pub fn apply_steer(&mut self, text: &str) {
        let steer_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        };
        self.messages.push(steer_msg);
    }

    /// Unified compaction pipeline (ADR 0003 Q1 / ADR 0019 ③): compress the
    /// oldest messages when estimated tokens of the full assembled request
    /// (system + skills/soul already in the system message + history) cross
    /// 85% of the real provider window. The protected tail (last two user
    /// turns + active exchange, never mid-tool-pair) stays verbatim; system /
    /// soul / skill bodies are never trimmed. Durable facts are flushed to
    /// USER.md before the running summary is extended. On summarizer failure
    /// the pass is DEFERRED — nothing is truncated (Q7). If still over the
    /// watermark after a successful pass, runs one more in-turn compact
    /// through the same pipeline (bounded at two passes).
    pub async fn compact_messages(&mut self, ctx: &CompactionContext<'_>) -> Result<bool> {
        const MAX_IN_TURN_PASSES: usize = 2;
        let mut compacted_any = false;
        for pass in 0..MAX_IN_TURN_PASSES {
            match self.compact_messages_once(ctx).await? {
                true => {
                    compacted_any = true;
                    let trigger_tokens = (ctx.context_window as f64
                        * crate::agent_prompt::COMPACT_TRIGGER_PCT)
                        as usize;
                    if crate::agent_prompt::estimate_tokens(&self.messages) <= trigger_tokens {
                        break;
                    }
                    if pass + 1 < MAX_IN_TURN_PASSES {
                        tracing::info!(
                            pass = pass + 1,
                            "Still over compaction watermark; running second in-turn compact"
                        );
                    }
                }
                false => break,
            }
        }
        Ok(compacted_any)
    }

    /// Single compaction pass. Returns `true` when a summary layer was applied.
    async fn compact_messages_once(&mut self, ctx: &CompactionContext<'_>) -> Result<bool> {
        if ctx.context_window == 0 {
            return Ok(false);
        }
        let trigger_tokens =
            (ctx.context_window as f64 * crate::agent_prompt::COMPACT_TRIGGER_PCT) as usize;
        if crate::agent_prompt::estimate_tokens(&self.messages) <= trigger_tokens {
            return Ok(false);
        }

        let tail_start =
            crate::agent_prompt::protected_tail_start(&self.messages, ctx.context_window);
        if tail_start == 0 || tail_start >= self.messages.len() {
            return Ok(false);
        }
        // Skip index 0 (system / skills / soul) — never summarize or trim it.
        let range: Vec<&ChatMessage> = self.messages.iter().skip(1).take(tail_start - 1).collect();
        if range.is_empty() {
            return Ok(false);
        }

        // Q5/Q6: durable-memory flush before the summary is written.
        let range_user_max = range
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i + 1) // range index 0 == message index 1
            .max();
        if Self::should_flush(range_user_max, self.last_flush_turn) {
            if let Some(path) = ctx.user_model_path {
                match crate::learning::flush_user_model(ctx.llm, path, &range, ctx.compaction_model)
                    .await
                {
                    Ok(true) => {
                        self.last_flush_turn = range_user_max;
                    }
                    Ok(false) => tracing::info!("User-model flush skipped: no durable facts"),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "User-model flush failed");
                    }
                }
            }
        }

        // Q2/Q7: extend the running summary; defer on failure.
        let layer = match self.summarize_with_llm(ctx, &range).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    range = range.len(),
                    "Compaction summary failed; deferring (no truncation)"
                );
                return Ok(false);
            }
        };

        self.apply_summary_layer(&layer, tail_start).await?;
        Ok(true)
    }

    /// Ask the summarizer (Q9 model override, else current model) to EXTEND
    /// the running summary with the new portion of the conversation.
    async fn summarize_with_llm(
        &self,
        ctx: &CompactionContext<'_>,
        to_summarize: &[&ChatMessage],
    ) -> Result<String> {
        let summary_text: String = to_summarize
            .iter()
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.as_ref().map(|c| c.as_text()).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let previous = self.summary.as_deref().unwrap_or("");
        let summary_prompt = format!(
            "You are maintaining a running summary of a long conversation.\n\
             {prev_block}\
             Below is the new portion of the conversation. EXTEND the previous summary with it:\n\
             - Preserve key facts, decisions, preferences, and open questions\n\
             - Merge new information; never contradict or repeat the previous summary\n\
             - Be concise — at most 300 words\n\
             - Output ONLY the new summary text (no preamble, no markers)\n\n\
             New conversation:\n{summary_text}",
            prev_block = if previous.is_empty() {
                String::new()
            } else {
                format!("Previous summary:\n{previous}\n\n")
            },
        );

        let summary_msg = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(
                    "You are a conversation summarizer.".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(summary_prompt)),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let response = match ctx.compaction_model {
            Some(model) => {
                ctx.llm
                    .chat_completion_with_model(&summary_msg, &[], model)
                    .await?
                    .message
            }
            None => ctx.llm.chat(&summary_msg, &[]).await?,
        };
        Ok(response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default())
    }

    pub fn prepare(&self, context_window: usize) -> crate::agent_prompt::PreparedPrompt {
        prepare_messages_for_llm(&self.messages, context_window)
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn into_messages(self) -> Vec<ChatMessage> {
        self.messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::config::ProviderType;
    use crate::llm::{ChatCompletion, ToolDefinition};
    use crate::provider::{OpenRouterProvider, Provider, ProviderConfig, ProviderRegistry};

    /// A provider that always fails: empty base_url makes the request a
    /// relative URL, so reqwest errors out without touching the network.
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
        let provider: Arc<dyn crate::provider::Provider> =
            Arc::new(OpenRouterProvider::new(config));
        let mut providers = HashMap::new();
        providers.insert("test".to_string(), provider);
        LlmClient::new(Arc::new(ProviderRegistry::new(
            providers,
            "test".to_string(),
        )))
    }

    /// Stub provider that returns a fixed summary text (no network).
    struct StubSummaryProvider {
        config: ProviderConfig,
        reply: String,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl StubSummaryProvider {
        fn new(reply: impl Into<String>) -> Self {
            Self {
                config: ProviderConfig {
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
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn into_llm(self) -> LlmClient {
            let mut providers = HashMap::new();
            providers.insert("stub".to_string(), Arc::new(self) as Arc<dyn Provider>);
            LlmClient::new(Arc::new(ProviderRegistry::new(
                providers,
                "stub".to_string(),
            )))
        }
    }

    #[async_trait::async_trait]
    impl Provider for StubSummaryProvider {
        fn name(&self) -> &str {
            &self.config.name
        }
        fn default_model(&self) -> &str {
            &self.config.default_model
        }
        fn supports_vision(&self) -> bool {
            self.config.supports_vision
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
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ChatCompletion {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(self.reply.clone())),
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

    fn oversized_history(system: &str) -> Vec<ChatMessage> {
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(system.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "UNIQUE_KEYWORD_A long initial request {}",
                "x".repeat(900)
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        for i in 0..15 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![crate::llm::ToolCall {
                    id: format!("call_{i}"),
                    call_type: "function".to_string(),
                    function: crate::llm::FunctionCall {
                        name: "search".to_string(),
                        arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                    },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(format!(
                    "tool result {}",
                    "z".repeat(200)
                ))),
                tool_calls: None,
                tool_call_id: Some(format!("call_{i}")),
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "UNIQUE_KEYWORD_B follow-up request".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
        });
        messages
    }

    fn manager(messages: Vec<ChatMessage>) -> ConversationManager {
        ConversationManager {
            messages,
            system_prompt: String::new(),
            memory: crate::memory::MemoryStore::open_in_memory().unwrap(),
            conversation_id: String::new(),
            summary: None,
            last_flush_turn: None,
        }
    }

    #[tokio::test]
    async fn should_flush_gate() {
        // no user message in range → never flush
        assert!(!ConversationManager::should_flush(None, None));
        // first flush with a user message → yes
        assert!(ConversationManager::should_flush(Some(3), None));
        // same range as last flush → no
        assert!(!ConversationManager::should_flush(Some(3), Some(3)));
        // newer user message than last flush → yes
        assert!(ConversationManager::should_flush(Some(7), Some(3)));
    }

    #[tokio::test]
    async fn apply_summary_layer_rebuilds_messages_and_persists() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", crate::platform::DEFAULT_BOT_ID, "layer_u1")
            .await
            .unwrap();
        let mut cm = manager(vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("sys".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("old request".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("old reply".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("latest request".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
        ]);
        cm.memory = store.clone();
        cm.conversation_id = conv.clone();

        cm.apply_summary_layer("layer one content", 3)
            .await
            .unwrap();

        // Rebuilt: system + summary block + tail from index 3.
        assert_eq!(cm.messages.len(), 3);
        assert_eq!(cm.messages[0].role, "system");
        assert_eq!(cm.messages[1].role, "system");
        assert!(
            cm.messages[1]
                .content
                .as_ref()
                .unwrap()
                .as_text()
                .contains("Previously compacted context:\nlayer one content"),
            "summary injected as system message: {}",
            cm.messages[1].content.as_ref().unwrap().as_text()
        );
        assert_eq!(
            cm.messages[2].content.as_ref().unwrap().as_text(),
            "latest request"
        );

        // Second layer extends, not replaces.
        cm.apply_summary_layer("layer two content", 2)
            .await
            .unwrap();
        let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
        assert!(
            summary_text.contains("layer one content")
                && summary_text.contains("layer two content"),
            "layered extension: {summary_text}"
        );
        assert_eq!(
            cm.summary.as_deref().unwrap(),
            "layer one content\n\nlayer two content"
        );

        // Persisted: [SUMMARY] rows reload.
        let reloaded = store.load_messages(&conv).await.unwrap();
        let summary_rows: Vec<String> = reloaded
            .iter()
            .filter_map(|m| {
                m.content
                    .as_ref()
                    .map(|c| c.as_text())
                    .filter(|t| t.starts_with("[SUMMARY]"))
            })
            .collect();
        assert_eq!(summary_rows.len(), 2, "one [SUMMARY] row per layer");
        assert!(summary_rows[0].contains("layer one content"));
        assert!(summary_rows[1].contains("layer two content"));
    }

    #[tokio::test]
    async fn apply_summary_layer_rejects_empty() {
        let mut cm = manager(vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }]);
        assert!(cm.apply_summary_layer("   ", 1).await.is_err());
    }

    #[tokio::test]
    async fn compact_messages_noop_below_threshold() {
        let mut cm = manager(vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("sys".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hi".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("how are you".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
        ]);
        let texts = |cm: &ConversationManager| -> Vec<(String, String)> {
            cm.messages
                .iter()
                .map(|m| {
                    (
                        m.role.clone(),
                        m.content.as_ref().map(|c| c.as_text()).unwrap_or_default(),
                    )
                })
                .collect()
        };
        let before = texts(&cm);
        let llm = failing_llm();
        let ctx = CompactionContext {
            llm: &llm,
            context_window: 100_000,
            compaction_model: None,
            user_model_path: None,
        };

        let result = cm.compact_messages(&ctx).await.unwrap();
        assert!(!result, "tiny conversation must not trigger compaction");
        assert_eq!(texts(&cm), before, "messages must be unchanged");
    }

    #[test]
    fn compact_range_boundary_lands_after_tool_pair() {
        use crate::agent_prompt::protected_tail_start;
        use crate::llm::{FunctionCall, ToolCall};

        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("system prompt".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "first request {}",
                "x".repeat(100)
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_split".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "lookup_thing".to_string(),
                    arguments: r#"{"query":"x"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("lookup result payload".to_string())),
            tool_calls: None,
            tool_call_id: Some("call_split".to_string()),
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "second request {}",
                "x".repeat(100)
            ))),
            tool_calls: None,
            tool_call_id: None,
        });

        let start = protected_tail_start(&messages, 1_000_000);
        // Boundary must not orphan the pair: both call and result are either
        // both in the tail or both summarized.
        let call_in_tail = messages[start..].iter().any(|m| {
            m.has_tool_calls()
                && m.tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|c| c.id == "call_split"))
        });
        let result_in_tail = messages[start..]
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("call_split"));
        assert_eq!(
            call_in_tail, result_in_tail,
            "tool pair must not be split at the boundary (start index in message vec)"
        );
    }

    #[tokio::test]
    async fn compact_messages_defers_on_llm_failure_never_truncates() {
        use crate::agent_prompt::{estimate_tokens, protected_tail_start};

        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("system prompt".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "UNIQUE_KEYWORD_A long initial request {}",
                "x".repeat(900)
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        for i in 0..15 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![crate::llm::ToolCall {
                    id: format!("call_{i}"),
                    call_type: "function".to_string(),
                    function: crate::llm::FunctionCall {
                        name: "search".to_string(),
                        arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                    },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(format!(
                    "tool result {}",
                    "z".repeat(200)
                ))),
                tool_calls: None,
                tool_call_id: Some(format!("call_{i}")),
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "UNIQUE_KEYWORD_B follow-up request".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
        });

        let mut cm = manager(messages);
        let llm = failing_llm();
        let original_len = cm.messages.len();
        let window = estimate_tokens(&cm.messages);
        assert!(window > 0);

        let ctx = CompactionContext {
            llm: &llm,
            context_window: window,
            compaction_model: None,
            user_model_path: None,
        };
        let result = cm.compact_messages(&ctx).await.unwrap();

        // LLM failure → defer: no compaction, no truncation, nothing lost.
        assert!(!result, "must defer when summarization fails");
        assert_eq!(cm.messages.len(), original_len, "messages unchanged");

        let texts: Vec<String> = cm
            .messages
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("UNIQUE_KEYWORD_A")),
            "initial request preserved verbatim"
        );
        assert!(
            texts.last().unwrap().contains("UNIQUE_KEYWORD_B"),
            "latest user intent preserved verbatim"
        );
        assert!(
            texts
                .iter()
                .all(|t| t.len() >= 200 || !t.contains("UNIQUE_KEYWORD_A")),
            "no 200-char truncation anywhere"
        );

        // Second attempt: protected tail must include both user turns.
        let tail = protected_tail_start(&cm.messages, window);
        assert!(
            cm.messages[tail..].iter().any(|m| m
                .content
                .as_ref()
                .map(|c| c.as_text())
                .is_some_and(|t| t.contains("UNIQUE_KEYWORD_B"))),
            "protected tail contains the latest user turn"
        );
    }

    #[tokio::test]
    async fn compact_success_path_preserves_user_intent() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", crate::platform::DEFAULT_BOT_ID, "compact_u1")
            .await
            .unwrap();
        let mut cm = manager(vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("sys".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(format!(
                    "UNIQUE_KEYWORD_A old request {}",
                    "x".repeat(800)
                ))),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("old reply".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("middle message".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(
                    "UNIQUE_KEYWORD_B follow-up".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
            },
        ]);
        cm.conversation_id = conv.clone();

        let tail = crate::agent_prompt::protected_tail_start(&cm.messages, 1_000_000);
        assert_eq!(
            tail, 3,
            "old request + reply summarized, follow-up protected"
        );
        cm.apply_summary_layer("user asked about UNIQUE_KEYWORD_A topic", tail)
            .await
            .unwrap();

        // System message at index 1 carries the summary; the latest intent is verbatim.
        assert_eq!(cm.messages[1].role, "system");
        let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
        assert!(
            summary_text.contains("UNIQUE_KEYWORD_A"),
            "summary preserves the old intent: {summary_text}"
        );
        assert_eq!(
            cm.messages
                .last()
                .unwrap()
                .content
                .as_ref()
                .unwrap()
                .as_text(),
            "UNIQUE_KEYWORD_B follow-up"
        );
    }

    #[tokio::test]
    async fn compact_messages_over_watermark_applies_summary_layer() {
        use crate::agent_prompt::estimate_tokens;

        let mut cm = manager(oversized_history("system prompt"));
        let tokens_before = estimate_tokens(&cm.messages);
        assert!(tokens_before > 0);

        let llm = StubSummaryProvider::new("LAYER_SUMMARY_OK old search turns").into_llm();
        // Window == current estimate → already at 100% > 85% trigger.
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens_before,
            compaction_model: None,
            user_model_path: None,
        };

        let compacted = cm.compact_messages(&ctx).await.unwrap();
        assert!(compacted, "over watermark must trigger compaction");
        assert!(
            estimate_tokens(&cm.messages) < tokens_before,
            "compaction must shrink estimated tokens"
        );
        assert_eq!(cm.messages[1].role, "system");
        let summary = cm.messages[1].content.as_ref().unwrap().as_text();
        assert!(
            summary.contains("Previously compacted context"),
            "summary injected as system message: {summary}"
        );
        assert!(
            summary.contains("LAYER_SUMMARY_OK"),
            "stub layer present: {summary}"
        );
        assert!(
            cm.messages
                .last()
                .unwrap()
                .content
                .as_ref()
                .unwrap()
                .as_text()
                .contains("UNIQUE_KEYWORD_B"),
            "latest user intent stays verbatim"
        );
    }

    #[tokio::test]
    async fn compact_preserves_system_skills_and_soul_bodies() {
        use crate::agent_prompt::estimate_tokens;

        // Markers stand in for skill catalog + soul file bodies embedded in
        // the assembled system prompt (ADR 0019 ③: never trim these).
        let system = concat!(
            "SYSTEM_PROMPT_MARKER full instructions
",
            "SOUL_BODY_MARKER identity and preferences verbatim forever
",
            "SKILL_BODY_MARKER complete skill documentation do not cut
",
            "more system padding ",
        );
        let system = format!("{system}{}", "S".repeat(200));
        let mut cm = manager(oversized_history(&system));
        let tokens_before = estimate_tokens(&cm.messages);
        let llm = StubSummaryProvider::new("compacted older turns").into_llm();
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens_before,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());

        let sys_text = cm.messages[0].content.as_ref().unwrap().as_text();
        assert_eq!(cm.messages[0].role, "system");
        assert!(
            sys_text.contains("SYSTEM_PROMPT_MARKER"),
            "system prompt intact"
        );
        assert!(
            sys_text.contains("SOUL_BODY_MARKER identity and preferences verbatim forever"),
            "soul body fully preserved: {sys_text}"
        );
        assert!(
            sys_text.contains("SKILL_BODY_MARKER complete skill documentation do not cut"),
            "skill body fully preserved: {sys_text}"
        );
        assert!(
            sys_text.contains(&"S".repeat(200)),
            "system padding not truncated"
        );
    }

    fn persist_test_config() -> Config {
        // keep(): temp dir must outlive the loaded config.
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
        Config::load(&path).unwrap()
    }

    /// Short native PDF fixture (same shape as file_processor tests).
    fn short_pdf_bytes(page_text: &str) -> Vec<u8> {
        use pdf_extract::{Dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        let font_id = doc.add_object(Object::Dictionary(font));

        let escaped = page_text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let content = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET");
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let mut resources = Dictionary::new();
        let mut fonts = Dictionary::new();
        fonts.set("F1", Object::Reference(font_id));
        resources.set("Font", Object::Dictionary(fonts));
        let mut page = Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("Parent", Object::Reference(pages_id));
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page.set("Contents", Object::Reference(content_id));
        page.set("Resources", Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        let mut pages = Dictionary::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        pages.set("Count", Object::Integer(1));
        doc.set_object(pages_id, Object::Dictionary(pages));

        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = doc.add_object(Object::Dictionary(catalog));
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut out = Vec::new();
        doc.save_to(&mut out).expect("write pdf");
        out
    }

    #[tokio::test]
    async fn add_incoming_persists_plain_user_text_not_empty() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let mut cm = ConversationManager::new(
            &store,
            "telegram",
            crate::platform::DEFAULT_BOT_ID,
            "persist_plain",
            "sys".to_string(),
            &skills,
            &config,
        )
        .await
        .unwrap();

        let msg = IncomingMessage {
            platform: "telegram".to_string(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            user_id: "persist_plain".to_string(),
            chat_id: "1".to_string(),
            user_name: "tester".to_string(),
            text: "Hi".to_string(),
            attachments: vec![],
            schedule_id: None,
        };

        let (combined, image_parts) = cm.add_incoming(&msg, &config, false).await.unwrap();
        assert!(image_parts.is_empty());
        // Same text the model is given for a plain turn.
        assert_eq!(combined, "Hi");

        let conv = store
            .get_or_create_conversation(
                "telegram",
                crate::platform::DEFAULT_BOT_ID,
                "persist_plain",
            )
            .await
            .unwrap();
        let rows = store.load_messages(&conv).await.unwrap();
        let user_rows: Vec<_> = rows.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_rows.len(), 1, "one persisted user row");
        assert_eq!(
            user_rows[0].content.as_ref().unwrap().as_text(),
            "Hi",
            "DB row must equal the text the model was given, not empty"
        );
    }

    #[tokio::test]
    async fn add_incoming_persists_user_text_plus_attachment_text() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let mut cm = ConversationManager::new(
            &store,
            "telegram",
            crate::platform::DEFAULT_BOT_ID,
            "persist_attach",
            "sys".to_string(),
            &skills,
            &config,
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let pdf_path = dir.path().join("note.pdf");
        std::fs::write(&pdf_path, short_pdf_bytes("attachment ocr body")).unwrap();

        let msg = IncomingMessage {
            platform: "telegram".to_string(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            user_id: "persist_attach".to_string(),
            chat_id: "1".to_string(),
            user_name: "tester".to_string(),
            text: "Please read this".to_string(),
            attachments: vec![crate::platform::Attachment {
                kind: crate::platform::AttachmentKind::Pdf,
                path: pdf_path,
                mime_type: "application/pdf".to_string(),
                file_name: Some("note.pdf".to_string()),
            }],
            schedule_id: None,
        };

        let (combined, image_parts) = cm.add_incoming(&msg, &config, false).await.unwrap();
        assert!(
            image_parts.is_empty(),
            "short PDF is text-only, no vision parts"
        );

        assert!(
            combined.starts_with("Please read this\n\n[File: note.pdf]\n"),
            "combined must include user text and attachment text, got: {combined:?}"
        );
        assert!(
            combined.contains("attachment ocr body"),
            "combined must include extracted attachment body: {combined:?}"
        );
        // Not only the attachment return (which would omit the user text).
        assert!(
            combined.contains("Please read this"),
            "must include typed user text, not only attachment return"
        );
        assert_ne!(combined, "", "must not be empty");

        let conv = store
            .get_or_create_conversation(
                "telegram",
                crate::platform::DEFAULT_BOT_ID,
                "persist_attach",
            )
            .await
            .unwrap();
        let rows = store.load_messages(&conv).await.unwrap();
        let user_rows: Vec<_> = rows.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_rows.len(), 1);
        assert_eq!(
            user_rows[0].content.as_ref().unwrap().as_text(),
            combined,
            "DB row must match the combined content the model sees"
        );
    }
}
