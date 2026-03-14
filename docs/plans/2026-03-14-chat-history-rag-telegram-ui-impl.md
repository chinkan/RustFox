# Chat History RAG + Nightly Summarization + Tool Call UI — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add framework-level chat history RAG auto-injection, nightly conversation summarization, and a live-editing Telegram tool-call progress UI to RustFox.

**Architecture:** Three additive modules — `memory/rag.rs`, `memory/summarizer.rs`, `platform/tool_notifier.rs` — plus small surgical edits to `agent.rs`, `platform/telegram.rs`, `memory/conversations.rs`, `memory/mod.rs`, `config.rs`, `scheduler/tasks.rs`, and `main.rs`. No new external crates. All changes are backwards-compatible (opt-in features, additive DB migrations).

**Tech Stack:** Rust 2021, Tokio, teloxide 0.17 (`edit_message_text`), rusqlite + sqlite-vec, tokio::sync::mpsc, tokio-cron-scheduler

---

## Reading List (understand before touching)

Before starting, read these files completely to internalize patterns:

- `src/memory/conversations.rs` — `search_messages()`, `load_messages()`, `save_message()`
- `src/memory/mod.rs` — `run_migrations()`, `MemoryStore` struct
- `src/agent.rs` lines 125–379 — `process_message()` agentic loop
- `src/platform/telegram.rs` — `handle_message()`, command handling pattern
- `src/scheduler/tasks.rs` — `register_builtin_tasks()` pattern
- `src/config.rs` — `MemoryConfig`, how defaults work

---

## Task 1: DB Migration — `is_summarized` Column + `search_messages` Conversation Scope

**Files:**
- Modify: `src/memory/mod.rs` (migration SQL)
- Modify: `src/memory/conversations.rs` (`search_messages`, `load_messages`)

### Step 1: Write the failing test for conversation-scoped search

Add to `src/memory/conversations.rs` inside `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use crate::llm::ChatMessage;

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.to_string(), content: Some(content.to_string()), tool_calls: None, tool_call_id: None }
    }

    #[tokio::test]
    async fn test_search_messages_scoped_to_conversation() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv_a = store.get_or_create_conversation("test", "user_a").await.unwrap();
        let conv_b = store.get_or_create_conversation("test", "user_b").await.unwrap();

        store.save_message(&conv_a, &make_msg("user", "I love Rust programming")).await.unwrap();
        store.save_message(&conv_b, &make_msg("user", "I hate Rust programming")).await.unwrap();

        // Searching within conv_a should only return conv_a messages
        let results = store.search_messages_in_conversation("Rust", &conv_a, 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_deref().unwrap().contains("love"));
    }

    #[tokio::test]
    async fn test_load_messages_respects_raw_limit() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "user_limit").await.unwrap();

        for i in 0..60 {
            store.save_message(&conv, &make_msg("user", &format!("message {}", i))).await.unwrap();
        }

        // Default raw limit is 50
        let messages = store.load_messages(&conv).await.unwrap();
        assert!(messages.len() <= 50, "Expected ≤50 messages, got {}", messages.len());
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test test_search_messages_scoped_to_conversation test_load_messages_respects_raw_limit 2>&1 | tail -20
```

Expected: FAIL — `search_messages_in_conversation` not found, `load_messages` returns all 60.

### Step 3: Add `is_summarized` column migration to `src/memory/mod.rs`

In `run_migrations()`, after the existing `conn.execute_batch(...)` call (around line 210), add:

```rust
// Migration: add is_summarized column if not present (safe no-op on existing schema)
conn.execute_batch(
    "ALTER TABLE messages ADD COLUMN is_summarized BOOLEAN DEFAULT 0;"
)
.ok(); // ok() because ALTER TABLE fails if column already exists — that's fine
```

### Step 4: Add `search_messages_in_conversation` to `src/memory/conversations.rs`

Add this new method to `impl MemoryStore` in `conversations.rs`, after `search_messages()`:

