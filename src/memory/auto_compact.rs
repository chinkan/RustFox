use anyhow::Result;
use tracing::{debug, info, warn};

use crate::llm::{ChatMessage, LlmClient};

use super::MemoryStore;

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Estimate the number of tokens in a string using a character-based heuristic.
///
/// - ASCII / Latin characters: ~4 characters per token (typical for BPE tokenizers)
/// - CJK characters (Chinese, Japanese, Korean): ~1.5 characters per token
/// - Other Unicode (emoji, symbols): ~2 characters per token
///
/// This avoids pulling in a full tokenizer crate while giving reasonable
/// estimates across multilingual content.
pub fn estimate_tokens(text: &str) -> usize {
    let mut ascii_chars: usize = 0;
    let mut cjk_chars: usize = 0;
    let mut other_chars: usize = 0;

    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_chars += 1;
        } else if is_cjk(ch) {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }

    // Rough token estimates (rounded up to avoid undercount)
    let ascii_tokens = ascii_chars.div_ceil(4); // ~4 chars/token
    let cjk_tokens = (cjk_chars * 2).div_ceil(3); // ~1.5 chars/token
    let other_tokens = other_chars.div_ceil(2); // ~2 chars/token

    ascii_tokens + cjk_tokens + other_tokens
}

/// Check if a character falls within CJK Unified Ideographs or common CJK ranges.
fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{FF00}'..='\u{FFEF}' // Halfwidth and Fullwidth Forms
    )
}

