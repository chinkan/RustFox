# Send File from Sandbox — Design

## Summary

Add a built-in `send_file` tool that lets the AI agent send files it created in the sandbox directly to the user via Telegram. The agent calls the tool with a file path and optional caption, and the bot delivers the file as a document in the chat.

## Motivation

The agent can already create files in the sandbox (`write_file`) but has no way to deliver them to the user. Users must request file contents via `read_file`, which is impractical for binaries, images, or large outputs. A `send_file` tool closes the loop: create → deliver.

## Design

### Tool definition

A new entry in `builtin_tool_definitions()` in `src/tools.rs`:

- **Name:** `send_file`
- **Description:** "Send a file from the sandbox to the current chat. The file must already exist in the sandbox."
- **Parameters:**
  - `path` (string, required) — file path, relative to sandbox or absolute within sandbox
  - `caption` (string, optional) — text caption attached to the document

### Execution

Handled in `agent.rs::execute_tool()` as a new match arm, **before** the built-in fallthrough to `tools::execute_builtin_tool()`. Unlike `read_file`/`write_file`, execution lives in `agent.rs` (not `execute_builtin_tool()`) because it needs the Telegram `Bot` instance and a parsed `ChatId`, which aren't available in the stateless `tools.rs` functions.

The tool uses the existing `bot: Arc<Bot>` on the `Agent` struct to call `send_document()` on the Telegram API. `chat_id` is already available as a parameter to `execute_tool`.

Concrete API call pattern:
```rust
let file_name = full_path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
let input_file = InputFile::memory(bytes).file_name(file_name);
bot.send_document(chat_id, input_file).caption(caption).await?;
```
Telegram auto-detects MIME from content — no explicit MIME type needed.

### Signature change

`execute_tool()` changes `chat_id: &str` to `chat_id: ChatId` (teloxide type) so the `send_file` arm can pass it directly. The two main call sites (sequential tool group, parallel agent tool group) parse the string once before looping.

The subagent call site (`agent.rs:1788`) passes `""` for both `user_id` and `chat_id` — this is safe because subagents use explicit tool whitelists and will never include `send_file` in their available tools, so this code path is never reached for `send_file`. No change needed at that call site.

### Path validation

Reuses the existing `validate_sandbox_path()` from `tools.rs`, which is made `pub` to allow access from `agent.rs`.

### File size limit

Check file size **before** reading bytes using `tokio::fs::metadata()` on the validated path. If the file exceeds 50 MB (Telegram's per-file limit for the standard Bot API), the tool returns an error to the LLM so it can inform the user — avoids loading a huge file into memory only to reject it.

### Error handling

| Scenario | Behaviour |
|----------|-----------|
| File doesn't exist | Error returned to LLM |
| File outside sandbox | `validate_sandbox_path` denies access |
| File > 50 MB | Error with size info |
| Telegram API failure | Error returned to LLM for retry |

All errors flow back into the agent loop as tool result strings, same as any other built-in tool.

### Notifier

A friendly tool name is added in `platform/tool_notifier.rs`:

```rust
"send_file" => return "📤 Sending a file".to_string(),
```

### User-facing flow

```
User: "Can you send me the report you created?"
  → LLM: write_file("report.pdf", ...)        # create file in sandbox
  → LLM: send_file(path="report.pdf", caption="Here's your report")
  → agent.rs: validate path → read bytes → bot.send_document()
  → File arrives in Telegram chat
  → LLM: "File sent successfully!" + text reply
```

## Files changed

| File | Change |
|------|--------|
| `src/tools.rs` | Add `send_file` to `builtin_tool_definitions()` |
| `src/tools.rs` | Make `validate_sandbox_path` `pub` |
| `src/agent.rs` | Change `execute_tool` signature: `chat_id: &str` → `ChatId` |
| `src/agent.rs` | Parse `chat_id` at call sites |
| `src/agent.rs` | Add `"send_file"` arm in `execute_tool()` |
| `src/platform/tool_notifier.rs` | Add friendly name for `"send_file"` |

## Out of scope

- Sending multiple files in one tool call (the agent can call `send_file` multiple times)
- Scheduled/automatic file sending (use `schedule_task` + `send_file`)
- Non-Telegram platform support (can be added later with platform abstraction)
- File streaming or chunked upload (Telegram API handles this internally)