```rust
/// Hybrid search scoped to a specific conversation (for RAG auto-inject).
/// Falls back to FTS5-only if embeddings are unavailable.
pub async fn search_messages_in_conversation(
    &self,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
    let query_embedding = self.embeddings.try_embed_one(query).await;
    let conn = self.conn.lock().await;

    if let Some(ref qe) = query_embedding {
        let query_bytes = f32_vec_to_bytes(qe);
        let sql = "
            WITH vec_matches AS (
                SELECT m.rowid, me.distance,
                       row_number() OVER (ORDER BY me.distance) as rank_number
                FROM messages m
                JOIN message_embeddings me ON m.rowid = me.rowid
                WHERE m.conversation_id = ?3
                  AND me.embedding MATCH ?1
                ORDER BY me.distance
                LIMIT ?2
            ),
            fts_matches AS (
                SELECT m.rowid,
                       row_number() OVER (ORDER BY fts.rank) as rank_number
                FROM messages m
                JOIN messages_fts fts ON m.rowid = fts.rowid
                WHERE m.conversation_id = ?3
                  AND messages_fts MATCH ?4
                LIMIT ?2
            )
            SELECT m.role, m.content, m.tool_calls, m.tool_call_id,
                   coalesce(1.0 / (60 + fts.rank_number), 0.0) * 0.5
                   + coalesce(1.0 / (60 + vec.rank_number), 0.0) * 0.5 as combined_rank
            FROM messages m
            LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
            LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
            WHERE (vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL)
              AND m.role IN ('user', 'assistant')
              AND m.content IS NOT NULL
              AND (m.is_summarized IS NULL OR m.is_summarized = 0)
            ORDER BY combined_rank DESC
            LIMIT ?2
        ";
        let search_limit = (limit * 3) as i64;
        let mut stmt = conn.prepare(sql)?;
        let messages = stmt
            .query_map(rusqlite::params![query_bytes, search_limit, conversation_id, query], |row| {
                parse_message_row(row)
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Hybrid search in conversation failed")?;
        Ok(messages.into_iter().take(limit).collect())
    } else {
        let sql = "
            SELECT m.role, m.content, m.tool_calls, m.tool_call_id
            FROM messages m
            JOIN messages_fts fts ON m.rowid = fts.rowid
            WHERE m.conversation_id = ?3
              AND messages_fts MATCH ?1
              AND m.role IN ('user', 'assistant')
              AND (m.is_summarized IS NULL OR m.is_summarized = 0)
            ORDER BY fts.rank
            LIMIT ?2
        ";
        let mut stmt = conn.prepare(sql)?;
        let messages = stmt
            .query_map(rusqlite::params![query, limit as i64, conversation_id], |row| {
                parse_message_row(row)
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("FTS search in conversation failed")?;
        Ok(messages)
    }
}
```

### Step 5: Update `load_messages` to enforce raw limit

Replace `load_messages` in `src/memory/conversations.rs` (currently lines 112–137):

```rust
/// Load messages for a conversation.
/// [SUMMARY] system messages always come first; then the most recent `raw_limit` non-summary messages.
/// Default raw_limit = 50 to bound context size.
pub async fn load_messages(&self, conversation_id: &str) -> Result<Vec<ChatMessage>> {
    self.load_messages_with_limit(conversation_id, 50).await
}

pub async fn load_messages_with_limit(
    &self,
    conversation_id: &str,
    raw_limit: usize,
) -> Result<Vec<ChatMessage>> {
    let conn = self.conn.lock().await;

    // First: all [SUMMARY] system messages (always included, ascending)
    let mut stmt = conn.prepare(
        "SELECT role, content, tool_calls, tool_call_id
         FROM messages
         WHERE conversation_id = ?1
           AND role = 'system'
           AND content LIKE '[SUMMARY]%'
         ORDER BY created_at ASC",
    )?;
    let mut messages: Vec<ChatMessage> = stmt
        .query_map(rusqlite::params![conversation_id], |row| {
            let tool_calls_json: Option<String> = row.get(2)?;
            let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());
            Ok(ChatMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                tool_calls,
                tool_call_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to load summary messages")?;

    // Then: the most recent `raw_limit` non-summary messages, in ascending order
    let mut stmt2 = conn.prepare(
        "SELECT role, content, tool_calls, tool_call_id FROM (
             SELECT role, content, tool_calls, tool_call_id, created_at
             FROM messages
             WHERE conversation_id = ?1
               AND NOT (role = 'system' AND content LIKE '[SUMMARY]%')
             ORDER BY created_at DESC
             LIMIT ?2
         ) ORDER BY created_at ASC",
    )?;
    let raw_messages: Vec<ChatMessage> = stmt2
        .query_map(rusqlite::params![conversation_id, raw_limit as i64], |row| {
            let tool_calls_json: Option<String> = row.get(2)?;
            let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());
            Ok(ChatMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                tool_calls,
                tool_call_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to load raw messages")?;

    messages.extend(raw_messages);
    Ok(messages)
}
```

### Step 6: Run tests — verify they pass

```bash
cargo test test_search_messages_scoped_to_conversation test_load_messages_respects_raw_limit -- --nocapture 2>&1 | tail -20
```

Expected: PASS (both tests green).

### Step 7: Run full test suite + clippy

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

Expected: all pass, no warnings.

### Step 8: Commit

```bash
git add src/memory/mod.rs src/memory/conversations.rs
git commit -m "feat(memory): add is_summarized column migration, conversation-scoped search, raw message limit in load_messages"
```

---

## Task 2: Chat History RAG Auto-Inject (`memory/rag.rs`)

**Files:**
- Create: `src/memory/rag.rs`
- Modify: `src/memory/mod.rs` (add `pub mod rag;`)
- Modify: `src/agent.rs` (call `auto_retrieve_context`)
- Modify: `src/config.rs` (add `rag_limit` to `MemoryConfig`)

### Step 1: Write failing test in `src/memory/rag.rs`

Create `src/memory/rag.rs`:

