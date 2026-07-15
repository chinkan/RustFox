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
        let history = memory.load_messages(&conversation_id).await.unwrap_or_default();

        let now = chrono::Local::now();
        let context_prompt = format!(
            "\n\nCurrent date and time: {} ({})",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.format("%A")
        );

        let system_msg = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!("{system_prompt}{context_prompt}"))),
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
        ).await;

        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(augmented_text)),
            tool_calls: None,
            tool_call_id: None,
        };
        self.memory.save_message(&self.conversation_id, &user_msg).await?;

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
            self.system_prompt.push_str(&format!("\n\n# Retrieved Context\n{rag_block}"));
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

    pub async fn compact_tier3(&mut self, context_window: usize) {
        let total: usize = self.messages.iter().map(|m| m.content.as_ref().map(|c| c.as_text().len()).unwrap_or(0)).sum();
        if total > context_window / 2 {
            let system = self.messages.first().cloned();
            let recent: Vec<ChatMessage> = self.messages.iter().skip(1).rev().take(20).cloned().collect();
            let mut trimmed = Vec::new();
            if let Some(sys) = system { trimmed.push(sys); }
            trimmed.extend(recent.into_iter().rev());
            self.messages = trimmed;
        }
    }

    pub async fn compact_tier4(&mut self, llm: &LlmClient, context_window: usize) -> Result<bool> {
        let total: usize = self.messages.iter().map(|m| m.content.as_ref().map(|c| c.as_text().len()).unwrap_or(0)).sum();
        if total <= context_window / 2 { return Ok(false); }

        let system = self.messages.first().cloned();
        let keep_count = 10.min(self.messages.len().saturating_sub(1));
        let keep_from = self.messages.len().saturating_sub(keep_count);
        let to_summarize: Vec<&ChatMessage> = self.messages.iter().skip(1).take(keep_from.saturating_sub(1)).collect();
        if to_summarize.is_empty() { return Ok(false); }

        let summary_text: String = to_summarize.iter()
            .map(|m| format!("{}: {}", m.role, m.content.as_ref().map(|c| c.as_text()).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n");

        let summary_prompt = format!("Summarize the following conversation, preserving key decisions and facts:\n\n{summary_text}");
        let summary_msg = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("You are a conversation summarizer.".to_string())),
                tool_calls: None, tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(summary_prompt)),
                tool_calls: None, tool_call_id: None,
            },
        ];

        match llm.chat(&summary_msg, &[]).await {
            Ok(summary) => {
                let summary_entry = ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text(format!("[Previous conversation summarized: {}]", summary.content.as_ref().map(|c| c.as_text()).unwrap_or_default()))),
                    tool_calls: None, tool_call_id: Some("summary".to_string()),
                };
                let mut new_msgs = Vec::new();
                if let Some(sys) = system { new_msgs.push(sys); }
                new_msgs.push(summary_entry);
                new_msgs.extend(self.messages.iter().skip(keep_from).cloned());
                self.messages = new_msgs;
                Ok(true)
            }
            Err(e) => {
                tracing::warn!("Compaction tier 4 failed: {e}");
                self.compact_tier3(context_window).await;
                Ok(false)
            }
        }
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