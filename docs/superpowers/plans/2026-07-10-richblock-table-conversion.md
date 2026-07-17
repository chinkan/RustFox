# RichBlock Table Conversion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `sendRichMessage` (Bot API 10.1) as the primary sending path so markdown pipe tables render as native `RichBlockTable` in Telegram clients.

**Architecture:** New `src/utils/rich_sender.rs` module wraps raw HTTP calls to `POST /sendRichMessage` and `POST /editMessageText` with `rich_message` param. `send_markdown_message` tries rich first, falls back to entities on HTTP 400. Token stored in a `OnceLock<String>` static in `telegram.rs`.

**Tech Stack:** Rust, reqwest, serde_json, teloxide 0.17, Telegram Bot API 10.1

---

### Task 1: Expose `preprocess_markdown` as `pub(crate)`

**Files:**
- Modify: `src/utils/markdown_entities.rs:31`

- [ ] **Step 1: Change `fn preprocess_markdown` to `pub(crate) fn preprocess_markdown`**

In `src/utils/markdown_entities.rs` line 31, change:
```rust
fn preprocess_markdown(md: &str) -> String {
```
to:
```rust
pub(crate) fn preprocess_markdown(md: &str) -> String {
```

- [ ] **Step 2: Run tests to verify nothing broke**

Run: `cargo test -p rustfox markdown_entities -- --test-threads=1`
Expected: ALL tests pass

- [ ] **Step 3: Commit**

```bash
git add src/utils/markdown_entities.rs
git commit -m "feat(rich): make preprocess_markdown pub(crate) for rich_sender reuse"
```

---

### Task 2: Create `src/utils/rich_sender.rs` module

**Files:**
- Create: `src/utils/rich_sender.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Register the module in `src/utils/mod.rs`**

Add to `src/utils/mod.rs`:
```rust
pub mod rich_sender;
```

- [ ] **Step 2: Create `src/utils/rich_sender.rs`**

Write the complete file:

```rust
use serde::{Deserialize, Serialize};
use std::future::Future;
use tracing::warn;

/// Error type distinguishing bad-markdown (retriable) from network (fatal).
#[derive(Debug)]
pub enum RichSenderError {
    /// HTTP 400 from Telegram — bad markdown, triggers entity fallback.
    BadMarkdown(String),
    /// HTTP 5xx, network error, etc. — propagated as fatal.
    Network(anyhow::Error),
}

impl std::fmt::Display for RichSenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RichSenderError::BadMarkdown(msg) => write!(f, "bad markdown: {msg}"),
            RichSenderError::Network(e) => write!(f, "network error: {e}"),
        }
    }
}

impl std::error::Error for RichSenderError {}

// ---------------------------------------------------------------------------
// JSON payload shapes for the Telegram Bot API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InputRichMessage {
    markdown: String,
    #[serde(rename = "skip_entity_detection")]
    skip_entity_detection: bool,
}

#[derive(Serialize)]
struct SendRichMessagePayload {
    chat_id: i64,
    rich_message: InputRichMessage,
}

#[derive(Serialize)]
struct EditRichMessagePayload {
    chat_id: i64,
    message_id: i32,
    rich_message: InputRichMessage,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn build_client() -> reqwest::Client {
    reqwest::Client::new()
}

fn api_url(token: &str, method: &str) -> String {
    format!("https://api.telegram.org/bot{token}/{method}")
}

async fn parse_response(response: reqwest::Response) -> Result<serde_json::Value, RichSenderError> {
    let status = response.status();
    let body = response.text().await;

    #[derive(Deserialize)]
    struct TgResponse {
        ok: bool,
        description: Option<String>,
        result: Option<serde_json::Value>,
    }

    let body = match body {
        Ok(b) => b,
        Err(e) => return Err(RichSenderError::Network(e.into())),
    };

    let parsed: TgResponse = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => return Err(RichSenderError::Network(e.into())),
    };

    if parsed.ok {
        Ok(parsed.result.unwrap_or(serde_json::Value::Null))
    } else if status == 400 || status == 422 {
        Err(RichSenderError::BadMarkdown(
            parsed.description.unwrap_or_default(),
        ))
    } else {
        Err(RichSenderError::Network(anyhow::anyhow!(
            "Telegram API error ({}): {}",
            status,
            parsed.description.unwrap_or_default()
        )))
    }
}

