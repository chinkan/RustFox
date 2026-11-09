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
    /// Running summary of compacted history (ADR 0003 Q2, ADR-0064).
    /// One structured handoff document, persisted as a single `[SUMMARY]`
    /// row (Q8), injected as a system message.
    summary: Option<String>,
    /// Highest message index whose user turn was already flushed to USER.md (Q6).
    last_flush_turn: Option<usize>,
}

/// Tool results longer than this are shortened before the summarizer sees them.
const TOOL_RESULT_PRUNE_CHARS: usize = 160;
/// Prefix kept from a pruned tool result so a short head can still be quoted.
const TOOL_RESULT_KEEP_CHARS: usize = 80;

/// Latest structured handoff wins. Pre-handoff `[SUMMARY]` snippets still join.
pub(crate) fn fold_persisted_summary_parts(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    if let Some(latest) = parts.iter().rev().find(|part| is_structured_handoff(part)) {
        return Some(latest.trim().to_string());
    }
    Some(parts.join("\n\n"))
}

fn is_structured_handoff(text: &str) -> bool {
    crate::agent_prompt::HANDOFF_SECTION_HEADINGS
        .iter()
        .all(|heading| text.contains(&format!("## {heading}")))
}

fn prune_tool_results_for_summary(messages: &[&ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|message| {
            if message.role != "tool" {
                return (*message).clone();
            }
            let Some(content) = message.content.as_ref() else {
                return (*message).clone();
            };
            let text = content.as_text();
            if text.chars().count() <= TOOL_RESULT_PRUNE_CHARS {
                return (*message).clone();
            }
            let keep: String = text.chars().take(TOOL_RESULT_KEEP_CHARS).collect();
            let mut pruned = (*message).clone();
            pruned.content = Some(MessageContent::Text(format!(
                "{keep}\n[tool result pruned]"
            )));
            pruned
        })
        .collect()
}

fn format_messages_for_summary(messages: &[&ChatMessage]) -> String {
    messages
        .iter()
        .map(|message| format_message_for_summary(message))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_message_for_summary(message: &ChatMessage) -> String {
    let mut lines = Vec::new();
    if let Some(text) = message.content.as_ref().map(|content| content.as_text()) {
        if !text.trim().is_empty() {
            lines.push(text);
        }
    }
    if let Some(calls) = &message.tool_calls {
        for call in calls {
            lines.push(format!(
                "tool_call id={} name={} arguments={}",
                call.id, call.function.name, call.function.arguments
            ));
        }
    }
    if let Some(id) = &message.tool_call_id {
        if !lines.iter().any(|line| line.contains(id)) {
            lines.push(format!("tool_call_id={id}"));
        }
    }
    if lines.is_empty() {
        format!("{}:", message.role)
    } else {
        format!("{}: {}", message.role, lines.join("\n"))
    }
}

fn build_handoff_user_prompt(previous: &str, transcript: &str) -> String {
    let prev_block = if previous.trim().is_empty() {
        String::new()
    } else {
        format!("Previous handoff (update these sections in place):\n{previous}\n\n")
    };
    format!(
        "{}\n\n{prev_block}New conversation:\n{transcript}",
        crate::agent_prompt::handoff_summary_instructions()
    )
}

fn finish_structured_handoff(model_text: &str, preserve_corpus: &str) -> String {
    let (preamble, mut sections) = parse_handoff_sections(model_text);
    let preamble = preamble.trim();
    if !preamble.is_empty() {
        let ctx = sections[6].trim().to_string();
        if ctx.is_empty() || ctx == "None" {
            sections[6] = preamble.to_string();
        } else if !ctx.contains(preamble) {
            sections[6] = format!("{ctx}\n{preamble}");
        }
    }
    let draft = render_handoff(&sections);
    let missing: Vec<String> = extract_opaque_ids(preserve_corpus)
        .into_iter()
        .filter(|id| !draft.contains(id))
        .collect();
    if !missing.is_empty() {
        let ctx = &mut sections[6];
        let mut body = ctx.trim().to_string();
        if body.is_empty() || body == "None" {
            body.clear();
        }
        for id in missing {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&id);
        }
        *ctx = body;
    }
    render_handoff(&sections)
}