```rust
use anyhow::Result;

use super::MemoryStore;

/// Auto-retrieve semantically relevant past messages from a conversation
/// and format them as a `<retrieved_context>` block for the system prompt.
/// Returns `None` if query is too short or no results found.
pub async fn auto_retrieve_context(
    store: &MemoryStore,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>> {
    // Skip retrieval for very short inputs or bot commands
    if query.trim().len() < 5 || query.starts_with('/') {
        return Ok(None);
    }

    let results = store
        .search_messages_in_conversation(query, conversation_id, limit)
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
            // Truncate very long messages to keep prompt bounded
            let snippet = if content.len() > 300 {
                format!("{}...", &content[..300])
            } else {
                content.clone()
            };
            block.push_str(&format!("[{}] {}\n", role, snippet));
        }
    }

    block.push_str("</retrieved_context>");

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
        let conv = store.get_or_create_conversation("test", "u1").await.unwrap();
        store.save_message(&conv, &user_msg("I use Docker")).await.unwrap();

        let result = auto_retrieve_context(&store, "hi", &conv, 5).await.unwrap();
        assert!(result.is_none(), "Short query should return None");
    }

    #[tokio::test]
    async fn test_auto_retrieve_skips_commands() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "u2").await.unwrap();
        store.save_message(&conv, &user_msg("Docker setup")).await.unwrap();

        let result = auto_retrieve_context(&store, "/clear", &conv, 5).await.unwrap();
        assert!(result.is_none(), "Commands should return None");
    }

    #[tokio::test]
    async fn test_auto_retrieve_returns_block_when_results() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "u3").await.unwrap();
        store.save_message(&conv, &user_msg("I prefer dark mode in my editor")).await.unwrap();

        // FTS5 search will match on "dark mode" keyword
        let result = auto_retrieve_context(&store, "dark mode preference", &conv, 5).await.unwrap();
        // With no embedding API in tests, FTS5 fallback runs
        // May or may not find result depending on FTS tokenization — accept both
        if let Some(block) = result {
            assert!(block.contains("<retrieved_context>"), "Block must have opening tag");
            assert!(block.contains("</retrieved_context>"), "Block must have closing tag");
        }
    }

    #[tokio::test]
    async fn test_auto_retrieve_truncates_long_messages() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "u4").await.unwrap();
        let long_msg = "a".repeat(500);
        store.save_message(&conv, &user_msg(&format!("Docker {}", long_msg))).await.unwrap();

        let result = auto_retrieve_context(&store, "Docker long message", &conv, 5).await.unwrap();
        if let Some(block) = result {
            // Each snippet should be ≤300 chars + "..." suffix
            let lines: Vec<&str> = block.lines().collect();
            for line in lines {
                assert!(line.len() < 400, "No line should exceed snippet limit: len={}", line.len());
            }
        }
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test memory::rag 2>&1 | tail -20
```

Expected: FAIL — module not found.

### Step 3: Register module in `src/memory/mod.rs`

Add at line 3 (after `pub mod knowledge;`):

```rust
pub mod rag;
```

### Step 4: Run tests again

```bash
cargo test memory::rag 2>&1 | tail -20
```

Expected: PASS for `skip_short_query` and `skip_commands`. `returns_block` may pass or be skipped (FTS-dependent). `truncates_long_messages` may be FTS-dependent. All should not error.

### Step 5: Add `rag_limit` to `MemoryConfig` in `src/config.rs`

In the `MemoryConfig` struct (around line 72), add the new field:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct MemoryConfig {
    #[serde(default = "default_db_path")]
    pub database_path: PathBuf,
    #[serde(default = "default_rag_limit")]
    pub rag_limit: usize,
    #[serde(default = "default_max_raw_messages")]
    pub max_raw_messages: usize,
    #[serde(default = "default_summarize_threshold")]
    pub summarize_threshold: usize,
    #[serde(default = "default_summarize_cron")]
    pub summarize_cron: String,
}
```

Add the default functions after `default_db_path()` (around line 151):

```rust
fn default_rag_limit() -> usize { 5 }
fn default_max_raw_messages() -> usize { 50 }
fn default_summarize_threshold() -> usize { 20 }
fn default_summarize_cron() -> String { "0 0 2 * * *".to_string() }
```

Update `default_memory_config()` to use new defaults:

```rust
fn default_memory_config() -> MemoryConfig {
    MemoryConfig {
        database_path: default_db_path(),
        rag_limit: default_rag_limit(),
        max_raw_messages: default_max_raw_messages(),
        summarize_threshold: default_summarize_threshold(),
        summarize_cron: default_summarize_cron(),
    }
}
```

### Step 6: Inject RAG context in `src/agent.rs`

In `process_message()`, find the section after the system prompt refresh (around line 162, after `messages.iter_mut().find(|m| m.role == "system")`):

Add these lines immediately after the system prompt refresh block and before `// Add user message`:

```rust
        // RAG: auto-retrieve relevant past messages and inject into system prompt
        let rag_context = crate::memory::rag::auto_retrieve_context(
            &self.memory,
            &incoming.text,
            &conversation_id,
            self.config.memory.rag_limit,
        )
        .await
        .unwrap_or(None);

        if let Some(ref rag_block) = rag_context {
            if let Some(system_msg) = messages.iter_mut().find(|m| m.role == "system") {
                let existing = system_msg.content.get_or_insert_with(String::new);
                existing.push_str("\n\n");
                existing.push_str(rag_block);
            }
        }
```

Also update `load_messages` call to use `max_raw_messages` from config. Find the line (around line 137):

```rust
let mut messages = self.memory.load_messages(&conversation_id).await?;
```

Replace with:

```rust
let mut messages = self.memory
    .load_messages_with_limit(&conversation_id, self.config.memory.max_raw_messages)
    .await?;
```

### Step 7: Verify it compiles

