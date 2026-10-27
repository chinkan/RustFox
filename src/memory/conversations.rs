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
    /// Get or create an active (non-archived) conversation for a platform+bot+user.
    /// Empty `bot_id` is normalized to `"default"`. If all existing conversations for
    /// that triple are archived, a new one is created.
    ///
    /// **Legacy claim/remap:** migration backfills pre-§7.3 rows to `bot_id = "default"`.
    /// Callers that know install topology should use
    /// [`Self::get_or_create_conversation_with_claim`] with
    /// [`crate::config::Config::bot_claims_legacy_default`]. This convenience
    /// path claims only for `"main"` (multi-bot §7.3 default).
    pub async fn get_or_create_conversation(
        &self,
        platform: &str,
        bot_id: &str,
        user_id: &str,
    ) -> Result<String> {
        let claim_legacy = crate::platform::normalize_bot_id(bot_id) == "main";
        self.get_or_create_conversation_with_claim(platform, bot_id, user_id, claim_legacy)
            .await
    }

    /// Like [`Self::get_or_create_conversation`], but `claim_legacy` controls whether
    /// a miss remaps the active `(platform, "default", user_id)` row onto this
    /// `bot_id` (so upgrade history is not orphaned).
    ///
    /// Use [`crate::config::Config::bot_claims_legacy_default`] for the PO-locked
    /// policy (sole custom id claims; multi-bot only `"main"`; secondary never).
    pub async fn get_or_create_conversation_with_claim(
        &self,
        platform: &str,
        bot_id: &str,
        user_id: &str,
        claim_legacy: bool,
    ) -> Result<String> {
        let bot_id = crate::platform::normalize_bot_id(bot_id);
        let conn = self.conn.lock().await;

        // Try to find an existing active conversation
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM conversations
                 WHERE platform = ?1 AND bot_id = ?2 AND user_id = ?3
                   AND (is_archived IS NULL OR is_archived = 0)
                 ORDER BY updated_at DESC LIMIT 1",
                rusqlite::params![platform, bot_id, user_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing {
            return Ok(id);
        }

        // Legacy claim: remap active default-row to this bot_id when allowed.
        // `"default"` looking up `"default"` already returned above if present.
        if claim_legacy && bot_id != crate::platform::DEFAULT_BOT_ID {
            let legacy: Option<String> = conn
                .query_row(
                    "SELECT id FROM conversations
                     WHERE platform = ?1 AND bot_id = ?2 AND user_id = ?3
                       AND (is_archived IS NULL OR is_archived = 0)
                     ORDER BY updated_at DESC LIMIT 1",
                    rusqlite::params![platform, crate::platform::DEFAULT_BOT_ID, user_id],
                    |row| row.get(0),
                )
                .ok();

            if let Some(id) = legacy {
                conn.execute(
                    "UPDATE conversations SET bot_id = ?1, updated_at = datetime('now')
                     WHERE id = ?2",
                    rusqlite::params![bot_id, &id],
                )
                .context("Failed to claim legacy default conversation")?;
                return Ok(id);
            }
        }

        // Create a new conversation
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO conversations (id, platform, bot_id, user_id) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&id, platform, bot_id, user_id],
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
                "INSERT INTO message_embeddings (rowid, embedding, is_summarized, role) VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![rowid, embedding_bytes, &message.role],
            )?;
        }

        Ok(id)
    }

    /// Clear a conversation (soft archive: mark as archived, don't delete messages).
    /// Scoped to `(platform, bot_id, user_id)` so clearing bot A does not archive bot B.
    pub async fn clear_conversation(
        &self,
        platform: &str,
        bot_id: &str,
        user_id: &str,
    ) -> Result<()> {
        let bot_id = crate::platform::normalize_bot_id(bot_id);
        let conn = self.conn.lock().await;

        conn.execute(
            "UPDATE conversations SET is_archived = 1, updated_at = datetime('now')
             WHERE platform = ?1 AND bot_id = ?2 AND user_id = ?3",
            rusqlite::params![platform, bot_id, user_id],
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
            "SELECT m.role, m.content, m.tool_calls, m.tool_call_id
             FROM messages m
             JOIN conversations c ON m.conversation_id = c.id
             WHERE m.conversation_id = ?1
               AND m.role = 'system'
               AND m.content LIKE '[SUMMARY]%'
               AND (c.is_archived IS NULL OR c.is_archived = 0)
             ORDER BY m.created_at ASC",
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
                SELECT m.role, m.content, m.tool_calls, m.tool_call_id, m.created_at
                FROM messages m
                JOIN conversations c ON m.conversation_id = c.id
                WHERE m.conversation_id = ?1
                  AND NOT (m.role = 'system' AND m.content LIKE '[SUMMARY]%')
                  AND (c.is_archived IS NULL OR c.is_archived = 0)
                ORDER BY m.created_at DESC
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
                      AND is_summarized = 0
                      AND role IN ('user', 'assistant')
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
                       coalesce(1.0 / (?5 + fts.rank_number), 0.0) * ?7
                       + coalesce(1.0 / (?5 + vec.rank_number), 0.0) * ?6 as combined_rank
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
            let rrf_k = self.config.rrf_k;
            let rrf_weight_fts = self.config.rrf_weight_fts;
            let rrf_weight_vec = self.config.rrf_weight_vec;
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(
                    rusqlite::params![
                        query_bytes,
                        search_limit,
                        query,
                        conversation_id,
                        rrf_k,
                        rrf_weight_vec,
                        rrf_weight_fts
                    ],
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
                      AND is_summarized = 0
                      AND role IN ('user', 'assistant')
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
                       coalesce(1.0 / (?4 + fts.rank_number), 0.0) * ?6
                       + coalesce(1.0 / (?4 + vec.rank_number), 0.0) * ?5 as combined_rank
                FROM messages m
                LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
                LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
                WHERE (vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL)
                  AND m.role IN ('user', 'assistant')
                  AND (m.is_summarized IS NULL OR m.is_summarized = 0)
                ORDER BY combined_rank DESC
                LIMIT ?2
            ";
            let search_limit = (limit * 3) as i64;
            let rrf_k = self.config.rrf_k;
            let rrf_weight_fts = self.config.rrf_weight_fts;
            let rrf_weight_vec = self.config.rrf_weight_vec;
            let mut stmt = conn.prepare(sql)?;
            let messages = stmt
                .query_map(
                    rusqlite::params![
                        query_bytes,
                        search_limit,
                        query,
                        rrf_k,
                        rrf_weight_vec,
                        rrf_weight_fts
                    ],
                    parse_message_row,
                )?
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
                  AND m.role IN ('user', 'assistant')
                  AND (m.is_summarized IS NULL OR m.is_summarized = 0)
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

    /// Recent user/assistant messages for Memory browse (empty query).
    /// FTS cannot MATCH an empty string, so browse lists rows directly.
    pub async fn recent_messages(&self, limit: usize) -> Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT role, content, tool_calls, tool_call_id
             FROM messages
             WHERE role IN ('user', 'assistant')
               AND (is_summarized IS NULL OR is_summarized = 0)
               AND content IS NOT NULL
               AND length(trim(content)) > 0
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;
        let messages = stmt
            .query_map(rusqlite::params![limit as i64], parse_message_row)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list recent messages")?;
        Ok(messages)
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
        reasoning_content: None,
    })
}