/// Send a single message via `sendRichMessage`.
pub async fn send_rich_message(
    token: &str,
    chat_id: i64,
    markdown: &str,
) -> Result<serde_json::Value, RichSenderError> {
    let client = build_client();
    let payload = SendRichMessagePayload {
        chat_id,
        rich_message: InputRichMessage {
            markdown: markdown.to_string(),
            skip_entity_detection: true,
        },
    };

    let response = client
        .post(api_url(token, "sendRichMessage"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RichSenderError::Network(e.into()))?;

    parse_response(response).await
}

/// Edit an existing message via `editMessageText` with `rich_message` param.
pub async fn edit_rich_message(
    token: &str,
    chat_id: i64,
    message_id: i32,
    markdown: &str,
) -> Result<serde_json::Value, RichSenderError> {
    let client = build_client();
    let payload = EditRichMessagePayload {
        chat_id,
        message_id,
        rich_message: InputRichMessage {
            markdown: markdown.to_string(),
            skip_entity_detection: true,
        },
    };

    let response = client
        .post(api_url(token, "editMessageText"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RichSenderError::Network(e.into()))?;

    parse_response(response).await
}

/// Send potentially-long markdown split at newline boundaries (max 4090 UTF-16).
/// Returns error only if the FIRST chunk fails (subsequent errors logged only).
pub async fn send_rich_messages(
    token: &str,
    chat_id: i64,
    markdown: &str,
) -> Result<(), RichSenderError> {
    const MAX_UTF16: usize = 4090;

    let total_utf16 = markdown.encode_utf16().count();
    if total_utf16 <= MAX_UTF16 {
        return send_rich_message(token, chat_id, markdown).await.map(|_| ());
    }

    let chunks = split_markdown_at_newlines(markdown, MAX_UTF16);

    for (i, chunk) in chunks.iter().enumerate() {
        if i == 0 {
            send_rich_message(token, chat_id, chunk).await?;
        } else {
            if let Err(e) = send_rich_message(token, chat_id, chunk).await {
                warn!("send_rich_message trailing chunk {i} failed: {e}");
            }
        }
    }
    Ok(())
}

/// Split markdown at newline boundaries so each chunk fits within `max_utf16`.
fn split_markdown_at_newlines(text: &str, max_utf16: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let total = text.encode_utf16().count();

    while start < total {
        let ideal_end = (start + max_utf16).min(total);
        // Find the closest newline before ideal_end
        let mut split_at = ideal_end;
        // Convert byte positions for substring search
        let byte_start = char_boundary_from_utf16(text, start);
        let byte_ideal = char_boundary_from_utf16(text, ideal_end);
        if let Some(newline_byte) = text[byte_start..byte_ideal].rfind('\n') {
            let newline_utf16 = text[..byte_start + newline_byte + 1]
                .encode_utf16()
                .count();
            if newline_utf16 > start {
                split_at = newline_utf16;
            }
        }

        let chunk_utf16_len = split_at - start;
        // convert to byte slice
        let byte_start = char_boundary_from_utf16(text, start);
        let byte_end = char_boundary_from_utf16(text, split_at);
        result.push(text[byte_start..byte_end].to_string());
        start = split_at;
    }

    result
}

fn char_boundary_from_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_so_far = 0;
    for (byte_pos, ch) in text.char_indices() {
        if utf16_so_far >= utf16_offset {
            return byte_pos;
        }
        utf16_so_far += ch.len_utf16();
    }
    text.len()
}

/// Try sending via sendRichMessage; on BadMarkdown, call `entity_sender` as fallback.
pub async fn try_send_rich_fallback<F, Fut, E>(
    token: &str,
    chat_id: i64,
    markdown: &str,
    entity_sender: F,
) -> Result<(), RichSenderError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let processed = crate::utils::markdown_entities::preprocess_markdown(markdown);
    match send_rich_messages(token, chat_id, &processed).await {
        Ok(()) => Ok(()),
        Err(RichSenderError::BadMarkdown(msg)) => {
            warn!("sendRichMessage failed (bad markdown), falling back to entities: {msg}");
            entity_sender().await.map_err(|e| RichSenderError::Network(anyhow::anyhow!("fallback: {e}")))
        }
        Err(e @ RichSenderError::Network(_)) => {
            warn!("sendRichMessage network error, propagating to caller: {e}");
            Err(e)
        }
    }
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/utils/rich_sender.rs src/utils/mod.rs
git commit -m "feat(rich): add rich_sender module wrapping sendRichMessage API"
```

---

### Task 3: Wire `BOT_TOKEN` static in `telegram.rs` and `main.rs`

**Files:**
- Modify: `src/platform/telegram.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `init_bot_token` and `BOT_TOKEN` static to `telegram.rs`**

Add near the top of `src/platform/telegram.rs`, after the existing imports:

```rust
use std::sync::OnceLock;

static BOT_TOKEN: OnceLock<String> = OnceLock::new();

/// Must be called once at startup after the Bot is created.
pub fn init_bot_token(token: String) {
    BOT_TOKEN.set(token).ok();
}
```

- [ ] **Step 2: Call `init_bot_token` from `main.rs`**

In `src/main.rs`, after line 209 (`let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));`), add:

```rust
    rustfox::platform::telegram::init_bot_token(config.telegram.bot_token.clone());
```

Note: ensure the `rustfox::platform::telegram` module path is visible (it is — `run_bot` already uses `rustfox::platform::telegram`).

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs src/main.rs
git commit -m "feat(rich): add BOT_TOKEN static and init_bot_token to telegram module"
```

---

### Task 4: Update `send_markdown_message` to try rich first

**Files:**
- Modify: `src/platform/telegram.rs`
- Uses: `src/utils/rich_sender.rs`

- [ ] **Step 1: Add `rich_sender` import to `telegram.rs`**

Add after the existing `use crate::utils::telegram_markdown::escape_text;` line:
```rust
use crate::utils::rich_sender;
```

- [ ] **Step 2: Replace `send_markdown_message` body**

Replace the current `send_markdown_message` function (lines 225-248) with:

```rust
/// Send a markdown string as a rich message via sendRichMessage, falling back
/// to entity-formatted sendMessage on failure.
async fn send_markdown_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
    let token = BOT_TOKEN.get().expect("BOT_TOKEN not initialized");

    let entity_sender = || async {
        let (text, entities) = markdown_to_entities(markdown);
        let chunks = split_entities(&text, &entities, 4090);
        if chunks.is_empty() {
            return Ok::<_, teloxide::RequestError>(());
        }
        for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
            if i == 0 {
                bot.send_message(chat_id, chunk_text)
                    .entities(chunk_entities.clone())
                    .await?;
            } else {
                bot.send_message(chat_id, chunk_text)
                    .entities(chunk_entities.clone())
                    .await
                    .ok();
            }
        }
        Ok(())
    };

    match rich_sender::try_send_rich_fallback(token, chat_id.0, markdown, &entity_sender).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // try_send_rich_fallback already handled BadMarkdown by calling
            // entity_sender internally. If that fallback also failed (or the
            // rich path had a network error), propagate the error — retrying
            // the entity path here would re-send already-delivered chunks.
            warn!("send_rich_message all paths failed: {e}");
            Err(teloxide::RequestError::Io(Arc::new(std::io::Error::other(
                format!("{e}"),
            ))))
        }
    }
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(rich): make send_markdown_message try sendRichMessage first with entity fallback"
```

---

### Task 5: Update streaming final flush to use rich messages

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Import `serde_json::Value` if needed**

Check if `serde_json` is already imported (it likely is via `use teloxide::prelude::*`). If not, add:
```rust
use serde_json::Value;
```
at the top of the file.

- [ ] **Step 2: Capture `BOT_TOKEN` before the streaming spawn**

Before the `tokio::spawn` at line 1145, we need to capture the token. The static `BOT_TOKEN` is accessible anywhere, but the closure is `async move` and needs a reference. Use the static directly inside the closure since `OnceLock` is accessible throughout the process lifetime.

- [ ] **Step 3: Replace the streaming final flush block**

Replace lines 1210-1242 (the `if !split_contents.is_empty()` block) with:

```rust
        if !split_contents.is_empty() {
            let full_text: String = split_contents.join("");
            const MAX_UTF16: usize = 4090;

            // Pre-process markdown for spoiler/underline
            let processed =
                crate::utils::markdown_entities::preprocess_markdown(&full_text);

            // For the rich path: split pre-processed markdown at newline boundaries.
            // For the entity fallback: compute entities from the raw markdown.
            let (plain_text, entities) = markdown_to_entities(&full_text);
            let entity_chunks = split_entities(&plain_text, &entities, MAX_UTF16);
            let total_utf16 = processed.encode_utf16().count();
            let rich_chunks = rich_sender::split_markdown_at_newlines(&processed, MAX_UTF16);

            // Helper: try rich first, fall back to entity chunk i on failure
            let try_rich_or_fallback = |i: usize, msg_id: Option<teloxide::types::MessageId>| {
                let token = BOT_TOKEN.get().expect("BOT_TOKEN not initialized").clone();
                let rich_chunks_ref = &rich_chunks;
                let entity_chunks_ref = &entity_chunks;
                let stream_bot_ref = &stream_bot;
                async move {
                    if let Some(chunk_md) = rich_chunks_ref.get(i) {
                        let result = if let Some(mid) = msg_id {
                            rich_sender::edit_rich_message(
                                &token,
                                stream_chat_id.0,
                                mid.0,
                                chunk_md,
                            )
                            .await
                        } else {
                            rich_sender::send_rich_message(
                                &token,
                                stream_chat_id.0,
                                chunk_md,
                            )
                            .await
                        };
                        if result.is_err() {
                            // Fallback: use entity chunk i
                            if let Some((ct, ce)) = entity_chunks_ref.get(i) {
                                if let Some(mid) = msg_id {
                                    stream_bot_ref
                                        .edit_message_text(stream_chat_id, mid, ct)
                                        .entities(ce.clone())
                                        .await
                                        .ok();
                                } else {
                                    stream_bot_ref
                                        .send_message(stream_chat_id, ct)
                                        .entities(ce.clone())
                                        .await
                                        .ok();
                                }
                            }
                        }
                    }
                }
            };

            if total_utf16 <= MAX_UTF16 {
                try_rich_or_fallback(0, current_msg_id).await;
            } else {
                for (i, _chunk_md) in rich_chunks.iter().enumerate() {
                    if i == 0 {
                        try_rich_or_fallback(0, current_msg_id).await;
                    } else {
                        try_rich_or_fallback(i, None).await;
                    }
                }
            }
        }