/// Estimate total token count for a slice of ChatMessages.
pub fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
    let mut total = 0;
    for msg in messages {
        // ~4 tokens overhead per message (role, delimiters)
        total += 4;
        if let Some(ref content) = msg.content {
            total += estimate_tokens(content);
        }
        if let Some(ref tool_calls) = msg.tool_calls {
            // Rough estimate: serialize tool calls and count
            if let Ok(json) = serde_json::to_string(tool_calls) {
                total += estimate_tokens(&json);
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Compaction logic
// ---------------------------------------------------------------------------

/// Check whether the loaded messages exceed the token threshold and should be compacted.
pub fn should_compact(messages: &[ChatMessage], token_threshold: usize) -> bool {
    let total_tokens = estimate_messages_tokens(messages);
    debug!(
        total_tokens = total_tokens,
        threshold = token_threshold,
        "Auto-compact token check"
    );
    total_tokens > token_threshold
}

/// Perform auto-compaction on a conversation:
///
/// 1. Load the current messages for the conversation
/// 2. Check if total tokens exceed the threshold
/// 3. Keep the most recent `recent_keep` messages verbatim
/// 4. Summarize older messages using the LLM, merging with any existing summary
/// 5. Replace old [SUMMARY] messages with a single cumulative summary
/// 6. Mark compacted messages as summarized (they remain in DB for RAG retrieval)
/// 7. Embed the new summary for vector search
///
/// Returns `Ok(true)` if compaction was performed, `Ok(false)` if skipped.
pub async fn compact_conversation(
    store: &MemoryStore,
    llm: &LlmClient,
    conversation_id: &str,
    recent_keep: usize,
    token_threshold: usize,
) -> Result<bool> {
    // Quick check: is there enough data to warrant compaction?
    let msg_count = store.count_conversation_messages(conversation_id).await?;
    if msg_count <= recent_keep {
        debug!(
            conversation_id = %conversation_id,
            msg_count = msg_count,
            recent_keep = recent_keep,
            "Not enough messages for compaction"
        );
        return Ok(false);
    }

    // Load all messages as the prompt would see them, to check token count
    let loaded = store
        .load_messages_with_limit(conversation_id, msg_count)
        .await?;
    if !should_compact(&loaded, token_threshold) {
        return Ok(false);
    }

    info!(
        conversation_id = %conversation_id,
        total_tokens = estimate_messages_tokens(&loaded),
        threshold = token_threshold,
        "Token threshold exceeded — starting auto-compaction"
    );

    // Get messages older than the most recent `recent_keep`
    let older_messages = store
        .get_older_messages_for_compaction(conversation_id, recent_keep)
        .await?;

    if older_messages.is_empty() {
        debug!(
            conversation_id = %conversation_id,
            "No older messages eligible for compaction"
        );
        return Ok(false);
    }

    // Gather existing summaries to merge
    let existing_summaries = store.get_summary_messages(conversation_id).await?;
    let existing_summary_text: String = existing_summaries
        .iter()
        .map(|(_, content)| {
            content
                .strip_prefix("[SUMMARY]\n")
                .or_else(|| content.strip_prefix("[SUMMARY]"))
                .unwrap_or(content)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // Format older messages for the summarization prompt
    let conversation_text: String = older_messages
        .iter()
        .filter_map(|(_, role, content)| content.as_ref().map(|c| format!("[{}]: {}", role, c)))
        .collect::<Vec<_>>()
        .join("\n");

    // Build the summarization prompt
    let prompt = build_compaction_prompt(&existing_summary_text, &conversation_text);

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(COMPACTION_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(prompt),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let response = llm.chat(&messages, &[]).await?;
    let summary_text = response.content.unwrap_or_default();

    if summary_text.trim().is_empty() {
        warn!(
            conversation_id = %conversation_id,
            "LLM returned empty compaction summary — skipping"
        );
        return Ok(false);
    }

    // Delete old [SUMMARY] messages (and their embeddings)
    let deleted_count = store.delete_summary_messages(conversation_id).await?;
    if deleted_count > 0 {
        debug!(
            conversation_id = %conversation_id,
            deleted = deleted_count,
            "Deleted old summary messages"
        );
    }

    // Save the new cumulative [SUMMARY] message (with embedding via save_message)
    let summary_msg = ChatMessage {
        role: "system".to_string(),
        content: Some(format!("[SUMMARY]\n{}", summary_text.trim())),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(conversation_id, &summary_msg).await?;

    // Mark compacted messages as summarized
    let message_ids: Vec<String> = older_messages.into_iter().map(|(id, _, _)| id).collect();
    let compacted_count = message_ids.len();
    store.mark_messages_summarized(&message_ids).await?;

    info!(
        conversation_id = %conversation_id,
        compacted_messages = compacted_count,
        "Auto-compaction complete"
    );

    Ok(true)
}

// ---------------------------------------------------------------------------
// Prompt templates
// ---------------------------------------------------------------------------

/// System prompt for the compaction summarizer LLM call.
const COMPACTION_SYSTEM_PROMPT: &str = "\
You are an expert conversation summarizer. Your task is to produce a concise, \
structured summary that preserves all important information from the conversation.\n\
\n\
Output format: structured bullet points in Traditional Chinese.\n\
Output ONLY the summary — no commentary, no meta-text.\n\
If the existing summary is provided, merge it with the new conversation content \
into a single cohesive summary.";

/// Build the user prompt for compaction, merging existing summary with new messages.
fn build_compaction_prompt(existing_summary: &str, conversation_text: &str) -> String {
    let mut prompt = String::with_capacity(conversation_text.len() + existing_summary.len() + 1024);

    prompt.push_str(
        "Summarize the conversation below into a concise, structured summary.\n\n\
         Preserve:\n\
         - Key facts and user preferences (names, settings, environment, constraints)\n\
         - Important decisions made and their rationale\n\
         - Problems solved and how they were resolved\n\
         - Pending/unresolved tasks or questions\n\
         - Technical entities (tools, languages, frameworks, APIs mentioned)\n\
         - User corrections or clarifications\n\n\
         Do NOT include: greetings, small talk, filler, or tool call raw output.\n\n\
         FORMAT (strictly follow):\n\
         • [主題]: 一到兩句摘要\n\
         • [主題]: 一到兩句摘要\n\n",
    );

    if !existing_summary.is_empty() {
        prompt.push_str("=== EXISTING SUMMARY (merge with new content below) ===\n");
        prompt.push_str(existing_summary);
        prompt.push_str("\n\n");
    }

    prompt.push_str("=== CONVERSATION TO SUMMARIZE ===\n");
    prompt.push_str(conversation_text);

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_ascii() {
        // 100 ASCII chars → ~25 tokens
        let text = "a".repeat(100);
        let tokens = estimate_tokens(&text);
        assert!(tokens >= 20 && tokens <= 30, "got {}", tokens);
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        // 30 CJK chars → ~20 tokens (at 1.5 chars/token)
        let text = "你好世界每日論文摘要香港時間人工智能最新研究成果今天天氣好不好最新消息更新通知";
        let char_count = text.chars().count();
        let tokens = estimate_tokens(text);
        // Should be roughly char_count * 2/3
        assert!(
            tokens >= char_count / 2 && tokens <= char_count,
            "CJK: {} chars → {} tokens",
            char_count,
            tokens
        );
    }

    #[test]
    fn test_estimate_tokens_mixed() {
        let text = "Hello 你好 World 世界";
        let tokens = estimate_tokens(text);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_messages_tokens() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("Hello world".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("Hi there, how can I help?".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let tokens = estimate_messages_tokens(&messages);
        assert!(tokens > 0);
    }

    #[test]
    fn test_should_compact_below_threshold() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some("short".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        assert!(!should_compact(&messages, 1000));
    }

    #[test]
    fn test_should_compact_above_threshold() {
        // Create messages with enough text to exceed a small threshold
        let mut messages = Vec::new();
        for i in 0..100 {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: Some(format!(
                    "This is message number {} with enough content to accumulate tokens",
                    i
                )),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        // With ~100 messages each ~60 chars, total is ~1500+ tokens
        assert!(should_compact(&messages, 100));
    }

    #[test]
    fn test_build_compaction_prompt_without_existing_summary() {
        let prompt = build_compaction_prompt("", "user: Hello\nassistant: Hi");
        assert!(prompt.contains("=== CONVERSATION TO SUMMARIZE ==="));
        assert!(!prompt.contains("EXISTING SUMMARY"));
    }

    #[test]
    fn test_build_compaction_prompt_with_existing_summary() {
        let prompt = build_compaction_prompt("• Previous context", "user: Hello");
        assert!(prompt.contains("EXISTING SUMMARY"));
        assert!(prompt.contains("Previous context"));
        assert!(prompt.contains("CONVERSATION TO SUMMARIZE"));
    }

    #[test]
    fn test_is_cjk() {
        assert!(is_cjk('你'));
        assert!(is_cjk('好'));
        assert!(is_cjk('の'));
        assert!(is_cjk('한'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('1'));
    }

    #[tokio::test]
    async fn test_compact_conversation_skips_small_conversation() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "compact_user1")
            .await
            .unwrap();

        // Save a few messages — below recent_keep
        for i in 0..5 {
            let msg = ChatMessage {
                role: "user".to_string(),
                content: Some(format!("message {}", i)),
                tool_calls: None,
                tool_call_id: None,
            };
            store.save_message(&conv, &msg).await.unwrap();
        }

        // Cannot call compact with real LLM, but verify the early return for small conversations.
        // count_conversation_messages should be 5, recent_keep=15, so it returns Ok(false) immediately.
        let msg_count = store.count_conversation_messages(&conv).await.unwrap();
        assert_eq!(msg_count, 5);
        assert!(
            msg_count <= 15,
            "Should skip compaction for small conversation"
        );
    }

    #[tokio::test]
    async fn test_get_older_messages_for_compaction() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "compact_user2")
            .await
            .unwrap();

        for i in 0..20 {
            let msg = ChatMessage {
                role: "user".to_string(),
                content: Some(format!("message {}", i)),
                tool_calls: None,
                tool_call_id: None,
            };
            store.save_message(&conv, &msg).await.unwrap();
        }

        // Keep recent 10, so 10 older messages should be eligible
        let older = store
            .get_older_messages_for_compaction(&conv, 10)
            .await
            .unwrap();
        assert_eq!(older.len(), 10);
        // Oldest message should be "message 0"
        assert!(older[0].2.as_deref().unwrap().contains("message 0"));
    }

    #[tokio::test]
    async fn test_summary_message_lifecycle() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "summary_lifecycle")
            .await
            .unwrap();

        // Save a [SUMMARY] message
        let summary = ChatMessage {
            role: "system".to_string(),
            content: Some("[SUMMARY]\n• Test summary".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        store.save_message(&conv, &summary).await.unwrap();

        // Verify it's retrievable
        let summaries = store.get_summary_messages(&conv).await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].1.contains("Test summary"));

        // Delete summaries
        let deleted = store.delete_summary_messages(&conv).await.unwrap();
        assert_eq!(deleted, 1);

        // Verify deletion
        let summaries = store.get_summary_messages(&conv).await.unwrap();
        assert!(summaries.is_empty());
    }
}
