# Rich Message Conversion — Native Tables via sendRichMessage

**Date:** 2026-07-10
**Feature:** Convert RustFox's markdown→entity pipeline to use `sendRichMessage` (Bot API 10.1)
**Primary benefit:** Native `RichBlockTable` rendering for markdown pipe tables

## Problem

RustFox currently renders markdown pipe tables as plain text with pipe separators:

```
A | B
1 | 2
```

Telegram Bot API 10.1 (June 11, 2026) introduced `sendRichMessage` with `RichBlockTable`
— native styled tables with borders, striping, captions, and per-cell formatting. The
existing entity-based `sendMessage` path cannot produce these tables.

## Solution

Add a new `sendRichMessage`-based sending path that tunnels raw markdown through the
`InputRichMessage` API. Telegram's server-side RichMessage markdown parser recognizes
pipe tables and converts them to `RichBlockTable` blocks automatically.

### Architecture

```
LLM output (markdown with | tables |)
    │
    ▼
preprocess_markdown()   (spoiler ||...|| / <u>...</u>)
    │
    ▼
send_rich_message()  ──►  POST /sendRichMessage
    │                              │
    │                              ├─ success (200) → native Telegram rendering
    │                              │   (RichBlockTable, rich text, lists, etc.)
    │                              │
    │                              └─ RichSenderError::BadMarkdown(400)
    │                                   ──► sendMessage(entities) fallback
    │
    ▼
edit_rich_message()  ──►  POST /editMessageText { message_id, rich_message }
                            (streaming final flush only)

Error type:
  RichSenderError::BadMarkdown(StatusCode)  → triggers fallback
  RichSenderError::Network(String)          → propagated to caller (fatal)
```

### `src/utils/rich_sender.rs` — new module

#### Rust structs for JSON serialization

```rust
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
```

#### `RichSenderError` enum

```rust
#[derive(Debug)]
pub enum RichSenderError {
    /// HTTP 400 from Telegram — bad markdown, triggers entity fallback.
    BadMarkdown(String),
    /// HTTP 5xx, network error, etc. — propagated as fatal.
    Network(anyhow::Error),
}
```

#### `send_rich_message(token, chat_id, markdown) -> Result<Message, RichSenderError>`

Single message via `POST /bot{token}/sendRichMessage` with payload:

```json
{
  "chat_id": 12345,
  "rich_message": {
    "markdown": "...",
    "skip_entity_detection": true
  }
}
```

- Pre-processes markdown using `preprocess_markdown()` (made `pub(crate)` in `markdown_entities.rs`)
- Returns `Message` deserialized from Telegram's response on success
- Returns `RichSenderError::BadMarkdown` on HTTP 400
- Returns `RichSenderError::Network` on 5xx, timeout, connection failure

#### `edit_rich_message(token, chat_id, msg_id, markdown) -> Result<Message, RichSenderError>`

Edit existing message via `POST /bot{token}/editMessageText` with payload:

```json
{
  "chat_id": 12345,
  "message_id": 678,
  "rich_message": {
    "markdown": "...",
    "skip_entity_detection": true
  }
}
```

Note: `editMessageText` requires both `chat_id` AND `message_id`.
The `rich_message` field is an additional parameter alongside the existing fields.

#### `send_rich_messages(token, chat_id, markdown) -> Result<(), RichSenderError>`

Auto-chunks long markdown content at `\n` boundaries (max 4090 UTF-16 code units —
matching the existing entity split limit for consistency). Sends chunks sequentially
via `send_rich_message`. Returns error if the first chunk fails (subsequent chunk
errors are logged but ignored).

#### `try_send_rich_fallback(token, chat_id, markdown, entity_sender) -> Result<()>`

Helper that implements the try-rich-then-fallback pattern:

1. Pre-process markdown via `preprocess_markdown()`
2. Try `send_rich_messages`
3. On `RichSenderError::BadMarkdown`: call `entity_sender` closure with original markdown
4. On `RichSenderError::Network`: propagate to caller (fatal)

### Changes to existing files

#### `src/utils/markdown_entities.rs`

- Change `preprocess_markdown()` from private `fn` to `pub(crate) fn` so `rich_sender.rs` can call it.
- `postprocess_entities()`, `markdown_to_entities()`, `split_entities()` — unchanged (used by fallback path).

#### `src/platform/telegram.rs`

**Token storage:**

Add a module-level `OnceLock<String>` to store the bot token at startup:

```rust
use std::sync::OnceLock;

static BOT_TOKEN: OnceLock<String> = OnceLock::new();

pub fn init_bot_token(token: String) {
    BOT_TOKEN.set(token).ok();
}
```

Called once during `run_bot()` in `main.rs` after `Bot::new(&config.telegram.bot_token)`.

**`send_markdown_message()` (line 225):**

