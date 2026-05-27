//! In-memory prompt preparation and compaction for LLM context management.
//!
//! This module provides functionality to prepare conversation history for LLM calls,
//! including automatic compaction of tool-heavy conversations to stay within context
//! limits while preserving recent and relevant information.

use crate::llm::ChatMessage;

const COMPACTION_MESSAGE_COUNT_THRESHOLD: usize = 10;
const COMPACTION_PROMPT_CHAR_THRESHOLD: usize = 20_000;
const TOOL_ARGUMENT_COMPACT_THRESHOLD: usize = 1_000;
const TOOL_RESULT_COMPACT_THRESHOLD: usize = 2_000;
const TOOL_RESULT_PREVIEW_CHARS: usize = 1_000;
const PRESERVED_TOOL_GROUPS: usize = 2;

/// Truncate a string to at most `max_chars` characters at a safe character boundary.
///
/// Returns a new string containing up to `max_chars` characters from `value`.
/// If the string is longer, it is truncated at a character boundary (not mid-codepoint).
fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

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

/// Estimate the character count of a prompt from its messages.
///
/// Counts message content lengths and tool call argument lengths.
pub fn estimate_prompt_chars(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content_chars = msg.content.as_deref().map(|c| c.len()).unwrap_or(0);
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
///
/// If the previous message role is `tool`, mentions "tool result above".
/// Otherwise mentions "user's request above".
pub fn recovery_nudge_for(messages: &[ChatMessage]) -> ChatMessage {
    let previous_is_tool = messages.last().is_some_and(|msg| msg.role == "tool");

    let content = if previous_is_tool {
        "Continue from the tool result above. Either call the next required tool or provide a concise user-visible final answer.".to_string()
    } else {
        "Continue from the user's request above. Either call the next required tool or provide a concise user-visible final answer.".to_string()
    };

    ChatMessage {
        role: "system".to_string(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Prepare messages for LLM by applying compaction if needed.
///
/// Applies compaction only when:
/// - Message count > 10 AND
/// - Estimated prompt size > 20,000 chars
pub fn prepare_messages_for_llm(messages: &[ChatMessage]) -> PreparedPrompt {
    let original_message_count = messages.len();
    let original_prompt_chars = estimate_prompt_chars(messages);

    let should_compact = original_message_count > COMPACTION_MESSAGE_COUNT_THRESHOLD
        && original_prompt_chars > COMPACTION_PROMPT_CHAR_THRESHOLD;

    let prepared_messages = if should_compact {
        compact_tool_heavy_history(messages)
    } else {
        messages.to_vec()
    };

    let prepared_message_count = prepared_messages.len();
    let prepared_prompt_chars = estimate_prompt_chars(&prepared_messages);

    PreparedPrompt {
        messages: prepared_messages,
        stats: PromptStats {
            original_message_count,
            prepared_message_count,
            original_prompt_chars,
            prepared_prompt_chars,
            compaction_applied: should_compact,
        },
    }
}

/// Compact tool-heavy conversation history while preserving recent context.
///
/// Strategy:
/// - Preserve all system and user messages unchanged
/// - Preserve the most recent two assistant-with-tool-calls groups and their tool results
/// - Compact older assistant tool arguments longer than 1,000 chars
/// - Compact older tool results longer than 2,000 chars
/// - Maintain message order
pub fn compact_tool_heavy_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    // First, identify tool groups: assistant messages with tool calls and their following tool messages
    let mut tool_groups: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut current_group: Option<(usize, Vec<usize>)> = None;

    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "assistant"
            && msg
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        {
            // Start a new tool group
            if let Some(group) = current_group.take() {
                tool_groups.push(group);
            }
            current_group = Some((idx, Vec::new()));
        } else if msg.role == "tool" {
            // Add to current group if one exists
            if let Some((_, ref mut tool_indices)) = current_group {
                tool_indices.push(idx);
            }
        } else if current_group.is_some() {
            // Non-tool message encountered, close current group
            if let Some(group) = current_group.take() {
                tool_groups.push(group);
            }
        }
    }
    // Don't forget the last group
    if let Some(group) = current_group {
        tool_groups.push(group);
    }

    // Determine which indices to preserve (most recent PRESERVED_TOOL_GROUPS)
    let preserved_groups_start = tool_groups.len().saturating_sub(PRESERVED_TOOL_GROUPS);
    let mut preserved_indices = std::collections::HashSet::new();
    for group in tool_groups.iter().skip(preserved_groups_start) {
        preserved_indices.insert(group.0);
        for &tool_idx in &group.1 {
            preserved_indices.insert(tool_idx);
        }
    }

    // Compact messages
    messages
        .iter()
        .enumerate()
        .map(|(idx, msg)| {
            // Never compact system or user messages
            if msg.role == "system" || msg.role == "user" {
                return msg.clone();
            }

            // Don't compact preserved indices
            if preserved_indices.contains(&idx) {
                return msg.clone();
            }

            // Compact assistant tool calls
            if msg.role == "assistant"
                && msg
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
            {
                return compact_assistant_tool_calls(msg);
            }

            // Compact tool results
            if msg.role == "tool" {
                return compact_tool_result(msg);
            }

            msg.clone()
        })
        .collect()
}

/// Compact assistant message with tool calls by shortening long arguments.
fn compact_assistant_tool_calls(msg: &ChatMessage) -> ChatMessage {
    let mut compacted = msg.clone();

    if let Some(tool_calls) = &mut compacted.tool_calls {
        for call in tool_calls.iter_mut() {
            let args_len = call.function.arguments.len();
            if args_len > TOOL_ARGUMENT_COMPACT_THRESHOLD {
                let preview = if call.function.arguments.chars().count() > 200 {
                    format!("{}...", truncate_chars(&call.function.arguments, 200))
                } else {
                    call.function.arguments.clone()
                };

                let compacted_json = serde_json::json!({
                    "_rustfox_compacted_arguments": true,
                    "tool_name": call.function.name,
                    "original_char_count": args_len,
                    "preview": preview,
                });

                call.function.arguments =
                    serde_json::to_string(&compacted_json).unwrap_or_else(|_| "{}".to_string());
            }
        }
    }

    compacted
}

/// Compact tool result message by truncating long content.
fn compact_tool_result(msg: &ChatMessage) -> ChatMessage {
    let mut compacted = msg.clone();

    if let Some(content) = &compacted.content {
        if content.len() > TOOL_RESULT_COMPACT_THRESHOLD {
            let preview = truncate_chars(content, TOOL_RESULT_PREVIEW_CHARS);
            compacted.content = Some(format!(
                "[rustfox compacted tool result: {} chars]\n{}...",
                content.len(),
                preview
            ));
        }
    }

    compacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    #[test]
    fn estimate_prompt_chars_counts_content_and_tool_arguments() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()), // 5 chars
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("Hi".to_string()), // 2 chars
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "test_tool".to_string(),
                        arguments: r#"{"arg":"value"}"#.to_string(), // 15 chars
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("result".to_string()), // 6 chars
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
        ];

        let total = estimate_prompt_chars(&messages);
        assert_eq!(total, 5 + 2 + 15 + 6); // 28 chars
    }

    #[test]
    fn recovery_nudge_mentions_tool_result_when_previous_message_is_tool() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("result".to_string()),
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
            .contains("tool result above"));
    }

    #[test]
    fn recovery_nudge_mentions_user_request_when_previous_message_is_user() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some("Hello".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];

        let nudge = recovery_nudge_for(&messages);
        assert_eq!(nudge.role, "system");
        assert!(nudge
            .content
            .as_ref()
            .unwrap()
            .contains("user's request above"));
    }

    #[test]
    fn prepare_messages_skips_compaction_for_short_prompts() {
        // Less than 10 messages
        let messages: Vec<ChatMessage> = (0..5)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: Some(format!("message {}", i)),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();

        let result = prepare_messages_for_llm(&messages);
        assert!(!result.stats.compaction_applied);
        assert_eq!(result.messages.len(), messages.len());

        // More than 10 messages but under char threshold
        let short_messages: Vec<ChatMessage> = (0..15)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: Some(format!("msg{}", i)), // Very short
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();

        let result2 = prepare_messages_for_llm(&short_messages);
        assert!(!result2.stats.compaction_applied);
    }

    #[test]
    fn compaction_preserves_newest_two_tool_groups_and_compacts_older_group() {
        // Build a conversation with 3 tool groups
        let mut messages = Vec::new();

        // System message
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some("You are a helpful assistant.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        // User message
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some("Do task 1".to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        // Old tool group 1 (should be compacted)
        let long_args = "x".repeat(1500);
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "old_call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "old_tool".to_string(),
                    arguments: long_args.clone(),
                },
            }]),
            tool_call_id: None,
        });

        let long_result = "y".repeat(2500);
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(long_result.clone()),
            tool_calls: None,
            tool_call_id: Some("old_call_1".to_string()),
        });

        // Recent tool group 1 (should be preserved)
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "recent_call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "recent_tool_1".to_string(),
                    arguments: "x".repeat(1500),
                },
            }]),
            tool_call_id: None,
        });

        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("y".repeat(2500)),
            tool_calls: None,
            tool_call_id: Some("recent_call_1".to_string()),
        });

        // Recent tool group 2 (should be preserved)
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "recent_call_2".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "recent_tool_2".to_string(),
                    arguments: "z".repeat(1500),
                },
            }]),
            tool_call_id: None,
        });

        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("w".repeat(2500)),
            tool_calls: None,
            tool_call_id: Some("recent_call_2".to_string()),
        });

        let compacted = compact_tool_heavy_history(&messages);

        // System and user messages should be unchanged
        assert_eq!(compacted[0].role, "system");
        assert_eq!(compacted[1].role, "user");

        // Old tool group should be compacted
        let old_assistant = &compacted[2];
        assert_eq!(old_assistant.role, "assistant");
        let old_args = &old_assistant.tool_calls.as_ref().unwrap()[0]
            .function
            .arguments;
        assert!(old_args.contains("_rustfox_compacted_arguments"));
        assert!(old_args.len() < long_args.len());

        let old_tool = &compacted[3];
        assert_eq!(old_tool.role, "tool");
        let old_content = old_tool.content.as_ref().unwrap();
        assert!(old_content.contains("rustfox compacted tool result"));
        assert!(old_content.len() < long_result.len());

        // Recent tool groups should be preserved unchanged
        let recent1_assistant = &compacted[4];
        assert_eq!(
            recent1_assistant.tool_calls.as_ref().unwrap()[0]
                .function
                .arguments
                .len(),
            1500
        );

        let recent1_tool = &compacted[5];
        assert_eq!(recent1_tool.content.as_ref().unwrap().len(), 2500);

        let recent2_assistant = &compacted[6];
        assert_eq!(
            recent2_assistant.tool_calls.as_ref().unwrap()[0]
                .function
                .arguments
                .len(),
            1500
        );

        let recent2_tool = &compacted[7];
        assert_eq!(recent2_tool.content.as_ref().unwrap().len(), 2500);
    }

    #[test]
    fn compacted_message_order_is_unchanged() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("System".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("User 1".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "tool".to_string(),
                        arguments: "x".repeat(1500),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("y".repeat(2500)),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("Final response".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let compacted = compact_tool_heavy_history(&messages);

        // Check that roles are in the same order
        let original_roles: Vec<_> = messages.iter().map(|m| m.role.as_str()).collect();
        let compacted_roles: Vec<_> = compacted.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(original_roles, compacted_roles);

        // Check message count is unchanged
        assert_eq!(messages.len(), compacted.len());
    }

    #[test]
    fn truncate_chars_handles_unicode_safely() {
        // ASCII text
        assert_eq!(truncate_chars("hello world", 5), "hello");
        assert_eq!(truncate_chars("hello", 10), "hello");

        // Emoji (multi-byte UTF-8)
        let emoji_text = "Hello 👋 World 🌍";
        let truncated = truncate_chars(emoji_text, 8);
        assert_eq!(truncated, "Hello 👋 ");
        assert_eq!(truncated.chars().count(), 8);

        // CJK characters
        let cjk_text = "你好世界";
        let truncated = truncate_chars(cjk_text, 2);
        assert_eq!(truncated, "你好");
        assert_eq!(truncated.chars().count(), 2);

        // Mixed: emoji + CJK + ASCII
        let mixed = "Test 测试 🎉 emoji 表情";
        let truncated = truncate_chars(mixed, 11);
        assert_eq!(truncated, "Test 测试 🎉 e");
        assert_eq!(truncated.chars().count(), 11);
    }

    #[test]
    fn compaction_handles_unicode_tool_arguments_safely() {
        // Create a long tool argument with emoji and CJK that would panic with byte slicing
        // This string has multi-byte UTF-8 characters; byte index 200 could fall mid-character
        let long_args_with_unicode = format!(
            "{{\"message\": \"{}测试🎉{}\"}}",
            "x".repeat(900),
            "y".repeat(200)
        );
        assert!(long_args_with_unicode.len() > TOOL_ARGUMENT_COMPACT_THRESHOLD);

        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "unicode_tool".to_string(),
                    arguments: long_args_with_unicode.clone(),
                },
            }]),
            tool_call_id: None,
        };

        // This should not panic
        let compacted = compact_assistant_tool_calls(&message);
        let compacted_args = &compacted.tool_calls.as_ref().unwrap()[0].function.arguments;

        // Verify it's been compacted
        assert!(compacted_args.contains("_rustfox_compacted_arguments"));
        assert!(compacted_args.contains("unicode_tool"));
        assert!(compacted_args.contains("original_char_count"));
        assert!(compacted_args.contains("preview"));

        // Parse and verify the preview is valid UTF-8 and shorter than original
        let parsed: serde_json::Value = serde_json::from_str(compacted_args).unwrap();
        let preview = parsed["preview"].as_str().unwrap();
        assert!(preview.len() < long_args_with_unicode.len());
        assert!(preview.chars().count() <= 203); // 200 chars + "..."
    }

    #[test]
    fn compaction_handles_unicode_tool_results_safely() {
        // Create a long tool result with emoji and CJK that would panic with byte slicing
        let long_result_with_unicode =
            format!("Result: {}🌟测试{}💯", "a".repeat(1000), "b".repeat(1500));
        assert!(long_result_with_unicode.len() > TOOL_RESULT_COMPACT_THRESHOLD);

        let message = ChatMessage {
            role: "tool".to_string(),
            content: Some(long_result_with_unicode.clone()),
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
        };

        // This should not panic
        let compacted = compact_tool_result(&message);
        let compacted_content = compacted.content.as_ref().unwrap();

        // Verify it's been compacted
        assert!(compacted_content.contains("rustfox compacted tool result"));
        assert!(compacted_content.len() < long_result_with_unicode.len());

        // Verify the preview portion is valid UTF-8 and respects character boundaries
        // The format is: "[rustfox compacted tool result: N chars]\n{preview}..."
        let lines: Vec<&str> = compacted_content.lines().collect();
        assert_eq!(lines.len(), 2);
        let preview_line = lines[1];
        assert!(preview_line.ends_with("..."));

        // Verify we can count characters without panicking (proves valid UTF-8)
        let preview_without_ellipsis = preview_line.trim_end_matches("...");
        let char_count = preview_without_ellipsis.chars().count();
        assert!(char_count <= TOOL_RESULT_PREVIEW_CHARS);
    }

    #[test]
    fn compaction_with_unicode_preserves_structure() {
        // Test full compaction flow with Unicode content
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("System prompt 系统提示".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("User request with emoji 🚀".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        // Add an OLD tool group with Unicode content (this should be compacted)
        let unicode_args = format!(
            "{{\"data\": \"{}中文{}🎨{}\"}}",
            "x".repeat(800),
            "y".repeat(200),
            "z".repeat(100)
        );
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "old_call".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "unicode_tool".to_string(),
                    arguments: unicode_args,
                },
            }]),
            tool_call_id: None,
        });

        let unicode_result = format!("Result 结果: {}🌈{}", "a".repeat(1500), "b".repeat(1200));
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(unicode_result),
            tool_calls: None,
            tool_call_id: Some("old_call".to_string()),
        });

        // Add recent tool group 1 (should be preserved)
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "recent_call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "recent_tool_1".to_string(),
                    arguments: "x".repeat(1500),
                },
            }]),
            tool_call_id: None,
        });

        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("y".repeat(2500)),
            tool_calls: None,
            tool_call_id: Some("recent_call_1".to_string()),
        });

        // Add recent tool group 2 (should be preserved)
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "recent_call_2".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "recent_tool_2".to_string(),
                    arguments: "z".repeat(1500),
                },
            }]),
            tool_call_id: None,
        });

        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("w".repeat(2500)),
            tool_calls: None,
            tool_call_id: Some("recent_call_2".to_string()),
        });

        // Compact and verify no panics
        let compacted = compact_tool_heavy_history(&messages);

        // Verify structure is preserved
        assert_eq!(compacted.len(), messages.len());
        assert_eq!(compacted[0].role, "system");
        assert_eq!(compacted[1].role, "user");
        assert_eq!(compacted[2].role, "assistant");
        assert_eq!(compacted[3].role, "tool");

        // Verify OLD tool group was compacted
        let compacted_args = &compacted[2].tool_calls.as_ref().unwrap()[0]
            .function
            .arguments;
        assert!(compacted_args.contains("_rustfox_compacted_arguments"));

        let compacted_result = compacted[3].content.as_ref().unwrap();
        assert!(compacted_result.contains("rustfox compacted tool result"));

        // Verify recent groups are preserved unchanged
        assert_eq!(
            compacted[4].tool_calls.as_ref().unwrap()[0]
                .function
                .arguments
                .len(),
            1500
        );
        assert_eq!(compacted[5].content.as_ref().unwrap().len(), 2500);
        assert_eq!(
            compacted[6].tool_calls.as_ref().unwrap()[0]
                .function
                .arguments
                .len(),
            1500
        );
        assert_eq!(compacted[7].content.as_ref().unwrap().len(), 2500);
    }
}
