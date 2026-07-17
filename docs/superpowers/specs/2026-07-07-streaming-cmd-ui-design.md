# Interactive Command Terminal & Markdown Fixes

**Date:** 2026-07-07
**Branch:** `feat/streaming-cmd-ui`
**Status:** Design — awaiting review

## Overview

Three interconnected improvements to the Telegram bot's tool-calling UX:
1. Show the actual shell command in the verbose tool notification
2. Interactive per-command Telegram message with live output streaming and a cancel button
3. Fix raw-markdown display during streaming and add message splitting for all command responses

---

## Feature 1: Show Command in Verbose Mode

### Problem

`format_args_preview()` in `src/platform/tool_notifier.rs` treats `"command"` as a sensitive key and redacts it. Users see `"💻 Running a command"` with no detail about what is being run, even when `/verbose` mode is enabled.

### Solution

Stop treating `"command"` as a sensitive key in `is_sensitive_key()`. The `format_args_preview()` function already truncates values to 60 chars and omits nested/array arguments — it is safe for command display.

**Changes:**

| File | Change |
|------|--------|
| `src/platform/tool_notifier.rs` | Remove `"command"` from the `is_sensitive_key()` check list |
| `src/platform/tool_notifier.rs` | The preview value from `format_args_preview()` already goes through `truncate_chars()` so long commands are capped |

**Result:**
- Verbose card shows: `💻 Running a command: cargo build --release... -- running`
- Interactive command message also shows the full command (Feature 2)

---

## Feature 2: Interactive Command Terminal with Cancel

### Problem

`execute_command` in `src/tools.rs` uses `tokio::process::Command::output()` which blocks until the child exits. A long-lived process (e.g. `pnpm dev`, `cargo watch`) hangs the entire agent loop with no way to abort.

### Solution

Replace the blocking `.output()` call with an interactive flow that:
1. Sends a dedicated Telegram message showing the command and live output
2. Includes an inline `[Cancel]` button
3. Streams stdout/stderr to the message in near-real-time
4. Kills the child process when Cancel is pressed
5. Returns `"⚠️ User cancelled"` to the LLM

### Architecture

#### New shared state

```rust
// Added to Agent struct (src/agent.rs)
pub running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>,

pub struct RunningCommand {
    pub cancel_tx: oneshot::Sender<()>,
}
```

The `cancel_tx` channel is the bridge between the Telegram callback handler and the waiting agent loop.

#### Flow (in `Agent::execute_tool`)

```
execute_tool("execute_command", {command: "pnpm dev"}, user_id, chat_id)
  │
  ├─ 1. Generate cmd_id = format!("cmd_{}", uuid::Uuid::new_v4())
  │
  ├─ 2. bot.send_message(chat_id,
  │        "💻 Running: `pnpm dev`\n\n```\n⏳ Starting...\n```",
  │        reply_markup = [[Cancel]]  // InlineKeyboardButton::callback("Cancel", "cancel_cmd:{cmd_id}")
  │      )
  │
  ├─ 3. Spawn process:
  │      tokio::process::Command::new("sh")
  │        .arg("-c").arg(command)
  │        .stdout(Stdio::piped())
  │        .stderr(Stdio::piped())
  │        .current_dir(sandbox_dir)
  │        .spawn()
  │
  ├─ 4. Register: running_commands.insert(cmd_id, RunningCommand { cancel_tx })
  │
  ├─ 5. Spawn background reader task:
  │      Reads lines from stdout/stderr via BufReader
  │      Sends lines through mpsc::channel
  │
  ├─ 6. Main loop (tokio::select!):
  │
  │     loop {
  │       tokio::select! {
  │         Some(line) = output_rx.recv() => {
  │           append to buffer
  │           if last_edit > 500ms ago: update_message(buffer)
  │         }
  │         status = child.wait() => {
  │           // Process completed naturally
  │           update_message(final_output, remove keyboard)
  │           return output_string
  │         }
  │         _ = cancel_rx => {
  │           // User pressed Cancel
  │           child.kill().await.ok();
  │           update_message("❌ Cancelled: ...", remove keyboard)
  │           return "⚠️ User cancelled the command"
  │         }
  │       }
  │     }
  │
  └─ 7. Remove from registry (always, in a finally block)
```

#### Message update function

