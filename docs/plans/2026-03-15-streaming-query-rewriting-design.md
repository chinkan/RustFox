# Design: LLM Streaming + Query Rewriting for RAG

**Date:** 2026-03-15
**Branch:** `claude/chat-history-rag-telegram-T4Jmo`
**Status:** Approved, ready for implementation
**Extends:** `2026-03-14-chat-history-rag-telegram-ui.md`

---

## Overview

Two features previously marked "out of scope" are now in scope:

1. **Query Rewriting** — Before RAG vector search, rewrite ambiguous follow-up questions into self-contained standalone queries using the last 3 messages as context. Eliminates pronoun/reference failures ("what did he do?" → "what did Linus Torvalds do?").
2. **LLM Response Streaming** — The final text response from the LLM is streamed token-by-token to Telegram via live `edit_message_text` updates. Tool-calling iterations remain non-streaming (required for tool call parsing). Visible typing effect improves UX.

---

## Approach

- **Query Rewriting:** New module `memory/query_rewriter.rs`, called from `memory/rag.rs` before vector search. Falls back to original query on any failure (non-fatal).
- **Streaming:** New `chat_stream()` method on `LlmClient`. Agent loop detects final iteration, uses streaming call. Telegram platform spawns a receiver task that batches tokens and edits message every 500ms. One new Cargo feature flag: `reqwest/stream`.

---

## Feature A: Query Rewriting

### Architecture

**New file:** `src/memory/query_rewriter.rs`
**Modified:** `src/memory/rag.rs` (call rewriter before search)
**Modified:** `src/memory/mod.rs` (add `pub mod query_rewriter;`)
**Modified:** `src/agent.rs` (pass `llm` + `recent_messages` to `auto_retrieve_context`)

### Signature Change to `auto_retrieve_context`

```rust
pub async fn auto_retrieve_context(
    store: &MemoryStore,
    llm: &LlmClient,                    // NEW: for rewrite LLM call
    query: &str,
    recent_messages: &[ChatMessage],    // NEW: last 3 messages for context
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>>
```

### `rewrite_for_rag` Function

```rust
pub async fn rewrite_for_rag(
    llm: &LlmClient,
    user_message: &str,
    recent_messages: &[ChatMessage],   // last ≤3 non-system messages
) -> String   // always returns a string (fallback = original)
```

Returns the original `user_message` unchanged on any failure. Never returns an error — non-fatal by design.

### Rewrite Prompt (Optimised for 20B OSS Models)

```
Rewrite the QUESTION below as a single, self-contained search query.
Use the CONVERSATION HISTORY to resolve any unclear pronouns or references.
Output ONLY the rewritten query. No explanation.

RULES:
- Replace pronouns (he/she/it/they/that/this/there) with the specific name or thing
- If the question is already clear and self-contained, output it unchanged
- Maximum 30 words

CONVERSATION HISTORY (most recent last):
{role}: {content}
...

QUESTION: {user_message}

REWRITTEN QUERY:
```

### Data Flow

```
auto_retrieve_context(store, llm, query, recent_msgs, conv_id, limit)
    │
    ├─ rewrite_for_rag(llm, query, recent_msgs[last 3])
    │     ├─ Build rewrite prompt with conversation history
    │     ├─ llm.chat(&messages, &[])   (tools: empty — text-only call)
    │     ├─ Extract response text → trim → take first line
    │     └─ On error/empty → return original query as fallback
    │
    └─ search_messages_in_conversation(rewritten_query, conv_id, limit)
          └─ Result injected as <retrieved_context> into system prompt
```

### Key Decisions

- **Rewrite scope:** Only affects the RAG search query. Original user message is unchanged for the main LLM.
- **Context window:** Last 3 non-system messages — enough for pronoun resolution without inflating the rewrite prompt.
- **Failure mode:** Returns original query silently. Logged at `debug!` level.
- **No timeout config:** A rewrite call is fast (<500ms typical). If it hangs, the overall request timeout governs.

---

## Feature B: LLM Response Streaming

### Architecture

**Modified:** `src/llm.rs` (add `chat_stream()`, update `ChatRequest`, add SSE parser)
**Modified:** `Cargo.toml` (`reqwest` gains `stream` feature)
**Modified:** `src/agent.rs` (detect final iteration, call `chat_stream` with token sender)
**Modified:** `src/platform/telegram.rs` (spawn streaming receiver task)

### `Cargo.toml` Change

```toml
reqwest = { version = "0.12", features = ["json", "stream"] }
```

No other new crates. SSE parsing is done with standard string operations.

### `LlmClient::chat_stream()` — New Method

```rust
pub async fn chat_stream(
    &self,
    messages: &[ChatMessage],
    model: &str,
    token_tx: tokio::sync::mpsc::Sender<String>,
) -> Result<()>
```

Implementation:
1. POST `{ model, messages, tools: null, stream: true, max_tokens }` to OpenRouter
2. Get response as byte stream via `response.bytes_stream()` (reqwest stream feature)
3. Parse SSE lines:
   - Skip lines not starting with `data: `
   - Skip `data: [DONE]`
   - Parse `data: {...}` as JSON → extract `choices[0].delta.content`
   - Send each non-empty content token via `token_tx.send(token).await`
