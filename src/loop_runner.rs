use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conversation::ConversationManager;
use crate::langsmith::LangSmithClient;
use crate::llm::{ChatMessage, LlmClient, MessageContent};
use crate::loop_detector::LoopDetector;
use crate::mcp::McpManager;
use crate::platform::sender::PlatformSender;
use crate::platform::tool_notifier::ToolEvent;
use crate::tool_registry::{ToolContext, ToolRegistry};

/// Await `fut`, but return `Err(())` immediately when `cancel` fires.
/// Leaves the tool future dropped (dangling tool call stays in the transcript
/// for ADR-0039 checkpoint). Used so wall-clock timeout's ~2s wait can land
/// `LoopOutcome::Cancelled` even when stuck in a long tool (#250).
async fn await_tool_or_cancel<T>(
    cancel: Option<&CancellationToken>,
    fut: impl Future<Output = T>,
) -> Result<T, ()> {
    match cancel {
        Some(token) => {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(()),
                result = fut => Ok(result),
            }
        }
        None => Ok(fut.await),
    }
}

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

/// ADR-0020 steer source: returns user messages that arrived mid-run, already
/// processed (attachments, `[Steer] ` prefix) and persisted. Empty = none.
pub type SteerFn<'a> =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Vec<ChatMessage>> + Send + 'a>> + Send + Sync + 'a>;

pub struct LoopConfig {
    pub max_iterations: u32,
    pub empty_response_retry_limit: u32,
    pub context_window: usize,
    pub loop_detection_enabled: bool,
    /// Exact-repetition threshold for [`crate::loop_detector::LoopDetector`] (default 3).
    pub loop_detection_threshold: usize,
    pub interactive_loop_callback: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub langsmith_project: Option<String>,
    pub model: Option<String>,
    pub tool_event_tx: Option<mpsc::Sender<ToolEvent>>,
    pub stream_token_tx: Option<mpsc::Sender<String>>,
    pub recovery_nudge: Option<String>,
    /// ADR-0023 §7: `session_key(bot_id, user_id)` for once-per-conversation Exa offer.
    pub session_key: Option<String>,
}

/// Parent chain created by `process_message_outcome`. Subagent loops reuse it
/// and must not open a second `rustfox_request` run.
#[derive(Debug, Clone)]
pub struct TurnTrace {
    pub chain_run_id: String,
    pub project: String,
}

/// Which guard replaced the model's FinalResponse text.
///
/// Interactive chat still shows that text. The scheduler treats
/// `Some` as "not a real completion" unless a fresh artifact proves the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    /// Capability cold-intro survived one regenerate (`COLD_INTRO_CLARIFY_REWRITE`).
    ColdIntroClarify,
    /// Post–empty-progress lost-context / empty-inbound claim was rewritten.
    LostContextClarify,
    /// Short meta-token filler aborted to the clarify sentence.
    MetaFillerAbort,
}

impl GuardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ColdIntroClarify => "cold_intro_clarify",
            Self::LostContextClarify => "lost_context_clarify",
            Self::MetaFillerAbort => "meta_filler_abort",
        }
    }
}

#[derive(Debug)]
pub enum LoopOutcome {
    /// `iterations` is the number of LLM rounds in this loop.
    /// A turn that answers on the first round is 1.
    FinalResponse {
        text: String,
        iterations: u32,
        /// Which guard replaced the model text. Interactive chat still
        /// delivers `text`. The scheduler treats `Some` as unverified unless
        /// a fresh artifact matches (#330 / #333).
        rewritten_by_guard: Option<GuardKind>,
    },
    /// Cancelled before/at iteration boundary. `iterations` is completed LLM
    /// rounds so far (0 if cancelled before the first completion).
    Cancelled {
        iterations: u32,
    },
    MaxIterations,
}

/// ADR-0045 (B) / #231 empty-progress clarify: user-visible FinalResponse when
/// loop_detection sees N consecutive empty-progress iterations. Do **not**
/// mention MCP or duplicate-tool internals.
pub const EMPTY_PROGRESS_CLARIFY: &str =
    "I wasn't making progress on that. Could you clarify what you'd like me to do next?";