```rust
fn format_command_message(command: &str, output: &str, status: CmdStatus) -> String {
    let icon = match status {
        Running => "💻",
        Completed => "✅",
        Cancelled => "❌",
    };
    let header = format!("{} Running: `{}`\n\n", icon, escape_text(command));
    let body = if output.is_empty() {
        "⏳ Starting...".to_string()
    } else {
        // Cap at ~3500 chars, show tail with truncation marker
        let capped = truncate_tail(output, 3500);
        format!("```\n{}\n```", capped)
    };
    format!("{}{}", header, body)
}
```

`truncate_tail(s, limit)`: If `s` exceeds `limit` chars, keep the last `limit` chars and prepend `"...(truncated)\n"`. Lives in `src/utils/strings.rs` alongside existing `truncate_chars`.

#### Callback handler (src/platform/telegram.rs)

Extend `handle_model_callback` to handle `cancel_cmd:*`:

```rust
if let Some(cmd_id) = data.strip_prefix("cancel_cmd:") {
    let mut map = agent.running_commands.lock().await; // tokio::sync::Mutex
    if let Some(cmd) = map.remove(&cmd_id.to_string()) {
        let _ = cmd.cancel_tx.send(());
        bot.answer_callback_query(callback_id)
            .text("⛔ Command cancelled")
            .await?;
    } else {
        bot.answer_callback_query(callback_id)
            .text("Command already finished")
            .await?;
    }
    return Ok(());
}
```

#### Agent.rs changes

Add an explicit arm in `execute_tool` for `"execute_command"` (currently it falls through to the catch-all `tools::execute_builtin_tool`):

```rust
"execute_command" => {
    // Interactive flow — needs bot, chat_id, running_commands
    self.execute_command_interactive(arguments, user_id, chat_id).await
}
```

#### New method: `Agent::execute_command_interactive`

```rust
async fn execute_command_interactive(
    &self,
    arguments: &Value,
    user_id: &str,
    chat_id: ChatId,
) -> String {
    // Uses self.bot (Arc<Bot>), self.config.sandbox.allowed_directory,
    // and self.running_commands (Arc<tokio::sync::Mutex<HashMap<...>>>)
}
```

This does NOT delegate to `tools::execute_builtin_tool`. The tool definition entry for `execute_command` in `tools.rs` (line 169) stays — it's needed for the LLM to see the tool. Execution is intercepted in `agent.rs`.

#### Edge cases

| Case | Handling |
|------|----------|
| Command exits quickly (< 500ms) | Single message edit: Running → Completed. No intermediate state visible |
| Command produces no output | Show "Command completed with no output" |
| Output exceeds 3500 chars | Keep tail; prepend "...(truncated)\n" |
| Cancel button clicked but process already exited | Registry entry cleaned up; answer callback "Already finished" |
| Multiple commands running | Each has its own message + cmd_id. Independently cancellable |
| Agent loop times out (max_iterations) | The tool is still awaited, so timeout won't fire until command completes. Wrap the entire select loop in `tokio::time::timeout(300s, ...)`. On timeout: kill child, return `"⚠️ Command timed out (300s)"` to LLM |

---

## Feature 3: Markdown Fixes & Message Splitting

### Problem

1. **Split streaming messages show raw markdown**: When the buffer exceeds 3800 chars during streaming, the split message is sent as plain text with visible `**markdown**` syntax. Only the final flush message is properly entity-formatted.

2. **Command responses (`/tools`, `/skills`, `/start`) can exceed 4096 chars**: These use `escape_text() + ParseMode::MarkdownV2` and don't split. Telegram silently rejects or truncates them.

3. **No formatting in command responses**: Using `escape_text()` strips all formatting capability.

### Solution

#### 3a. Retroactive entity-formatting of split messages

During streaming, track every `msg_id` we send (including split messages). On final flush, re-edit ALL tracked messages with entity-based formatting, not just the last one.