4. Drop sender when stream ends (signals receiver that streaming is complete)

### SSE Chunk Format (OpenRouter)

```json
data: {"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}
data: {"choices":[{"delta":{"content":" world"},"finish_reason":null}]}
data: [DONE]
```

Parsing: split response bytes by newlines, match `data: ` prefix, parse JSON, extract `.choices[0].delta.content`.

### Agent Loop Change

In `process_message()`, the final iteration (one where `response.tool_calls` is None/empty) switches to streaming:

```rust
// On final iteration: use streaming if token_tx provided
if let Some(ref tx) = stream_token_tx {
    self.llm.chat_stream(&messages, &self.config.model, tx.clone()).await?;
    // Content is assembled by receiver; return assembled string
    return Ok(assembled_content);
} else {
    let response = self.llm.chat(&messages, &all_tools).await?;
    // ... existing logic
}
```

**Detecting "final iteration":** Rather than predicting ahead of time, we keep the existing structure. The streaming path is used for the **last** LLM call only — implemented by passing `tools: &[]` (empty) on the streaming call so the model cannot emit tool calls. This is the same constraint we use for summarization.

**Assembled content:** The platform assembles the full string from tokens for saving to DB.

### `process_message` Signature Addition

```rust
pub async fn process_message(
    &self,
    incoming: &IncomingMessage,
    tool_event_tx: Option<mpsc::Sender<ToolEvent>>,
    stream_token_tx: Option<mpsc::Sender<String>>,   // NEW
) -> Result<String>
```

### Telegram Receiver Task

Spawned in `platform/telegram.rs` alongside (or instead of, when verbose) the tool notifier:

```rust
// Send initial empty message to get a message ID
let stream_msg = bot.send_message(chat_id, "…").await?;

tokio::spawn(async move {
    let mut buffer = String::new();
    let mut last_edit = Instant::now();

    while let Some(token) = token_rx.recv().await {
        buffer.push_str(&token);

        // Edit every 500ms or every 20 tokens
        if last_edit.elapsed() >= Duration::from_millis(500) || buffer.len() % 20 == 0 {
            bot.edit_message_text(chat_id, stream_msg.id, &buffer).await.ok();
            last_edit = Instant::now();
        }
    }

    // Final edit with complete content
    if !buffer.is_empty() {
        bot.edit_message_text(chat_id, stream_msg.id, &buffer).await.ok();
    }
});
```

**Message splitting:** If `buffer.len() > 3800`, send a new message and continue editing that one.

**Interaction with verbose tool UI:** When verbose is on, the notifier message is deleted before the streaming message is sent (clean transition from tool progress → streaming text).

### Data Flow

```
Telegram message received
    │
    ├─ (verbose) ToolCallNotifier spawned → shows tool progress
    │
    ├─ create (stream_token_tx, stream_token_rx)
    ├─ spawn streaming receiver task (edits Telegram message)
    │
    └─ agent.process_message(incoming, tool_event_tx, stream_token_tx)
          │
          ├─ [TOOL ITERATIONS] — non-streaming, normal chat() calls
          │      ToolCallNotifier edits progress message each tool
          │
          └─ [FINAL ITERATION] — no tools → chat_stream() called
                 │
                 ├─ OpenRouter SSE stream → tokens sent via stream_token_tx
                 ├─ Receiver task edits Telegram message in real-time
                 └─ process_message assembles + returns full string for DB
```

---

## Updated File Change Table

Building on the original plan, the new files/modifications:

| File | Change |
|------|--------|
| `memory/query_rewriter.rs` | **New** — `rewrite_for_rag()` |
| `memory/rag.rs` | Update `auto_retrieve_context()` signature + call rewriter |
| `memory/mod.rs` | Add `pub mod query_rewriter;` |
| `agent.rs` | Pass `llm` + `recent_messages` to `auto_retrieve_context`; add `stream_token_tx` to `process_message`; use `chat_stream` on final iteration |
| `llm.rs` | Add `chat_stream()`, SSE parsing, `stream: bool` field on `ChatRequest` |
| `Cargo.toml` | Add `stream` feature to `reqwest` |
| `platform/telegram.rs` | Spawn streaming receiver task; update `process_message` call signature |

---

## Testing Plan

| Component | Test |
|-----------|------|
| `rewrite_for_rag` | Unit: mock LLM output, verify pronoun replacement |
| `rewrite_for_rag` fallback | Unit: simulate LLM failure, verify returns original query |
| `auto_retrieve_context` signature | Unit: existing tests updated to pass `llm` and `recent_msgs` |
| SSE parser | Unit: feed mock SSE byte sequences, verify token extraction |
| `chat_stream` contract | Unit: verify sender is closed when `[DONE]` received |
| Token batching | Unit: verify Telegram edit is not called more often than rate limit |

---

## Out of Scope (Unchanged)

- Cross-user RAG or shared knowledge retrieval
- Graph RAG or hierarchical summarization
- Adaptive query rewriting (pronoun-detection heuristic) — we always rewrite
- Streaming during tool-call iterations
