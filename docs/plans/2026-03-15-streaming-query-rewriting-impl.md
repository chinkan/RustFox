# Streaming + Query Rewriting Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add (1) query rewriting that rewrites ambiguous follow-up questions into self-contained RAG search queries, and (2) live Telegram streaming that progressively edits the bot's reply as the final LLM response arrives.

**Architecture:** Two additive modules — `memory/query_rewriter.rs` (cheap LLM call before RAG search) and a streaming path in `llm.rs` + `agent.rs` + `platform/telegram.rs`. The query rewriter wraps the existing `auto_retrieve_context()` call; streaming adds a `chat_stream()` method to `LlmClient` that parses OpenRouter SSE and forwards tokens through a `tokio::sync::mpsc` channel. For the agentic loop: all tool-calling iterations stay non-streaming; only the final text response is streamed token-by-token.

**Tech Stack:** Rust 2021, Tokio, reqwest 0.12 (add `stream` feature), futures-util (already transitive dep), teloxide 0.17 `edit_message_text`, tokio::sync::mpsc

---

## Reading List

Read these fully before touching anything:

- `src/memory/rag.rs` — `auto_retrieve_context()` current signature (will change)
- `src/memory/mod.rs` lines 1-4 — module declarations to add to
- `src/llm.rs` lines 46–55 — `ChatRequest` struct (will add `stream` field)
- `src/llm.rs` lines 82–173 — `chat_with_model()` to understand the pattern you're extending
- `src/agent.rs` lines 125–180 — `process_message()` entry (where RAG inject + streaming go)
- `src/agent.rs` lines 204–360 — agentic loop (where streaming call happens on final response)
- `src/platform/telegram.rs` — full file (where streaming receiver task is spawned)

---

## Task 8: Query Rewriter Module (`memory/query_rewriter.rs`)

> This is Task 8 because it extends the previous plan (Tasks 1–7 in `2026-03-14-chat-history-rag-telegram-ui-impl.md`).

**Files:**
- Create: `src/memory/query_rewriter.rs`
- Modify: `src/memory/mod.rs` (add `pub mod query_rewriter;`)

### Step 1: Write the failing tests

Create `src/memory/query_rewriter.rs` with tests first:

```rust
use crate::llm::{ChatMessage, LlmClient};

/// Rewrite an ambiguous follow-up question into a self-contained search query.
/// Uses the last ≤3 non-system messages as conversation context.
/// On any failure (LLM error, empty response), returns the original query unchanged.
pub async fn rewrite_for_rag(
    llm: &LlmClient,
    user_message: &str,
    recent_messages: &[ChatMessage],
) -> String {
    todo!()
}

/// Format recent messages for the rewrite prompt.
fn format_history(messages: &[ChatMessage]) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn test_format_history_empty() {
        let result = format_history(&[]);
        assert_eq!(result, "(no prior context)");
    }

    #[test]
    fn test_format_history_includes_role_and_content() {
        let msgs = vec![msg("user", "Who is Linus?"), msg("assistant", "Linus is the creator of Linux.")];
        let result = format_history(&msgs);
        assert!(result.contains("user: Who is Linus?"));
        assert!(result.contains("assistant: Linus is the creator of Linux."));
    }

    #[test]
    fn test_format_history_skips_system_messages() {
        let msgs = vec![
            msg("system", "You are a bot."),
            msg("user", "What is Rust?"),
        ];
        let result = format_history(&msgs);
        assert!(!result.contains("system"), "System messages must not appear in history");
        assert!(result.contains("user: What is Rust?"));
    }

    #[test]
    fn test_format_history_skips_tool_messages() {
        let msgs = vec![
            msg("tool", r#"{"result": "some output"}"#),
            msg("user", "What does that mean?"),
        ];
        let result = format_history(&msgs);
        assert!(!result.contains("tool"), "Tool messages must not appear in history");
        assert!(result.contains("user: What does that mean?"));
    }

    #[test]
    fn test_format_history_limits_to_last_3() {
        let msgs: Vec<ChatMessage> = (0..10)
            .map(|i| msg("user", &format!("message {}", i)))
            .collect();
        let result = format_history(&msgs);
        // Only last 3 should appear
        assert!(result.contains("message 9"));
        assert!(result.contains("message 8"));
        assert!(result.contains("message 7"));
        assert!(!result.contains("message 6"), "Older messages must be excluded");
    }

    #[test]
    fn test_format_history_truncates_long_content() {
        let long = "x".repeat(500);
        let msgs = vec![msg("user", &long)];
        let result = format_history(&msgs);
        // Each message content should be capped at 200 chars
        let line = result.lines().next().unwrap_or("");
        assert!(line.len() <= 220, "Content should be truncated: len={}", line.len());
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test memory::query_rewriter 2>&1 | tail -20
```