```rust
// New state in stream_handle task
let mut sent_msg_ids: Vec<(ChatId, MessageId)> = Vec::new();

// When sending/finalizing a split message:
if let Ok(sent) = bot.send_message(chat_id, &buffer).await {
    sent_msg_ids.push((chat_id, sent.id));
}

// On final flush:
let (plain_text, entities) = markdown_to_entities(&full_buffer);
let chunks = split_entities(&plain_text, &entities, 4090);

for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
    if i < sent_msg_ids.len() {
        // Re-edit existing message with proper entities
        let (cid, mid) = sent_msg_ids[i];
        bot.edit_message_text(cid, mid, chunk_text)
            .entities(chunk_entities.clone())
            .await
            .ok();
    } else {
        // Overflow chunks: send as new messages
        bot.send_message(chat_id, chunk_text)
            .entities(chunk_entities.clone())
            .await
            .ok();
    }
}
```

This ensures EVERY message the user sees is properly formatted, not just the last one.

#### 3b. Entity-based command responses

Replace all `escape_text() + ParseMode::MarkdownV2` command response paths with the entity approach:

| Command | Current | New |
|---------|---------|-----|
| `/start` | `escape_text(help)` + MarkdownV2 | `markdown_to_entities(help)` + `.entities()` |
| `/tools` | `escape_text(tool_list)` + MarkdownV2 | Entity-based + `split_entities()` + grouping |
| `/skills` | `escape_text(skill_list)` + MarkdownV2 | Entity-based + `split_entities()` |
| `/clear` | `escape_text(msg)` + MarkdownV2 | Entity-based |
| `/verbose` response | `escape_text(msg)` + MarkdownV2 | Entity-based |
| `/queryrewrite` | `escape_text(msg)` + MarkdownV2 | Entity-based |
| Error messages | `escape_text(err)` + MarkdownV2 | Entity-based |

#### 3c. Tool grouping for `/tools`

Group tools by origin to make the list scannable:

```
📦 Built-in tools (12)
  - read_file: Read a file...
  - write_file: Write a file...
  - execute_command: Execute a shell command...
  ...

🔍 MCP: brave-search (3)
  - mcp_brave-search_search_web: Search...
  ...

📧 MCP: google-workspace (5)
  - mcp_google-workspace_query_gmail_emails: Query...
  ...
```

If the full list exceeds ~4000 UTF-16 units, truncate and append:
```
... and 8 more tools. Use /tools <server> to see specific tools.
```

#### 3d. Extend markdown_to_entities

Add support in `src/utils/markdown_entities.rs` for:

- **Blockquotes** (`Tag::BlockQuote`): Prefix contained text with `> ` in plain text (cosmetic only — Telegram has no native blockquote entity for the entity-based send path)
- **Tables** (`Tag::Table`, `Tag::TableHead`, `Tag::TableRow`, `Tag::TableCell`): Render as plain text with columns padded to max width per column

These are handled by adding `Event::Start` / `Event::End` arms in the pulldown-cmark parser loop.

---

## Files Changed

| File | Feature | Changes |
|------|---------|---------|
| `src/agent.rs` | F2, F3 | Add `running_commands` field to `Agent`; add `execute_command_interactive()`; add explicit `"execute_command"` arm in `execute_tool()` |
| `src/tools.rs` | F2 | No change — `execute_builtin_tool` stays for other tools; interactive command is handled in `agent.rs` |
| `src/platform/tool_notifier.rs` | F1 | Remove `"command"` from `is_sensitive_key()` |
| `src/platform/telegram.rs` | F2, F3 | Add `cancel_cmd:*` handler to `handle_model_callback`; entity-based command responses; retroactive split-msg formatting; track `sent_msg_ids` in stream_handle |
| `src/utils/markdown_entities.rs` | F3 | Add blockquote and table support |
| `src/utils/telegram_markdown.rs` | F3 | Minimal or no change (entity approach replaces most callers) |
| `src/platform/telegram.rs` (test) | F3 | Update `test_command_responses_use_escape_text` — after migration `markdown_to_entities` replaces `escape_text` in command responses |

## Verification

- `cargo clippy -- -D warnings` — no new warnings
- `cargo test` — all existing tests pass; add tests for:
  - `truncate_tail()` utility function
  - `format_command_message()` formatting
  - Retroactive split-msg entity formatting (integration test)
  - Blockquote rendering in `markdown_to_entities`
  - Table rendering in `markdown_to_entities`
- Manual verify on Telegram:
  - `/verbose` → execute a long command → cancel button works
  - `/tools` with 50+ tools → properly split and grouped
  - Streaming a long markdown response → all split messages formatted