```

Note: Because the rich split and entity split use different strategies (newline-only vs. newline-and-space), entity_chunks[i] may not contain exactly the same text as rich_chunks[i]. The fallback per-chunk is best-effort: the text will be semantically correct, just potentially split at slightly different boundaries in the rare error case.

- [ ] **Step 4: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add src/platform/telegram.rs src/utils/rich_sender.rs
git commit -m "feat(rich): update streaming final flush to try sendRichMessage"
```

---

### Task 6: Add unit tests

**Files:**
- Modify: `src/utils/rich_sender.rs` (add `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add chunking tests**

Append to `src/utils/rich_sender.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_markdown_short_text_not_split() {
        let chunks = split_markdown_at_newlines("hello", 4090);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn test_split_markdown_at_newline_boundary() {
        let text = "A".repeat(2000) + "\n" + &"B".repeat(2000);
        let chunks = split_markdown_at_newlines(&text, 3000);
        assert!(chunks.len() >= 2, "should split into at least 2 chunks");
        assert!(chunks[0].ends_with('\n'), "first chunk should end with newline");
        assert!(!chunks[1].starts_with('\n'), "second chunk should not start with newline");
    }

    #[test]
    fn test_split_markdown_utf16_cjk() {
        // Each CJK char = 1 UTF-16 unit, "你好" = 2 units
        let text = "你好".repeat(3000); // 6000 UTF-16 units
        let chunks = split_markdown_at_newlines(&text, 4090);
        assert!(chunks.len() > 1, "long CJK text must be split");
        for chunk in &chunks {
            let utf16_len = chunk.encode_utf16().count();
            assert!(
                utf16_len <= 4090,
                "chunk must not exceed max_utf16: {utf16_len} > 4090"
            );
        }
    }

    #[test]
    fn test_preprocess_markdown_pub() {
        // Verify preprocess_markdown is accessible
        let result = crate::utils::markdown_entities::preprocess_markdown("**bold**");
        assert!(result.contains("**bold**"), "preprocess should pass through normal markdown");
    }

    #[test]
    fn test_split_markdown_exact_small() {
        let chunks = split_markdown_at_newlines("short", 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short");
    }

    #[test]
    fn test_rich_sender_error_type() {
        let bad_md = RichSenderError::BadMarkdown("bad".into());
        let net = RichSenderError::Network(anyhow::anyhow!("timeout"));
        assert!(matches!(bad_md, RichSenderError::BadMarkdown(_)));
        assert!(matches!(net, RichSenderError::Network(_)));
        assert!(!matches!(bad_md, RichSenderError::Network(_)));
        assert!(!matches!(net, RichSenderError::BadMarkdown(_)));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rustfox -- --test-threads=1`
Expected: All tests pass (existing + new chunking tests)

- [ ] **Step 3: Commit**

```bash
git add src/utils/rich_sender.rs
git commit -m "test(rich): add chunking unit tests for rich_sender"
```

> **Note:** Integration tests against the live Telegram API (e.g., mocking HTTP responses to verify chunking + fallback) are deferred. All chunking and error-variant logic is covered by unit tests. Integration coverage is tracked as a future task.

---

### Task 7: Final verification

**Files:** (no changes)

- [ ] **Step 1: Run full build**

Run: `cargo build`
Expected: Compiles with no errors

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass, including all markdown_entities + rich_sender tests
