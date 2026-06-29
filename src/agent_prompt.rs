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

/// Percentage of context_window that triggers Tier 1 (observation masking).
const OBSERVATION_MASK_PCT: f64 = 0.20;
/// Percentage that triggers Tier 2 (context collapse).
const COLLAPSE_PCT: f64 = 0.60;
/// Percentage that triggers Tier 3 (auto compact).
pub const COMPACT_PCT: f64 = 0.85;
/// Percentage that triggers Tier 4 (reactive compact).
#[allow(dead_code)]
pub(crate) const REACTIVE_PCT: f64 = 0.95;
/// Minimum turns between Tier 3/4 compactions.
pub const COMPACT_TURN_GAP: usize = 5;
/// Number of most recent tool groups to preserve verbatim.
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

/// Per-conversation compaction metadata.
///
/// Tracked in-memory alongside the message list. The agent loop increments
/// `current_turn` each iteration and updates `last_compact_turn` after
/// Tier 3/4 fires.
#[derive(Debug, Clone)]
pub struct ConversationMeta {
    pub last_compact_turn: usize,
    pub has_attempted_reactive_compact: bool,
    pub is_compact_agent: bool,
    pub current_turn: usize,
}

impl ConversationMeta {
    pub fn new() -> Self {
        Self {
            last_compact_turn: 0,
            has_attempted_reactive_compact: false,
            is_compact_agent: false,
            current_turn: 0,
        }
    }
}

impl Default for ConversationMeta {
    fn default() -> Self {
        Self::new()
    }
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

/// Create a recovery nudge message appropriate for the conversation context.
pub fn recovery_nudge_for(messages: &[ChatMessage]) -> ChatMessage {
    let previous_is_tool = messages.last().is_some_and(|msg| msg.role == "tool");

    let content = if previous_is_tool {
        "Continue from the tool result above. Either call the next required tool or provide a concise user-visible final answer.".to_string()
    } else {
        "Continue from the user's request above. Either call the next required tool or provide a concise user-visible final answer.".to_string()
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
        result.insert(
            1,
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(
                    "★ earlier conversation collapsed ★".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
            },
        );
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

        // Safety net: if still over hard cap, keep all sys/user + the 2 newest messages (1 preserved pair)
        if estimate_prompt_bytes(&after_tier2) > PROMPT_HARD_CAP_BYTES {
            let preserved_count = after_tier2
                .iter()
                .filter(|m| m.role == "system" || m.role == "user")
                .count();
            let mut hard_cap_messages: Vec<ChatMessage> = Vec::with_capacity(preserved_count + 2);
            for m in &after_tier2 {
                if m.role == "system" || m.role == "user" {
                    hard_cap_messages.push(m.clone());
                }
            }
            // Append the 2 newest non-system/user messages (preserves latest preserved pair)
            let mut newest_pair: Vec<ChatMessage> = Vec::with_capacity(2);
            for m in after_tier2.iter().rev() {
                if m.role != "system" && m.role != "user" {
                    newest_pair.push(m.clone());
                    if newest_pair.len() == 2 {
                        break;
                    }
                }
            }
            hard_cap_messages.extend(newest_pair.into_iter().rev());
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

/// Check whether Tier 3 auto-compact should trigger.
///
/// All conditions must be true:
/// - Message count > compact_min_message_count
/// - Estimated prompt bytes > context_window * COMPACT_PCT
/// - Turn gap >= COMPACT_TURN_GAP since last compact
/// - Not already in compact agent loop (recursion guard)
pub fn should_auto_compact(
    messages: &[ChatMessage],
    meta: &ConversationMeta,
    context_window: usize,
) -> bool {
    let threshold = (context_window as f64 * COMPACT_PCT) as usize;
    let bytes = estimate_prompt_bytes(messages);

    bytes > threshold
        && messages.len() > compact_min_message_count(context_window)
        && meta.current_turn - meta.last_compact_turn >= COMPACT_TURN_GAP
        && !meta.is_compact_agent
}

/// Create the summary prompt content used for Tier 3 and Tier 4 LLM calls.
/// Returns a system-role message instructing the LLM to summarize.
pub fn build_compact_summary_prompt() -> ChatMessage {
    let prompt_text = vec![
        "Your task is to create a detailed summary of the conversation so far.",
        "",
        "Your summary must include these sections:",
        "",
        "1. Primary Request and Intent: What was the user's original request?",
        "2. Key Technical Concepts: Technologies, frameworks, approaches discussed",
        "3. Files and Code Sections: Files read, created, or modified",
        "4. Errors and Fixes: Errors encountered and how they were fixed",
        "5. All User Messages: List ALL user messages verbatim (not tool results)",
        "6. Pending Tasks: What tasks were explicitly requested but not completed",
        "7. Current Work: Exactly what was being worked on before this summary",
        "8. Next Step: The next logical action based on the most recent user request",
        "",
        "IMPORTANT: Do NOT call any tools. Respond with text only.",
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
            "★ COMPACT SUMMARY — previous {} messages compressed into this summary (now {} messages) ★",
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
        let ctx = 2000;
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
    fn should_auto_compact_checks_bytes_turns_and_recursion_guard() {
        let mut meta = ConversationMeta {
            last_compact_turn: 0,
            has_attempted_reactive_compact: false,
            is_compact_agent: false,
            current_turn: 10,
        };

        let small: Vec<ChatMessage> = (0..3)
            .map(|_| ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hi".to_string())),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        assert!(!should_auto_compact(&small, &meta, 512_000));

        let few_but_big: Vec<ChatMessage> = (0..5)
            .map(|_| ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("x".repeat(100_000))),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        assert!(!should_auto_compact(&few_but_big, &meta, 512_000));

        let many_big: Vec<ChatMessage> = (0..23)
            .map(|_| ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("x".repeat(50_000))),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();

        meta.is_compact_agent = true;
        assert!(!should_auto_compact(&many_big, &meta, 512_000));
        meta.is_compact_agent = false;

        meta.last_compact_turn = 8;
        assert!(!should_auto_compact(&many_big, &meta, 512_000));
        meta.last_compact_turn = 0;

        assert!(should_auto_compact(&many_big, &meta, 512_000));
    }

    #[test]
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
    fn should_auto_compact_needs_minimum_message_count() {
        let meta = ConversationMeta::new();
        let few: Vec<ChatMessage> = (0..5)
            .map(|_| ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("x".repeat(100_000))),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        assert!(!should_auto_compact(&few, &meta, 512_000));
    }

    #[test]
    fn conversation_meta_defaults_to_zero() {
        let meta = ConversationMeta::new();
        assert_eq!(meta.last_compact_turn, 0);
        assert!(!meta.has_attempted_reactive_compact);
        assert!(!meta.is_compact_agent);
        assert_eq!(meta.current_turn, 0);
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
}
