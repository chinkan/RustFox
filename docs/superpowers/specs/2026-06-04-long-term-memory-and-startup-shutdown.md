# Long-Term Memory Survival & Startup/Shutdown Notifications

## Feature 1: Soft Archive on `/clear`

### Problem

`/clear` calls `MemoryStore::clear_conversation()` which deletes messages,
embeddings, AND the conversation record from SQLite. Past conversation history
becomes permanently inaccessible to the agent, even though:

- `search_messages()` already searches **all** conversations (unscoped) —
  verified at `memory/conversations.rs:282`.
- `search_messages_in_conversation()` is scoped but unused after deletion.
- The `knowledge` table survives (separate from conversations).

### Solution

Replace hard delete with a soft archive. This is a single-phase change.
(Knowledge snapshot on clear is deferred to a later spec.)

**How it works:**

1. Add `is_archived INTEGER DEFAULT 0` column to `conversations` table.
2. `clear_conversation()` sets `is_archived = 1` on the current conversation.
   Does NOT delete messages, embeddings, or conversation record.
3. `get_or_create_conversation()` must filter `WHERE is_archived = 0` —
   **critical**: without this filter, the query returns the archived
   conversation and no new conversation is created, breaking the feature.
4. `load_messages_with_limit()` adds `WHERE c.is_archived = 0` so active
   context stays clean (only loads from non-archived conversations).
5. Existing `search_messages()` (unscoped) naturally finds archived
   conversation content—no change needed.
6. User-facing response changes from `"Conversation cleared."` to
   `"Conversation archived. Past messages remain searchable."`.

### Schema Changes

```sql
ALTER TABLE conversations ADD COLUMN is_archived INTEGER DEFAULT 0;
```
(This uses `.ok()` — safe no-op if column already exists, same pattern
as the `is_summarized` migration at `mod.rs:293`.)

Existing index `idx_conversations_user ON conversations(platform, user_id, updated_at)`
remains sufficient — `is_archived` filter only excludes archived rows
(majority of rows are non-archived), so index selectivity is adequate.

### Code Changes

| File | Change |
|------|--------|
| `memory/mod.rs` (run_migrations) | Add `ALTER TABLE ... is_archived` migration with `.ok()` |
| `memory/conversations.rs` | Modify `get_or_create_conversation`: add `WHERE is_archived = 0` to the existing query. Without this, the archived conversation is returned and no new one is created. |
| `memory/conversations.rs` | Modify `clear_conversation`: replace DELETE with `UPDATE conversations SET is_archived = 1`. Remove ALL DELETE statements (message_embeddings, messages, conversations). |
| `memory/conversations.rs` | Modify `load_messages_with_limit`: JOIN to conversations and add `WHERE c.is_archived = 0`. |
| `platform/telegram.rs` | Update `/clear` response text to `"Conversation archived. Past messages remain searchable."` |
| `platform/telegram.rs` | Update `/clear` command description in `supported_commands()` from "Clear" to "Archive the current conversation" |
| `agent.rs` | No change needed — `Agent::clear_conversation()` delegates to `memory.clear_conversation()` |

### Tests

| Test | What it verifies |
|------|------------------|
| `test_clear_archives_instead_of_deleting` | After clear, messages still exist in DB |
| `test_get_or_create_skips_archived` | get_or_create_conversation returns a NEW conversation for an archived user |
| `test_search_messages_finds_archived` | search_messages returns results from archived conversations |
| `test_load_messages_excludes_archived` | load_messages_with_limit returns empty for archived conv |

### Estimated lines: ~50

---

## Feature 2: Startup / Shutdown Notifications

### Problem

The bot starts and stops silently. Users don't know when RustFox restarts or
goes offline.

### Solution

**Startup:**
- `platform::telegram::run()` is called from `main.rs:337` and
  `Dispatcher::dispatch().await` blocks forever.
- The notification must be sent **before** `.dispatch().await`, after the
  dispatcher is built but before it starts polling.
- Send to every user in `config.telegram.allowed_user_ids` (available in
  `main.rs` at that point).