/// ADR-0045 / #315: one-shot nudge after empty-progress threshold so the model
/// can produce a real FinalResponse; post-guard rewrites dishonest lost-context FN.
pub const EMPTY_PROGRESS_CLARIFY_NUDGE: &str = "[System] You weren't making progress on the prior step. Ask a short clarifying question about the user's latest message. Do not claim the inbound message was empty or that prior conversation context was lost. Do not list capabilities or greet as if starting fresh.";

/// ADR-0045 / #298 short meta-token allowlist (trim; optional trailing `.`).
/// `Done` deliberately omitted — legitimate FinalResponse `"done"` / `"Done."` must not clarify.
const META_TOKEN_ALLOWLIST: &[&str] = &["Output", "Emit", "Final", "Write", "Ok"];

/// True when `line` (already conceptually one line) is an allowlist stub.
fn is_meta_token_stub(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let t = t.strip_suffix('.').unwrap_or(t);
    META_TOKEN_ALLOWLIST
        .iter()
        .any(|tok| t.eq_ignore_ascii_case(tok))
}

/// ADR-0045 / #298: short meta-token FinalResponse filler.
///
/// Filler when:
/// - **single stub:** entire trimmed content is only an allowlist token + optional `.`; or
/// - **≥3-line / alternating streak:** ≥3 non-empty lines, each an allowlist stub.
///
/// Benign short substantive answers must return false.
pub fn is_meta_token_filler(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let non_empty: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return false;
    }
    // Single stub (one non-empty line that is only the allowlist token).
    if non_empty.len() == 1 {
        return is_meta_token_stub(non_empty[0]);
    }
    // ≥3-line / alternating streak of allowlist stubs.
    non_empty.len() >= 3 && non_empty.iter().all(|l| is_meta_token_stub(l))
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
    steer: Option<SteerFn<'a>>,
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
            steer: None,
        }
    }

    /// Drain mid-run user messages at each step (ADR-0020 drain points A/B).
    pub fn with_steer(mut self, steer: SteerFn<'a>) -> Self {
        self.steer = Some(steer);
        self
    }

    /// Drained steer messages (empty when steering is off).
    async fn drain_steer(&self) -> Vec<ChatMessage> {
        match &self.steer {
            Some(steer) => steer().await,
            None => Vec::new(),
        }
    }

    fn trace_now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn session_name(&self) -> String {
        self.config
            .langsmith_project
            .clone()
            .unwrap_or_else(|| "default".to_string())
    }

    /// Start a child run under the parent chain. Returns None when this loop
    /// has no chain (nothing to parent). A disabled client still receives the
    /// call; `LangSmithClient` returns before any HTTP.
    fn begin_child(
        &self,
        name: &str,
        run_type: crate::langsmith::RunType,
        inputs: Value,
    ) -> Option<String> {
        let (Some(ls), Some(parent)) = (self.langsmith, self.chain_run_id.as_ref()) else {
            return None;
        };
        let id = uuid::Uuid::new_v4().to_string();
        ls.start_run(crate::langsmith::RunParams {
            id: id.clone(),
            name: name.to_string(),
            run_type,
            parent_run_id: Some(parent.clone()),
            inputs,
            session_name: self.session_name(),
            start_time: Self::trace_now(),
        });
        Some(id)
    }

    fn finish_child(&self, id: Option<String>, outputs: Option<Value>, error: Option<String>) {
        let (Some(ls), Some(id)) = (self.langsmith, id) else {
            return;
        };
        ls.end_run(crate::langsmith::EndRunParams {
            id,
            outputs,
            error,
            end_time: Self::trace_now(),
        });
    }

    pub async fn run(
        &self,
        messages: &mut MessageContainer,
        user_id: &str,
        chat_id: &str,
    ) -> Result<LoopOutcome> {
        let context_window = self.config.context_window;
        let mut empty_count = 0u32;
        let mut iterations;
        // Wire the existing LoopDetector (dropped when AgenticLoop was extracted).
        // Exact tool+args repetition → recovery nudge; repeated detection → stop.
        // Auto-nudge for all paths (incl. portal); Telegram interactive pause stays
        // out of this ponytail so portal cannot hang waiting on a oneshot.
        let mut loop_detector = LoopDetector::new(
            self.config.loop_detection_threshold,
            self.config.loop_detection_enabled,
        );
        let mut loop_recovery_count = 0u32;
        const MAX_LOOP_RECOVERIES: u32 = 2;
        // ADR-0045 (B): consecutive empty-progress / no-FinalResponse streak.
        // Reuses agent.loop_detection.threshold (default 3) when enabled.
        let mut empty_progress_streak = 0usize;
        // ADR-0043 / #277: anti cold-intro on shared Active final-reply path.
        let has_prior_continuity =
            crate::cold_intro::has_prior_continuity(&messages.messages_snapshot());
        let mut cold_intro_regen_used = false;
        // ADR-0045 / #315: armed when empty-progress threshold forces clarify;
        // subsequent FinalResponse lost-context claims (menu or not) rewrite.
        let mut after_empty_progress_clarify = false;

        for iteration in 0..self.config.max_iterations {
            if let Some(ref cancel) = self.cancel {
                if cancel.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled {
                        iterations: iteration,
                    });
                }
            }
            // Drain point A: messages that arrived during the last step.
            for msg in self.drain_steer().await {
                messages.push_user(msg);
            }

            let prepared = messages.prepare(context_window);

            // Assemble builtin → MCP, then exact keep-first dedupe (ADR-0019 slice ②).
            let mut all = self.tools.all_definitions();
            all.extend(self.mcp.tool_definitions());
            let all = crate::llm::dedupe_tool_definitions_keep_first(all);
            let tool_defs = if let Some(ref whitelist) = self.config.allowed_tools {
                all.into_iter()
                    .filter(|d| whitelist.contains(&d.function.name))
                    .collect()
            } else {
                all
            };

            let llm_run = self.begin_child(
                "llm_call",
                crate::langsmith::RunType::Llm,
                serde_json::json!({ "messages": &prepared.messages }),
            );
            let llm_result = if let Some(ref model) = self.config.model {
                self.llm
                    .chat_completion_with_model(&prepared.messages, &tool_defs, model)
                    .await
                    .map(|completion| completion.message)
            } else {
                self.llm.chat(&prepared.messages, &tool_defs).await
            };
            let msg = match llm_result {
                Ok(msg) => msg,
                Err(e) => {
                    self.finish_child(llm_run, None, Some(format!("{e:#}")));
                    return Err(e);
                }
            };
            let text = msg
                .content
                .as_ref()
                .map(|c| c.as_text())
                .unwrap_or_default();
            let tool_calls = msg.tool_calls.clone().unwrap_or_default();
            self.finish_child(
                llm_run,
                Some(serde_json::json!({
                    "content": text,
                    "tool_calls": msg.tool_calls,
                })),
                None,
            );
            iterations = iteration + 1;

            if text.is_empty() && tool_calls.is_empty() {
                if let Some(ref nudge) = self.config.recovery_nudge {
                    let nudge_msg = ChatMessage {
                        role: "user".to_string(),
                        content: Some(MessageContent::from_text(nudge.clone())),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    };
                    messages.push_user(nudge_msg);
                }
                // ADR-0045 (B): when loop_detection is enabled, N consecutive
                // empty-progress iterations → clarify FinalResponse (no silent
                // burn / no MCP-duplicate internals in user copy).
                if self.config.loop_detection_enabled {
                    empty_progress_streak += 1;
                    if empty_progress_streak >= self.config.loop_detection_threshold {
                        tracing::warn!(
                            streak = empty_progress_streak,
                            threshold = self.config.loop_detection_threshold,
                            "Empty-progress loop detected — forcing clarify FinalResponse"
                        );
                        // First hit: arm #315 post-clarify guard + nudge once so a
                        // dishonest lost-context / menu regen can be caught on the
                        // shared FinalResponse path. Still-empty after arm → ship
                        // fixed clarify (ADR-0045 B unchanged outcome).
                        if !after_empty_progress_clarify {
                            after_empty_progress_clarify = true;
                            messages.push_user(ChatMessage {
                                role: "user".to_string(),
                                content: Some(MessageContent::from_text(
                                    EMPTY_PROGRESS_CLARIFY_NUDGE.to_string(),
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            });
                            continue;
                        }
                        return Ok(LoopOutcome::FinalResponse {
                            text: EMPTY_PROGRESS_CLARIFY.to_string(),
                            iterations,
                            rewritten_by_guard: None,
                        });
                    }
                    continue;
                }
                empty_count += 1;
                if empty_count >= self.config.empty_response_retry_limit {
                    return Ok(LoopOutcome::FinalResponse {
                        text: "I'm having trouble processing that. Please try again.".to_string(),
                        iterations,
                        rewritten_by_guard: None,
                    });
                }
                continue;
            }

            // Non-empty progress resets the empty-progress streak.
            empty_progress_streak = 0;

            if !tool_calls.is_empty() {
                loop_detector.record(&tool_calls, iteration as usize);
                if let Some(loop_info) = loop_detector.detect_loop() {
                    tracing::warn!(
                        tool = %loop_info.tool_name,
                        count = loop_info.call_count,
                        recovery = loop_recovery_count,
                        "Tool-call loop detected — no progress on repeated tool+args"
                    );
                    loop_recovery_count += 1;
                    if loop_recovery_count > MAX_LOOP_RECOVERIES {
                        return Ok(LoopOutcome::FinalResponse {
                            text: format!(
                                "I noticed I was calling {} repeatedly without progress, so I stopped. Please try rephrasing your request.",
                                loop_info.tool_name
                            ),
                            iterations,
                            rewritten_by_guard: None,
                        });
                    }
                    // Push the assistant tool_calls, then nudge results (do not re-execute).
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(MessageContent::Text(text.clone())),
                        tool_calls: Some(tool_calls.clone()),
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                    let nudge_text = format!(
                        "Error: You have called {} {} times with the same arguments.                          The result has not changed. Try a different approach or answer the user now.",
                        loop_info.tool_name, loop_info.call_count,
                    );
                    for tc in &tool_calls {
                        messages.push_tool_result(&tc.id, nudge_text.clone());
                    }
                    loop_detector.clear();
                    continue;
                }

                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(text.clone())),
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                    reasoning_content: None,
                });

                for tc in &tool_calls {
                    let args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Object(serde_json::Map::new()));
                    let tool_run = self.begin_child(
                        &tc.function.name,
                        crate::langsmith::RunType::Tool,
                        serde_json::json!({ "arguments": &args }),
                    );

                    if let Some(ref whitelist) = self.config.allowed_tools {
                        if !whitelist.contains(&tc.function.name) {
                            let rejected = format!(
                                "Tool '{}' is not available to this agent.",
                                tc.function.name
                            );
                            self.finish_child(tool_run, None, Some(rejected.clone()));
                            messages.push_tool_result(&tc.id, rejected);
                            continue;
                        }
                    }

                    // Check special tool handler first (for invoke_agent/spawn_agents)
                    if let Some(ref handler) = self.special_tool_handler {
                        let fut = (handler)(&tc.function.name, &args, user_id, chat_id);
                        let special = match await_tool_or_cancel(self.cancel.as_ref(), fut).await {
                            Ok(v) => v,
                            Err(()) => {
                                self.finish_child(
                                    tool_run,
                                    None,
                                    Some("cancelled during tool".into()),
                                );
                                return Ok(LoopOutcome::Cancelled { iterations });
                            }
                        };
                        if let Some(result) = special {
                            // Peer cycle/depth/self-invoke must abort the turn so
                            // Telegram clears Working and surfaces an error reply
                            // (soft tool results left the UI stuck on hung LLM retries).
                            if crate::peer_invoke::is_hard_invoke_error(&result) {
                                self.finish_child(tool_run, None, Some(result.clone()));
                                return Err(anyhow::anyhow!("{result}"));
                            }
                            self.finish_child(
                                tool_run,
                                Some(serde_json::json!({ "result": &result })),
                                None,
                            );
                            messages.push_tool_result(&tc.id, result);
                            continue;
                        }
                    }

                    let tool_fut = async {
                        if tc.function.name.starts_with("mcp_") {
                            self.mcp.call_tool(&tc.function.name, &args).await
                        } else if tc.function.name == "web_search"
                            && !self.mcp.is_connected(crate::mcp_presets::EXA_NAME)
                        {
                            if let Some(key) = self.config.session_key.as_deref() {
                                let _ = self
                                    .platform_sender
                                    .offer_exa_web_search(chat_id, key)
                                    .await;
                            }
                            Ok(crate::mcp_presets::WEB_SEARCH_STUB_RESULT.to_string())
                        } else {
                            let ctx = (self.make_tool_ctx)(user_id, chat_id);
                            self.tools.execute(&tc.function.name, args, ctx).await
                        }
                    };
                    let executed = match await_tool_or_cancel(self.cancel.as_ref(), tool_fut).await
                    {
                        Ok(v) => v,
                        Err(()) => {
                            self.finish_child(tool_run, None, Some("cancelled during tool".into()));
                            return Ok(LoopOutcome::Cancelled { iterations });
                        }
                    };
                    match executed {
                        Ok(result) => {
                            self.finish_child(
                                tool_run,
                                Some(serde_json::json!({ "result": &result })),
                                None,
                            );
                            messages.push_tool_result(&tc.id, result);
                        }
                        Err(e) => {
                            let err = format!("Error: {e}");
                            self.finish_child(tool_run, None, Some(err.clone()));
                            messages.push_tool_result(&tc.id, err);
                        }
                    }
                }
                continue;
            }

            if !text.is_empty() {
                // Drain point B: a message arrived while the model drafted this
                // reply. Keep the draft as context and answer again (counts
                // toward max_iterations).
                let drained = self.drain_steer().await;
                if !drained.is_empty() {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(MessageContent::Text(text)),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                    for msg in drained {
                        messages.push_user(msg);
                    }
                    continue;
                }

                // ADR-0043 / #277 structural guard (all Active peers): with prior
                // continuity, reject a capability cold-intro once; if still cold
                // after regenerate, rewrite to a short clarify (no menu).
                // The regenerate `continue` is not a rewrite. Last writer wins
                // if more than one guard fires on the same reply.
                let mut text = text;
                let mut rewritten_by_guard = None;
                if has_prior_continuity && crate::cold_intro::is_capability_cold_intro(&text) {
                    if !cold_intro_regen_used {
                        cold_intro_regen_used = true;
                        tracing::warn!(
                            "Anti cold-intro: rejecting capability menu final reply; regenerating once"
                        );
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: Some(MessageContent::Text(text)),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        });
                        messages.push_user(ChatMessage {
                            role: "user".to_string(),
                            content: Some(MessageContent::from_text(
                                crate::cold_intro::COLD_INTRO_REGEN_NUDGE.to_string(),
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        });
                        continue;
                    }
                    tracing::warn!(
                        "Anti cold-intro: regenerate still cold-intro; rewriting to clarify"
                    );
                    text = crate::cold_intro::COLD_INTRO_CLARIFY_REWRITE.to_string();
                    rewritten_by_guard = Some(GuardKind::ColdIntroClarify);
                }

                // ADR-0045 / #315: after empty-progress force-clarify, lost-context /
                // empty-inbound claims (with or without ≥2 menu lines) → rewrite.
                // Closes the C1 hole where menu-reject regen ships lost-context alone.
                if after_empty_progress_clarify
                    && has_prior_continuity
                    && crate::cold_intro::has_lost_context_signal(&text)
                {
                    tracing::warn!(
                        "Post–empty-progress: lost-context / empty-inbound FinalResponse — rewriting to clarify"
                    );
                    text = crate::cold_intro::COLD_INTRO_CLARIFY_REWRITE.to_string();
                    rewritten_by_guard = Some(GuardKind::LostContextClarify);
                }

                // ADR-0045 / #298: short meta-token filler → abort→clarify
                // (same family as empty-progress; no silent drop; no MCP/DeepSeek
                // internals in user copy). Shared Active FinalResponse path.
                if is_meta_token_filler(&text) {
                    tracing::warn!(
                        preview = %text.chars().take(80).collect::<String>(),
                        "Short meta-token FinalResponse filler — forcing clarify"
                    );
                    text = EMPTY_PROGRESS_CLARIFY.to_string();
                    rewritten_by_guard = Some(GuardKind::MetaFillerAbort);
                }

                if let Some(ref tx) = self.config.stream_token_tx {
                    let _ = LlmClient::stream_text(text.clone(), tx.clone()).await;
                }
                return Ok(LoopOutcome::FinalResponse {
                    text,
                    iterations,
                    rewritten_by_guard,
                });
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

    pub fn push_user(&mut self, msg: ChatMessage) {
        match self {
            MessageContainer::Conversation(cm) => cm.add_user_turn(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }

    pub fn push_tool_result(&mut self, tool_call_id: &str, result: String) {
        let msg = ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(result)),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            reasoning_content: None,
        };
        match self {
            MessageContainer::Conversation(cm) => cm.add_tool_result(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }

    /// Snapshot current messages (ADR-0039 timeout checkpoint).
    pub fn messages_snapshot(&self) -> Vec<ChatMessage> {
        match self {
            MessageContainer::Conversation(cm) => cm.messages().to_vec(),
            MessageContainer::Plain(msgs) => msgs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel_registry::CancelRegistry;
    use crate::config::ProviderType;
    use crate::llm::{ChatMessage, LlmClient, MessageContent};
    use crate::mcp::McpManager;
    use crate::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
    use crate::provider::{OpenRouterProvider, ProviderConfig, ProviderRegistry};
    use crate::scheduler::checkpoint::TranscriptCheckpoint;
    use crate::tool_registry::ToolRegistry;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::RwLock;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn meta_token_single_stub_is_filler() {
        for s in [
            "Write.",
            "Emit.",
            "Output",
            "Final.",
            "Ok.",
            "  write.  ",
            "OK",
        ] {
            assert!(is_meta_token_filler(s), "expected filler for {s:?}");
        }
    }

    #[test]
    fn meta_token_streak_is_filler() {
        let streak = "Write.\nEmit.\nOk.";
        assert!(is_meta_token_filler(streak));
        let alt = "Output.\nOk.\nFinal.\nWrite.";
        assert!(is_meta_token_filler(alt));
    }

    #[test]
    fn meta_token_benign_short_is_not_filler() {
        for s in [
            "done",
            "Done",
            "Done.",
            "all done",
            "Ok, I'll do that.",
            "Done with the report.",
            "Yes",
            "42",
            "Write a poem.",
            "Write.\nSomething useful.",
            "Write.\nEmit.", // only 2 stub lines — not a ≥3 streak
        ] {
            assert!(!is_meta_token_filler(s), "false-positive on {s:?}");
        }
    }

    struct NopSender;

    #[async_trait]
    impl PlatformSender for NopSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_loop_config() -> LoopConfig {
        LoopConfig {
            max_iterations: 4,
            empty_response_retry_limit: 2,
            context_window: 8_000,
            loop_detection_enabled: false,
            loop_detection_threshold: 3,
            interactive_loop_callback: false,
            allowed_tools: None,
            langsmith_project: None,
            model: Some("p1/m1".into()),
            tool_event_tx: None,
            stream_token_tx: None,
            recovery_nudge: None,
            session_key: None,
        }
    }

    fn test_llm(uri: String) -> LlmClient {
        let cfg = ProviderConfig {
            name: "p1".into(),
            provider_type: ProviderType::OpenRouter,
            base_url: uri,
            api_key: Some("k".into()),
            default_model: "m1".into(),
            supports_vision: false,
            max_tokens: 64,
            discover_models: false,
            context_window: 8_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        };
        let provider: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(cfg));
        let mut providers = std::collections::HashMap::new();
        providers.insert("p1".into(), provider);
        let reg = Arc::new(ProviderRegistry::new(providers, "p1".into()));
        LlmClient::with_request_timeout_secs(reg, 30)
    }

    /// #250: cancel while a long tool is in flight must return Cancelled
    /// quickly with a transcript that can form a checkpoint (dangling tool id).
    #[tokio::test]
    async fn cancel_during_inflight_tool_yields_cancelled_transcript() {
        let server = MockServer::start().await;
        let tool_body = r#"{
          "choices":[{
            "message":{
              "role":"assistant",
              "content":"",
              "tool_calls":[{
                "id":"call_sleep",
                "type":"function",
                "function":{"name":"slow_tool","arguments":"{}"}
              }]
            },
            "finish_reason":"tool_calls"
          }]
        }"#;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tool_body))
            .mount(&server)
            .await;

        let llm = test_llm(server.uri());
        let tools = ToolRegistry::new();
        let mcp = McpManager::new();
        let config = test_loop_config();
        let cancel = CancellationToken::new();
        let sender = NopSender;

        // Simulate stuck execute_command sleep: does NOT poll cancel itself
        // (the pre-#250 bug). Loop must interrupt via await_tool_or_cancel.
        let special: ToolHandlerFn = Box::new(move |name, _args, _uid, _cid| {
            let name = name.to_string();
            Box::pin(async move {
                if name != "slow_tool" {
                    return None;
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
                Some("done".into())
            })
        });

        let agentic = AgenticLoop::new(
            &llm,
            &tools,
            &mcp,
            &config,
            Some(cancel.clone()),
            None,
            None,
            &sender,
            Box::new(|_u, _c| crate::tool_registry::ToolContext {
                sandbox_dir: std::env::temp_dir(),
                home_dir: None,
                sender: Arc::new(NopSender),
                cancel_registry: Arc::new(CancelRegistry::new()),
                user_id: "u".into(),
                chat_id: "1".into(),
                bot_id: "default".into(),
                tool_ui_mode: crate::tool_registry::ToolUiMode::Silent,
            }),
            Some(special),
        );

        let mut messages = MessageContainer::Plain(vec![ChatMessage {
            role: "user".into(),
            content: Some(MessageContent::from_text("run sleep")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }]);

        let started = Instant::now();
        let cancel_later = cancel.clone();
        let run_task = async {
            // Let the LLM return and the slow tool start, then cancel (mirrors
            // scheduler timeout → cancel_processing → ~2s wait).
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                cancel_later.cancel();
            });
            agentic.run(&mut messages, "u", "1").await
        };
        let outcome = tokio::time::timeout(Duration::from_secs(2), run_task)
            .await
            .expect("Cancelled must return within 2s (scheduler timeout wait)")
            .expect("loop must not error");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must not wait for the 60s tool"
        );
        match outcome {
            LoopOutcome::Cancelled { iterations } => {
                assert!(iterations >= 1, "LLM round must have completed");
            }
            other => panic!("expected Cancelled, got {other:?}"),
        }

        let snap = messages.messages_snapshot();
        let cp = TranscriptCheckpoint::from_messages(snap, 1);
        assert!(
            cp.dangling_tool_ids.contains(&"call_sleep".to_string()),
            "in-flight tool must be dangling so timeout gate can checkpoint: {:?}",
            cp.dangling_tool_ids
        );
        let cont = cp.messages_for_continue();
        assert!(
            crate::scheduler::checkpoint::find_dangling_tool_ids(&cont).is_empty(),
            "continue synthesis must close dangling ids"
        );
    }

    #[tokio::test]
    async fn await_tool_or_cancel_prefers_cancel() {
        let token = CancellationToken::new();
        token.cancel();
        let slow = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            42
        };
        let result = await_tool_or_cancel(Some(&token), slow).await;
        assert_eq!(result, Err(()));
    }
}