fn parse_handoff_sections(text: &str) -> (String, [String; 7]) {
    let mut preamble = String::new();
    let mut sections = std::array::from_fn(|_| String::new());
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if let Some(idx) = heading_index(line) {
            current = Some(idx);
            continue;
        }
        if let Some(idx) = current {
            if !sections[idx].is_empty() {
                sections[idx].push('\n');
            }
            sections[idx].push_str(line);
        } else if !preamble.is_empty() {
            preamble.push('\n');
            preamble.push_str(line);
        } else {
            preamble.push_str(line);
        }
    }
    (preamble, sections)
}

fn heading_index(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    crate::agent_prompt::HANDOFF_SECTION_HEADINGS
        .iter()
        .position(|heading| trimmed == format!("## {heading}"))
}

fn render_handoff(sections: &[String; 7]) -> String {
    let mut out = String::new();
    for (idx, heading) in crate::agent_prompt::HANDOFF_SECTION_HEADINGS
        .iter()
        .enumerate()
    {
        if idx > 0 {
            out.push('\n');
        }
        let body = if *heading == "Progress" {
            normalize_progress(sections[idx].trim())
        } else {
            let raw = sections[idx].trim();
            if raw.is_empty() {
                "None".to_string()
            } else {
                raw.to_string()
            }
        };
        out.push_str(&format!("## {heading}\n{body}\n"));
    }
    out.trim_end().to_string()
}

fn normalize_progress(body: &str) -> String {
    let mut buckets = [String::new(), String::new(), String::new()];
    let mut current: Option<usize> = None;
    let mut orphan = String::new();
    for line in body.lines() {
        if let Some(idx) = crate::agent_prompt::HANDOFF_PROGRESS_BUCKETS
            .iter()
            .position(|bucket| line.trim() == format!("### {bucket}"))
        {
            current = Some(idx);
            continue;
        }
        if let Some(idx) = current {
            if !buckets[idx].is_empty() {
                buckets[idx].push('\n');
            }
            buckets[idx].push_str(line);
        } else if !orphan.is_empty() {
            orphan.push('\n');
            orphan.push_str(line);
        } else {
            orphan.push_str(line);
        }
    }
    let orphan = orphan.trim();
    if !orphan.is_empty() && orphan != "None" && buckets[1].trim().is_empty() {
        buckets[1] = orphan.to_string();
    }
    let mut out = String::new();
    for (idx, name) in crate::agent_prompt::HANDOFF_PROGRESS_BUCKETS
        .iter()
        .enumerate()
    {
        if idx > 0 {
            out.push('\n');
        }
        let text = buckets[idx].trim();
        let text = if text.is_empty() { "None" } else { text };
        out.push_str(&format!("### {name}\n{text}"));
    }
    out
}

fn opaque_id_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)https?://[^\s<>\]\)"']+|\b(?:call|msg)_[A-Za-z0-9_-]+\b|#[0-9]+|\b[0-9a-f]{7,40}\b|(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+\.[A-Za-z0-9]+"#,
        )
        .expect("opaque id regex")
    })
}

