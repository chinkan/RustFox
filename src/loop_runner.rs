use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conversation::ConversationManager;
use crate::langsmith::LangSmithClient;
use crate::llm::{ChatMessage, LlmClient, MessageContent};
use crate::mcp::McpManager;
use crate::platform::sender::PlatformSender;
use crate::platform::tool_notifier::ToolEvent;
use crate::tool_registry::{ToolContext, ToolRegistry};

pub type ToolHandlerFn = Box<
    dyn Fn(
            &str,
            &Value,
            &str,
            &str,
        ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'static>>
        + Send
        + Sync,
>;

pub struct LoopConfig {
    pub max_iterations: u32,
    pub empty_response_retry_limit: u32,
    pub context_window: usize,
    pub loop_detection_enabled: bool,
    pub interactive_loop_callback: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub langsmith_project: Option<String>,
    pub model: Option<String>,
    pub tool_event_tx: Option<mpsc::Sender<ToolEvent>>,
    pub stream_token_tx: Option<mpsc::Sender<String>>,
    pub recovery_nudge: Option<String>,
}

pub enum LoopOutcome {
    FinalResponse(String),
    Cancelled,
    MaxIterations,
}

#[allow(dead_code)]
#[allow(clippy::type_complexity)]
pub struct AgenticLoop<'a> {
    llm: &'a LlmClient,
    tools: &'a ToolRegistry,
    mcp: &'a McpManager,
    config: &'a LoopConfig,
    cancel: Option<CancellationToken>,
    chain_run_id: Option<String>,
    langsmith: Option<&'a LangSmithClient>,
    platform_sender: &'a dyn PlatformSender,
    make_tool_ctx: Box<dyn Fn(&str, &str) -> ToolContext + Send + Sync + 'a>,
    special_tool_handler: Option<ToolHandlerFn>,
}

#[allow(clippy::type_complexity)]
impl<'a> AgenticLoop<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm: &'a LlmClient,
        tools: &'a ToolRegistry,
        mcp: &'a McpManager,
        config: &'a LoopConfig,
        cancel: Option<CancellationToken>,
        chain_run_id: Option<String>,
        langsmith: Option<&'a LangSmithClient>,
        platform_sender: &'a dyn PlatformSender,
        make_tool_ctx: Box<dyn Fn(&str, &str) -> ToolContext + Send + Sync + 'a>,
        special_tool_handler: Option<ToolHandlerFn>,
    ) -> Self {
        Self {
            llm,
            tools,
            mcp,
            config,
            cancel,
            chain_run_id,
            langsmith,
            platform_sender,
            make_tool_ctx,
            special_tool_handler,
        }
    }

    pub async fn run(
        &self,
        messages: &mut MessageContainer,
        user_id: &str,
        chat_id: &str,
    ) -> Result<LoopOutcome> {
        let context_window = self.config.context_window;
        let mut empty_count = 0u32;

        for _iteration in 0..self.config.max_iterations {
            if let Some(ref cancel) = self.cancel {
                if cancel.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled);
                }
            }

            let prepared = messages.prepare(context_window);

            let tool_defs = if let Some(ref whitelist) = self.config.allowed_tools {
                let mut all = self.tools.all_definitions();
                all.extend(self.mcp.tool_definitions());
                all.into_iter()
                    .filter(|d| whitelist.contains(&d.function.name))
                    .collect()
            } else {
                let mut all = self.tools.all_definitions();
                all.extend(self.mcp.tool_definitions());
                all
            };

            let (text, tool_calls) = if let Some(ref model) = self.config.model {
                let completion = self
                    .llm
                    .chat_completion_with_model(&prepared.messages, &tool_defs, model)
                    .await?;
                let text = completion
                    .message
                    .content
                    .as_ref()
                    .map(|c| c.as_text())
                    .unwrap_or_default();
                let tool_calls = completion.message.tool_calls.clone().unwrap_or_default();
                (text, tool_calls)
            } else {
                let msg = self.llm.chat(&prepared.messages, &tool_defs).await?;
                let text = msg
                    .content
                    .as_ref()
                    .map(|c| c.as_text())
                    .unwrap_or_default();
                let tool_calls = msg.tool_calls.clone().unwrap_or_default();
                (text, tool_calls)
            };

            if text.is_empty() && tool_calls.is_empty() {
                empty_count += 1;
                if let Some(ref nudge) = self.config.recovery_nudge {
                    let nudge_msg = ChatMessage {
                        role: "user".to_string(),
                        content: Some(MessageContent::from_text(nudge.clone())),
                        tool_calls: None,
                        tool_call_id: None,
                    };
                    match messages {
                        MessageContainer::Conversation(cm) => cm.add_user_turn(nudge_msg),
                        MessageContainer::Plain(msgs) => msgs.push(nudge_msg),
                    }
                }
                if empty_count >= self.config.empty_response_retry_limit {
                    return Ok(LoopOutcome::FinalResponse(
                        "I'm having trouble processing that. Please try again.".to_string(),
                    ));
                }
                continue;
            }

            if !tool_calls.is_empty() {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(text.clone())),
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                });

                for tc in &tool_calls {
                    if let Some(ref whitelist) = self.config.allowed_tools {
                        if !whitelist.contains(&tc.function.name) {
                            messages.push_tool_result(
                                &tc.id,
                                format!(
                                    "Tool '{}' is not available to this agent.",
                                    tc.function.name
                                ),
                            );
                            continue;
                        }
                    }

                    let args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Object(serde_json::Map::new()));

                    // Check special tool handler first (for invoke_agent/spawn_agents)
                    if let Some(ref handler) = self.special_tool_handler {
                        let fut = (handler)(&tc.function.name, &args, user_id, chat_id);
                        if let Some(result) = fut.await {
                            messages.push_tool_result(&tc.id, result);
                            continue;
                        }
                    }

                    let result = if tc.function.name.starts_with("mcp_") {
                        self.mcp
                            .call_tool(&tc.function.name, &args)
                            .await
                            .unwrap_or_else(|e| format!("Error: {e}"))
                    } else {
                        let ctx = (self.make_tool_ctx)(user_id, chat_id);
                        self.tools
                            .execute(&tc.function.name, args, ctx)
                            .await
                            .unwrap_or_else(|e| format!("Error: {e}"))
                    };

                    messages.push_tool_result(&tc.id, result);
                }
                continue;
            }

            if !text.is_empty() {
                if let Some(ref tx) = self.config.stream_token_tx {
                    let _ = LlmClient::stream_text(text.clone(), tx.clone()).await;
                }
                return Ok(LoopOutcome::FinalResponse(text));
            }

            empty_count += 1;
        }

        Ok(LoopOutcome::MaxIterations)
    }
}

pub enum MessageContainer {
    Conversation(Box<ConversationManager>),
    Plain(Vec<ChatMessage>),
}

impl MessageContainer {
    pub fn prepare(&self, context_window: usize) -> crate::agent_prompt::PreparedPrompt {
        match self {
            MessageContainer::Conversation(cm) => cm.prepare(context_window),
            MessageContainer::Plain(msgs) => {
                crate::agent_prompt::prepare_messages_for_llm(msgs, context_window)
            }
        }
    }

    pub fn push(&mut self, msg: ChatMessage) {
        match self {
            MessageContainer::Conversation(cm) => cm.add_assistant_turn(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }

    pub fn push_tool_result(&mut self, tool_call_id: &str, result: String) {
        let msg = ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(result)),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        };
        match self {
            MessageContainer::Conversation(cm) => cm.add_tool_result(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }
}
