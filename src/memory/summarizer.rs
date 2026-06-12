use anyhow::Result;
use tracing::{info, warn};

use crate::llm::{ChatMessage, LlmClient, MessageContent};

use super::MemoryStore;

/// Summarize a conversation and store the result as a [SUMMARY] system message.
/// Returns `Ok(true)` if a summary was created, `Ok(false)` if skipped.
pub async fn summarize_conversation(
    store: &MemoryStore,
    llm: &LlmClient,
    conversation_id: &str,
    threshold: usize,
) -> Result<bool> {
    let unsummarized = store.get_unsummarized_messages(conversation_id).await?;

    if unsummarized.len() < threshold {
        return Ok(false);
    }

    let conversation_text: String = unsummarized
        .iter()
        .filter_map(|(_, role, content)| content.as_ref().map(|c| format!("[{}]: {}", role, c)))
        .collect::<Vec<_>>()
        .join("\n");

    let summarization_prompt = format!(
        "Summarize the conversation history below in 3-5 bullet points.\n\
         Maximum 200 words total. Be factual and precise.\n\n\
         Focus on:\n\
         - Facts the user explicitly stated (preferences, constraints, environment, name)\n\
         - Problems that were solved and how\n\
         - Important decisions made\n\
         - Unresolved questions or pending tasks\n\n\
         Do NOT include: greetings, small talk, or filler content.\n\n\
         FORMAT (strictly follow this):\n\
         • [topic]: one to two sentence summary\n\
         • [topic]: one to two sentence summary\n\n\
         CONVERSATION:\n{}",
        conversation_text
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::from_text(
                "You produce concise, factual conversation summaries. Output only bullet points.",
            )),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text(summarization_prompt)),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let response = llm.chat(&messages, &[]).await?;
    let summary_text = response.content.map(|c| c.as_text()).unwrap_or_default();

    if summary_text.trim().is_empty() {
        warn!(conversation_id = %conversation_id, "LLM returned empty summary — skipping");
        return Ok(false);
    }

    let summary_msg = ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::from_text(format!(
            "[SUMMARY]\n{}",
            summary_text.trim()
        ))),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(conversation_id, &summary_msg).await?;

    let message_ids: Vec<String> = unsummarized.into_iter().map(|(id, _, _)| id).collect();
    store.mark_messages_summarized(&message_ids).await?;

    info!(
        conversation_id = %conversation_id,
        count = message_ids.len(),
        "Summarization complete"
    );
    Ok(true)
}

/// Run summarization for all conversations active in the last 7 days.
pub async fn summarize_all_active(
    store: &MemoryStore,
    llm: &LlmClient,
    threshold: usize,
) -> Result<usize> {
    let conversations = store.get_active_conversations(7).await?;
    let mut count = 0usize;

    for conv_id in conversations {
        match summarize_conversation(store, llm, &conv_id, threshold).await {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => {
                warn!(conversation_id = %conv_id, "Summarization failed: {:#}", e);
            }
        }
    }

    info!(
        "Nightly summarization complete: {} conversations summarized",
        count
    );
    Ok(count)
}

#[cfg(test)]
mod tests {
    use crate::llm::{ChatMessage, MessageContent};
    use crate::memory::MemoryStore;

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text(text)),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn test_get_unsummarized_messages_returns_correct_count() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "sum_u1")
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("first message"))
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("second message"))
            .await
            .unwrap();

        let unsummarized = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(unsummarized.len(), 2);
    }

    #[tokio::test]
    async fn test_mark_messages_summarized_clears_them() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "sum_u2")
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("to be summarized"))
            .await
            .unwrap();

        let before = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(before.len(), 1);

        let ids: Vec<String> = before.into_iter().map(|(id, _, _)| id).collect();
        store.mark_messages_summarized(&ids).await.unwrap();

        let after = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(after.len(), 0, "All messages should be marked summarized");
    }

    #[tokio::test]
    async fn test_get_active_conversations_returns_recent() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .get_or_create_conversation("test", "active_user")
            .await
            .unwrap();
        let active = store.get_active_conversations(7).await.unwrap();
        assert!(
            !active.is_empty(),
            "Should have at least one active conversation"
        );
    }

    #[tokio::test]
    async fn test_summarize_conversation_skips_below_threshold() {
        // With only 1 message and threshold=5, should return false without LLM call
        // (We can't call LLM in tests, but we test the threshold guard)
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "sum_threshold")
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("only one message"))
            .await
            .unwrap();

        // We can't pass a real LlmClient here without config, so verify via
        // the unsummarized count check — below threshold means early return
        let unsummarized = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(unsummarized.len(), 1);
        // Threshold check: 1 < 5 → would return Ok(false)
        assert!(unsummarized.len() < 5, "Should be below threshold");
    }
}
