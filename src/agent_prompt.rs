//! In-memory prompt preparation and compaction for LLM context management.
//!
//! Tiers 1-2 (sync, 0 LLM cost):
//!   Tier 1: observation_mask — replace old tool results with placeholder,
//!           neutralize old [RustFox compacted:...] markers
//!   Tier 2: collapse_context — remove oldest tool groups entirely,
//!           insert boundary marker
//!
//! Tiers 3-4 live in agent.rs (async, require LLM call).

use crate::llm::{ChatMessage, MessageContent};

/// A single compressed message, rendered as a structured marker line.
///
/// Part of the public compaction API: the unified `compact_messages` pipeline
/// emits one `CompressedMessage` per summarized message so downstream readers
/// (and the LLM itself) can tell what kind of content was compressed.
#[derive(Debug, Clone)]
pub struct CompressedMessage {
    pub role: String,
    /// "tool_call" | "tool_result" | "user" | "assistant" | "system"
    pub original_type: String,
    pub summary: String,
    pub key_data: Option<serde_json::Value>,
}

impl CompressedMessage {
    /// Render this message as a structured marker line. Tool messages embed
    /// `key_data` (e.g. counts/status codes) when present.
    ///
    /// Formats:
    /// - `[Tool: NAME] description | result: SUMMARY | status: ok|error`
    /// - `[User] TOPIC: SUMMARY`
    /// - `[Assistant] ACTION: DECISION_SUMMARY`
    /// - `[System] EVENT: NOTABLE_INFO`
    pub fn to_marker(&self) -> String {
        let summary = self.summary.trim();
        match self.original_type.as_str() {
            "tool_call" => {
                let name = self
                    .key_data
                    .as_ref()
                    .and_then(|k| k.get("name"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!summary.is_empty()).then_some(summary))
                    .unwrap_or("unknown");
                let args = self
                    .key_data
                    .as_ref()
                    .and_then(|k| k.get("args"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                format!("[Tool: {name}] {args}").trim_end().to_string()
            }
            "tool_result" => {
                let name = self
                    .key_data
                    .as_ref()
                    .and_then(|k| k.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let description = self
                    .key_data
                    .as_ref()
                    .and_then(|k| k.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let status = if let Some(s) = self
                    .key_data
                    .as_ref()
                    .and_then(|k| k.get("status"))
                    .and_then(|v| v.as_str())
                {
                    s.to_string()
                } else if summary.contains("Error") || summary.contains("error") {
                    "error".to_string()
                } else {
                    "ok".to_string()
                };
                let mut marker =
                    format!("[Tool: {name}] {description} | result: {summary} | status: {status}");
                if let Some(kd) = &self.key_data {
                    if let Ok(extra) = serde_json::to_string(kd) {
                        marker.push_str(&format!(" | {extra}"));
                    }
                }
                marker
            }
            "user" => format!("[User] TOPIC: {summary}"),
            "assistant" => format!("[Assistant] ACTION: {summary}"),
            "system" => format!("[System] EVENT: {summary}"),
            other => format!("[{other}] {summary}"),
        }
    }
}

/// Percentage of context_window that triggers Tier 1 (observation masking).
const OBSERVATION_MASK_PCT: f64 = 0.70;
/// Percentage that triggers Tier 2 (context collapse).
const COLLAPSE_PCT: f64 = 0.75;
/// Utilization fraction (estimated tokens / context_window) at which the
/// unified compaction pipeline begins summarizing the oldest messages.
pub const COMPACT_TRIGGER_PCT: f64 = 0.85;
/// Minimum turns between Tier 3/4 compactions.
pub const PRESERVED_TOOL_GROUPS: usize = 2;
/// Absolute hard cap safety net (applied regardless of context_window).
const PROMPT_HARD_CAP_BYTES: usize = 100_000;
/// Minimum message count to consider Tier 3 compact.
const COMPACT_MIN_MESSAGE_COUNT: usize = 15;

/// Keep for backward compat — `is_compacted_regurgitation` references this.
pub const COMPACTION_MARKER_PREFIX: &str = "[RustFox compacted:";

/// Statistics about prompt preparation and compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptStats {
    pub original_message_count: usize,
    pub prepared_message_count: usize,
    pub original_prompt_chars: usize,
    pub prepared_prompt_chars: usize,
    pub compaction_applied: bool,
}

/// A prepared prompt ready for LLM consumption.
#[derive(Debug, Clone)]
pub struct PreparedPrompt {
    pub messages: Vec<ChatMessage>,
    pub stats: PromptStats,
}

/// Estimate the byte size of a prompt from its messages.
pub fn estimate_prompt_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content_chars = msg.content.as_ref().map(|c| c.as_text().len()).unwrap_or(0);
            let tool_args_chars = msg
                .tool_calls
                .as_ref()
                .map(|calls| {
                    calls
                        .iter()
                        .map(|call| call.function.arguments.len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            content_chars + tool_args_chars
        })
        .sum()
}

/// Estimate token count from messages, CJK-aware.
///
/// CJK characters cost ~1 token each; Latin/other characters ~1/4 token.
/// Tool-call arguments count toward the total. This is the single token
/// estimate used for the compaction trigger (ADR 0003 Q3).
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let mut latin_chars = 0usize;
    let mut cjk_chars = 0usize;
    for msg in messages {
        if let Some(content) = msg.content.as_ref() {
            count_chars(&content.as_text(), &mut latin_chars, &mut cjk_chars);
        }
        if let Some(calls) = msg.tool_calls.as_ref() {
            for call in calls {
                count_chars(&call.function.arguments, &mut latin_chars, &mut cjk_chars);
            }
        }
    }
    latin_chars / 4 + cjk_chars
}

fn count_chars(text: &str, latin: &mut usize, cjk: &mut usize) {
    for ch in text.chars() {
        if is_cjk(ch) {
            *cjk += 1;
        } else {
            *latin += 1;
        }
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2EFF | // CJK Radicals Supplement
        0x3000..=0x303F | // CJK punctuation
        0x3040..=0x30FF | // Hiragana + Katakana
        0x3400..=0x4DBF | // CJK Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0xAC00..=0xD7AF   // Hangul
    )
}

/// First index of the protected tail — messages from this index on are kept
/// verbatim (ADR 0003 Q4): the last two user turns plus the active exchange,
/// capped at 20% of `window` tokens. The boundary never splits a
/// [tool_call, tool_result] pair. Returns 0 when nothing can be protected
/// (no user messages) — callers treat 0 as "do not compact".
pub fn protected_tail_start(messages: &[ChatMessage], window: usize) -> usize {
    let user_idx: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if user_idx.is_empty() {
        return 0;
    }
    let last_user = *user_idx.last().expect("non-empty");
    let base = if user_idx.len() >= 2 {
        user_idx[user_idx.len() - 2]
    } else {
        user_idx[0]
    };
    if base == 0 {
        return 0; // would protect the system message — nothing to compact
    }

    let mut start = base;
    let cap_tokens = window / 5;
    while start < last_user && estimate_tokens(&messages[start..]) > cap_tokens {
        start += 1;
    }

    // Never split a [tool_call, tool_result] pair.
    loop {
        let mut changed = false;
        let mut i = start;
        while i < messages.len() {
            let msg = &messages[i];
            if msg.has_tool_calls() {
                let call_ids: Vec<&str> = msg
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|c| c.id.as_str())
                    .collect();
                let mut last_result = i;
                for (offset, m) in messages.iter().skip(i + 1).enumerate() {
                    let j = i + 1 + offset;
                    if m.role == "tool"
                        && m.tool_call_id
                            .as_deref()
                            .is_some_and(|id| call_ids.contains(&id))
                    {
                        last_result = j;
                    } else if m.role != "tool" {
                        break;
                    }
                }
                if last_result + 1 > start {
                    start = last_result + 1;
                    changed = true;
                }
            }
            i += 1;
        }
        if start > 0 {
            let prev = &messages[start - 1];
            if prev.role == "tool" {
                let call_in_tail = messages[start..].iter().any(|m| {
                    m.has_tool_calls()
                        && m.tool_calls.as_ref().is_some_and(|calls| {
                            calls
                                .iter()
                                .any(|c| Some(c.id.as_str()) == prev.tool_call_id.as_deref())
                        })
                });
                if call_in_tail {
                    start -= 1;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    start
}

/// Create a recovery nudge message appropriate for the conversation context.
pub fn recovery_nudge_for(messages: &[ChatMessage]) -> ChatMessage {
    let previous_is_tool = messages.last().is_some_and(|msg| msg.role == "tool");

    let content = if previous_is_tool {
        "Continue from the tool result above. Read the ## STATE block in the compact summary for context. Either call the next required tool or provide a final answer.".to_string()
    } else {
        "Continue from the user's request above. Read the ## STATE block in the compact summary for context. Either call the next required tool or provide a final answer.".to_string()
    };

    ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text(content)),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Tier 1: Observation Masking
///
/// Replace old tool result content with a masked placeholder. The LLM
/// knows it made the call but the bulky payload is gone. Also neutralize
/// any old [RustFox compacted:...] markers in tool call arguments.
///
/// Trigger: estimated bytes > context_window * OBSERVATION_MASK_PCT
/// Applies to: tool results older than PRESERVED_TOOL_GROUPS.
pub fn observation_mask(messages: &[ChatMessage], context_window: usize) -> Vec<ChatMessage> {
    let threshold = (context_window as f64 * OBSERVATION_MASK_PCT) as usize;
    if estimate_prompt_bytes(messages) <= threshold {
        return messages.to_vec();
    }

    // Identify tool groups
    let tool_groups = find_tool_groups(messages);
    let preserved_start = tool_groups.len().saturating_sub(PRESERVED_TOOL_GROUPS);

    let mut preserved_indices = std::collections::HashSet::new();
    for group in tool_groups.iter().skip(preserved_start) {
        preserved_indices.insert(group.assistant_idx);
        for &ti in &group.tool_result_indices {
            preserved_indices.insert(ti);
        }
    }

    messages
        .iter()
        .enumerate()
        .map(|(idx, msg)| {
            // System and user: pass through
            if msg.role == "system" || msg.role == "user" {
                return msg.clone();
            }

            // Preserved indices: pass through
            if preserved_indices.contains(&idx) {
                return msg.clone();
            }

            // Neutralize old compaction markers in tool call arguments
            if msg.role == "assistant" && msg.has_tool_calls() {
                let mut compacted = msg.clone();
                if let Some(calls) = &mut compacted.tool_calls {
                    for call in calls.iter_mut() {
                        if call.function.arguments.contains(COMPACTION_MARKER_PREFIX) {
                            call.function.arguments = call
                                .function
                                .arguments
                                .replace(COMPACTION_MARKER_PREFIX, "[compacted");
                        }
                    }
                }
                return compacted;
            }

            // Mask tool results
            if msg.role == "tool" {
                let mut masked = msg.clone();
                masked.content = Some(MessageContent::Text(
                    "[previous tool result — masked]".to_string(),
                ));
                return masked;
            }

            msg.clone()
        })
        .collect()
}

/// Tier 2: Context Collapse
///
/// Remove the oldest 50% of non-preserved tool groups (assistant + tool
/// messages). Insert a structural boundary marker at the collapse point
/// so the LLM doesn't perceive a confusing jump.
///
/// Trigger: still > context_window * COLLAPSE_PCT after Tier 1.
pub fn collapse_context(messages: &[ChatMessage], context_window: usize) -> Vec<ChatMessage> {
    let threshold = (context_window as f64 * COLLAPSE_PCT) as usize;
    if estimate_prompt_bytes(messages) <= threshold {
        return messages.to_vec();
    }

    let tool_groups = find_tool_groups(messages);
    let non_preserved_count = tool_groups.len().saturating_sub(PRESERVED_TOOL_GROUPS);
    // Remove oldest 50% of non-preserved groups only
    let keep_start = non_preserved_count / 2;

    let mut keep_indices = std::collections::HashSet::new();

    for (i, group) in tool_groups.iter().enumerate() {
        if i >= keep_start {
            keep_indices.insert(group.assistant_idx);
            for &ti in &group.tool_result_indices {
                keep_indices.insert(ti);
            }
        }
    }

    // Always keep system + user messages
    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "system" || msg.role == "user" {
            keep_indices.insert(idx);
        }
    }

    // Keep messages after the last kept group
    if let Some(last_keep) = tool_groups
        .iter()
        .rev()
        .find(|g| keep_indices.contains(&g.assistant_idx))
    {
        for idx in (last_keep.assistant_idx + 1)..messages.len() {
            keep_indices.insert(idx);
        }
    }

    // Build result: filter to kept indices in order, inserting boundary marker
    let mut result: Vec<ChatMessage> = Vec::new();
    let mut inserted_boundary = false;

    // Find the first index that is kept after the removal zone
    let boundary_group_idx = tool_groups
        .iter()
        .position(|g| keep_indices.contains(&g.assistant_idx));

    for (idx, msg) in messages.iter().enumerate() {
        if keep_indices.contains(&idx) {
            if !inserted_boundary {
                if let Some(bg_idx) = boundary_group_idx {
                    if let Some(group) = tool_groups.iter().find(|g| g.assistant_idx == idx) {
                        if tool_groups
                            .iter()
                            .position(|g| g.assistant_idx == group.assistant_idx)
                            == Some(bg_idx)
                        {
                            result.push(ChatMessage {
                                role: "system".to_string(),
                                content: Some(MessageContent::Text(
                                    "★ earlier conversation collapsed ★".to_string(),
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                            });
                            inserted_boundary = true;
                        }
                    }
                } else {
                    inserted_boundary = true;
                }
            }
            result.push(msg.clone());
        }
    }

    // If boundary still not inserted and messages were removed
    if !inserted_boundary && messages.len() > result.len() {
        let boundary_msg = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(
                "★ earlier conversation collapsed ★".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
        };
        if result.is_empty() {
            result.push(boundary_msg);
        } else {
            result.insert(1, boundary_msg);
        }
    }

    result
}

/// Helper: identify tool groups in a message list.
pub struct ToolGroup {
    pub assistant_idx: usize,
    pub tool_result_indices: Vec<usize>,
}

pub fn find_tool_groups(messages: &[ChatMessage]) -> Vec<ToolGroup> {
    let mut groups: Vec<ToolGroup> = Vec::new();
    let mut current: Option<ToolGroup> = None;

    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "assistant" && msg.has_tool_calls() {
            if let Some(g) = current.take() {
                groups.push(g);
            }
            current = Some(ToolGroup {
                assistant_idx: idx,
                tool_result_indices: Vec::new(),
            });
        } else if msg.role == "tool" {
            if let Some(ref mut g) = current {
                g.tool_result_indices.push(idx);
            }
        } else if current.is_some() {
            if let Some(g) = current.take() {
                groups.push(g);
            }
        }
    }
    if let Some(g) = current {
        groups.push(g);
    }
    groups
}

/// Prepare messages for LLM by applying compaction if needed.
///
/// Sync function: applies Tiers 1-2 only.
/// - Tier 1: observation masking (replace old tool results with placeholder)
/// - Tier 2: context collapse (remove oldest tool groups, insert boundary marker)
///
/// If still over PROMPT_HARD_CAP_BYTES after both tiers, reduce preserved
/// groups to 1 as a last resort.
pub fn prepare_messages_for_llm(messages: &[ChatMessage], context_window: usize) -> PreparedPrompt {
    let original_message_count = messages.len();
    let original_prompt_bytes = estimate_prompt_bytes(messages);

    let obs_threshold = (context_window as f64 * OBSERVATION_MASK_PCT) as usize;
    let coll_threshold = (context_window as f64 * COLLAPSE_PCT) as usize;

    let should_compact = original_prompt_bytes > obs_threshold
        && original_message_count > compact_min_message_count(context_window);

    let prepared_messages = if should_compact {
        // Tier 1: observation masking
        let after_tier1 = observation_mask(messages, context_window);

        // Tier 2: context collapse if still over COLLAPSE_PCT
        let after_tier2 = if estimate_prompt_bytes(&after_tier1) > coll_threshold {
            collapse_context(&after_tier1, context_window)
        } else {
            after_tier1
        };

        // Safety net (ADR 0003 Q7 step 3): if still over the hard cap after
        // Tiers 1-2, drop the OLDEST non-protected traffic only — the last two
        // user turns + active exchange always survive.
        if estimate_prompt_bytes(&after_tier2) > PROMPT_HARD_CAP_BYTES {
            let tail_start = protected_tail_start(&after_tier2, context_window).max(1);
            let mut hard_cap_messages: Vec<ChatMessage> =
                Vec::with_capacity(after_tier2.len().saturating_sub(tail_start) + 2);
            if let Some(system) = after_tier2.first() {
                hard_cap_messages.push(system.clone());
            }
            if tail_start < after_tier2.len() {
                if tail_start > 1 {
                    hard_cap_messages.push(ChatMessage {
                        role: "system".to_string(),
                        content: Some(MessageContent::Text(
                            "★ earlier conversation dropped — memory compaction failed ★"
                                .to_string(),
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                hard_cap_messages.extend(after_tier2.iter().skip(tail_start).cloned());
            }
            hard_cap_messages
        } else {
            after_tier2
        }
    } else {
        messages.to_vec()
    };

    let prepared_message_count = prepared_messages.len();
    let prepared_prompt_bytes = estimate_prompt_bytes(&prepared_messages);

    PreparedPrompt {
        messages: prepared_messages,
        stats: PromptStats {
            original_message_count,
            prepared_message_count,
            original_prompt_chars: original_prompt_bytes,
            prepared_prompt_chars: prepared_prompt_bytes,
            compaction_applied: should_compact,
        },
    }
}

/// Returns minimum message count to consider compaction, scaled by
/// context_window size. Larger windows need more messages to justify
/// the LLM cost of Tier 3.
fn compact_min_message_count(context_window: usize) -> usize {
    // Base: 15 messages. Scale linearly: 15 for 128K, 30 for 1M, etc.
    let base = COMPACT_MIN_MESSAGE_COUNT;
    let window_k = context_window / 512_000; // 512K = 128K tokens
    base + (base / 2) * window_k
}

/// Create the summary prompt content used for Tier 3 and Tier 4 LLM calls.
/// Returns a system-role message instructing the LLM to summarize.
pub fn build_compact_summary_prompt() -> ChatMessage {
    let prompt_text = vec![
        "You are producing a compact state summary of the conversation below.",
        "",
        "OUTPUT FORMAT:",
        "",
        "## STATE",
        "```yaml",
        "stage: <problem_definition | investigation | implementation | review | complete>",
        "decisions:",
        "  - <decision made>",
        "pending:",
        "  - <still to do>",
        "last_action: <tool call name + brief result>",
        "last_action_result: <what happened>",
        "conversation_phase: <summary of current focus>",
        "```",
        "",
        "## CONTEXT",
        "- <bullet point of key fact, file, error, or finding>",
        "- <bullet point>",
        "- <bullet point>",
        "",
        "CRITICAL RULES:",
        "- State must be precise enough for the LLM to continue without re-reading history",
        "- Include ALL pending items the user explicitly requested",
        "- Include ALL error messages and their resolutions",
        "- Be specific with file paths and tool names",
        "- Do NOT call any tools. Respond with text only.",
    ]
    .join("\n");

    ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text(prompt_text)),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Build the compact boundary marker message (pre-compact stats).
pub fn build_compact_boundary_marker(original_count: usize, compacted_count: usize) -> ChatMessage {
    ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text(format!(
            "★ COMPACT SUMMARY — previous {} messages → YAML state + narrative ({} messages) ★",
            original_count, compacted_count,
        ))),
        tool_calls: None,
        tool_call_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    #[test]
    fn estimate_prompt_bytes_counts_content_and_tool_arguments() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hello".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "test_tool".to_string(),
                        arguments: r#"{"arg":"value"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("result".to_string())),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
        ];

        let total = estimate_prompt_bytes(&messages);
        assert_eq!(total, 5 + 2 + 15 + 6);
    }

    #[test]
    fn recovery_nudge_mentions_tool_result_when_previous_message_is_tool() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hello".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("result".to_string())),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
        ];

        let nudge = recovery_nudge_for(&messages);
        assert_eq!(nudge.role, "system");
        assert!(nudge
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("tool result above"));
    }

    #[test]
    fn recovery_nudge_mentions_user_request_when_previous_message_is_user() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Hello".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }];

        let nudge = recovery_nudge_for(&messages);
        assert_eq!(nudge.role, "system");
        assert!(nudge
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("user's request above"));
    }

    #[test]
    fn prepare_messages_skips_compaction_for_short_prompts() {
        let messages: Vec<ChatMessage> = (0..5)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(format!("message {}", i))),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();

        let result = prepare_messages_for_llm(&messages, 512_000);
        assert!(!result.stats.compaction_applied);
        assert_eq!(result.messages.len(), messages.len());
    }

    #[test]
    fn observation_mask_replaces_old_tool_results_and_keeps_recent() {
        let ctx = 1000;
        let mut messages = Vec::new();

        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("hello".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });

        let old_args = "x".repeat(500);
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "old_tool".to_string(),
                    arguments: old_args,
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("y".repeat(800))),
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
        });

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_2".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "recent_tool".to_string(),
                    arguments: "test".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("recent result".to_string())),
            tool_calls: None,
            tool_call_id: Some("call_2".to_string()),
        });

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_3".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "latest".to_string(),
                    arguments: "tiny".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("latest result".to_string())),
            tool_calls: None,
            tool_call_id: Some("call_3".to_string()),
        });

        let result = observation_mask(&messages, ctx);

        assert_eq!(result[0].role, "system");
        assert_eq!(result[1].role, "user");

        let old_content = result[3].content.as_ref().unwrap().as_text();
        assert!(
            old_content.contains("masked"),
            "old tool result should be masked: {}",
            old_content
        );

        let recent_content = result[5].content.as_ref().unwrap().as_text();
        assert_eq!(recent_content, "recent result");

        let latest_content = result[7].content.as_ref().unwrap().as_text();
        assert_eq!(latest_content, "latest result");

        assert_eq!(result.len(), messages.len());
    }

    #[test]
    fn collapse_context_removes_oldest_tool_groups_and_inserts_boundary_marker() {
        let ctx = 5000;
        let mut messages = Vec::new();

        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("user msg".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });

        for i in 0..4 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("c{}", i),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: format!("t{}", i),
                        arguments: "x".repeat(500),
                    },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("y".repeat(800))),
                tool_calls: None,
                tool_call_id: Some(format!("c{}", i)),
            });
        }

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("done".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });

        let result = collapse_context(&messages, ctx);

        assert_eq!(result[0].role, "system");
        assert_eq!(result[1].role, "user");

        let collapse_idx = result.iter().position(|m| {
            m.role == "system" && m.content.as_ref().unwrap().as_text().contains("collapsed")
        });
        assert!(
            collapse_idx.is_some(),
            "should have collapse boundary marker"
        );

        let preserved_start = collapse_idx.unwrap() + 1;
        assert!(
            preserved_start < result.len(),
            "should have messages after boundary"
        );
    }

    #[test]
    #[allow(clippy::vec_init_then_push)]
    fn find_tool_groups_detects_consecutive_tool_calls() {
        let mut messages = Vec::new();
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });
        // Group 1
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "c1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "t1".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("r1".to_string())),
            tool_calls: None,
            tool_call_id: Some("c1".to_string()),
        });
        // Group 2
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "c2".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "t2".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("r2".to_string())),
            tool_calls: None,
            tool_call_id: Some("c2".to_string()),
        });

        let groups = find_tool_groups(&messages);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].assistant_idx, 1);
        assert_eq!(groups[0].tool_result_indices, vec![2]);
        assert_eq!(groups[1].assistant_idx, 3);
        assert_eq!(groups[1].tool_result_indices, vec![4]);
    }

    #[test]
    fn prepare_messages_applies_tier2_when_tier1_not_enough() {
        let ctx = 50000;
        let mut messages = Vec::new();
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("do things".to_string())),
            tool_calls: None,
            tool_call_id: None,
        });
        for i in 0..6 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("c{}", i),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "tool".to_string(),
                        arguments: "x".repeat(8000),
                    },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("y".repeat(8000))),
                tool_calls: None,
                tool_call_id: Some(format!("c{}", i)),
            });
        }
        for i in 6..8 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("c{}", i),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "tool".to_string(),
                        arguments: "short".to_string(),
                    },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("preserved".to_string())),
                tool_calls: None,
                tool_call_id: Some(format!("c{}", i)),
            });
        }

        let result = prepare_messages_for_llm(&messages, ctx);
        assert!(result.stats.compaction_applied);
        assert!(result.messages.len() < messages.len());
        let has_boundary = result.messages.iter().any(|m| {
            m.role == "system" && m.content.as_ref().unwrap().as_text().contains("collapsed")
        });
        assert!(has_boundary);
    }

    #[test]
    fn hard_cap_fallback_drops_oldest_only_keeps_latest_user_intent() {
        let mut msgs = vec![chat_msg("system", "sys")];
        msgs.push(chat_msg("user", "request 0"));
        // 7 tool groups (16 msgs + sys + last user = 17 > compact_min_message_count(15)).
        // Preserved tool groups alone exceed PROMPT_HARD_CAP_BYTES, so the
        // obs/coll tiers cannot reduce below the cap → the hard-cap branch fires.
        for i in 0..7 {
            msgs.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("c{i}"),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "tool".to_string(),
                        arguments: "x".repeat(60_000),
                    },
                }]),
                tool_call_id: None,
            });
            msgs.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::Text("y".repeat(60_000))),
                tool_calls: None,
                tool_call_id: Some(format!("c{i}")),
            });
        }
        msgs.push(chat_msg("user", "request last"));
        let prepared = prepare_messages_for_llm(&msgs, 1_000);
        assert!(
            prepared.messages.iter().any(|m| m
                .content
                .as_ref()
                .map(|c| c.as_text() == "request last")
                .unwrap_or(false)),
            "last user turn survives the hard cap"
        );
        assert!(
            !prepared.messages.iter().any(|m| m
                .content
                .as_ref()
                .map(|c| c.as_text() == "request 0")
                .unwrap_or(false)),
            "oldest traffic dropped"
        );
    }

    #[test]
    fn compact_summary_prompt_contains_state_keywords() {
        let msg = build_compact_summary_prompt();
        let text = msg.content.as_ref().unwrap().as_text();
        assert!(text.contains("## STATE"), "Should have STATE section");
        assert!(text.contains("## CONTEXT"), "Should have CONTEXT section");
        assert!(text.contains("decisions:"), "Should have decisions field");
        assert!(text.contains("pending:"), "Should have pending field");
        assert!(
            text.contains("last_action:"),
            "Should have last_action field"
        );
    }

    #[test]
    fn compact_boundary_marker_contains_state_hint() {
        let msg = build_compact_boundary_marker(10, 1);
        let text = msg.content.as_ref().unwrap().as_text();
        assert!(text.contains("state"), "Should hint at state format");
    }

    fn chat_msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn tool_call_msg(id: &str, name: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![crate::llm::ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
        }
    }

    fn tool_result_msg(id: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text("result payload".to_string())),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
        }
    }

    #[test]
    fn protected_tail_start_keeps_last_two_user_turns() {
        let mut msgs = vec![chat_msg("system", "sys")];
        for i in 0..10 {
            msgs.push(chat_msg("user", &format!("turn {i}")));
            msgs.push(chat_msg("assistant", &format!("reply {i}")));
        }
        let start = protected_tail_start(&msgs, 1_000_000);
        let tail: Vec<&str> = msgs[start..].iter().map(|m| m.role.as_str()).collect();
        assert!(tail.contains(&"user"), "tail keeps user turns");
        assert_eq!(
            msgs[start..].iter().filter(|m| m.role == "user").count(),
            2,
            "exactly the last two user turns survive"
        );
        assert!(
            msgs[start..].iter().any(|m| {
                m.content
                    .as_ref()
                    .map(|c| c.as_text() == "turn 8")
                    .unwrap_or(false)
            }),
            "second-to-last user turn verbatim"
        );
        assert!(
            msgs[start..].iter().any(|m| {
                m.content
                    .as_ref()
                    .map(|c| c.as_text() == "turn 9")
                    .unwrap_or(false)
            }),
            "last user turn verbatim"
        );
    }

    #[test]
    fn protected_tail_start_never_splits_tool_pair() {
        // user, call, result, user — boundary must not land between call and result.
        let msgs = vec![
            chat_msg("system", "sys"),
            chat_msg("user", "old request"),
            tool_call_msg("call_a", "lookup_thing"),
            tool_result_msg("call_a"),
            chat_msg("assistant", "old answer"),
            chat_msg("user", "latest request"),
        ];
        let start = protected_tail_start(&msgs, 1_000_000);
        let tail = &msgs[start..];
        assert!(
            !(tail
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("call_a"))
                && !tail.iter().any(|m| {
                    m.has_tool_calls()
                        && m.tool_calls
                            .as_ref()
                            .is_some_and(|calls| calls.iter().any(|c| c.id == "call_a"))
                })),
            "orphaned tool result in tail"
        );
        assert!(
            !(tail.iter().any(|m| {
                m.has_tool_calls()
                    && m.tool_calls
                        .as_ref()
                        .is_some_and(|calls| calls.iter().any(|c| c.id == "call_a"))
            }) && !tail
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("call_a"))),
            "orphaned tool call in tail"
        );
    }

    #[test]
    fn protected_tail_start_caps_at_20_percent() {
        let mut msgs = vec![chat_msg("system", "sys")];
        for i in 0..8 {
            msgs.push(chat_msg("user", &format!("request {i}")));
            msgs.push(chat_msg("assistant", &"reply ".repeat(500)));
        }
        // window sized so the full tail (~4K chars) exceeds 20% of window tokens
        let window = estimate_tokens(&msgs) * 5 / 2; // tail cap = window/5 < tail tokens
        let start = protected_tail_start(&msgs, window);
        let tail_tokens = estimate_tokens(&msgs[start..]);
        assert!(
            tail_tokens <= window / 5 + estimate_tokens(&msgs[msgs.len() - 2..]),
            "tail must be capped near 20% (plus the mandatory last turn): {tail_tokens} > {}",
            window / 5
        );
        // last user turn always survives the cap
        assert!(
            msgs[start..].iter().any(|m| {
                m.content
                    .as_ref()
                    .map(|c| c.as_text().starts_with("request 7"))
                    .unwrap_or(false)
            }),
            "last user turn must never be dropped by the cap"
        );
    }

    #[test]
    fn protected_tail_start_returns_zero_without_user_messages() {
        let msgs = vec![
            chat_msg("system", "sys"),
            chat_msg("assistant", "a"),
            chat_msg("tool", "t"),
        ];
        assert_eq!(protected_tail_start(&msgs, 1_000_000), 0);
    }

    #[test]
    fn estimate_tokens_counts_cjk_and_latin() {
        use crate::llm::{ChatMessage, MessageContent};

        fn msg(text: &str) -> ChatMessage {
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(text.to_string())),
                tool_calls: None,
                tool_call_id: None,
            }
        }

        // 4 Latin chars ≈ 1 token.
        let latin = vec![msg("abcd")];
        assert_eq!(estimate_tokens(&latin), 1, "4 latin chars ≈ 1 token");

        // CJK chars cost 1 token each.
        let cjk = vec![msg("中文测试")];
        assert_eq!(estimate_tokens(&cjk), 4, "CJK chars ≈ 1 token each");

        // Mixed.
        let mixed = vec![msg("hello中文")];
        assert_eq!(estimate_tokens(&mixed), 1 + 2, "latin/4 + cjk");

        // Tool-call arguments count toward the total.
        let with_tool = vec![ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![crate::llm::ToolCall {
                id: "c1".to_string(),
                call_type: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: "search".to_string(),
                    arguments: r#"{"q":"abcd"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        }];
        assert_eq!(
            estimate_tokens(&with_tool),
            3,
            "tool args count (12 latin chars)"
        );
    }

    #[test]
    fn compressed_message_to_marker_formats() {
        let tool = CompressedMessage {
            role: "tool".to_string(),
            original_type: "tool_result".to_string(),
            summary: "wrote config file".to_string(),
            key_data: Some(serde_json::json!({"name": "write_file", "bytes": 42})),
        };
        let tool_marker = tool.to_marker();
        assert!(
            tool_marker.starts_with("[Tool: write_file]"),
            "{}",
            tool_marker
        );
        assert!(tool_marker.contains("| status: ok"), "{}", tool_marker);
        assert!(tool_marker.contains("\"bytes\":42"), "{}", tool_marker);

        let err_tool = CompressedMessage {
            role: "tool".to_string(),
            original_type: "tool_result".to_string(),
            summary: "Error: permission denied".to_string(),
            key_data: None,
        };
        assert!(
            err_tool.to_marker().contains("| status: error"),
            "{}",
            err_tool.to_marker()
        );

        let user = CompressedMessage {
            role: "user".to_string(),
            original_type: "user".to_string(),
            summary: "fix the bug".to_string(),
            key_data: None,
        };
        assert_eq!(user.to_marker(), "[User] TOPIC: fix the bug");

        let assistant = CompressedMessage {
            role: "assistant".to_string(),
            original_type: "assistant".to_string(),
            summary: "decided to use tokio".to_string(),
            key_data: None,
        };
        assert_eq!(
            assistant.to_marker(),
            "[Assistant] ACTION: decided to use tokio"
        );

        let system = CompressedMessage {
            role: "system".to_string(),
            original_type: "system".to_string(),
            summary: "sandbox dir set".to_string(),
            key_data: None,
        };
        assert_eq!(system.to_marker(), "[System] EVENT: sandbox dir set");
    }
}