fn extract_opaque_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for found in opaque_id_re().find_iter(text) {
        let id = found
            .as_str()
            .trim_end_matches([',', ';', ')', ']', '>', '"'])
            .to_string();
        if id.len() < 2 || out.iter().any(|seen: &String| seen == &id) {
            continue;
        }
        out.push(id);
    }
    out
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
            // ADR-0059: schedule segments stay in SQLite but are not interactive
            // LLM continuity. Reply-quote text does not use this prefix.
            if crate::scheduler::history::is_schedule_segment_message(&m) {
                continue;
            }
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
        let summary = fold_persisted_summary_parts(&folded_summary);

        let schedule_tz = crate::scheduler::timezone::resolve_schedule_timezone(
            config.schedule_timezone_config(),
        )
        .unwrap_or(crate::scheduler::timezone::ScheduleTimezone {
            tz: chrono_tz::UTC,
            source: crate::scheduler::timezone::TimezoneSource::Fallback,
        });
        let now = chrono::Utc::now().with_timezone(&schedule_tz.tz);
        let offset = schedule_tz.utc_offset_label(chrono::Utc::now());
        let context_prompt = format!(
            "\n\nCurrent date and time: {} {} ({}, UTC{})",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.format("%A"),
            schedule_tz.tz,
            offset
        );

        // ADR-0043 / #277: light continuity prompt when SQLite history already
        // has ≥1 prior user/assistant turn (excludes pure system/summary).
        let continuity_note = if crate::cold_intro::has_continuity_turns(&raw) {
            crate::cold_intro::CONTINUITY_LIGHT_PROMPT
        } else {
            ""
        };

        let system_msg = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!(
                "{system_prompt}{context_prompt}{continuity_note}"
            ))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                reasoning_content: None,
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
        // ADR-0056: device turns keep the owner's text and append id/room/fw
        // so the shared chat shows which board spoke. Absent metadata is a no-op.
        let text = crate::device_ws::annotate_user_text(
            &incoming.text,
            incoming.device_id.as_deref(),
            incoming.device_room.as_deref(),
            incoming.device_fw.as_deref(),
        );
        let (attachment_text, image_parts) = crate::file_processor::process_attachments(
            &incoming.attachments,
            &text,
            config,
            &self.memory,
            supports_vision,
        )
        .await;

        // Persist the same text the model will see (user text + attachment/OCR),
        // not the bare process_attachments return (empty when there is no attachment).
        let combined = Self::combine_user_and_attachment_text(&text, &attachment_text);

        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(combined.clone())),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        // Scheduled runs do not persist into the conversation they loaded.
        // That row is a fresh schedule conversation (ADR-0059), not the
        // interactive chat, and the result stays out of interactive history.
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

    /// Store one structured handoff (ADR 0003 Q2/Q8, ADR-0064).
    ///
    /// The summarizer already extended the previous document in place, so
    /// this replaces the running summary instead of concatenating layers.
    /// Rebuilds the message list as [system, summary block, protected tail]
    /// and persists a single `[SUMMARY]` row. Persistence failures are logged
    /// and ignored — the in-memory state wins.
    pub(crate) async fn apply_summary_layer(
        &mut self,
        layer: &str,
        tail_start: usize,
    ) -> Result<()> {
        let layer = layer.trim();
        if layer.is_empty() {
            anyhow::bail!("empty summary layer");
        }
        self.summary = Some(layer.to_string());

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
            reasoning_content: None,
        });
        new_msgs.extend(self.messages.iter().skip(tail_start).cloned());
        self.messages = new_msgs;

        let persisted = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!("[SUMMARY]\n{layer}"))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        if let Err(e) = self
            .memory
            .replace_summary_message(&self.conversation_id, &persisted)
            .await
        {
            tracing::warn!(error = %format!("{e:#}"), "Failed to persist summary document");
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
                // Distinct from `PROMPT_HARD_CAP_MARKER`. A failed summary
                // defers this pass and does not truncate (ADR 0003 Q7 / #376).
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

    /// Ask the summarizer (Q9 model override, else current model) to extend
    /// the running handoff in place (ADR 0003 Q2, ADR-0064).
    ///
    /// Tool results in the to-be-summarized range are pruned before the
    /// prompt is built. Opaque ids from the unpruned range and the previous
    /// handoff are copied back if the model drops them.
    async fn summarize_with_llm(
        &self,
        ctx: &CompactionContext<'_>,
        to_summarize: &[&ChatMessage],
    ) -> Result<String> {
        let previous = self.summary.as_deref().unwrap_or("");
        let preserve_corpus = format!("{previous}\n{}", format_messages_for_summary(to_summarize));
        let pruned = prune_tool_results_for_summary(to_summarize);
        let pruned_refs: Vec<&ChatMessage> = pruned.iter().collect();
        let transcript = format_messages_for_summary(&pruned_refs);
        let summary_prompt = build_handoff_user_prompt(previous, &transcript);

        let summary_msg = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(
                    "You are a conversation summarizer.".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(summary_prompt)),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
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
        let raw = response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        Ok(finish_structured_handoff(&raw, &preserve_corpus))
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
            extra_headers: vec![],
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
        replies: Vec<String>,
        calls: std::sync::atomic::AtomicUsize,
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl StubSummaryProvider {
        fn new(reply: impl Into<String>) -> Self {
            Self::scripted(
                vec![reply.into()],
                Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        }

        fn scripted(replies: Vec<String>, captured: Arc<std::sync::Mutex<Vec<String>>>) -> Self {
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
                    extra_headers: vec![],
                },
                replies,
                calls: std::sync::atomic::AtomicUsize::new(0),
                captured,
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
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            model: &str,
            _max_tokens: u32,
        ) -> anyhow::Result<ChatCompletion> {
            let prompt = messages
                .iter()
                .filter(|m| m.role == "user")
                .filter_map(|m| m.content.as_ref().map(|c| c.as_text()))
                .collect::<Vec<_>>()
                .join("\n");
            self.captured.lock().expect("prompt capture").push(prompt);
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let reply = self
                .replies
                .get(n)
                .or_else(|| self.replies.last())
                .cloned()
                .unwrap_or_default();
            Ok(ChatCompletion {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(reply)),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
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
            reasoning_content: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "UNIQUE_KEYWORD_A long initial request {}",
                "x".repeat(900)
            ))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                reasoning_content: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(format!(
                    "tool result {}",
                    "z".repeat(200)
                ))),
                tool_calls: None,
                tool_call_id: Some(format!("call_{i}")),
                reasoning_content: None,
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "UNIQUE_KEYWORD_B follow-up request".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("old request".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("old reply".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("latest request".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
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

        // Second apply replaces the one structured document (#384).
        cm.apply_summary_layer("layer two content", 2)
            .await
            .unwrap();
        let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
        assert!(
            summary_text.contains("layer two content"),
            "replacement document injected: {summary_text}"
        );
        assert!(
            !summary_text.contains("layer one content"),
            "previous layer must not stay concatenated: {summary_text}"
        );
        assert_eq!(cm.summary.as_deref().unwrap(), "layer two content");

        // Persisted: one [SUMMARY] row, the current document.
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
        assert_eq!(summary_rows.len(), 1, "one stored summary document");
        assert!(summary_rows[0].contains("layer two content"));
        assert!(!summary_rows[0].contains("layer one content"));
    }

    #[tokio::test]
    async fn apply_summary_layer_rejects_empty() {
        let mut cm = manager(vec![ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hi".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("how are you".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
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
            reasoning_content: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "first request {}",
                "x".repeat(100)
            ))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
            reasoning_content: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("lookup result payload".to_string())),
            tool_calls: None,
            tool_call_id: Some("call_split".to_string()),
            reasoning_content: None,
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "second request {}",
                "x".repeat(100)
            ))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
            reasoning_content: None,
        }];
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "UNIQUE_KEYWORD_A long initial request {}",
                "x".repeat(900)
            ))),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                reasoning_content: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(format!(
                    "tool result {}",
                    "z".repeat(200)
                ))),
                tool_calls: None,
                tool_call_id: Some(format!("call_{i}")),
                reasoning_content: None,
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "UNIQUE_KEYWORD_B follow-up request".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
        assert!(
            texts.iter().all(|t| {
                t != crate::agent_prompt::PROMPT_HARD_CAP_MARKER
                    && !t.contains("memory compaction failed")
                    && !t.contains("hard-cap truncation")
            }),
            "summary failure must not insert the hard-cap marker"
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
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(format!(
                    "UNIQUE_KEYWORD_A old request {}",
                    "x".repeat(800)
                ))),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("old reply".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("middle message".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(
                    "UNIQUE_KEYWORD_B follow-up".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
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
    async fn continuity_light_prompt_appended_when_history_has_turns() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let cid = store
            .get_or_create_conversation_with_claim(
                "telegram",
                crate::platform::DEFAULT_BOT_ID,
                "cont_light",
                false,
            )
            .await
            .unwrap();
        store
            .save_message(
                &cid,
                &ChatMessage {
                    role: "user".into(),
                    content: Some(MessageContent::from_text("prior")),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            )
            .await
            .unwrap();
        store
            .save_message(
                &cid,
                &ChatMessage {
                    role: "assistant".into(),
                    content: Some(MessageContent::from_text("ack")),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            )
            .await
            .unwrap();

        let cm = ConversationManager::new(
            &store,
            "telegram",
            crate::platform::DEFAULT_BOT_ID,
            "cont_light",
            "system base".into(),
            &skills,
            &config,
        )
        .await
        .unwrap();
        let sys = cm.messages()[0]
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        assert!(
            sys.contains(crate::cold_intro::CONTINUITY_LIGHT_PROMPT.trim()),
            "system prompt should carry continuity light prompt: {sys}"
        );
    }

    #[tokio::test]
    async fn continuity_light_prompt_absent_on_empty_history() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let cm = ConversationManager::new(
            &store,
            "telegram",
            crate::platform::DEFAULT_BOT_ID,
            "cont_empty",
            "system base".into(),
            &skills,
            &config,
        )
        .await
        .unwrap();
        let sys = cm.messages()[0]
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        assert!(
            !sys.contains("Continuity note:"),
            "empty history must not inject continuity note: {sys}"
        );
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
            device_id: None,
            device_room: None,
            device_fw: None,
            voice_origin: false,
            voice_spoken_lang: None,
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

    /// ADR-0048: full composed reply-to user text persists on the existing
    /// history path (no new schema). Continuity paths untouched.
    #[tokio::test]
    async fn add_incoming_persists_composed_reply_to_user_text() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let mut cm = ConversationManager::new(
            &store,
            "telegram",
            crate::platform::DEFAULT_BOT_ID,
            "persist_reply_to",
            "sys".to_string(),
            &skills,
            &config,
        )
        .await
        .unwrap();

        let composed = crate::platform::telegram_reply_to::compose_reply_to_user_text(
            "bot",
            42,
            &crate::platform::telegram_reply_to::ReplyQuote::Text("prior".into()),
            "and now?",
        );
        let msg = IncomingMessage {
            platform: "telegram".to_string(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            user_id: "persist_reply_to".to_string(),
            chat_id: "1".to_string(),
            user_name: "tester".to_string(),
            text: composed.clone(),
            attachments: vec![],
            schedule_id: None,
            device_id: None,
            device_room: None,
            device_fw: None,
            voice_origin: false,
            voice_spoken_lang: None,
        };

        let (combined, _) = cm.add_incoming(&msg, &config, false).await.unwrap();
        assert_eq!(combined, composed);

        let conv = store
            .get_or_create_conversation(
                "telegram",
                crate::platform::DEFAULT_BOT_ID,
                "persist_reply_to",
            )
            .await
            .unwrap();
        let rows = store.load_messages(&conv).await.unwrap();
        let user_rows: Vec<_> = rows.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_rows.len(), 1);
        assert_eq!(
            user_rows[0].content.as_ref().unwrap().as_text(),
            composed.as_str(),
            "SQLite must store the full composed reply-to text"
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
            device_id: None,
            device_room: None,
            device_fw: None,
            voice_origin: false,
            voice_spoken_lang: None,
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

    #[tokio::test]
    async fn interactive_manager_omits_schedule_segment_from_prompt() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let cid = store
            .get_or_create_conversation("telegram", "default", "sched_prompt")
            .await
            .unwrap();
        store
            .save_message(
                &cid,
                &ChatMessage {
                    role: "user".into(),
                    content: Some(MessageContent::from_text("earlier chat")),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            )
            .await
            .unwrap();
        store
            .save_message(
                &cid,
                &ChatMessage {
                    role: "assistant".into(),
                    content: Some(MessageContent::from_text(
                        "[schedule:sched-1]\ninbox is quiet",
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            )
            .await
            .unwrap();

        let cm = ConversationManager::new(
            &store,
            "telegram",
            "default",
            "sched_prompt",
            "system base".into(),
            &skills,
            &config,
        )
        .await
        .unwrap();
        let blob = cm
            .messages()
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("earlier chat"), "{blob}");
        assert!(
            !blob.contains("[schedule:sched-1]") && !blob.contains("inbox is quiet"),
            "interactive prompt must omit the schedule segment: {blob}"
        );
    }

    fn sample_handoff(goal: &str) -> String {
        format!(
            "## Goal\n{goal}\n\
             ## Constraints & Preferences\nNone\n\
             ## Progress\n### Done\nNone\n### In Progress\nNone\n### Blocked\nNone\n\
             ## Key Decisions\nNone\n\
             ## Relevant Files\nNone\n\
             ## Next Steps\nNone\n\
             ## Critical Context\nNone\n"
        )
    }

    fn assert_handoff_headings_once(text: &str) {
        let mut cursor = 0;
        for heading in [
            "## Goal",
            "## Constraints & Preferences",
            "## Progress",
            "## Key Decisions",
            "## Relevant Files",
            "## Next Steps",
            "## Critical Context",
        ] {
            let found = text[cursor..]
                .find(heading)
                .unwrap_or_else(|| panic!("missing {heading} in order:\n{text}"));
            assert_eq!(
                text.matches(heading).count(),
                1,
                "{heading} must appear once:\n{text}"
            );
            cursor += found + heading.len();
        }
        for bucket in ["### Done", "### In Progress", "### Blocked"] {
            assert!(text.contains(bucket), "missing {bucket}:\n{text}");
        }
    }

    fn text_msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Old user + tool_calls-only assistant + bulky tool result, then two
    /// protected user turns. Window == token estimate so Tier 3 fires.
    fn handoff_compact_history() -> Vec<ChatMessage> {
        let mut messages = vec![text_msg("system", "system prompt")];
        messages.push(text_msg(
            "user",
            &format!(
                "old request msg_opaque_42 https://github.com/chinkan/RustFox/issues/384 {}",
                "x".repeat(900)
            ),
        ));
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![crate::llm::ToolCall {
                id: "call_opaque_9f3a".to_string(),
                call_type: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"src/conversation.rs"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(format!(
                "head-ok {}BULKY_TAIL_MARKER",
                "Q".repeat(400)
            ))),
            tool_calls: None,
            tool_call_id: Some("call_opaque_9f3a".to_string()),
            reasoning_content: None,
        });
        messages.push(text_msg("user", "middle turn stays protected"));
        messages.push(text_msg("user", "UNIQUE_KEYWORD_B follow-up"));
        messages
    }

    fn captured_user_prompt(captured: &Arc<std::sync::Mutex<Vec<String>>>) -> String {
        captured.lock().expect("prompt capture").join("\n")
    }

    #[tokio::test]
    async fn first_compact_stores_seven_section_handoff_and_replaces_blind_extend_prompt() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handoff = sample_handoff("Ship the structured handoff");
        let llm =
            StubSummaryProvider::scripted(vec![handoff.clone()], Arc::clone(&captured)).into_llm();
        let mut cm = manager(handoff_compact_history());
        let tokens = crate::agent_prompt::estimate_tokens(&cm.messages);
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());
        let stored = cm.summary.as_deref().expect("summary stored");
        assert_handoff_headings_once(stored);
        assert!(stored.contains("Ship the structured handoff"));

        let prompt = captured_user_prompt(&captured);
        assert!(prompt.contains("## Constraints & Preferences"), "{prompt}");
        assert!(prompt.contains("MUST PRESERVE"), "{prompt}");
        assert!(prompt.contains("in place"), "{prompt}");
        assert!(prompt.contains("1500"), "{prompt}");
        assert!(prompt.contains("3000"), "{prompt}");
        assert!(
            !prompt.contains("at most 300 words"),
            "blind extend prompt must be gone: {prompt}"
        );
    }

    #[tokio::test]
    async fn extend_updates_handoff_sections_in_place() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let next = sample_handoff("SECOND_GOAL_TOKEN");
        let llm = StubSummaryProvider::scripted(vec![next], Arc::clone(&captured)).into_llm();
        let mut cm = manager(handoff_compact_history());
        cm.summary = Some(sample_handoff("FIRST_GOAL_TOKEN"));
        let tokens = crate::agent_prompt::estimate_tokens(&cm.messages);
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());
        let stored = cm.summary.as_deref().expect("summary stored");
        assert_handoff_headings_once(stored);
        assert!(stored.contains("SECOND_GOAL_TOKEN"), "{stored}");
        assert!(
            !stored.contains("FIRST_GOAL_TOKEN"),
            "extend must replace the document, not concatenate layers: {stored}"
        );
        let prompt = captured_user_prompt(&captured);
        assert!(
            prompt.contains("FIRST_GOAL_TOKEN"),
            "extend prompt includes the previous handoff: {prompt}"
        );
    }

    #[tokio::test]
    async fn opaque_ids_survive_paraphrase_prone_extend() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut previous = sample_handoff("keep going");
        previous = previous.replace(
            "## Critical Context\nNone",
            "## Critical Context\n\
             src/conversation.rs\n\
             https://github.com/chinkan/RustFox/pull/327\n\
             #384\n\
             57146ed9dc9c2ef47c41931a0707cb8cd7410bf9\n\
             call_opaque_9f3a",
        );
        let llm = StubSummaryProvider::scripted(
            vec![sample_handoff("paraphrased the files and the ticket")],
            Arc::clone(&captured),
        )
        .into_llm();
        let mut cm = manager(handoff_compact_history());
        cm.summary = Some(previous);
        let tokens = crate::agent_prompt::estimate_tokens(&cm.messages);
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());
        let stored = cm.summary.as_deref().expect("summary stored");
        for id in [
            "src/conversation.rs",
            "https://github.com/chinkan/RustFox/pull/327",
            "#384",
            "57146ed9dc9c2ef47c41931a0707cb8cd7410bf9",
            "call_opaque_9f3a",
            "msg_opaque_42",
            "https://github.com/chinkan/RustFox/issues/384",
        ] {
            assert!(stored.contains(id), "opaque id dropped: {id}\n{stored}");
        }
    }

    #[tokio::test]
    async fn tool_calls_only_assistant_contributes_name_and_arguments() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let llm = StubSummaryProvider::scripted(
            vec![sample_handoff("noted the read")],
            Arc::clone(&captured),
        )
        .into_llm();
        let mut cm = manager(handoff_compact_history());
        let tokens = crate::agent_prompt::estimate_tokens(&cm.messages);
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());
        let prompt = captured_user_prompt(&captured);
        assert!(prompt.contains("read_file"), "tool name missing: {prompt}");
        assert!(
            prompt.contains(r#"{"path":"src/conversation.rs"}"#),
            "tool arguments missing: {prompt}"
        );
        assert!(
            prompt.contains("call_opaque_9f3a"),
            "tool_call id missing: {prompt}"
        );
    }

    #[tokio::test]
    async fn compact_prunes_tool_results_before_extend_prompt() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let llm = StubSummaryProvider::scripted(
            vec![sample_handoff("pruned then extended")],
            Arc::clone(&captured),
        )
        .into_llm();
        let mut cm = manager(handoff_compact_history());
        let tokens = crate::agent_prompt::estimate_tokens(&cm.messages);
        let ctx = CompactionContext {
            llm: &llm,
            context_window: tokens,
            compaction_model: None,
            user_model_path: None,
        };

        assert!(cm.compact_messages(&ctx).await.unwrap());
        let prompt = captured_user_prompt(&captured);
        assert!(
            prompt.contains("[tool result pruned]"),
            "summarizer input must show pruned tool results: {prompt}"
        );
        assert!(
            !prompt.contains("BULKY_TAIL_MARKER"),
            "unpruned tool body reached the extend prompt: {prompt}"
        );
        assert!(
            prompt.contains("## Constraints & Preferences"),
            "prune runs on the transcript the extend prompt sends: {prompt}"
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
            "protected tail stays verbatim"
        );
    }

    #[tokio::test]
    async fn load_uses_latest_structured_handoff_not_layer_concat() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let cid = store
            .get_or_create_conversation("test", crate::platform::DEFAULT_BOT_ID, "handoff_load")
            .await
            .unwrap();
        for body in [
            "[SUMMARY]\nlegacy layer one",
            &format!("[SUMMARY]\n{}", sample_handoff("ONLY_HANDOFF_GOAL")),
        ] {
            store
                .save_message(
                    &cid,
                    &ChatMessage {
                        role: "system".into(),
                        content: Some(MessageContent::from_text(body)),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                )
                .await
                .unwrap();
        }

        let cm = ConversationManager::new(
            &store,
            "test",
            crate::platform::DEFAULT_BOT_ID,
            "handoff_load",
            "system base".into(),
            &skills,
            &config,
        )
        .await
        .unwrap();
        let summary = cm.summary.as_deref().expect("handoff reloaded");
        assert!(summary.contains("ONLY_HANDOFF_GOAL"), "{summary}");
        assert!(
            !summary.contains("legacy layer one"),
            "legacy layer must not concatenate onto the structured document: {summary}"
        );
        assert_handoff_headings_once(summary);
    }

    #[tokio::test]
    async fn load_still_joins_legacy_summary_layers_without_a_handoff() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let config = persist_test_config();
        let skills = crate::skills::SkillRegistry::new();
        let cid = store
            .get_or_create_conversation("test", crate::platform::DEFAULT_BOT_ID, "legacy_load")
            .await
            .unwrap();
        for body in ["[SUMMARY]\nlegacy one", "[SUMMARY]\nlegacy two"] {
            store
                .save_message(
                    &cid,
                    &ChatMessage {
                        role: "system".into(),
                        content: Some(MessageContent::from_text(body)),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                )
                .await
                .unwrap();
        }

        let cm = ConversationManager::new(
            &store,
            "test",
            crate::platform::DEFAULT_BOT_ID,
            "legacy_load",
            "system base".into(),
            &skills,
            &config,
        )
        .await
        .unwrap();
        let summary = cm.summary.as_deref().expect("legacy summaries");
        assert!(
            summary.contains("legacy one") && summary.contains("legacy two"),
            "{summary}"
        );
    }
}