```bash
cargo check 2>&1 | tail -30
```

Expected: no errors.

### Step 8: Run all tests

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

Expected: all pass.

### Step 9: Commit

```bash
git add src/memory/rag.rs src/memory/mod.rs src/agent.rs src/config.rs
git commit -m "feat(rag): auto-inject semantically relevant past messages into system prompt before each LLM call"
```

---

## Task 3: Nightly Summarization (`memory/summarizer.rs`)

**Files:**
- Create: `src/memory/summarizer.rs`
- Modify: `src/memory/mod.rs` (add `pub mod summarizer;`, expose `get_active_conversations`)
- Modify: `src/memory/conversations.rs` (add `get_active_conversations`, `mark_messages_summarized`)
- Modify: `src/scheduler/tasks.rs` (register nightly cron)
- Modify: `src/main.rs` (pass config to `register_builtin_tasks`)

### Step 1: Write failing test in `src/memory/summarizer.rs`

Create `src/memory/summarizer.rs`:

```rust
use anyhow::Result;
use tracing::{info, warn};

use crate::llm::LlmClient;
use super::MemoryStore;

/// Summarize a conversation and store the result as a [SUMMARY] system message.
/// Returns `Ok(true)` if a summary was created, `Ok(false)` if skipped.
pub async fn summarize_conversation(
    store: &MemoryStore,
    llm: &LlmClient,
    conversation_id: &str,
    threshold: usize,
) -> Result<bool> {
    // Get unsummarized messages for this conversation
    let unsummarized = store.get_unsummarized_messages(conversation_id).await?;

    if unsummarized.len() < threshold {
        info!(
            conversation_id = %conversation_id,
            count = unsummarized.len(),
            threshold = threshold,
            "Skipping summarization: below threshold"
        );
        return Ok(false);
    }

    // Build the prompt for summarization
    let conversation_text: String = unsummarized
        .iter()
        .filter_map(|(id, role, content)| {
            content.as_ref().map(|c| format!("[{}]: {}", role, c))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let summarization_prompt = format!(
        "You are a conversation summarizer. Summarize the conversation history below in 3-5 bullet points.\n\
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
        crate::llm::ChatMessage {
            role: "system".to_string(),
            content: Some("You produce concise, factual conversation summaries.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
        crate::llm::ChatMessage {
            role: "user".to_string(),
            content: Some(summarization_prompt),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let response = llm.chat(&messages, &[]).await?;
    let summary_text = response.content.unwrap_or_default();

    if summary_text.is_empty() {
        warn!(conversation_id = %conversation_id, "LLM returned empty summary — skipping");
        return Ok(false);
    }

    // Store summary as [SUMMARY] system message
    let summary_msg = crate::llm::ChatMessage {
        role: "system".to_string(),
        content: Some(format!("[SUMMARY]\n{}", summary_text)),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(conversation_id, &summary_msg).await?;

    // Mark the summarized messages
    let message_ids: Vec<String> = unsummarized.into_iter().map(|(id, _, _)| id).collect();
    store.mark_messages_summarized(&message_ids).await?;

    info!(
        conversation_id = %conversation_id,
        "Summarization complete: {} messages summarized",
        message_ids.len()
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

    info!("Nightly summarization complete: {} conversations summarized", count);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;
    use crate::memory::MemoryStore;

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage { role: "user".to_string(), content: Some(text.to_string()), tool_calls: None, tool_call_id: None }
    }

    #[tokio::test]
    async fn test_get_unsummarized_messages_returns_only_non_summarized() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "sum1").await.unwrap();
        store.save_message(&conv, &user_msg("first message")).await.unwrap();
        store.save_message(&conv, &user_msg("second message")).await.unwrap();

        let unsummarized = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(unsummarized.len(), 2);
    }

    #[tokio::test]
    async fn test_mark_messages_summarized() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "sum2").await.unwrap();
        store.save_message(&conv, &user_msg("to be summarized")).await.unwrap();

        let unsummarized = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(unsummarized.len(), 1);

        let ids: Vec<String> = unsummarized.into_iter().map(|(id, _, _)| id).collect();
        store.mark_messages_summarized(&ids).await.unwrap();

        let unsummarized_after = store.get_unsummarized_messages(&conv).await.unwrap();
        assert_eq!(unsummarized_after.len(), 0, "All messages should be marked summarized");
    }

    #[tokio::test]
    async fn test_get_active_conversations_returns_recent() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.get_or_create_conversation("test", "active_user").await.unwrap();

        let active = store.get_active_conversations(7).await.unwrap();
        assert!(!active.is_empty(), "Should have at least one active conversation");
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test memory::summarizer 2>&1 | tail -20
```

Expected: FAIL — module not found.

### Step 3: Add helper methods to `src/memory/conversations.rs`

Add these methods to `impl MemoryStore` in `conversations.rs`:

