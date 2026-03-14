# Design: Chat History RAG + Nightly Summarization + Tool Call UI

**Date:** 2026-03-14
**Branch:** `claude/chat-history-rag-telegram-T4Jmo`
**Status:** Approved, ready for implementation

---

## Overview

Three features are being added to RustFox to address context loss and improve user experience:

1. **Chat History RAG** — Framework auto-injects semantically relevant past messages into every LLM turn (no LLM token cost to decide to search).
2. **Nightly Summarization** — A cron job summarizes each active conversation nightly, keeping context bounded as history grows.
3. **Tool Call UI** — A live-edited Telegram message shows the user what tool the agent is currently calling. Toggled per-user with `/verbose`.

---

## Approach: Framework-Layer (Approach B)

Minimal, additive Rust code (~300 lines). No new crates. Reuses:
- Existing `search_messages()` hybrid RRF (vector + FTS5) in `memory/conversations.rs`
- Existing `tokio-cron-scheduler` in `scheduler/`
- Existing `teloxide` `edit_message_text` API
- Existing `remember/recall` knowledge table for user settings

---

## Feature 1: Chat History RAG

### Architecture

**New file:** `memory/rag.rs`
**Modified:** `agent.rs::process_message()`, `memory/mod.rs`

### How It Works

Before every LLM call in the agentic loop:

1. `auto_retrieve_context(query, conversation_id, limit=5)` is called
2. Calls existing `search_messages()` with hybrid RRF (vector cosine + FTS5)
3. If results found, a `<retrieved_context>` block is prepended to the system prompt
4. Skipped if user input is a `/command` or fewer than 5 chars

### Injected Format (System Prompt Block)

```
<retrieved_context>
Relevant past conversation snippets retrieved by semantic search:

[2026-01-10 14:32 UTC] user: I prefer TypeScript over JavaScript for all projects
[2026-02-01 09:15 UTC] assistant: You mentioned your Docker setup uses Portainer on port 9000
[2026-03-01 18:44 UTC] user: My timezone is Hong Kong (UTC+8)
</retrieved_context>
```

Using `<retrieved_context>` XML-style tags ensures reliable parsing by small models (20B and below) without extra prompt instructions.

### Fallback

If embedding API is unavailable, `try_embed_one()` returns `None` and `search_messages()` falls back to FTS5-only — already handled, no code change needed.

### Key Decisions

- **Limit: 5** — Enough context without inflating prompt size for small models
- **Per-conversation isolation** — Only retrieves from the same user's conversation
- **Auto-inject only** — Keep existing `search_memory` tool for LLM-triggered deeper searches
- **Insertion point** — System prompt extension, not as a fake user/assistant message (cleaner)

---

## Feature 2: Nightly Summarization

### Architecture

**New file:** `memory/summarizer.rs`
**Modified:** `memory/conversations.rs` (load_messages), `memory/mod.rs`, `main.rs` (cron registration), DB schema (migration)

### Schema Change

```sql
-- Additive, migration-safe
ALTER TABLE messages ADD COLUMN is_summarized BOOLEAN DEFAULT 0;
```

### How It Works

1. On startup, `main.rs` registers a nightly cron: `"0 0 2 * * *"` (2am UTC)
2. Job calls `summarize_all_active_conversations()`:
   - Queries conversations with `updated_at > NOW() - 7 days`
   - For each: load unsummarized messages
   - If fewer than 20 messages → skip
   - LLM call with summarization prompt → returns concise bullet-point summary
   - Store as `ChatMessage { role: "system", content: "[SUMMARY]\n<bullets>" }`
   - Mark summarized messages with `is_summarized = true`

### Summarization Prompt (Optimized for Small OSS Models)

```
You are a conversation summarizer. Summarize the conversation history below in 3-5 bullet points.
Maximum 200 words total. Be factual and precise.

Focus on:
- Facts the user explicitly stated (preferences, constraints, environment)
- Problems that were solved and how
- Important decisions made
- Unresolved questions or tasks

Do NOT include: greetings, small talk, or filler content.

FORMAT (strictly):
• [topic]: one to two sentence summary
• [topic]: one to two sentence summary
...

CONVERSATION:
{messages}
```

### Updated `load_messages()` Behaviour

When loading messages for a conversation:
1. Always include `[SUMMARY]` messages (role=system, content starts with `[SUMMARY]`) at the top
2. Then load the most recent 50 unsummarized raw messages
3. Total context stays bounded regardless of conversation length

### Configuration

New optional config field (with sensible default):
```toml
[memory]
database_path = "rustfox.db"
summarize_cron = "0 0 2 * * *"   # Optional, default: 2am UTC daily
max_raw_messages = 50             # Optional, default: 50
summarize_threshold = 20          # Optional, default: min messages before summarizing
```

---

## Feature 3: Tool Call UI (Live-Edited Telegram Message)

### Architecture

**New file:** `platform/tool_notifier.rs`
**Modified:** `platform/telegram.rs` (add `/verbose` command, pass notifier), `agent.rs` (add tool event channel), `memory/mod.rs` (persist verbose setting)

### User Settings Storage

Stored in existing `knowledge` table:
```
category: "settings"
key: "tool_ui_enabled"
value: "true" | "false"
```

Loaded via `recall("settings", "tool_ui_enabled")` at start of each `process_message()`.

### New Bot Command

`/verbose` — toggles tool call UI per user. Responds with:
- `"🔧 Tool call UI enabled. I'll show you what I'm working on."` (when enabling)
- `"🔇 Tool call UI disabled. I'll respond silently."` (when disabling)

