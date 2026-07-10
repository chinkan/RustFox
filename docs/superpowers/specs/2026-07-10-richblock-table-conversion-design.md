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
    │                              └─ HTTP 400 ──► sendMessage(entities) fallback
    │
    ▼
edit_rich_message()  ──►  POST /editMessageText { rich_message }
                            (streaming final flush only)
```

### New components

#### `src/utils/rich_sender.rs` — 3 public functions

##### `send_rich_message(bot_token, chat_id, markdown) -> Result<Message>`

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

- Pre-processes markdown using existing `preprocess_markdown()` (spoiler/underline)
- Returns the `Message` on success
- Returns `Err` on HTTP 400 (parse failure triggers fallback) or network errors

##### `edit_rich_message(bot_token, chat_id, msg_id, markdown) -> Result<Message>`

Edit existing message via `POST /bot{token}/editMessageText` with `rich_message` parameter.
Same payload shape minus `chat_id`/`message_id`.

##### `send_rich_messages(bot_token, chat_id, markdown) -> Result<()>`

Auto-chunks long markdown content at `\n` boundaries (max 4000 UTF-16 code units).
Sends chunks sequentially via `send_rich_message`. Returns error if the first chunk
fails (subsequent chunk errors are logged but ignored, matching current behaviour).

#### `try_send_rich_fallback(bot_token, chat_id, markdown, entity_sender)` — fallback helper

1. Pre-process markdown
2. Try `send_rich_messages`
3. On HTTP 400: call `entity_sender` closure (the existing entity pipeline)
4. On network error: propagate error

### Changes to existing files

#### `src/platform/telegram.rs`

**`send_markdown_message()` (line 225):**

Replace current entity-only implementation with:

```rust
async fn send_markdown_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
    let token = bot.inner().token();  // or passed from main
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
        Ok(())
    };
    try_send_rich_fallback(token, chat_id, markdown, entity_sender).await?;
    Ok(())
}
```

**Streaming final flush (lines 1214-1241):**

On final flush, for the first chunk (which has an existing `msg_id` from streaming):
```rust
edit_rich_message(token, chat_id, msg_id, chunk_markdown).await.ok()
```
Fall back to current entity-based edit on failure.

For trailing chunks (new messages):
```rust
send_rich_message(token, chat_id, chunk_markdown).await.ok()
```

The streaming path pre-processes the full accumulated markdown.

#### `src/utils/markdown_entities.rs`

No changes needed — entity pipeline is retained as the fallback path.

### Data flow

1. LLM produces markdown (may include `| A | B |\n|---|---|\n| 1 | 2 |` tables)
2. `send_markdown_message` receives the markdown string
3. `preprocess_markdown()` converts `||spoiler||` and `<u>underline</u>` (same as today)
4. `send_rich_messages` chunks at `\n` boundaries, max 4000 UTF-16 per chunk
5. Each chunk POSTed to `/sendRichMessage` with `InputRichMessage { markdown, skip_entity_detection: true }`
6. Telegram server parses markdown into RichBlock tree — tables become `RichBlockTable`
7. On HTTP 400: retry with `markdown_to_entities` + `sendMessage(entities)` path

### Error handling

| Scenario | Behaviour |
|----------|-----------|
| `sendRichMessage` returns 400 (bad markdown) | Fall back to entity-based `sendMessage` |
| `sendRichMessage` returns 5xx or network error | Propagate to caller (same as current) |
| First chunk fails | Return error to caller |
| Subsequent chunk fails | Log warning, skip chunk (same as current entity split) |
| `edit_rich_message` fails (streaming flush) | Fall back to entity-based edit |

### Testing

- **Unit test `test_rich_sender_chunking`**: verify markdown is split at newline boundaries, max 4000 UTF-16
- **Integration test `test_rich_sender_api`**: mock HTTP responses for sendRichMessage
- **Existing entity tests unchanged**: entity path still works as fallback

### Dependencies

No new crate dependencies. `reqwest` already in `Cargo.toml` (used by `llm.rs`).
`serde_json` already in `Cargo.toml` (used everywhere).

### Implementation order

1. Create `src/utils/rich_sender.rs` with `send_rich_message`, `send_rich_messages`, `edit_rich_message`, `try_send_rich_fallback`
2. Modify `src/platform/telegram.rs` — replace `send_markdown_message` to try rich first
3. Modify streaming final flush to try rich message for edits
4. Add unit tests for chunking + API error fallback
5. Build, lint, test
