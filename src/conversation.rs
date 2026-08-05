use anyhow::Result;

use crate::agent_prompt::prepare_messages_for_llm;
use crate::config::Config;
use crate::llm::{ChatMessage, ContentPart, LlmClient, MessageContent};
use crate::memory::MemoryStore;
use crate::platform::IncomingMessage;
use crate::skills::SkillRegistry;

pub struct ConversationManager {
    messages: Vec<ChatMessage>,
    system_prompt: String,
    memory: MemoryStore,
    conversation_id: String,
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
        messages.extend(history);

        Ok(Self {
            messages,
            system_prompt,
            memory: memory.clone(),
            conversation_id,
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

    /// Unified compaction pipeline: compress the oldest messages once total
    /// utilization crosses `agent_prompt::COMPACT_TRIGGER_PCT` of the context
    /// window. The fraction of oldest messages summarized follows the graduated
    /// `agent_prompt::COMPACT_LADDER`; the system message (index 0) and the
    /// newest 8 messages always stay verbatim.
    ///
    /// Summarization is attempted via the LLM and rendered as structured marker
    /// lines; on LLM failure a sync structured extraction is used instead (no
    /// LLM call). Returns `Ok(true)` when compaction happened, `Ok(false)` when
    /// nothing was summarized.
    pub async fn compact_messages(
        &mut self,
        llm: &LlmClient,
        context_window: usize,
    ) -> Result<bool> {
        let total: usize = self
            .messages
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text().len()).unwrap_or(0))
            .sum();
        let utilization = total as f64 / context_window as f64;
        if utilization < crate::agent_prompt::COMPACT_TRIGGER_PCT {
            return Ok(false);
        }

        // Graduated fraction of the oldest (non-system) messages to compress,
        // clamped so at most len-9 messages are summarized (system + newest 8
        // stay verbatim).
        let fraction = crate::agent_prompt::compact_fraction(utilization);
        let max_summarize = self.messages.len().saturating_sub(9);
        let summarize_count = ((self.messages.len().saturating_sub(1)) as f64 * fraction) as usize;
        let summarize_count = summarize_count.min(max_summarize);
        if summarize_count == 0 {
            return Ok(false);
        }

        let to_summarize: Vec<&ChatMessage> =
            self.messages.iter().skip(1).take(summarize_count).collect();
        let preserved_tail_start = summarize_count + 1;

        let summary_text = match self.summarize_with_llm(llm, &to_summarize).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("LLM compaction failed ({e}); using sync structured summary");
                self.build_sync_summary(&to_summarize)
            }
        };

        let heading = format!(
            "★ COMPACTED CONTEXT — {} messages summarized ★\n",
            to_summarize.len()
        );
        let summary_entry = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!("{heading}{summary_text}"))),
            tool_calls: None,
            tool_call_id: Some("summary".to_string()),
        };

        let mut new_msgs =
            Vec::with_capacity(2 + self.messages.len().saturating_sub(preserved_tail_start));
        if let Some(system) = self.messages.first().cloned() {
            new_msgs.push(system);
        }
        new_msgs.push(summary_entry);
        new_msgs.extend(self.messages.iter().skip(preserved_tail_start).cloned());
        self.messages = new_msgs;
        Ok(true)
    }

    /// Ask the LLM to summarize `to_summarize` as structured marker lines.
    async fn summarize_with_llm(
        &self,
        llm: &LlmClient,
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

        let summary_prompt = format!(
            "Summarize the following conversation, preserving key decisions and facts.\n\
             Output the summary AS STRUCTURED MARKER LINES, one per message, in exactly these formats:\n\
             [Tool: NAME] description | result: SUMMARY | status: ok|error\n\
             [User] TOPIC: SUMMARY\n\
             [Assistant] ACTION: DECISION_SUMMARY\n\
             [System] EVENT: NOTABLE_INFO\n\n\
             Conversation:\n{summary_text}"
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

        let response = llm.chat(&summary_msg, &[]).await?;
        Ok(response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default())
    }

    /// Sync fallback: build structured marker lines from `to_summarize`
    /// without any LLM call.
    fn build_sync_summary(&self, to_summarize: &[&ChatMessage]) -> String {
        const MAX_CHARS: usize = 200;
        let mut lines: Vec<String> = Vec::new();
        for m in to_summarize {
            let text = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let truncated: String = text.chars().take(MAX_CHARS).collect();
            let marker = match m.role.as_str() {
                "user" => format!("[User] TOPIC: {truncated}"),
                "assistant" => format!("[Assistant] ACTION: {truncated}"),
                "tool" => {
                    let id = m.tool_call_id.as_deref().unwrap_or("unknown");
                    let status = if text.contains("Error") || text.contains("error") {
                        "error"
                    } else {
                        "ok"
                    };
                    format!("[Tool: {id}] result: {truncated} | status: {status}")
                }
                "system" => format!("[System] EVENT: {truncated}"),
                _ => format!("{}: {truncated}", m.role),
            };
            lines.push(marker);
        }
        lines.join("\n")
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
        }
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

        let result = cm.compact_messages(&llm, 100_000).await.unwrap();
        assert!(!result, "tiny conversation must not trigger compaction");
        assert_eq!(texts(&cm), before, "messages must be unchanged");
    }

    #[tokio::test]
    async fn compact_messages_sync_fallback_produces_markers() {
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("system prompt".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "user question {} {}",
                "x".repeat(90),
                0
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        for i in 0..10 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text(format!(
                    "assistant reply {} {}",
                    "x".repeat(80),
                    i
                ))),
                tool_calls: None,
                tool_call_id: None,
            });
            let result = if i == 3 {
                format!("Error: file not found {}", "y".repeat(70))
            } else {
                format!("tool result {} {}", "y".repeat(80), i)
            };
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(result)),
                tool_calls: None,
                tool_call_id: Some(format!("tool_{i}")),
            });
        }
        let last_content = messages.last().unwrap().content.clone();
        let mut cm = manager(messages);
        let llm = failing_llm();

        let result = cm.compact_messages(&llm, 1_000).await.unwrap();
        assert!(result, "LLM failure must still compact via sync fallback");

        // system + 1 summary entry + newest 8 preserved
        assert_eq!(cm.messages.len(), 10);
        assert_eq!(cm.messages[0].role, "system");
        assert_eq!(cm.messages[1].role, "user");
        assert_eq!(cm.messages[1].tool_call_id.as_deref(), Some("summary"));

        let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
        assert!(
            summary_text.contains("★ COMPACTED CONTEXT — 13 messages summarized ★"),
            "missing heading: {}",
            summary_text
        );
        assert!(summary_text.contains("[User] TOPIC:"), "{summary_text}");
        assert!(
            summary_text.contains("[Assistant] ACTION:"),
            "{summary_text}"
        );
        assert!(summary_text.contains("[Tool: tool_3]"), "{summary_text}");
        assert!(summary_text.contains("| status: error"), "{summary_text}");

        // Preserved tail: newest 8 messages verbatim, in order.
        assert_eq!(
            cm.messages
                .last()
                .unwrap()
                .content
                .as_ref()
                .map(|c| c.as_text()),
            last_content.as_ref().map(|c| c.as_text())
        );
        assert_eq!(cm.messages[2].role, "assistant");
        assert!(cm.messages[2].content.as_ref().unwrap().as_text().len() > 50);
    }
}