- Medium-level status message:
  - "RustFox is online"
  - Model name: `config.openrouter.model`
  - MCP servers connected: count from `mcp_manager`
  - Skills loaded: count from `skills.len()`
  - Memory status: "embedding enabled" or "FTS5 only"

**Shutdown:**
- Register `tokio::signal::ctrl_c()` and `SIGTERM` handler in `main.rs`
  **before** calling `platform::telegram::run()` (which blocks).
- The handler captures `bot: Arc<Bot>` and `allowed_user_ids: Vec<u64>` by
  cloning before dispatch.
- On signal: send "RustFox going offline" to each allowed user.
- Wait 2 seconds for delivery, then `std::process::exit(0)`.
- Both startup and shutdown notifications are best-effort: log failures,
  never block startup/shutdown.

### Data Flow

```
main.rs flow:
  1. Build Agent, McpManager, Scheduler
  2. Clone bot + allowed_user_ids for signal handler
  3. Register SIGINT/SIGTERM handler (captures clones)
  4. Call platform::telegram::run()
  5.   Inside run():
       a. Build dispatcher
       b. Send startup notifications (async, best-effort)
       c. dispatcher.dispatch().await  ← blocks
```

### Code Changes

| File | Change |
|------|--------|
| `platform/telegram.rs` | Add `notify_startup(bot, allowed_user_ids, model, mcp_count, skills_count, embedding_enabled)` |
| `platform/telegram.rs` | Add `notify_shutdown(bot, allowed_user_ids)` |
| `platform/telegram.rs` | Call `notify_startup` after dispatcher build, before `.dispatch()` |
| `main.rs` | Clone `bot` + `allowed_user_ids` before `platform::telegram::run()` |
| `main.rs` | Register signal handler with `tokio::signal`, captures clones |
| `main.rs` | Signal handler calls `notify_shutdown` then exits after 2s delay |

### Estimated lines: ~60

---

## Feature 3: Startup Message Content

Medium detail level as requested. Example:

```
RustFox is online 🦊
Model: moonshotai/kimi-k2.6
MCP: 2 servers connected
Skills: 15 loaded
Memory: embedding enabled
```

`skills_count` requires `agent.skills.read().await` (async RwLock) — the
notification function must be async. This is fine since it runs before
`.dispatch().await`.

---

## Implementation Order

1. Schema migration: add `is_archived` column to conversations (`.ok()` pattern)
2. Modify `get_or_create_conversation`: filter `WHERE is_archived = 0`
3. Modify `load_messages_with_limit`: filter `WHERE c.is_archived = 0`
4. Modify `clear_conversation`: set `is_archived = 1` instead of DELETE
5. Update `/clear` response text + command description
6. Add `notify_startup` / `notify_shutdown` to `platform/telegram.rs`
7. Wire startup notification before `Dispatcher::dispatch()`
8. Wire shutdown signal handler in `main.rs` with 2s grace period
9. Write tests for archive behavior

---

## Deferred (future spec)

- Knowledge snapshot on `/clear`: LLM summarization of archived conversation
  stored as knowledge entries. Underspecified — needs prompt design, model
  selection, sync/async decision, error handling.

---

## References

- **`search_messages()`** at `memory/conversations.rs:282` — searches ALL
  conversations unscoped. Verified: no `WHERE conversation_id = ?` filter.
- **`clear_conversation()`** at `memory/conversations.rs:113` — currently
  deletes embeddings, messages, and conversation record.
- **`get_or_create_conversation()`** at `memory/conversations.rs:19` —
  `ORDER BY updated_at DESC LIMIT 1` with no `is_archived` filter.
- **Existing migration pattern**: `mod.rs:293` — `.ok()` on ALTER TABLE for
  idempotent column addition.
- **Hermes Agent**: Same SQLite + FTS5 approach for cross-session search.
- **arxiv 2603.05344** (§2.5 Memory): Recommends durable facts across sessions
  with automatic recall — validates the soft-archive approach.