#[cfg(test)]
mod tests {
    use crate::llm::{ChatMessage, MessageContent};

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::from_text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[tokio::test]
    async fn test_search_messages_scoped_to_conversation() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv_a = store
            .get_or_create_conversation("test", "default", "user_a")
            .await
            .unwrap();
        let conv_b = store
            .get_or_create_conversation("test", "default", "user_b")
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
            .get_or_create_conversation("test", "default", "user_limit")
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
    async fn test_bot_id_isolates_conversations_for_same_user() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv_a = store
            .get_or_create_conversation("telegram", "bot_a", "user1")
            .await
            .unwrap();
        let conv_b = store
            .get_or_create_conversation("telegram", "bot_b", "user1")
            .await
            .unwrap();
        assert_ne!(
            conv_a, conv_b,
            "same platform+user on different bots must get distinct conversations"
        );

        // Re-fetch returns the same ids
        let again_a = store
            .get_or_create_conversation("telegram", "bot_a", "user1")
            .await
            .unwrap();
        let again_b = store
            .get_or_create_conversation("telegram", "bot_b", "user1")
            .await
            .unwrap();
        assert_eq!(conv_a, again_a);
        assert_eq!(conv_b, again_b);
    }

    #[tokio::test]
    async fn test_main_claims_legacy_default_conversation() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        // History created under migration backfill bot_id = "default"
        let legacy_id = store
            .get_or_create_conversation("telegram", "default", "legacy_user")
            .await
            .unwrap();
        store
            .save_message(&legacy_id, &make_msg("user", "hello from before multi-bot"))
            .await
            .unwrap();

        // First lookup as primary alias "main" must reclaim the same row
        let claimed = store
            .get_or_create_conversation("telegram", "main", "legacy_user")
            .await
            .unwrap();
        assert_eq!(
            claimed, legacy_id,
            "main must reclaim the legacy default conversation id"
        );

        let conn = store.connection();
        let conn = conn.lock().await;
        let stored_bot: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = ?1",
                rusqlite::params![&claimed],
                |row| row.get(0),
            )
            .unwrap();
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations
                 WHERE platform = 'telegram' AND user_id = 'legacy_user'
                   AND (is_archived IS NULL OR is_archived = 0)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert_eq!(stored_bot, "main", "bot_id column must be remapped to main");
        assert_eq!(
            active_count, 1,
            "must not create a duplicate blank conversation"
        );

        let messages = store.load_messages(&claimed).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]
                .content
                .as_ref()
                .map(|c| c.as_text())
                .unwrap()
                .contains("hello from before multi-bot"),
            "legacy messages must still load after claim"
        );
    }

    #[tokio::test]
    async fn test_secondary_bot_does_not_steal_default_history() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let legacy_id = store
            .get_or_create_conversation("telegram", "default", "shared_user")
            .await
            .unwrap();
        store
            .save_message(&legacy_id, &make_msg("user", "primary history"))
            .await
            .unwrap();

        let researcher = store
            .get_or_create_conversation("telegram", "researcher", "shared_user")
            .await
            .unwrap();
        assert_ne!(
            researcher, legacy_id,
            "secondary bot must not steal default history"
        );

        let conn = store.connection();
        let conn = conn.lock().await;
        let legacy_bot: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = ?1",
                rusqlite::params![&legacy_id],
                |row| row.get(0),
            )
            .unwrap();
        let researcher_bot: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = ?1",
                rusqlite::params![&researcher],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert_eq!(legacy_bot, "default");
        assert_eq!(researcher_bot, "researcher");
    }

    #[tokio::test]
    async fn test_sole_custom_id_claims_legacy_default_conversation() {
        // PO lock: exactly one [[bots]] with non-default id owns claim/routing.
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let legacy_id = store
            .get_or_create_conversation("telegram", "default", "sole_custom_user")
            .await
            .unwrap();
        store
            .save_message(&legacy_id, &make_msg("user", "pre-multi-bot history"))
            .await
            .unwrap();

        let sole_bots = [crate::config::BotConfig {
            id: "fox".into(),
            bot_token: "tok-fox".into(),
            allowed_user_ids: vec![1],
            persona: "main".into(),
            system_prompt_file: None,
            model: None,
            tools: None,
            fully_silent: false,
        }];
        assert!(
            crate::config::Config::bot_claims_legacy_default(&sole_bots, "fox"),
            "sole custom id must be allowed to claim"
        );

        let claimed = store
            .get_or_create_conversation_with_claim("telegram", "fox", "sole_custom_user", true)
            .await
            .unwrap();
        assert_eq!(
            claimed, legacy_id,
            "sole custom id must reclaim the legacy default conversation id"
        );

        let conn = store.connection();
        let conn = conn.lock().await;
        let stored_bot: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = ?1",
                rusqlite::params![&claimed],
                |row| row.get(0),
            )
            .unwrap();
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations
                 WHERE platform = 'telegram' AND user_id = 'sole_custom_user'
                   AND (is_archived IS NULL OR is_archived = 0)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert_eq!(
            stored_bot, "fox",
            "bot_id column must be remapped to sole custom id"
        );
        assert_eq!(
            active_count, 1,
            "must not invent a second default / orphan blank conversation"
        );

        let messages = store.load_messages(&claimed).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]
                .content
                .as_ref()
                .map(|c| c.as_text())
                .unwrap()
                .contains("pre-multi-bot history"),
            "legacy messages must still load after sole-custom claim"
        );
    }

    #[tokio::test]
    async fn test_multi_bot_secondary_still_does_not_claim_via_policy() {
        // PO lock: two or more bots — §7.3 unchanged; secondary must not steal.
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let legacy_id = store
            .get_or_create_conversation("telegram", "default", "multi_user")
            .await
            .unwrap();
        store
            .save_message(&legacy_id, &make_msg("user", "primary history"))
            .await
            .unwrap();

        let multi_bots = [
            crate::config::BotConfig {
                id: "main".into(),
                bot_token: "tok-main".into(),
                allowed_user_ids: vec![1],
                persona: "main".into(),
                system_prompt_file: None,
                model: None,
                tools: None,
                fully_silent: false,
            },
            crate::config::BotConfig {
                id: "researcher".into(),
                bot_token: "tok-research".into(),
                allowed_user_ids: vec![1],
                persona: "researcher".into(),
                system_prompt_file: None,
                model: None,
                tools: None,
                fully_silent: false,
            },
        ];
        assert!(crate::config::Config::bot_claims_legacy_default(
            &multi_bots,
            "main"
        ));
        assert!(!crate::config::Config::bot_claims_legacy_default(
            &multi_bots,
            "researcher"
        ));

        let claim_secondary =
            crate::config::Config::bot_claims_legacy_default(&multi_bots, "researcher");
        let researcher = store
            .get_or_create_conversation_with_claim(
                "telegram",
                "researcher",
                "multi_user",
                claim_secondary,
            )
            .await
            .unwrap();
        assert_ne!(
            researcher, legacy_id,
            "multi-bot secondary must not steal default history"
        );

        let claim_main = crate::config::Config::bot_claims_legacy_default(&multi_bots, "main");
        let claimed_main = store
            .get_or_create_conversation_with_claim("telegram", "main", "multi_user", claim_main)
            .await
            .unwrap();
        assert_eq!(
            claimed_main, legacy_id,
            "multi-bot main must still claim legacy default"
        );
    }

    #[tokio::test]
    async fn test_clear_one_bot_does_not_archive_other() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let conv_a = store
            .get_or_create_conversation("telegram", "bot_a", "user1")
            .await
            .unwrap();
        let conv_b = store
            .get_or_create_conversation("telegram", "bot_b", "user1")
            .await
            .unwrap();

        store
            .clear_conversation("telegram", "bot_a", "user1")
            .await
            .unwrap();

        let conn = store.connection();
        let conn = conn.lock().await;
        let archived_a: i64 = conn
            .query_row(
                "SELECT is_archived FROM conversations WHERE id = ?1",
                rusqlite::params![&conv_a],
                |row| row.get(0),
            )
            .unwrap();
        let archived_b: i64 = conn
            .query_row(
                "SELECT is_archived FROM conversations WHERE id = ?1",
                rusqlite::params![&conv_b],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert_eq!(archived_a, 1);
        assert_eq!(archived_b, 0);

        // Clearing bot_a yields a fresh conversation; bot_b keeps its id
        let new_a = store
            .get_or_create_conversation("telegram", "bot_a", "user1")
            .await
            .unwrap();
        let still_b = store
            .get_or_create_conversation("telegram", "bot_b", "user1")
            .await
            .unwrap();
        assert_ne!(new_a, conv_a);
        assert_eq!(still_b, conv_b);
    }

    #[tokio::test]
    async fn test_migration_backfills_null_bot_id_to_default() {
        use rusqlite::Connection;

        // Simulate a pre-§7.3 DB: conversations without bot_id (or NULL).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                user_id TEXT NOT NULL,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_archived INTEGER DEFAULT 0
            );
            INSERT INTO conversations (id, platform, user_id) VALUES ('c1', 'telegram', 'u1');
            ",
        )
        .unwrap();

        // Run the same ALTER + backfill steps as run_migrations.
        conn.execute_batch("ALTER TABLE conversations ADD COLUMN bot_id TEXT;")
            .ok();
        conn.execute_batch(
            "UPDATE conversations SET bot_id = 'default' WHERE bot_id IS NULL OR bot_id = '';",
        )
        .unwrap();

        let bot_id: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = 'c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bot_id, "default");

        // Also verify open_in_memory (full migrations) yields default on empty bot_id lookup
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();
        let id = store
            .get_or_create_conversation("telegram", "", "legacy_user")
            .await
            .unwrap();
        let conn = store.connection();
        let conn = conn.lock().await;
        let stored: String = conn
            .query_row(
                "SELECT bot_id FROM conversations WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "default");
    }

    #[tokio::test]
    async fn test_clear_archives_instead_of_deleting() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let conv = store
            .get_or_create_conversation("test", "default", "archive_u2")
            .await
            .unwrap();
        let msg = crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some(crate::llm::MessageContent::from_text("hello world")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Clear
        store
            .clear_conversation("test", "default", "archive_u2")
            .await
            .unwrap();

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
            .get_or_create_conversation("test", "default", "archive_u1")
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
            .get_or_create_conversation("test", "default", "archive_u1")
            .await
            .unwrap();

        assert_ne!(
            conv, conv2,
            "Must create a new conversation when previous is archived"
        );

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

    #[tokio::test]
    async fn test_search_messages_finds_archived_content() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let conv = store
            .get_or_create_conversation("test", "default", "archive_search_u1")
            .await
            .unwrap();
        let msg = crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some(crate::llm::MessageContent::from_text(
                "I love Rust programming and async runtimes",
            )),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Archive
        store
            .clear_conversation("test", "default", "archive_search_u1")
            .await
            .unwrap();

        // search_messages should still find the content from archived conversations
        let results = store.search_messages("Rust", 5).await.unwrap();
        assert!(
            !results.is_empty(),
            "search_messages must find content in archived conversations"
        );
        assert!(
            results.iter().any(|m| m
                .content
                .as_ref()
                .is_some_and(|c| c.as_text().contains("Rust"))),
            "Archived message content must be searchable"
        );
    }

    #[tokio::test]
    async fn test_load_messages_excludes_archived() {
        let store = crate::memory::MemoryStore::open_in_memory().unwrap();

        let conv = store
            .get_or_create_conversation("test", "default", "archive_u3")
            .await
            .unwrap();
        let msg = crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some(crate::llm::MessageContent::from_text("test")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Archive
        store
            .clear_conversation("test", "default", "archive_u3")
            .await
            .unwrap();

        // load_messages should return empty for an archived conversation
        let messages = store.load_messages(&conv).await.unwrap();
        assert!(
            messages.is_empty(),
            "Archived conversation should return no messages via load_messages"
        );
    }
}