### ToolCallNotifier Struct

```rust
pub struct ToolCallNotifier {
    bot: Bot,
    chat_id: ChatId,
    status_msg: Option<Message>,
    tool_log: Vec<ToolEntry>,
    last_edit: Instant,
}

struct ToolEntry {
    name: String,
    args_preview: String,  // First 60 chars of args JSON
    status: ToolStatus,    // Running | Done | Error
}
```

### Agentic Loop Integration

Event channel: `tokio::sync::mpsc::channel::<ToolEvent>(32)` created per request.

`agent.rs` sends events:
- `ToolEvent::Started { name, args_preview }` — before `execute_tool()`
- `ToolEvent::Completed { name, success }` — after `execute_tool()` returns

The `ToolCallNotifier` task (spawned per request) receives events and edits the message.

### Message Format

Initial message (sent before loop):
```
⏳ Working...
```

Updated as tools run:
```
⏳ Working...

🔧 search_memory("Docker preferences") ✅
🔧 read_skill_file("coding-assistant") ✅
🔧 execute_command("cargo check") ⏳
```

Completion (message deleted before final response is sent for clean UX).

### Rate Limit Guard

Telegram rate limit: ~1 edit/second per chat.

Implementation: track `last_edit: Instant`. If `elapsed < 1s`, defer edit by `tokio::time::sleep(1s - elapsed)` before editing. This prevents Telegram 429 errors during rapid multi-tool sequences.

### Error Status

```
🔧 execute_command("cargo build") ❌
```

Errors do not stop the loop — consistent with existing behaviour where tool errors are returned to LLM as result strings.

---

## Data Flow Diagram

```
User message
    │
    ▼
platform/telegram.rs::handle_message()
    │
    ├─ Check /verbose → toggle knowledge["settings"]["tool_ui_enabled"]
    │
    ▼
agent.rs::process_message()
    │
    ├─ memory::rag::auto_retrieve_context(query, conv_id) ──► sqlite-vec hybrid search
    │     │
    │     └─ Prepend <retrieved_context> to system_prompt (if results)
    │
    ├─ spawn ToolCallNotifier task (if verbose enabled)
    │     └─ tokio::mpsc::Receiver<ToolEvent>
    │
    ├─ AGENTIC LOOP (max 25 iterations):
    │     │
    │     ├─ LLM call (OpenRouter)
    │     │
    │     ├─ For each tool_call:
    │     │     ├─ Send ToolEvent::Started → notifier edits Telegram message
    │     │     ├─ execute_tool()
    │     │     └─ Send ToolEvent::Completed → notifier edits Telegram message
    │     │
    │     └─ If text response → exit loop
    │
    ├─ Delete status message (if verbose)
    └─ Send final response (split ≤4000 chars)

NIGHTLY (2am UTC):
scheduler → memory::summarizer::summarize_all_active_conversations()
    └─ For each active conversation:
          └─ LLM summarization call → store [SUMMARY] system message
```

---

## Files to Create/Modify

| File | Change |
|------|--------|
| `memory/rag.rs` | **New** — `auto_retrieve_context()` |
| `memory/summarizer.rs` | **New** — `summarize_conversation()`, `summarize_all_active_conversations()` |
| `memory/mod.rs` | Add `rag` and `summarizer` modules; expose new functions |
| `memory/conversations.rs` | Update `load_messages()` to handle [SUMMARY] + raw limit; add `is_summarized` column migration |
| `platform/tool_notifier.rs` | **New** — `ToolCallNotifier`, `ToolEvent`, mpsc integration |
| `platform/telegram.rs` | Add `/verbose` command handler; load verbose setting; pass notifier channel to agent |
| `agent.rs` | Add `mpsc::Sender<ToolEvent>` param to `process_message()`; call `auto_retrieve_context()`; emit tool events |
| `main.rs` | Register nightly summarization cron on startup |
| `config.rs` | Add optional `summarize_cron`, `max_raw_messages`, `summarize_threshold` to `MemoryConfig` |

---

## System Prompt Additions

The dynamic system prompt already includes skills and agents context. We add:

**Always-present section (near top of prompt):**
```
## Memory & Context
You have persistent memory. When you see <retrieved_context>, use those past conversation snippets to maintain continuity. If you see [SUMMARY] messages, they capture the essence of earlier conversations — treat them as ground truth for user preferences and history.
```

This brief, explicit instruction helps small models reliably use the injected context without confusion.

---

## Security & Performance

- RAG retrieval: bounded by `limit=5`, single SQLite query, no external call (uses existing embedding cache)
- Summarization: runs offline at 2am, LLM call count = active_conversations/day (typically 1 for single-user)
- Tool UI: single `mpsc` channel per request, auto-dropped on completion; no persistent state
- Verbose setting: stored in existing `knowledge` table, no schema changes

---

## Testing Plan

| Component | Test |
|-----------|------|
| `auto_retrieve_context` | Unit test: insert messages, verify retrieval by semantic similarity |
| `summarize_conversation` | Unit test: provide 25 mock messages, verify summary is stored |
| `load_messages` order | Unit test: verify [SUMMARY] appears before raw messages |
| Tool notifier rate limit | Unit test: simulate rapid events, verify edit calls are throttled |
| `/verbose` command | Integration: send /verbose, verify knowledge table updated |

---

## Out of Scope

- Query rewriting for follow-up question disambiguation (future improvement)
- Graph RAG or hierarchical summarization (overkill at current scale)
- Streaming final LLM response token-by-token to Telegram (separate feature)
- Cross-user RAG or shared knowledge retrieval