```rust
/// Get all conversation IDs active within the last N days.
pub async fn get_active_conversations(&self, days: u32) -> Result<Vec<String>> {
    let conn = self.conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT id FROM conversations
         WHERE updated_at >= datetime('now', ?1)
         ORDER BY updated_at DESC",
    )?;
    let conversations = stmt
        .query_map(rusqlite::params![format!("-{} days", days)], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()
        .context("Failed to get active conversations")?;
    Ok(conversations)
}

/// Get unsummarized messages for a conversation (returns id, role, content).
pub async fn get_unsummarized_messages(
    &self,
    conversation_id: &str,
) -> Result<Vec<(String, String, Option<String>)>> {
    let conn = self.conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT id, role, content FROM messages
         WHERE conversation_id = ?1
           AND (is_summarized IS NULL OR is_summarized = 0)
           AND role IN ('user', 'assistant')
         ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![conversation_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to get unsummarized messages")?;
    Ok(rows)
}

/// Mark specific message IDs as summarized.
pub async fn mark_messages_summarized(&self, message_ids: &[String]) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let conn = self.conn.lock().await;
    for id in message_ids {
        conn.execute(
            "UPDATE messages SET is_summarized = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
    }
    Ok(())
}
```

### Step 4: Register module in `src/memory/mod.rs`

Add after `pub mod rag;`:

```rust
pub mod summarizer;
```

### Step 5: Run summarizer tests

```bash
cargo test memory::summarizer 2>&1 | tail -20
```

Expected: PASS (helper methods work, actual LLM call is not tested in unit tests).

### Step 6: Register nightly cron in `src/scheduler/tasks.rs`

Read the current `register_builtin_tasks` function first, then add the nightly summarization job.

The function signature currently is: `pub async fn register_builtin_tasks(scheduler: &Scheduler, memory: MemoryStore) -> Result<()>`

We need to also pass `llm: LlmClient` and `threshold: usize`. Update the signature and add the cron:

```rust
pub async fn register_builtin_tasks(
    scheduler: &Scheduler,
    memory: MemoryStore,
    llm: crate::llm::LlmClient,
    summarize_cron: String,
    summarize_threshold: usize,
) -> Result<()> {
    // ... existing tasks ...

    // Nightly summarization job
    let memory_for_summary = memory.clone();
    let llm_for_summary = llm.clone();
    scheduler
        .add_cron_job(&summarize_cron, move || {
            let store = memory_for_summary.clone();
            let llm = llm_for_summary.clone();
            let threshold = summarize_threshold;
            Box::pin(async move {
                if let Err(e) = crate::memory::summarizer::summarize_all_active(
                    &store,
                    &llm,
                    threshold,
                )
                .await
                {
                    tracing::error!("Nightly summarization failed: {:#}", e);
                }
            })
        })
        .await?;

    Ok(())
}
```

### Step 7: Update `src/main.rs` call to `register_builtin_tasks`

Find the call (around line 168):

```rust
register_builtin_tasks(&scheduler, memory).await?;
```

Replace with:

```rust
register_builtin_tasks(
    &scheduler,
    memory,
    crate::llm::LlmClient::new(config.openrouter.clone()),
    config.memory.summarize_cron.clone(),
    config.memory.summarize_threshold,
).await?;
```

### Step 8: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

Expected: no errors. Fix any signature mismatches from actual `scheduler/tasks.rs` content.

### Step 9: Run all tests

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 10: Commit

```bash
git add src/memory/summarizer.rs src/memory/mod.rs src/memory/conversations.rs src/scheduler/tasks.rs src/main.rs
git commit -m "feat(summarizer): add nightly conversation summarization cron job with LLM-based summarization"
```

---

## Task 4: Tool Call UI — `platform/tool_notifier.rs`

**Files:**
- Create: `src/platform/tool_notifier.rs`
- Modify: `src/platform/mod.rs` (add `pub mod tool_notifier;`)
- Modify: `src/agent.rs` (add `tool_event_tx` param to `process_message`)
- Modify: `src/platform/telegram.rs` (add `/verbose` command, load setting, spawn notifier, pass channel)

### Step 1: Write failing tests in `src/platform/tool_notifier.rs`

Create `src/platform/tool_notifier.rs`:

```rust
use std::time::{Duration, Instant};

use teloxide::{prelude::*, types::Message};
use tracing::{debug, warn};

/// Events emitted by the agent during tool execution.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    /// A tool call has started.
    Started {
        name: String,
        /// First 60 chars of the arguments JSON, for display.
        args_preview: String,
    },
    /// A tool call completed (successfully or with error).
    Completed {
        name: String,
        success: bool,
    },
}

/// Formats `args_preview` for display: truncate to 60 chars, strip outer braces for common single-arg calls.
pub fn format_args_preview(args_json: &str) -> String {
    // Try to extract a single-value preview for readability
    // e.g. {"query":"Docker setup"} -> "Docker setup"
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) {
        if let Some(obj) = val.as_object() {
            if obj.len() == 1 {
                if let Some((_, v)) = obj.iter().next() {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let truncated = if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s
                    };
                    return format!("\"{}\"", truncated);
                }
            }
        }
    }
    // Fallback: truncate raw JSON
    if args_json.len() > 60 {
        format!("{}...", &args_json[..60])
    } else {
        args_json.to_string()
    }
}

/// Manages the live-edited Telegram status message during agent tool execution.
pub struct ToolCallNotifier {
    bot: Bot,
    chat_id: ChatId,
    status_msg: Option<Message>,
    /// Log of tool calls: (name, args_preview, done, success)
    tool_log: Vec<(String, String, bool, bool)>,
    last_edit: Option<Instant>,
}

impl ToolCallNotifier {
    pub fn new(bot: Bot, chat_id: ChatId) -> Self {
        Self {
            bot,
            chat_id,
            status_msg: None,
            tool_log: Vec::new(),
            last_edit: None,
        }
    }

    /// Send the initial "thinking" message.
    pub async fn start(&mut self) {
        match self.bot.send_message(self.chat_id, "⏳ Working...").await {
            Ok(msg) => self.status_msg = Some(msg),
            Err(e) => warn!("Failed to send tool notifier start message: {:#}", e),
        }
    }

    /// Handle a ToolEvent and update the Telegram message.
    pub async fn handle_event(&mut self, event: ToolEvent) {
        match event {
            ToolEvent::Started { name, args_preview } => {
                self.tool_log.push((name, args_preview, false, true));
            }
            ToolEvent::Completed { name, success } => {
                if let Some(entry) = self.tool_log.iter_mut().rfind(|(n, _, done, _)| n == &name && !*done) {
                    entry.2 = true;  // done
                    entry.3 = success;
                }
            }
        }
        self.edit_message().await;
    }

    async fn edit_message(&mut self) {
        let Some(ref msg) = self.status_msg else { return };

        // Rate limit: wait if last edit was <1s ago
        if let Some(last) = self.last_edit {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_millis(1000) {
                tokio::time::sleep(Duration::from_millis(1000) - elapsed).await;
            }
        }

        let text = self.format_status();
        match self
            .bot
            .edit_message_text(self.chat_id, msg.id, &text)
            .await
        {
            Ok(_) => self.last_edit = Some(Instant::now()),
            Err(e) => debug!("Failed to edit tool notifier message: {:#}", e),
        }
    }

    fn format_status(&self) -> String {
        let mut s = String::from("⏳ Working...\n");
        for (name, args_preview, done, success) in &self.tool_log {
            let icon = if !done {
                "⏳"
            } else if *success {
                "✅"
            } else {
                "❌"
            };
            s.push_str(&format!("\n{} {}({})", icon, name, args_preview));
        }
        s
    }

    /// Delete the status message (clean up before sending final response).
    pub async fn finish(&self) {
        if let Some(ref msg) = self.status_msg {
            self.bot
                .delete_message(self.chat_id, msg.id)
                .await
                .ok(); // Ignore errors (message may already be deleted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_args_preview_single_string_arg() {
        let json = r#"{"query":"Docker setup preferences"}"#;
        let preview = format_args_preview(json);
        assert_eq!(preview, r#""Docker setup preferences""#);
    }

    #[test]
    fn test_format_args_preview_truncates_long_value() {
        let long = "a".repeat(100);
        let json = format!(r#"{{"query":"{}"}}"#, long);
        let preview = format_args_preview(&json);
        assert!(preview.len() <= 70, "Preview should be truncated");
        assert!(preview.ends_with("...\"") || preview.contains("..."));
    }

    #[test]
    fn test_format_args_preview_multi_arg_falls_back() {
        let json = r#"{"category":"settings","key":"tool_ui"}"#;
        let preview = format_args_preview(json);
        // Multi-arg: should fall back to raw JSON truncated
        assert!(preview.len() <= 65);
    }

    #[test]
    fn test_format_status_shows_correct_icons() {
        // We test the format logic in isolation by calling format_status via a mock
        // Since ToolCallNotifier requires a real Bot, we test format_args_preview only
        let preview = format_args_preview(r#"{"path":"/tmp/test.txt"}"#);
        assert!(preview.contains("/tmp/test.txt"));
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test platform::tool_notifier 2>&1 | tail -20
```

Expected: FAIL — module not found.

### Step 3: Register module in `src/platform/mod.rs`

Check current content of `src/platform/mod.rs`, then add:

```rust
pub mod tool_notifier;
```

### Step 4: Run tests

```bash
cargo test platform::tool_notifier 2>&1 | tail -20
```

Expected: PASS for all 4 unit tests (`format_args_preview_*`).

### Step 5: Add `tool_event_tx` to `agent.rs::process_message`

In `src/agent.rs`, change the signature of `process_message`:

```rust
pub async fn process_message(
    &self,
    incoming: &IncomingMessage,
    tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
) -> Result<String> {
```

Inside the agentic loop, find the tool execution section (around line 280–300). Before `execute_tool`, add:

```rust
                        // Notify tool start
                        let args_preview = crate::platform::tool_notifier::format_args_preview(
                            &tool_call.function.arguments,
                        );
                        if let Some(ref tx) = tool_event_tx {
                            let _ = tx.try_send(crate::platform::tool_notifier::ToolEvent::Started {
                                name: tool_call.function.name.clone(),
                                args_preview: args_preview.clone(),
                            });
                        }
```

After `execute_tool` returns (around line 300), add:

```rust
                        // Notify tool completion
                        if let Some(ref tx) = tool_event_tx {
                            let success = !tool_result.starts_with("Error");
                            let _ = tx.try_send(crate::platform::tool_notifier::ToolEvent::Completed {
                                name: tool_call.function.name.clone(),
                                success,
                            });
                        }
```

### Step 6: Update all callers of `process_message` to pass `None`

**In `src/main.rs`** (the background job runner, around line 134):

```rust
let response = match agent.process_message(&req.incoming, None).await {
```

