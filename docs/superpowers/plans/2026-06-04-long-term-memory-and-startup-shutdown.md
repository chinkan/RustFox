# Long-Term Memory & Startup/Shutdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three independent features: (1) `/clear` archives instead of deleting, (2) startup notifications to allowed users, (3) graceful shutdown with notification.

**Architecture:** Soft archive using `is_archived` column on conversations table. Startup/shutdown via Telegram bot API messages sent to every allowed user before dispatcher starts and on SIGINT/SIGTERM.

**Tech Stack:** Rust, tokio, teloxide, rusqlite, sqlite-vec

---

### Files to Modify

| File | Responsibility |
|------|---------------|
| `src/memory/mod.rs` | DB migration: add `is_archived` column |
| `src/memory/conversations.rs` | Core archive logic: modify `get_or_create_conversation`, `clear_conversation`, `load_messages_with_limit` |
| `src/mcp.rs` | Add `server_count()` method for startup message |
| `src/platform/telegram.rs` | Update `/clear` response, add `notify_startup()` and `notify_shutdown()` |
| `src/main.rs` | Wire signal handler for graceful shutdown |

---

### Task 1: DB Migration — Add `is_archived` Column

**Files:**
- Modify: `src/memory/mod.rs`

- [ ] **Step 1: Add migration**

Add to `run_migrations()` in `src/memory/mod.rs`, after the existing `is_summarized` migration:

```rust
// Migration: add is_archived column (safe no-op if column already exists)
conn.execute_batch(
    "ALTER TABLE conversations ADD COLUMN is_archived INTEGER DEFAULT 0;",
)
.ok();
```

- [ ] **Step 2: Run existing tests to confirm nothing broke**

Run: `cargo test -p rustfox --lib memory::tests`
Expected: All pass

```bash
cargo test -p rustfox --lib memory::tests
```

- [ ] **Step 3: Commit**

```bash
git add src/memory/mod.rs
git commit -m "feat(memory): add is_archived column to conversations"
```

---

### Task 2: Modify `get_or_create_conversation` — Skip Archived

**Files:**
- Modify: `src/memory/conversations.rs:27-35`

- [ ] **Step 1: Write the failing test**

Add to `memory/conversations.rs` test module:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox --lib memory::conversations::tests::test_get_or_create_skips_archived -- --nocapture`
Expected: FAIL — `assert_ne` fails because the old archived conversation is returned

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_get_or_create_skips_archived -- --nocapture
```

- [ ] **Step 3: Modify `get_or_create_conversation` to filter archived**

Change the SQL query in `get_or_create_conversation()` (`conversations.rs:29-31`) from:

```rust
"SELECT id FROM conversations
 WHERE platform = ?1 AND user_id = ?2
 ORDER BY updated_at DESC LIMIT 1"
```

to:

```rust
"SELECT id FROM conversations
 WHERE platform = ?1 AND user_id = ?2 AND (is_archived IS NULL OR is_archived = 0)
 ORDER BY updated_at DESC LIMIT 1"
```

- [ ] **Step 4: Run test to verify it passes**