```rust
async fn send_markdown_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
    let token = BOT_TOKEN.get().expect("BOT_TOKEN not initialized");
    let entity_sender = |md: &str| {
        let (text, entities) = markdown_to_entities(md);
        let chunks = split_entities(&text, &entities, 4090);
        for (i, (t, e)) in chunks.iter().enumerate() {
            if i == 0 {
                bot.send_message(chat_id, t).entities(e.clone()).await?;
            } else {
                bot.send_message(chat_id, t).entities(e.clone()).await.ok();
            }
        }
        Ok::<_, teloxide::RequestError>(())
    };
    match try_send_rich_fallback(token, chat_id, markdown, &entity_sender).await {
        Ok(()) => Ok(()),
        Err(RichSenderError::Network(e)) => {
            tracing::warn!(error = %e, "send_rich_message network error, fallback skipped");
            // entity_sender already called inside try_send_rich_fallback on BadMarkdown;
            // Network errors mean we propagate
            entity_sender(markdown).await.map_err(|_| {
                teloxide::request::RequestError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })
        }
    }
}
```

**Streaming final flush (lines 1214-1241):**

On final flush, for the first chunk (existing `msg_id` from streaming):
```rust
if let Some(msg_id) = current_msg_id {
    if edit_rich_message(token, stream_chat_id, msg_id.0 as i32, &chunk_markdown).await.is_err() {
        // fallback to entity edit
        stream_bot.edit_message_text(stream_chat_id, msg_id, chunk_text)
            .entities(chunk_entities.clone()).await.ok();
    }
}
```

For trailing chunks (new messages):
```rust
if send_rich_message(token, stream_chat_id, &chunk_markdown).await.is_err() {
    stream_bot.send_message(stream_chat_id, chunk_text)
        .entities(chunk_entities.clone()).await.ok();
}
```

#### `src/main.rs`

After `let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));`, add:

```rust
rustfox::platform::telegram::init_bot_token(config.telegram.bot_token.clone());
```

### Data flow

1. LLM produces markdown (may include `| A | B |\n|---|---|\n| 1 | 2 |` tables)
2. `send_markdown_message` receives the markdown string
3. `preprocess_markdown()` converts `||spoiler||` and `<u>underline</u>` (same as today)
4. `send_rich_messages` chunks at `\n` boundaries, max 4090 UTF-16 per chunk
5. Each chunk POSTed to `/sendRichMessage` with `InputRichMessage { markdown, skip_entity_detection: true }`
6. Telegram server parses markdown into RichBlock tree — tables become `RichBlockTable`
7. On `RichSenderError::BadMarkdown`: retry with `markdown_to_entities` + `sendMessage(entities)` path
8. On `RichSenderError::Network`: log warning, try entity fallback as last resort

### Error handling

| Scenario | Behaviour |
|----------|-----------|
| `sendRichMessage` returns 400 (bad markdown) | Fall back to entity-based `sendMessage` for full content |
| `sendRichMessage` returns 5xx or network error | Log warning, attempt entity fallback |
| First chunk fails (any error) | Full content retried via entity fallback (entire message falls back) |
| Subsequent chunk fails after rich success | Log warning, skip chunk (degradation: part of message lost) |
| `edit_rich_message` fails (streaming flush) | Fall back to entity-based edit for that chunk |
| Bot API server too old (no `sendRichMessage`) | Every call returns 400, all messages fall back to entities |
| `preprocess_markdown` is called on original markdown (not pre-processed) for entity fallback | Entities handle spoiler/underline independently via their own pipeline |

### Testing

- **Unit test `test_rich_sender_chunking`**: verify markdown is split at newline boundaries, max 4090 UTF-16
- **Unit test `test_rich_sender_error_type`**: verify `RichSenderError` variants match expected discriminator
- **Integration test `test_rich_sender_api`**: mock HTTP responses for sendRichMessage (400 vs 200)
- **Existing entity tests unchanged**: entity path still works as fallback

### Dependencies

No new crate dependencies. `reqwest` already in `Cargo.toml` (used by `llm.rs`).
`serde_json` already in `Cargo.toml` (used everywhere).
`once_cell` already in dependency tree (transitive from teloxide); prefer `std::sync::OnceLock` (Rust 1.70+).

### Implementation order

1. Make `preprocess_markdown` `pub(crate)` in `markdown_entities.rs`
2. Create `src/utils/rich_sender.rs` with structs, `RichSenderError`, `send_rich_message`, `send_rich_messages`, `edit_rich_message`, `try_send_rich_fallback`
3. Add `init_bot_token()` and `BOT_TOKEN` static to `telegram.rs`; wire in `main.rs`
4. Modify `send_markdown_message()` in `telegram.rs` — try rich first, fall back to entities
5. Modify streaming final flush to try rich message for edits (new messages too)
6. Add unit tests for chunking + API error fallback
7. `cargo build`, `cargo clippy -- -D warnings`, `cargo test`