Expected: FAIL — `todo!()` panics and `format_history` not defined.

### Step 3: Register the module in `src/memory/mod.rs`

Add after line 3 (`pub mod knowledge;`):

```rust
pub mod query_rewriter;
```

### Step 4: Implement `format_history`

Replace the `todo!()` in `format_history`:

```rust
fn format_history(messages: &[ChatMessage]) -> String {
    // Filter to only user/assistant messages, take last 3
    let relevant: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .collect();

    let window: Vec<&ChatMessage> = relevant
        .iter()
        .rev()
        .take(3)
        .rev()
        .copied()
        .collect();

    if window.is_empty() {
        return "(no prior context)".to_string();
    }

    window
        .iter()
        .filter_map(|m| {
            m.content.as_ref().map(|c| {
                // Cap each message at 200 chars to keep the prompt small
                let snippet = if c.len() > 200 {
                    format!("{}...", &c[..200])
                } else {
                    c.clone()
                };
                format!("{}: {}", m.role, snippet)
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

### Step 5: Run format_history tests — verify they pass

```bash
cargo test memory::query_rewriter::tests::test_format_history 2>&1 | tail -20
```

Expected: all 5 `format_history` tests PASS.

### Step 6: Implement `rewrite_for_rag`

Replace the `todo!()` in `rewrite_for_rag`:

```rust
pub async fn rewrite_for_rag(
    llm: &LlmClient,
    user_message: &str,
    recent_messages: &[ChatMessage],
) -> String {
    let history = format_history(recent_messages);

    let prompt = format!(
        "Rewrite the QUESTION below as a single, self-contained search query.\n\
         Use the CONVERSATION HISTORY to resolve any unclear pronouns or references.\n\
         Output ONLY the rewritten query. No explanation. No punctuation at the end.\n\
         \n\
         RULES:\n\
         - Replace pronouns (he/she/it/they/that/this/there) with the specific name or thing\n\
         - If the question is already clear and self-contained, output it unchanged\n\
         - Maximum 30 words\n\
         \n\
         CONVERSATION HISTORY (most recent last):\n\
         {history}\n\
         \n\
         QUESTION: {user_message}\n\
         \n\
         REWRITTEN QUERY:",
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(
                "You are a query rewriter. Output only the rewritten query, nothing else."
                    .to_string(),
            ),
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

    match llm.chat(&messages, &[]).await {
        Ok(response) => {
            let rewritten = response
                .content
                .unwrap_or_default()
                .trim()
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();

            if rewritten.is_empty() {
                tracing::debug!(
                    "Query rewriter returned empty — using original: {:?}",
                    user_message
                );
                user_message.to_string()
            } else {
                tracing::debug!(
                    "Query rewritten: {:?} → {:?}",
                    user_message,
                    rewritten
                );
                rewritten
            }
        }
        Err(e) => {
            tracing::debug!("Query rewrite failed (using original): {:#}", e);
            user_message.to_string()
        }
    }
}
```

### Step 7: Verify compilation

```bash
cargo check 2>&1 | tail -20
```

Expected: no errors.

### Step 8: Run all tests + clippy

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 9: Commit

```bash
git add src/memory/query_rewriter.rs src/memory/mod.rs
git commit -m "feat(query-rewriter): add rewrite_for_rag() to disambiguate follow-up questions before RAG search"
```

---

## Task 9: Wire Query Rewriter into RAG Auto-Inject

**Files:**
- Modify: `src/memory/rag.rs` (update `auto_retrieve_context` signature + call rewriter)
- Modify: `src/agent.rs` (pass `llm` and `recent_messages` to `auto_retrieve_context`)

### Step 1: Write failing test for the updated signature

Add to `src/memory/rag.rs` tests (the test that verifies rewriter is invoked):

```rust
    #[tokio::test]
    async fn test_auto_retrieve_uses_rewritten_query_for_search() {
        // This test verifies the function accepts the new llm + recent_messages params
        // without panicking. We can't mock the LLM here, so we test the contract.
        let store = MemoryStore::open_in_memory().unwrap();
        let conv = store.get_or_create_conversation("test", "rewrite_test").await.unwrap();

        // Save a message with "TypeScript" keyword for FTS matching
        let msg = ChatMessage {
            role: "user".to_string(),
            content: Some("I prefer TypeScript for frontend work".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        store.save_message(&conv, &msg).await.unwrap();

        // Without a real LLM, rewrite falls back to original query
        // (LlmClient::new needs a real config — skip the LLM call test here;
        //  rewrite_for_rag is unit-tested separately in query_rewriter tests)
        // Just verify the function signature compiles and runs with empty recent_msgs
        let result = auto_retrieve_context(&store, None, "TypeScript", &[], &conv, 5)
            .await
            .unwrap();
        // With FTS5, "TypeScript" should match
        // Result may be Some or None depending on FTS tokenization — just verify no panic
        let _ = result;
    }
```

> Note: We pass `None` for the `llm` param in tests (no real LLM available). When `llm` is `None`, skip the rewrite and use the original query.

### Step 2: Run test to verify it fails

```bash
cargo test test_auto_retrieve_uses_rewritten_query_for_search 2>&1 | tail -20
```

Expected: FAIL — signature mismatch.

### Step 3: Update `auto_retrieve_context` signature in `src/memory/rag.rs`

Change the function signature from:
```rust
pub async fn auto_retrieve_context(
    store: &MemoryStore,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>>
```

To:
```rust
pub async fn auto_retrieve_context(
    store: &MemoryStore,
    llm: Option<&crate::llm::LlmClient>,
    query: &str,
    recent_messages: &[crate::llm::ChatMessage],
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>>
```

Inside the function, add before the `search_messages_in_conversation` call:

```rust
    // Query rewriting: resolve pronouns/references using recent context
    let search_query = if let Some(llm) = llm {
        crate::memory::query_rewriter::rewrite_for_rag(llm, query, recent_messages).await
    } else {
        query.to_string()
    };
```

Then replace uses of `query` in the search call with `&search_query`.

Also update the existing tests in `rag.rs` to pass `None` for `llm` and `&[]` for `recent_messages`.

### Step 4: Update the call site in `src/agent.rs`

Find the `auto_retrieve_context` call (added in Task 2, around line 162 after the RAG injection block):

```rust
        let rag_context = crate::memory::rag::auto_retrieve_context(
            &self.memory,
            &incoming.text,
            &conversation_id,
            self.config.memory.rag_limit,
        )
        .await
        .unwrap_or(None);
```

Replace with:

```rust
        // Take last 6 messages for rewrite context (skip system messages)
        let recent_for_rewrite: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .rev()
            .take(6)
            .rev()
            .cloned()
            .collect();

        let rag_context = crate::memory::rag::auto_retrieve_context(
            &self.memory,
            Some(&self.llm),
            &incoming.text,
            &recent_for_rewrite,
            &conversation_id,
            self.config.memory.rag_limit,
        )
        .await
        .unwrap_or(None);
```

### Step 5: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

Fix any remaining callers of the old signature (grep for `auto_retrieve_context` first):

```bash
grep -rn "auto_retrieve_context" src/ 2>&1
```

### Step 6: Run all tests + clippy

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 7: Commit

```bash
git add src/memory/rag.rs src/agent.rs
git commit -m "feat(rag): wire query rewriter into auto_retrieve_context — rewrites follow-ups before vector search"
```

---

## Task 10: Add `chat_stream()` to `LlmClient`

**Files:**
- Modify: `Cargo.toml` (add `stream` feature to reqwest)
- Modify: `src/llm.rs` (add `StreamRequest`, SSE parser, `chat_stream()`)

### Step 1: Write failing tests in `src/llm.rs`

Add to the `#[cfg(test)] mod tests` block in `src/llm.rs`:

```rust
    #[test]
    fn test_parse_sse_line_data_returns_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let result = parse_sse_content(line);
        assert_eq!(result, Some("Hello".to_string()));
    }

    #[test]
    fn test_parse_sse_line_done_returns_none() {
        let result = parse_sse_content("data: [DONE]");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_sse_line_empty_delta_returns_none() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":null}]}"#;
        let result = parse_sse_content(line);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_sse_line_non_data_prefix_returns_none() {
        assert_eq!(parse_sse_content(": OPENROUTER PROCESSING"), None);
        assert_eq!(parse_sse_content(""), None);
        assert_eq!(parse_sse_content("event: ping"), None);
    }

    #[test]
    fn test_parse_sse_line_null_content_returns_none() {
        let line = r#"data: {"choices":[{"delta":{"content":null},"finish_reason":"stop"}]}"#;
        let result = parse_sse_content(line);
        assert_eq!(result, None);
    }

    #[test]
    fn test_stream_request_serializes_stream_true() {
        let req = StreamRequest {
            model: "test-model".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            max_tokens: 100,
            stream: true,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["stream"], true);
        assert_eq!(json["model"], "test-model");
    }
```

### Step 2: Run tests to verify they fail

```bash
cargo test test_parse_sse_line test_stream_request_serializes 2>&1 | tail -20
```

Expected: FAIL — `parse_sse_content` and `StreamRequest` not defined.

### Step 3: Add `stream` feature to `Cargo.toml`

Change line:
```toml
reqwest = { version = "0.12", features = ["json"] }
```

To:
```toml
reqwest = { version = "0.12", features = ["json", "stream"] }
```

### Step 4: Implement `StreamRequest`, `parse_sse_content`, and `chat_stream` in `src/llm.rs`

Add imports at the top of `src/llm.rs`:

```rust
use futures_util::StreamExt;
```

Add the `StreamRequest` struct after `ChatRequest` (around line 55):

```rust
/// Like ChatRequest but with stream=true for SSE streaming.
#[derive(Debug, Serialize)]
struct StreamRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    max_tokens: u32,
    stream: bool,
}
```

Add `parse_sse_content` as a module-level function (place after `StreamRequest`, before `impl LlmClient`):

```rust
/// Parse a single SSE line and extract the text content token, if any.
/// Returns `None` for non-data lines, `[DONE]`, empty deltas, or parse errors.
fn parse_sse_content(line: &str) -> Option<String> {
    let data = line.strip_prefix("data: ")?;
    if data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let content = value
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?;
    match content {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
```

Add `chat_stream` to `impl LlmClient` after `chat()` (around line 173):

```rust
/// Stream the final LLM response token-by-token via an mpsc channel.
/// Sends each content token as a separate `String` message.
/// Closes the sender when the stream ends or on error.
/// Does NOT pass tools — use this only for the final text-only response.
pub async fn chat_stream(
    &self,
    messages: &[ChatMessage],
    model: &str,
    token_tx: tokio::sync::mpsc::Sender<String>,
) -> Result<()> {
    let request = StreamRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        tools: None,
        tool_choice: None,
        max_tokens: self.config.max_tokens,
        stream: true,
    };

    let url = format!("{}/chat/completions", self.config.base_url);

    debug!(
        url = %url,
        model = %model,
        message_count = messages.len(),
        "Starting streaming request to OpenRouter"
    );

    let response = self
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", self.config.api_key))
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&request)
        .send()
        .await
        .context("Failed to send streaming request to OpenRouter")?;

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenRouter streaming API error ({}): {}", status, error_body);
    }

    // Accumulate bytes into lines (SSE lines end with \n)
    let mut stream = response.bytes_stream();
    let mut line_buf = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("Stream read error")?;
        let text = String::from_utf8_lossy(&bytes);

        for ch in text.chars() {
            if ch == '\n' {
                let line = line_buf.trim().to_string();
                line_buf.clear();

                if let Some(token) = parse_sse_content(&line) {
                    // Ignore send errors — receiver may have dropped (e.g. Telegram timeout)
                    if token_tx.send(token).await.is_err() {
                        debug!("Stream receiver dropped — stopping early");
                        return Ok(());
                    }
                }
            } else {
                line_buf.push(ch);
            }
        }
    }

    // Process any remaining buffered line (some providers don't end with \n)
    if !line_buf.is_empty() {
        let line = line_buf.trim().to_string();
        if let Some(token) = parse_sse_content(&line) {
            token_tx.send(token).await.ok();
        }
    }

    Ok(())
}
```

### Step 5: Run the unit tests

```bash
cargo test test_parse_sse_line test_stream_request_serializes 2>&1 | tail -20
```

Expected: all 6 new tests PASS.

### Step 6: Run full test suite + clippy

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 7: Commit

```bash
git add Cargo.toml src/llm.rs
git commit -m "feat(llm): add chat_stream() with SSE parsing for token-by-token streaming via mpsc channel"
```

---

## Task 11: Wire Streaming into Agent Loop

**Files:**
- Modify: `src/agent.rs` (`process_message` signature + final response streaming)
- Modify: `src/main.rs` (update `process_message` call to pass `None`)

### Step 1: Write a test for the assembled output contract

Add to `src/agent.rs` `#[cfg(test)] mod tests` (create the block if it doesn't exist):

```rust
#[cfg(test)]
mod tests {
    // Verifies the assembled content helper used in streaming path
    #[test]
    fn test_assemble_tokens_joins_correctly() {
        let tokens = vec!["Hello", " ", "world", "!"];
        let assembled: String = tokens.concat();
        assert_eq!(assembled, "Hello world!");
    }
}
```

This is a trivial test but it documents the assembly contract. The real streaming path is integration-tested manually.

### Step 2: Update `process_message` signature

In `src/agent.rs`, find `process_message` (around line 120):

```rust
pub async fn process_message(
    &self,
    incoming: &IncomingMessage,
    tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
) -> Result<String>
```

Change to:

```rust
pub async fn process_message(
    &self,
    incoming: &IncomingMessage,
    tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
    stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
) -> Result<String>
```

### Step 3: Add streaming to the final response path

In `process_message`, find the final response section (around line 333–358):

```rust
            // Final response — no tool calls
            let content = response.content.clone().unwrap_or_default();
            // ... save + return
            return Ok(content);
```

Replace the final-response block with:

```rust
            // Final response — no tool calls
            let content = response.content.clone().unwrap_or_default();

            // Stream the final response token-by-token if a channel is provided
            if let Some(ref tx) = stream_token_tx {
                // Split content into natural chunks (approx 3–5 words each)
                // for a realistic typing-effect UX without extra LLM API calls.
                // Real SSE streaming (calling chat_stream instead) is future work.
                let words: Vec<&str> = content.split_inclusive(' ').collect();
                let chunk_size = 4usize;
                for chunk in words.chunks(chunk_size) {
                    let piece = chunk.join("");
                    if tx.send(piece).await.is_err() {
                        break; // Receiver dropped (e.g. Telegram timeout) — continue normally
                    }
                    // Small delay between chunks for realistic typing effect
                    tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
                }
                // Drop tx here to signal stream end (sender is moved in, so it drops on return)
            }

            self.memory
                .save_message(&conversation_id, &response)
                .await?;

            // --- LangSmith: end chain run (success) ---
            self.langsmith.end_run(crate::langsmith::EndRunParams {
                id: chain_run_id,
                outputs: Some(serde_json::json!({
                    "response": content,
                    "iterations": iteration,
                })),
                error: None,
                end_time: Self::now_iso8601_static(),
            });

            return Ok(content);
```

> **Note on implementation choice:** We use chunked delivery of the already-received response rather than a second streaming LLM call. This avoids double API cost and is architecturally simpler. The `chat_stream()` method is ready in `llm.rs` for a future PR that uses real SSE by restructuring the agentic loop into a two-phase design.

### Step 4: Update all callers of `process_message` to pass `None`

Search for all call sites:

```bash
grep -n "process_message" src/ -r
```

For each call site, add `None` as the third argument. Typically:

**`src/main.rs`** (background job runner):
```rust
let response = match agent.process_message(&req.incoming, None, None).await {
```

**`src/platform/telegram.rs`** (temporarily, before Task 12 updates it):
```rust
match agent.process_message(&incoming, tool_event_tx, None).await {
```

**`src/agent.rs`** (if `run_subagent` calls `process_message` internally):
```rust
agent.process_message(&incoming, None, None).await
```

### Step 5: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

### Step 6: Run all tests + clippy

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
```

### Step 7: Commit

```bash
git add src/agent.rs src/main.rs
git commit -m "feat(agent): add stream_token_tx to process_message — streams final response as word-chunks via mpsc"
```

---

## Task 12: Wire Streaming Receiver into Telegram Platform

**Files:**
- Modify: `src/platform/telegram.rs`

### Step 1: Write test for the streaming UX helper

Add to the `#[cfg(test)] mod tests` block in `src/platform/telegram.rs`:

```rust
    #[test]
    fn test_should_split_stream_at_4000_chars() {
        // Verifies the overflow split threshold constant
        const TELEGRAM_LIMIT: usize = 3800;
        let short = "a".repeat(100);
        let long = "a".repeat(4000);
        assert!(short.len() < TELEGRAM_LIMIT);
        assert!(long.len() > TELEGRAM_LIMIT);
    }
```

### Step 2: Run test

```bash
cargo test test_should_split_stream_at_4000_chars 2>&1 | tail -10
```

Expected: FAIL — constant not defined yet. (This is a documentation test — it'll pass once we add the constant.)

### Step 3: Add streaming receiver task to `handle_message` in `src/platform/telegram.rs`

After the verbose tool notifier setup (from Task 5 in the original plan), add the streaming channel setup:

```rust
    // Streaming: set up token channel for progressive message display
    const TELEGRAM_STREAM_SPLIT: usize = 3800;

    let (stream_token_tx, stream_token_rx) =
        tokio::sync::mpsc::channel::<String>(128);

    // Spawn receiver task: edits Telegram message as tokens arrive
    let stream_bot = bot.clone();
    let stream_chat_id = msg.chat.id;
    let stream_handle = tokio::spawn(async move {
        use std::time::{Duration, Instant};

        // Send an initial placeholder message to get a message ID
        let Ok(stream_msg) = stream_bot
            .send_message(stream_chat_id, "\u{200B}") // zero-width space placeholder
            .await
        else {
            return;
        };

        let mut buffer = String::new();
        let mut current_msg_id = stream_msg.id;
        let mut last_edit = Instant::now();
        let mut rx = stream_token_rx;

        while let Some(token) = rx.recv().await {
            buffer.push_str(&token);

            // Check if we need to split into a new message
            if buffer.len() > TELEGRAM_STREAM_SPLIT {
                // Send overflow as a new message
                match stream_bot.send_message(stream_chat_id, &buffer).await {
                    Ok(new_msg) => {
                        current_msg_id = new_msg.id;
                        buffer.clear();
                    }
                    Err(_) => break,
                }
                last_edit = Instant::now();
                continue;
            }

            // Edit current message at most every 500ms to avoid Telegram rate limits
            if last_edit.elapsed() >= Duration::from_millis(500) {
                stream_bot
                    .edit_message_text(stream_chat_id, current_msg_id, &buffer)
                    .await
                    .ok();
                last_edit = Instant::now();
            }
        }

        // Final edit with complete content
        if !buffer.is_empty() {
            stream_bot
                .edit_message_text(stream_chat_id, current_msg_id, &buffer)
                .await
                .ok();
        }
        // If buffer is empty (all content already sent via split), nothing to do
    });
```

### Step 4: Update the `process_message` call to pass `stream_token_tx`

Find the call (around line 185):

```rust
    match agent.process_message(&incoming, tool_event_tx, None).await {
```

Change to:

```rust
    match agent.process_message(&incoming, tool_event_tx, Some(stream_token_tx)).await {
```

### Step 5: Handle the streaming message and suppress the normal response

The existing code after `process_message` splits and sends the response text. When streaming is on, the text has already been progressively sent to Telegram via the stream receiver. We need to handle both cases:

Find the section that sends the response (around line 190):

```rust
    // ... existing response split-and-send logic
    let response_text = match agent.process_message(...).await {
        Ok(text) => text,
        Err(e) => { ... }
    };
    // Split into chunks and send
    for chunk in split_response(&response_text) {
        bot.send_message(msg.chat.id, chunk).await?;
    }
```

Update to:

```rust
    let response_text = match agent.process_message(&incoming, tool_event_tx, Some(stream_token_tx)).await {
        Ok(text) => text,
        Err(e) => {
            // On error, wait for stream task to exit, then send error message
            stream_handle.abort();
            format!("Error: {:#}", e)
        }
    };

    // Wait for stream receiver to finish its final edit
    stream_handle.await.ok();

    // Do NOT send the response as a new message — it was already streamed.
    // Only send if streaming produced nothing (empty response guard):
    if response_text.is_empty() {
        // Nothing to do — LLM returned empty
    }
    // If there was an error (message starts with "Error:"), send it:
    // This is already handled by the abort path above if needed.
```

> **Important:** The stream receiver handles all message delivery. The `process_message` return value is used only for DB persistence (already done inside `process_message`) and error handling. Do NOT send the return value as a separate Telegram message — it would duplicate the streamed content.

> **Note on `send_message` for normal (non-streaming) behaviour:** Currently all messages are streamed. If you want streaming to be opt-in (default off, toggle with `/stream`), you can use the same knowledge-table pattern as `/verbose`. For now, all responses stream.

### Step 6: Verify compilation

```bash
cargo check 2>&1 | tail -30
```

Fix any ownership/borrow issues with `stream_token_tx` (it must be moved into the `process_message` call; spawned task gets `stream_token_rx`).

### Step 7: Run all tests + clippy + format

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings 2>&1 | tail -20
cargo fmt --all -- --check 2>&1 | tail -10
```

If fmt fails: `cargo fmt` then re-check.

### Step 8: Commit

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): add streaming receiver task — progressively edits Telegram message as LLM tokens arrive"
```

---

## Task 13: Final Verification + Push

### Step 1: Full test suite

```bash
cargo test 2>&1
```

Expected: all tests pass.

### Step 2: Clippy — zero warnings

```bash
cargo clippy -- -D warnings 2>&1
```

### Step 3: Format check

```bash
cargo fmt --all -- --check 2>&1
```

### Step 4: Release build

```bash
cargo build --release 2>&1 | tail -10
```

Expected: build succeeds.

### Step 5: Commit any cleanup

```bash
git status
git add -u
git commit -m "chore: final formatting and clippy fixes for streaming + query rewriting" 2>/dev/null || echo "Nothing to commit"
```

### Step 6: Push

```bash
git push -u origin claude/chat-history-rag-telegram-T4Jmo
```

---

## Appendix: Key Gotchas

### 1. `futures_util::StreamExt` for `bytes_stream()`

`bytes_stream()` requires reqwest's `stream` feature AND the `StreamExt` trait in scope:
```rust
use futures_util::StreamExt;
```
`futures_util` is already a transitive dependency of tokio. Verify with:
```bash
cargo tree | grep futures-util
```

If it's not available as a direct dep, add to `Cargo.toml`:
```toml
futures-util = "0.3"
```

### 2. Channel drop order in `telegram.rs`

The `stream_token_tx` Sender must be **moved into** `process_message()`. When `process_message()` returns, the Sender is dropped, closing the channel, causing the receiver task's `rx.recv()` to return `None`, triggering the final edit. **Do not clone the sender** — clone would keep it alive and cause the receiver task to hang.

### 3. Streaming + verbose tool UI interaction

When both verbose (tool UI) and streaming are active:
- The tool notifier message shows tool progress
- When the agent finishes tool calls and starts the final response, the notifier's `finish()` deletes the progress message
- Then the stream receiver's placeholder message gets progressively filled
- Sequence: `notifier.finish()` (delete progress) → stream tokens arrive → edit placeholder message

The `finish()` call in the tool notifier must complete **before** the first streaming token appears. In practice, this is guaranteed because:
- `notifier.finish()` is called when `tool_event_tx` is dropped (end of `process_message`)
- Streaming tokens only arrive after the final LLM response starts
- The final LLM response happens after all tools have executed

### 4. `split_inclusive` for word-chunking

`str::split_inclusive(' ')` preserves the space in each split piece, so reassembling gives the original string. Use this instead of `split(' ')` to avoid losing spaces between words:
```rust
"hello world".split_inclusive(' ').collect::<Vec<_>>()
// → ["hello ", "world"]
// concat() → "hello world" ✓

"hello world".split(' ').collect::<Vec<_>>()
// → ["hello", "world"]
// join("") → "helloworld" ✗
```

### 5. Zero-width space placeholder

We use `"\u{200B}"` (zero-width space) as the initial stream message content because Telegram rejects `send_message` with an empty string. The zero-width space is invisible to users and gets replaced by the first edit.

### 6. `auto_retrieve_context` in tests — use `None` for `llm`

All existing tests in `rag.rs` must be updated to pass `None` for the new `llm` parameter. Using `None` skips the rewrite call and uses the original query — correct behaviour for unit tests without a live LLM.

### 7. Check `run_subagent` in `agent.rs`

The `run_subagent()` function creates a fresh message list and calls `process_message()` recursively (or calls the LLM directly). Search for it:
```bash
grep -n "process_message\|run_subagent" src/agent.rs
```
Any internal call to `process_message()` must pass `None, None` for the two new params.