Same command as Step 2. Expected: PASS

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_get_or_create_skips_archived -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/memory/conversations.rs
git commit -m "feat(memory): filter archived conversations in get_or_create"
```

---

### Task 3: Modify `clear_conversation` — Soft Archive Instead of Delete

**Files:**
- Modify: `src/memory/conversations.rs:113-139`

- [ ] **Step 1: Write the failing test**

Add to `memory/conversations.rs` test module:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox --lib memory::conversations::tests::test_clear_archives_instead_of_deleting -- --nocapture`
Expected: FAIL — message count is 0 after delete

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_clear_archives_instead_of_deleting -- --nocapture
```

- [ ] **Step 3: Replace `clear_conversation` implementation**

Replace the entire `clear_conversation` method body (`conversations.rs:113-139`):

```rust
pub async fn clear_conversation(&self, platform: &str, user_id: &str) -> Result<()> {
    let conn = self.conn.lock().await;

    // Soft archive: mark conversation as archived (don't delete messages)
    conn.execute(
        "UPDATE conversations SET is_archived = 1, updated_at = datetime('now')
         WHERE platform = ?1 AND user_id = ?2",
        rusqlite::params![platform, user_id],
    )?;

    Ok(())
}
```

Note: The DELETE statements for `message_embeddings`, `messages`, and `conversations`
are all removed. The conversation and its messages remain searchable.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_clear_archives_instead_of_deleting -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Run all memory tests to check nothing else broke**

```bash
cargo test -p rustfox --lib memory
```

Expected: All pass. (Existing tests like `test_search_messages_scoped_to_conversation` should still pass.)

- [ ] **Step 6: Commit**

```bash
git add src/memory/conversations.rs
git commit -m "feat(memory): soft archive on clear_conversation instead of delete"
```

---

### Task 4: Modify `load_messages_with_limit` — Skip Archived Conversations

**Files:**
- Modify: `src/memory/conversations.rs:149-194`

- [ ] **Step 1: Write the failing test**

Add to `memory/conversations.rs` test module:

```rust
#[tokio::test]
async fn test_load_messages_excludes_archived() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();

    let conv = store
        .get_or_create_conversation("test", "archive_u3")
        .await
        .unwrap();
    let msg = crate::llm::ChatMessage {
        role: "user".to_string(),
        content: Some(crate::llm::MessageContent::from_text("test")),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(&conv, &msg).await.unwrap();

    // Archive
    store.clear_conversation("test", "archive_u3").await.unwrap();

    // load_messages should return empty for an archived conversation
    let messages = store.load_messages(&conv).await.unwrap();
    assert!(
        messages.is_empty(),
        "Archived conversation should return no messages via load_messages"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_load_messages_excludes_archived -- --nocapture
```

Expected: FAIL — messages are still returned after archive

- [ ] **Step 3: Modify `load_messages_with_limit` to filter archived**

Both summary and raw queries join to `conversations` and check `is_archived`.

For the summary query, change the SQL in `conversations.rs:157-164`:

```rust
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
```

For the raw messages query, change the SQL in `conversations.rs:173-183`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_load_messages_excludes_archived -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Run all tests**

```bash
cargo test -p rustfox --lib
```

Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add src/memory/conversations.rs
git commit -m "feat(memory): skip archived conversations in load_messages"
```

---

### Task 5: Add `server_count()` to `McpManager`

**Files:**
- Modify: `src/mcp.rs`

- [ ] **Step 1: Add the method**

Add after `connect_all()` around line 383:

```rust
/// Number of connected MCP servers
pub fn server_count(&self) -> usize {
    self.connections.len()
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p rustfox
```

Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/mcp.rs
git commit -m "feat(mcp): add server_count() method"
```

---

### Task 6: Update `/clear` Response Text and Command Description

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Update `/clear` response**

Change line 211 in `telegram.rs` from:

```rust
bot.send_message(msg.chat.id, escape_text("Conversation cleared."))
```

to:

```rust
bot.send_message(msg.chat.id, escape_text("Conversation archived. Past messages remain searchable."))
```

- [ ] **Step 2: Update command description**

Change line 78 in `telegram.rs` from:

```rust
BotCommand::new("clear", "Clear the current conversation history"),
```

to:

```rust
BotCommand::new("clear", "Archive the current conversation, keeping past messages searchable"),
```

- [ ] **Step 3: Verify tests pass**

```bash
cargo test -p rustfox --lib platform::telegram::tests
```

Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): update /clear text to reflect archive behavior"
```

---

### Task 7: Add `search_messages` Test for Archived Content

**Files:**
- Test: `src/memory/conversations.rs`

- [ ] **Step 1: Write test verifying archived messages are searchable**

Add to `memory/conversations.rs` test module:

```rust
#[tokio::test]
async fn test_search_messages_finds_archived_content() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();

    let conv = store
        .get_or_create_conversation("test", "archive_search_u1")
        .await
        .unwrap();
    let msg = crate::llm::ChatMessage {
        role: "user".to_string(),
        content: Some(crate::llm::MessageContent::from_text(
            "I love Rust programming and async runtimes",
        )),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(&conv, &msg).await.unwrap();

    // Archive
    store.clear_conversation("test", "archive_search_u1").await.unwrap();

    // search_messages should still find the content from archived conversations
    let results = store.search_messages("Rust", 5).await.unwrap();
    assert!(
        !results.is_empty(),
        "search_messages must find content in archived conversations"
    );
    assert!(
        results.iter().any(|m| m.content.as_ref().map_or(false, |c| c.as_text().contains("Rust"))),
        "Archived message content must be searchable"
    );
}
```

- [ ] **Step 2: Run test**

```bash
cargo test -p rustfox --lib memory::conversations::tests::test_search_messages_finds_archived_content -- --nocapture
```

Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/memory/conversations.rs
git commit -m "test(memory): verify archived messages remain searchable"
```

---

### Task 8: Add Startup Notification Function

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add `notify_startup` function**

Add to `src/platform/telegram.rs`, before `run()`:

```rust
/// Send startup notification to all allowed users.
/// Best-effort: logs failures, never blocks startup.
pub async fn notify_startup(
    bot: &teloxide::Bot,
    allowed_user_ids: &[u64],
    model: &str,
    mcp_count: usize,
    skills_count: usize,
    embedding_enabled: bool,
) {
    let memory_status = if embedding_enabled {
        "embedding enabled"
    } else {
        "FTS5 only"
    };

    let msg = format!(
        "RustFox is online 🦊\n\
         Model: {}\n\
         MCP: {} server(s) connected\n\
         Skills: {} loaded\n\
         Memory: {}",
        model, mcp_count, skills_count, memory_status,
    );

    for &user_id in allowed_user_ids {
        let chat_id = teloxide::types::ChatId(user_id as i64);
        if let Err(e) = bot.send_message(chat_id, &msg).await {
            tracing::warn!(
                "Failed to send startup notification to user {}: {}",
                user_id,
                e
            );
        }
    }
}
```

- [ ] **Step 2: Add `notify_shutdown` function**

Add next to `notify_startup`:

```rust
/// Send shutdown notification to all allowed users.
/// Best-effort: logs failures, never blocks shutdown.
pub async fn notify_shutdown(
    bot: &teloxide::Bot,
    allowed_user_ids: &[u64],
) {
    let msg = "RustFox is going offline. Goodbye!";

    for &user_id in allowed_user_ids {
        let chat_id = teloxide::types::ChatId(user_id as i64);
        if let Err(e) = bot.send_message(chat_id, msg).await {
            tracing::warn!(
                "Failed to send shutdown notification to user {}: {}",
                user_id,
                e
            );
        }
    }
}
```

- [ ] **Step 3: Wire startup notification into `run()` — BEFORE agent is moved**

**Important:** `agent` is moved into `dptree::deps![agent]` on line 117, so `notify_startup` must be called BEFORE that point. Insert it right after `info!("Starting Telegram platform...")` on line 94, before the handler closure is defined.

In `run()` in `telegram.rs`, replace:

```rust
    info!("Starting Telegram platform...");
```

with:

```rust
    info!("Starting Telegram platform...");

    // Send startup notifications (best-effort) — before agent is moved into dptree
    notify_startup(
        &bot,
        &allowed_user_ids,
        &agent.config.openrouter.model,
        agent.mcp.server_count(),
        agent.skills.read().await.len(),
        agent.memory.embeddings.is_available(),
    )
    .await;
```

This goes between the `info!("Starting...")` and the `let commands = ...` block. The agent is still alive and `bot` has been cloned locally.

- [ ] **Step 4: Verify compilation**

```bash
cargo check -p rustfox
```

Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): add startup and shutdown notification functions"
```

---

### Task 9: Wire Shutdown Signal Handler in `main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Replace the `platform::telegram::run()` call with a select-based shutdown**

In `src/main.rs`, replace the existing call at line 337-342:

```rust
    // Run the Telegram platform
    info!("Bot is starting...");
    platform::telegram::run(
        agent,
        config.telegram.allowed_user_ids.clone(),
        Arc::clone(&bot),
    )
    .await?;
```

with:

```rust
    // Run the Telegram platform with signal-driven graceful shutdown
    info!("Bot is starting...");

    let dispatch_agent = Arc::clone(&agent);
    let dispatch_user_ids = config.telegram.allowed_user_ids.clone();
    let dispatch_bot = Arc::clone(&bot);

    let dispatch_handle = tokio::spawn(async move {
        platform::telegram::run(dispatch_agent, dispatch_user_ids, dispatch_bot).await
    });

    // Set up signal handlers (SIGINT via ctrl_c for portability, SIGTERM via unix signal)
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    )
    .expect("failed to create SIGTERM handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, shutting down...");
        }
        #[cfg(unix)]
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down...");
        }
        result = &mut dispatch_handle => {
            // Dispatch completed on its own (unlikely but handle gracefully)
            result??;
            return Ok(());
        }
    };

    // Send shutdown notification
    platform::telegram::notify_shutdown(&bot, &config.telegram.allowed_user_ids).await;

    // Brief grace period for message delivery
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    tracing::info!("Shutdown complete.");
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p rustfox
```

Expected: No errors

- [ ] **Step 3: Run full test suite**

```bash
cargo test
```

Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): add graceful shutdown with Telegram notification"
```

---

### Task 10: Full CI Verification

- [ ] **Step 1: Run cargo check**

```bash
cargo check
```

Expected: No errors

- [ ] **Step 2: Run cargo clippy**

```bash
cargo clippy -- -D warnings
```

Expected: No warnings

- [ ] **Step 3: Run cargo fmt**

```bash
cargo fmt --all -- --check
```

Expected: No formatting issues

- [ ] **Step 4: Run cargo test**

```bash
cargo test
```

Expected: All pass

- [ ] **Step 5: Run cargo build --release**

```bash
cargo build --release
```

Expected: Build succeeds

- [ ] **Step 6: Commit any CI fixes**

If any step failed, fix and commit.

- [ ] **Step 7: Final commit message**

```bash
git add -A
git commit -m "feat: long-term memory archive and startup/shutdown notifications"
```

---

## Self-Review Checklist

1. **Spec coverage:** Each spec requirement has a task:
   - Feature 1 (soft archive): Tasks 1-7 cover migration, get_or_create, clear_conversation, load_messages, response text, search test
   - Feature 2 (startup): Task 8 covers notify_startup, Task 9 covers wiring
   - Feature 3 (shutdown): Task 9 covers signal handler + notify_shutdown
   - All covered.

2. **Placeholder scan:** No TBD, TODO, or placeholder patterns. Every step has complete code.

3. **Type consistency:** 
   - `get_or_create_conversation` returns `Result<String>` — unchanged
   - `clear_conversation` returns `Result<()>` — unchanged
   - `load_messages_with_limit` returns `Result<Vec<ChatMessage>>` — unchanged
   - `server_count()` returns `usize` — consistent with `SkillRegistry::len()`
   - `notify_startup` / `notify_shutdown` take `&teloxide::Bot` — consistent with bot usage in `run()`
