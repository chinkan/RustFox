use anyhow::{Context, Result};
use uuid::Uuid;

use super::MemoryStore;
use crate::llm::{ChatMessage, MessageContent};

/// Cast a &[f32] to &[u8] for SQLite blob storage
pub(crate) fn f32_slice_to_bytes(floats: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(floats.as_ptr() as *const u8, floats.len() * 4) }
}

/// Cast Vec<f32> to Vec<u8> for SQLite blob storage
pub(crate) fn f32_vec_to_bytes(floats: &[f32]) -> Vec<u8> {
    f32_slice_to_bytes(floats).to_vec()
}

impl MemoryStore {
    /// Get or create an active (non-archived) conversation for a platform user.
    /// If all existing conversations for the user are archived, a new one is created.
    pub async fn get_or_create_conversation(
        &self,
        platform: &str,
        user_id: &str,
    ) -> Result<String> {
        let conn = self.conn.lock().await;

        // Try to find an existing active conversation
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM conversations
                 WHERE platform = ?1 AND user_id = ?2 AND (is_archived IS NULL OR is_archived = 0)
                 ORDER BY updated_at DESC LIMIT 1",
                rusqlite::params![platform, user_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing {
            return Ok(id);
        }

        // Create a new conversation
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO conversations (id, platform, user_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![&id, platform, user_id],
        )
        .context("Failed to create conversation")?;

        Ok(id)
    }

    /// Save a message to a conversation, with optional vector embedding
    pub async fn save_message(
        &self,
        conversation_id: &str,
        message: &ChatMessage,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let tool_calls_json = message
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default());

        // Generate embedding before acquiring the DB lock (async HTTP call)
        let content_text: Option<String> = message.content.as_ref().map(|c| c.as_text());
        let embedding = if let Some(ref content) = content_text {
            if !content.is_empty() && message.role != "tool" {
                self.embeddings.try_embed_one(content).await
            } else {
                None
            }
        } else {
            None
        };

        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content, tool_calls, tool_call_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                &id,
                conversation_id,
                &message.role,
                &content_text,
                &tool_calls_json,
                &message.tool_call_id,
            ],
        )
        .context("Failed to save message")?;

        let rowid = conn.last_insert_rowid();

        // Update conversation timestamp
        conn.execute(
            "UPDATE conversations SET updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![conversation_id],
        )?;

        // Store vector embedding if available
        if let Some(ref emb) = embedding {
            let embedding_bytes = f32_slice_to_bytes(emb);
            conn.execute(
                "INSERT INTO message_embeddings (rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![rowid, embedding_bytes],
            )?;
        }

        Ok(id)
    }

    /// Clear a conversation (soft archive: mark as archived, don't delete messages)
    pub async fn clear_conversation(&self, platform: &str, user_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;

        conn.execute(
            "UPDATE conversations SET is_archived = 1, updated_at = datetime('now')
             WHERE platform = ?1 AND user_id = ?2",
            rusqlite::params![platform, user_id],
        )?;

        Ok(())
    }

    /// Load all messages for a conversation, with raw message limit and [SUMMARY] messages first.
    #[allow(dead_code)]
    pub async fn load_messages(&self, conversation_id: &str) -> Result<Vec<ChatMessage>> {
        self.load_messages_with_limit(conversation_id, 50).await
    }

    /// Load messages for a conversation: [SUMMARY] system messages first, then the most recent
    /// `raw_limit` non-summary messages, all ordered by created_at ASC.
    pub async fn load_messages_with_limit(
        &self,
        conversation_id: &str,
        raw_limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;

        // Load all [SUMMARY] system messages ordered by created_at ASC
        let mut summary_stmt = conn.prepare(
            "SELECT role, content, tool_calls, tool_call_id
             FROM messages
             WHERE conversation_id = ?1
               AND role = 'system'
               AND content LIKE '[SUMMARY]%'
             ORDER BY created_at ASC",
        )?;
        let summaries = summary_stmt
            .query_map(rusqlite::params![conversation_id], |row| {
                parse_message_row(row)
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load summary messages")?;

        // Load the most recent raw_limit non-summary messages, re-ordered ASC
        let mut raw_stmt = conn.prepare(
            "SELECT role, content, tool_calls, tool_call_id FROM (
                SELECT role, content, tool_calls, tool_call_id, created_at
                FROM messages
                WHERE conversation_id = ?1
                  AND NOT (role = 'system' AND content LIKE '[SUMMARY]%')
                ORDER BY created_at DESC
                LIMIT ?2
            ) ORDER BY created_at ASC",
        )?;
        let raw_messages = raw_stmt
            .query_map(
                rusqlite::params![conversation_id, raw_limit as i64],
                parse_message_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load raw messages")?;

        let mut result = summaries;
        result.extend(raw_messages);
        Ok(result)
    }

    /// Conversation-scoped hybrid search using Reciprocal Rank Fusion (vector + FTS5).
    /// Falls back to FTS5-only if embeddings are not available.
    /// Only returns non-summarized messages with role 'user' or 'assistant'.
    #[allow(dead_code)]
    pub async fn search_messages_in_conversation(
        &self,
        query: &str,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let query_embedding = self.embeddings.try_embed_one(query).await;

        let conn = self.conn.lock().await;

        if let Some(ref qe) = query_embedding {
            // Hybrid search with Reciprocal Rank Fusion, scoped to conversation
            let query_bytes = f32_vec_to_bytes(qe);
            let sql = "
                WITH vec_matches AS (
                    SELECT rowid, distance,
                           row_number() OVER (ORDER BY distance) as rank_number
                    FROM message_embeddings
                    WHERE embedding MATCH ?1
                    ORDER BY distance
                    LIMIT ?2
                ),
                fts_matches AS (
                    SELECT rowid,
                           row_number() OVER (ORDER BY rank) as rank_number
                    FROM messages_fts
                    WHERE messages_fts MATCH ?3
                    LIMIT ?2
                )
                SELECT m.role, m.content, m.tool_calls, m.tool_call_id,
                       coalesce(1.0 / (60 + fts.rank_number), 0.0) * 0.5
                       + coalesce(1.0 / (60 + vec.rank_number), 0.0) * 0.5 as combined_rank
                FROM messages m
                LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
                LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
                WHERE (vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL)
                  AND m.conversation_id = ?4
                  AND m.role IN ('user', 'assistant')
                  AND (m.is_summarized IS NULL OR m.is_summarized = 0)
                ORDER BY combined_rank DESC
                LIMIT ?2
            ";

            let search_limit = (limit * 3) as i64;
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(
                    rusqlite::params![query_bytes, search_limit, query, conversation_id],
                    parse_message_row,
                )?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to hybrid-search messages in conversation")?;

            Ok(messages.into_iter().take(limit).collect())
        } else {
            // FTS5-only fallback, scoped to conversation
            let sql = "
                SELECT m.role, m.content, m.tool_calls, m.tool_call_id
                FROM messages m
                JOIN messages_fts fts ON m.rowid = fts.rowid
                WHERE messages_fts MATCH ?1
                  AND m.conversation_id = ?2
                  AND m.role IN ('user', 'assistant')
                  AND (m.is_summarized IS NULL OR m.is_summarized = 0)
                ORDER BY fts.rank
                LIMIT ?3
            ";
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(
                    rusqlite::params![query, conversation_id, limit as i64],
                    parse_message_row,
                )?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to FTS-search messages in conversation")?;

            Ok(messages)
        }
    }

    /// Hybrid search across messages using Reciprocal Rank Fusion (vector + FTS5).
    /// Falls back to FTS5-only if embeddings are not available.
    pub async fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<ChatMessage>> {
        // Try to get query embedding for vector search
        let query_embedding = self.embeddings.try_embed_one(query).await;

        let conn = self.conn.lock().await;

        if let Some(ref qe) = query_embedding {
            // Hybrid search with Reciprocal Rank Fusion
            let query_bytes = f32_vec_to_bytes(qe);
            let sql = "
                WITH vec_matches AS (
                    SELECT rowid, distance,
                           row_number() OVER (ORDER BY distance) as rank_number
                    FROM message_embeddings
                    WHERE embedding MATCH ?1
                    ORDER BY distance
                    LIMIT ?2
                ),
                fts_matches AS (
                    SELECT rowid,
                           row_number() OVER (ORDER BY rank) as rank_number
                    FROM messages_fts
                    WHERE messages_fts MATCH ?3
                    LIMIT ?2
                )
                SELECT m.role, m.content, m.tool_calls, m.tool_call_id,
                       coalesce(1.0 / (60 + fts.rank_number), 0.0) * 0.5
                       + coalesce(1.0 / (60 + vec.rank_number), 0.0) * 0.5 as combined_rank
                FROM messages m
                LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
                LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
                WHERE vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL
                ORDER BY combined_rank DESC
                LIMIT ?2
            ";

            let search_limit = (limit * 3) as i64;
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(rusqlite::params![query_bytes, search_limit, query], |row| {
                    parse_message_row(row)
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to hybrid-search messages")?;

            Ok(messages.into_iter().take(limit).collect())
        } else {
            // FTS5-only fallback
            let sql = "
                SELECT m.role, m.content, m.tool_calls, m.tool_call_id
                FROM messages m
                JOIN messages_fts fts ON m.rowid = fts.rowid
                WHERE messages_fts MATCH ?1
                ORDER BY fts.rank
                LIMIT ?2
            ";
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(rusqlite::params![query, limit as i64], |row| {
                    parse_message_row(row)
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to FTS-search messages")?;

            Ok(messages)
        }
    }

    /// Return all messages in a conversation that have not yet been summarized.
    /// Returns tuples of (message_id, role, content).
    pub async fn get_unsummarized_messages(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, role, content FROM messages
             WHERE conversation_id = ?1
               AND (is_summarized IS NULL OR is_summarized = 0)
             ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![conversation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load unsummarized messages")?;
        Ok(rows)
    }

    /// Mark a list of messages as summarized (is_summarized = 1).
    pub async fn mark_messages_summarized(&self, message_ids: &[String]) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().await;
        for id in message_ids {
            conn.execute(
                "UPDATE messages SET is_summarized = 1 WHERE id = ?1",
                rusqlite::params![id],
            )
            .context("Failed to mark message as summarized")?;
        }
        Ok(())
    }

    /// Return conversation IDs that have had activity in the last `days` days.
    pub async fn get_active_conversations(&self, days: u32) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id FROM conversations
             WHERE updated_at >= datetime('now', ?1)
             ORDER BY updated_at DESC",
        )?;
        let days_param = format!("-{} days", days);
        let ids = stmt
            .query_map(rusqlite::params![days_param], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()
            .context("Failed to load active conversations")?;
        Ok(ids)
    }
}

fn parse_message_row(row: &rusqlite::Row) -> rusqlite::Result<ChatMessage> {
    let tool_calls_json: Option<String> = row.get(2)?;
    let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());

    let content_str: Option<String> = row.get(1)?;
    Ok(ChatMessage {
        role: row.get(0)?,
        content: content_str.map(MessageContent::Text),
        tool_calls,
        tool_call_id: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, MessageContent};

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::from_text(content)),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn test_search_messages_scoped_to_conversation() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv_a = store
            .get_or_create_conversation("test", "user_a")
            .await
            .unwrap();
        let conv_b = store
            .get_or_create_conversation("test", "user_b")
            .await
            .unwrap();

        store
            .save_message(&conv_a, &make_msg("user", "I love Rust programming"))
            .await
            .unwrap();
        store
            .save_message(&conv_b, &make_msg("user", "I hate Rust programming"))
            .await
            .unwrap();

        let results = store
            .search_messages_in_conversation("Rust", &conv_a, 5)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap()
            .contains("love"));
    }

    #[tokio::test]
    async fn test_load_messages_respects_raw_limit() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv = store
            .get_or_create_conversation("test", "user_limit")
            .await
            .unwrap();

        for i in 0..60 {
            store
                .save_message(&conv, &make_msg("user", &format!("message {}", i)))
                .await
                .unwrap();
        }

        let messages = store.load_messages(&conv).await.unwrap();
        assert!(
            messages.len() <= 50,
            "Expected ≤50 messages, got {}",
            messages.len()
        );
    }

    #[tokio::test]
    async fn test_clear_archives_instead_of_deleting() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let conv = store
            .get_or_create_conversation("test", "archive_u2")
            .await
            .unwrap();
        let msg = crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some(crate::llm::MessageContent::from_text("hello world")),
            tool_calls: None,
            tool_call_id: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Clear
        store.clear_conversation("test", "archive_u2").await.unwrap();

        // Messages should still exist in DB
        let conn = store.conn.lock().await;
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
                rusqlite::params![&conv],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(msg_count > 0, "Messages must persist after archive");

        // Conversation should be marked archived
        let conn2 = store.conn.lock().await;
        let archived: Option<i64> = conn2
            .query_row(
                "SELECT is_archived FROM conversations WHERE id = ?1",
                rusqlite::params![&conv],
                |row| row.get(0),
            )
            .ok();
        drop(conn2);
        assert_eq!(archived, Some(1), "Conversation must be marked archived");
    }

    #[tokio::test]
    async fn test_get_or_create_skips_archived() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        // Create a conversation
        let conv = store
            .get_or_create_conversation("test", "archive_u1")
            .await
            .unwrap();

        // Manually archive it (simulating what clear_conversation will do)
        let conn = store.conn.lock().await;
        conn.execute(
            "UPDATE conversations SET is_archived = 1 WHERE id = ?1",
            rusqlite::params![&conv],
        )
        .unwrap();
        drop(conn);

        // get_or_create_conversation should return a NEW conversation
        let conv2 = store
            .get_or_create_conversation("test", "archive_u1")
            .await
            .unwrap();

        assert_ne!(conv, conv2, "Must create a new conversation when previous is archived");

        // The new conversation must not be archived
        let conn2 = store.conn.lock().await;
        let archived: i64 = conn2
            .query_row(
                "SELECT is_archived FROM conversations WHERE id = ?1",
                rusqlite::params![&conv2],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn2);
        assert_eq!(archived, 0, "New conversation must not be archived");
    }
}
