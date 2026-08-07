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
        user_id: &str,
        system_prompt: String,
        _skills: &SkillRegistry,
        _config: &Config,
    ) -> Result<Self> {
        let conversation_id = memory.get_or_create_conversation(platform, user_id).await?;
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

    pub async fn add_incoming(
        &mut self,
        incoming: &IncomingMessage,
        config: &Config,
        supports_vision: bool,
    ) -> Result<Vec<ContentPart>> {
        let (augmented_text, image_parts) = crate::file_processor::process_attachments(
            &incoming.attachments,
            &incoming.text,
            config,
            &self.memory,
            supports_vision,
        )
        .await;

        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(augmented_text)),
            tool_calls: None,
            tool_call_id: None,
        };
        self.memory
            .save_message(&self.conversation_id, &user_msg)
            .await?;

        Ok(image_parts)
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

    /// Unified compaction pipeline (ADR 0003 Q1): compress the oldest
    /// messages once total estimated tokens cross 85% of the real provider
    /// window. The protected tail (last two user turns + active exchange,
    /// never mid-tool-pair) stays verbatim. Durable facts are flushed to
    /// USER.md before the running summary is extended. On summarizer
    /// failure the pass is DEFERRED — nothing is truncated (Q7).
    pub async fn compact_messages(&mut self, ctx: &CompactionContext<'_>) -> Result<bool> {
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
    use crate::provider::{OpenRouterProvider, ProviderConfig, ProviderRegistry};

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
            .get_or_create_conversation("test", "layer_u1")
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
            "tool pair must not be split at the boundary (start={start})"
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
            .get_or_create_conversation("test", "compact_u1")
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
}
