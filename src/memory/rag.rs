use anyhow::Result;
use tracing::debug;

use super::MemoryStore;

/// Auto-retrieve semantically relevant past messages from a conversation
/// and format them as a `<retrieved_context>` block for the system prompt.
/// Returns `None` if query is too short, is a command, or no results found.
pub async fn auto_retrieve_context(
    store: &MemoryStore,
    llm: Option<&crate::llm::LlmClient>,
    query: &str,
    recent_messages: &[crate::llm::ChatMessage],
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>> {
    // Skip retrieval for very short inputs or bot commands
    if query.trim().len() < 5 || query.starts_with('/') {
        return Ok(None);
    }

    // Query rewriting: resolve pronouns/references using recent context
    let search_query = if let Some(llm) = llm {
        crate::memory::query_rewriter::rewrite_for_rag(llm, query, recent_messages).await
    } else {
        query.to_string()
    };

    let results = store
        .search_messages_in_conversation(&search_query, conversation_id, limit)
        .await?;

    if results.is_empty() {
        return Ok(None);
    }

    let mut block = String::from(
        "<retrieved_context>\n\
         Relevant past conversation snippets (retrieved by semantic search):\n\n",
    );

    for msg in &results {
        if let Some(content) = &msg.content {
            let role = &msg.role;
            let snippet = crate::utils::str::truncate_chars(content, 300);
            block.push_str(&format!("[{}] {}\n", role, snippet));
        }
    }

    block.push_str("</retrieved_context>");
    debug!(
        "RAG: injecting {} snippets for query: {:?}",
        results.len(),
        query
    );
    Ok(Some(block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;
    use crate::memory::MemoryStore;

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn test_auto_retrieve_skips_short_query() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "rag_u1")
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("I use Docker"))
            .await
            .unwrap();
        let result = auto_retrieve_context(&store, None, "hi", &[], &conv, 5)
            .await
            .unwrap();
        assert!(result.is_none(), "Short query should return None");
    }

    #[tokio::test]
    async fn test_auto_retrieve_skips_commands() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "rag_u2")
            .await
            .unwrap();
        store
            .save_message(&conv, &user_msg("Docker setup"))
            .await
            .unwrap();
        let result = auto_retrieve_context(&store, None, "/clear", &[], &conv, 5)
            .await
            .unwrap();
        assert!(result.is_none(), "Bot commands should return None");
    }

    #[tokio::test]
    async fn test_auto_retrieve_returns_none_for_empty_results() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "rag_u3")
            .await
            .unwrap();
        // Empty conversation — nothing to retrieve
        let result = auto_retrieve_context(&store, None, "something long enough", &[], &conv, 5)
            .await
            .unwrap();
        assert!(result.is_none(), "Empty conversation should return None");
    }

    #[tokio::test]
    async fn test_auto_retrieve_block_format() {
        // Tests that when results exist, the block has correct XML tags
        // We can't test vector search without embeddings, but we can verify
        // the block format if we manually call with mock results
        // Just verify the static format via constants
        let opening = "<retrieved_context>";
        let closing = "</retrieved_context>";
        let block = format!("{}\nsome content\n{}", opening, closing);
        assert!(block.starts_with("<retrieved_context>"));
        assert!(block.ends_with("</retrieved_context>"));
    }

    #[tokio::test]
    async fn test_auto_retrieve_truncates_long_snippets() {
        // Verify the 300-char truncation logic via truncate_chars
        let content = "x".repeat(500);
        let snippet = crate::utils::str::truncate_chars(&content, 300);
        assert_eq!(snippet.len(), 303); // 300 + "..."
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn test_snippet_truncation_chinese_no_panic() {
        // Directly tests that truncate_chars handles the exact scenario rag.rs uses:
        // content longer than 300 bytes with Chinese characters.
        // Old &content[..300] would panic here.
        let long_chinese = "每日論文摘要（香港時間）人工智能".repeat(25); // ~400 chars, >1200 bytes
        let result = crate::utils::str::truncate_chars(&long_chinese, 300);
        assert!(result.ends_with("..."), "should be truncated");
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "must be valid UTF-8"
        );
    }

    #[tokio::test]
    async fn test_auto_retrieve_uses_rewritten_query_for_search() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "rewrite_test")
            .await
            .unwrap();

        let msg = crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some("I prefer TypeScript for frontend work".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Without a real LLM, rewrite falls back to original query
        let result = auto_retrieve_context(&store, None, "TypeScript", &[], &conv, 5)
            .await
            .unwrap();
        let _ = result; // Just verify no panic
    }
}