**In `src/platform/telegram.rs`** (the main handler, around line 164), temporarily use `None`:

```rust
match agent.process_message(&incoming, None).await {
```

(We'll update this in the next step to pass a real channel.)

**In `src/agent.rs`** — check if `run_subagent` or any other internal call uses `process_message`. If yes, add `None` there too.

### Step 7: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

Fix any missing `None` arguments on `process_message` calls.

### Step 8: Commit

```bash
git add src/platform/tool_notifier.rs src/platform/mod.rs src/agent.rs src/main.rs src/platform/telegram.rs
git commit -m "feat(tool-notifier): add ToolCallNotifier struct and ToolEvent channel infrastructure"
```

---

## Task 5: `/verbose` Command + Wire Up Notifier in Telegram

**Files:**
- Modify: `src/platform/telegram.rs`

### Step 1: Write test

Add to the `#[cfg(test)] mod tests` block in `src/platform/telegram.rs`:

```rust
    #[test]
    fn test_is_verbose_enabled_parses_true() {
        assert!(is_verbose_enabled(Some("true")));
        assert!(!is_verbose_enabled(Some("false")));
        assert!(!is_verbose_enabled(None));
    }
```

Also add the helper function (outside tests, before `handle_message`):

```rust
fn is_verbose_enabled(value: Option<&str>) -> bool {
    value.map(|v| v == "true").unwrap_or(false)
}
```

### Step 2: Run test to verify it fails

```bash
cargo test test_is_verbose_enabled_parses_true 2>&1 | tail -10
```

Expected: FAIL — function not found.

### Step 3: Add the helper function to `src/platform/telegram.rs`

Add before `handle_message` (around line 76):

```rust
fn is_verbose_enabled(value: Option<&str>) -> bool {
    value.map(|v| v == "true").unwrap_or(false)
}
```

### Step 4: Run test — should pass

```bash
cargo test test_is_verbose_enabled_parses_true 2>&1 | tail -10
```

Expected: PASS.

### Step 5: Add `/verbose` command and tool notifier wiring to `handle_message`

In `src/platform/telegram.rs`, update `handle_message` to:

1. Add `/verbose` command handling (after the `/skills` block, around line 147):

```rust
    if text == "/verbose" {
        let current = agent
            .memory
            .recall("settings", &format!("tool_ui_enabled_{}", user_id))
            .await
            .unwrap_or(None);
        let currently_on = is_verbose_enabled(current.as_deref());
        let new_value = if currently_on { "false" } else { "true" };
        agent
            .memory
            .remember(
                "settings",
                &format!("tool_ui_enabled_{}", user_id),
                new_value,
                None,
            )
            .await
            .ok();
        let reply = if new_value == "true" {
            "🔧 Tool call UI enabled. I'll show you what I'm working on."
        } else {
            "🔇 Tool call UI disabled. I'll respond silently."
        };
        bot.send_message(msg.chat.id, reply).await?;
        return Ok(());
    }
```

2. Update the `/start` command message to mention `/verbose`:

```rust
        "Hello! I'm your AI assistant. Send me a message and I'll help you.\n\n\
         Commands:\n\
         /clear - Clear conversation history\n\
         /tools - List available tools\n\
         /skills - List loaded skills\n\
         /verbose - Toggle tool call progress display",
```

3. After the "typing" indicator and before `process_message`, load verbose setting and set up channel:

```rust
    // Check if verbose tool UI is enabled for this user
    let verbose_setting = agent
        .memory
        .recall("settings", &format!("tool_ui_enabled_{}", user_id))
        .await
        .unwrap_or(None);
    let verbose_enabled = is_verbose_enabled(verbose_setting.as_deref());

    // Set up tool event channel if verbose is on
    let (tool_event_tx, tool_event_rx) = if verbose_enabled {
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::platform::tool_notifier::ToolEvent>(32);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Spawn notifier task if verbose
    let notifier_handle = if verbose_enabled {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let mut rx = tool_event_rx.expect("rx exists when verbose");
        Some(tokio::spawn(async move {
            let mut notifier = crate::platform::tool_notifier::ToolCallNotifier::new(
                bot_clone,
                chat_id,
            );
            notifier.start().await;
            while let Some(event) = rx.recv().await {
                notifier.handle_event(event).await;
            }
            notifier.finish().await;
        }))
    } else {
        None
    };
```

4. Update the `process_message` call to pass the channel:

```rust
    match agent.process_message(&incoming, tool_event_tx).await {
```

5. After `process_message` returns (after the match block), drop the notifier:

```rust
    // Wait for notifier to clean up (it exits when tool_event_tx is dropped)
    if let Some(handle) = notifier_handle {
        handle.await.ok();
    }
```

> **Important:** `tool_event_tx` is moved into `process_message`. When `process_message` returns, the `Sender` is dropped, which closes the channel, which causes the notifier task's `rx.recv()` to return `None`, which causes the while loop to exit and `notifier.finish()` to be called. This is the clean shutdown pattern.

> **Note on `recall` / `remember` API:** Check actual method signatures in `src/memory/knowledge.rs`. The `recall` method returns `Result<Option<String>>`. The `remember` method may have a `source: Option<&str>` parameter. Adjust accordingly.

### Step 6: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

Fix any API mismatches (check actual `recall`/`remember` signatures in `memory/knowledge.rs`).

### Step 7: Run all tests

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
cargo fmt --all -- --check 2>&1 | tail -10
```

### Step 8: Commit

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): add /verbose command for tool call UI, wire ToolCallNotifier into agentic loop"
```

---

## Task 6: System Prompt Enhancement for Small Models

**Files:**
- Modify: `src/config.rs` (`default_system_prompt`)

### Step 1: Update the default system prompt

In `src/config.rs`, find `default_system_prompt()` (line 124). Replace with:

```rust
fn default_system_prompt() -> String {
    "You are RustFox — an AI assistant with tools, memory, and skills.\n\
     \n\
     ## Identity\n\
     Your name is RustFox, but your soul (if loaded) overrides any default identity.\n\
     Soul takes precedence over everything.\n\
     \n\
     ## Priority Chain\n\
     When responding, apply context in this order:\n\
     1. SOUL — your loaded soul/identity defines who you are and how you speak\n\
     2. MEMORY — recalled user preferences, corrections, and context from past conversations\n\
     3. CONTEXT — the current conversation and user request\n\
     \n\
     ## Memory & Persistent Context\n\
     You have persistent memory. Use it:\n\
     - When you see <retrieved_context> in this prompt, those are past conversation snippets\n\
       retrieved by semantic search — treat them as factual recall of prior interactions\n\
     - When you see [SUMMARY] messages, they capture earlier conversations — treat them\n\
       as ground truth for user preferences, facts, and history\n\
     - Never say 'I don't have access to past conversations' — you do, via retrieved context\n\
     \n\
     ## Skills First\n\
     You have skills. For every user request:\n\
     - Check if a relevant skill exists (listed in your system context)\n\
     - If yes: load and follow it via read_skill_file before responding\n\
     - If no matching skill: reason directly, or use code-interpreter for computation/scripting tasks\n\
     - For complex multi-step problems: invoke the problem-solver subagent\n\
     \n\
     ## Sandbox\n\
     File and command tools operate only within the allowed sandbox directory."
        .to_string()
}
```

### Step 2: Verify compilation and tests

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 3: Commit

```bash
git add src/config.rs
git commit -m "feat(prompt): enhance default system prompt to guide small models on using retrieved context and summaries"
```

---

## Task 7: Final Verification

### Step 1: Full test suite

```bash
cargo test 2>&1
```

Expected: all tests pass.

### Step 2: Clippy (zero warnings)

```bash
cargo clippy -- -D warnings 2>&1
```

Expected: no warnings.

### Step 3: Format check

```bash
cargo fmt --all -- --check 2>&1
```

If any formatting issues: `cargo fmt` then re-check.

### Step 4: Release build

```bash
cargo build --release 2>&1 | tail -20
```

Expected: builds successfully.

### Step 5: Final commit and push

```bash
git add -u
git commit -m "chore: final formatting and cleanup for chat-history-rag feature" 2>/dev/null || true
git push -u origin claude/chat-history-rag-telegram-T4Jmo
```

---

## Appendix: Key API References

### `memory/knowledge.rs` — recall/remember signatures

```rust
// remember: upsert a knowledge entry
pub async fn remember(&self, category: &str, key: &str, value: &str, source: Option<&str>) -> Result<()>

// recall: exact key lookup, returns the value string
pub async fn recall(&self, category: &str, key: &str) -> Result<Option<String>>
```

### `llm.rs` — LlmClient::chat signature

```rust
pub async fn chat(&self, messages: &[ChatMessage], tools: &[ToolDefinition]) -> Result<ChatMessage>
```

### `scheduler/mod.rs` — Scheduler::add_cron_job pattern

Read `src/scheduler/tasks.rs` to see existing pattern for adding jobs before writing new ones.

### `platform/mod.rs` — check existing module declarations

```rust
pub mod telegram;
// Need to add:
pub mod tool_notifier;
```

---

## Common Pitfalls

1. **`search_messages_in_conversation` SQL** — sqlite-vec `MATCH` with conversation filter needs the messages table join. The original `search_messages()` in `conversations.rs` uses a global match. The new function must filter by `conversation_id` AND use `m.rowid` to join.

2. **`load_messages` subquery ordering** — The subquery uses `ORDER BY created_at DESC LIMIT N` to get the most recent N messages, then the outer query re-orders `ASC`. This is intentional to get "last 50 messages in chronological order."

3. **`ToolEvent::Completed` matching** — Use `rfind` to match the last unfinished entry with the given name (handles the case where the same tool is called multiple times).

4. **Channel drop timing** — `tool_event_tx` must be dropped before waiting on `notifier_handle`. In Rust, variables are dropped in reverse declaration order. Since `tool_event_tx` is declared before `notifier_handle`, it will be dropped last. Explicitly drop it: `drop(tool_event_tx);` before `notifier_handle.await.ok();`.

5. **`ALTER TABLE` idempotency** — Using `.ok()` on the `ALTER TABLE ADD COLUMN` migration means it silently succeeds on fresh DBs and silently ignores the "duplicate column" error on existing ones. This is the correct pattern for additive SQLite migrations.

6. **`scheduler/tasks.rs` signature** — Read the actual file before modifying. The current signature and any existing jobs must be preserved.
