# Interactive Command Terminal & Markdown Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add interactive command execution with cancel button, show commands in verbose mode, fix raw markdown display during streaming, and add message splitting for command responses.

**Architecture:** Three independent-but-compatible features layered on the existing agent loop. Feature 1 is a one-line change. Feature 2 adds interactive command execution with `tokio::select!` + oneshot cancellation (new `RunningCommand` registry in `Agent`). Feature 3 retroactively formats split streaming messages with entities, converts all command responses from `escape_text` to entity-based formatting, and extends the markdown parser for blockquotes/tables.

**Tech Stack:** Rust, tokio, teloxide 0.17, pulldown-cmark 0.12

---

### Task 1: Show command in verbose mode (Feature 1)

**Files:**
- Modify: `src/platform/tool_notifier.rs:379-398`

- [ ] **Step 1: Remove `"command"` from the sensitive-key list**

```rust
// Before (line 379-398):
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "bearer",
        "authorization",
        "api_key",
        "apikey",
        "private_key",
        "cookie",
        "content",
        "command",   // <-- remove this
        "prompt",
        "message",
        "text",
    ]
    .iter()
    .any(|sensitive| lower.contains(sensitive))
}

// After:
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "bearer",
        "authorization",
        "api_key",
        "apikey",
        "private_key",
        "cookie",
        "content",
        "prompt",
        "message",
        "text",
    ]
    .iter()
    .any(|sensitive| lower.contains(sensitive))
}
```

Note: The `SAFE_KEYS` list (line 409) also contains `"name"` but not `"command"`. The `format_args_preview` function iterates `SAFE_KEYS` to build the display string, but for single-arg calls where the key is `"command"`, the path is:

1. `obj.len() == 1` → enters single-arg branch
2. `is_sensitive_key("command")` → previously returned `true`, now returns `false`
3. `key_matches_any("command", &SAFE_KEYS)` → returns `false` (not in list)
4. Falls to the `return String::new()` at line 447

So removing "command" from `is_sensitive_key` is not enough. We also need to add `"command"` to `SAFE_KEYS` so the single-arg path renders it:

```rust
const SAFE_KEYS: [&str; 14] = [
    "query",
    "path",
    "url",
    "title",
    "description",
    "step_id",
    "status",
    "skill_name",
    "agent",
    "model",
    "language",
    "technology",
    "name",
    "command",       // <-- add this
];
```

- [ ] **Step 2: Run tests to verify no regressions**

Run: `cargo test -p rustfox --lib platform::tool_notifier::tests 2>&1`
Expected: All existing tests pass. The test `test_format_args_preview_redacts_sensitive_single_arg` will now FAIL because it expects `{"command": "..."}` to be redacted, but it's no longer sensitive. We need to update that test.

- [ ] **Step 3: Fix the failing test**

In `src/platform/tool_notifier.rs`, find the test `test_format_args_preview_redacts_sensitive_single_arg` (line 607). It uses `api_key` as the test case. That should still be redacted. The test should still pass since `api_key` is still sensitive.

Check if any other test uses `"command"` as a sensitive key. If `test_format_args_preview_suppresses_secret_key_variants` includes a `command` variant, update it to use a different key.

The test `test_format_args_preview_multi_arg_keeps_multiple_safe_keys` (line 1041) should still pass.

- [ ] **Step 4: Commit**

```bash
git add src/platform/tool_notifier.rs
git commit -m "feat: show command text in verbose tool notification"
```

---

### Task 2: Add `truncate_tail` utility (Feature 2 prerequisite)

**Files:**
- Create: `src/utils/strings.rs` (modify existing)

- [ ] **Step 1: Write the failing test**

In `src/utils/strings.rs`, add to the `mod tests` block:

```rust
#[test]
fn test_truncate_tail_short_text() {
    let input = "hello world";
    let result = super::truncate_tail(input, 100);
    assert_eq!(result, "hello world");
}

#[test]
fn test_truncate_tail_exact() {
    let input = "hello";
    let result = super::truncate_tail(input, 5);
    assert_eq!(result, "hello");
}

#[test]
fn test_truncate_tail_truncated() {
    let input = "abcdefghijklmnopqrstuvwxyz";
    let result = super::truncate_tail(input, 10);
    assert!(result.starts_with("...(truncated)\n"));
    assert_eq!(result.len(), "abcdefghij".len() + "...(truncated)\n".len());
    assert!(result.ends_with("abcdefghij"));
}

#[test]
fn test_truncate_tail_chinese() {
    let input = "每日上午10點 arXiv AI 論文摘要（香港時間）這是一段很長的中文文字";
    let result = super::truncate_tail(input, 10);
    assert!(result.starts_with("...(truncated)\n"));
    let char_count = result.chars().count();
    // 10 tail chars + 16 prefix chars
    assert!(char_count <= 27, "too long: {} chars", char_count);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rustfox -- utils::strings::tests --test test_truncate_tail 2>&1`
Expected: FAIL — `truncate_tail` not defined

- [ ] **Step 3: Write the minimal implementation**

After the existing `truncate_chars` function in `src/utils/strings.rs`:

```rust
/// Keep the last `max_chars` characters of `s`. If `s` exceeds `max_chars`,
/// prepend `"...(truncated)\n"` to the tail.
/// Safe for any UTF-8 input.
pub fn truncate_tail(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let prefix = "...(truncated)\n";
    let tail: String = s.chars().skip(char_count.saturating_sub(max_chars)).collect();
    format!("{}{}", prefix, tail)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rustfox -- utils::strings::tests --test test_truncate_tail 2>&1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/utils/strings.rs
git commit -m "feat: add truncate_tail utility for command output display"
```

---

### Task 3: Add RunningCommand registry and interactive command execution (Feature 2 core)

**Files:**
- Modify: `src/agent.rs` — add `RunningCommand` struct, `running_commands` field to `Agent`, `execute_command_interactive` method, `"execute_command"` arm in `execute_tool`
- Modify: `src/agent.rs` — add imports for `tokio::sync::oneshot`, `tokio::process`, `tokio::io::{AsyncBufReadExt, BufReader}`, `std::process::Stdio`

- [ ] **Step 1: Add the RunningCommand struct and imports**

At the top of `src/agent.rs`, add to the existing imports:

```rust
use std::io::BufReader as StdBufReader;  // for sync BufReader
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::oneshot;
```

After the `ScheduledJobRequest` struct (around line 41), add:

```rust
/// A running shell command that can be cancelled by the user.
pub struct RunningCommand {
    pub cancel_tx: oneshot::Sender<()>,
}
```

- [ ] **Step 2: Add `running_commands` to `Agent` struct**

In the `Agent` struct (line 46), add a new field:

```rust
pub running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>,
```

The `Agent` struct is created in `Agent::new()` (find it by searching). Add initialization there:

```rust
running_commands: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
```

Also add `use std::collections::HashMap;` to the imports.

- [ ] **Step 3: Add `execute_command_interactive` method to `Agent`**

Find the `execute_tool` method (line 2476). Just before it, add:

```rust
async fn execute_command_interactive(
    &self,
    arguments: &serde_json::Value,
    _user_id: &str,
    chat_id: ChatId,
) -> String {
    use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
    use tokio::io::AsyncReadExt;
    use std::time::Instant;

    let command = match arguments["command"].as_str() {
        Some(c) => c,
        None => return "Error: Missing 'command' argument".to_string(),
    };

    let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());
    let sandbox_dir = &self.config.sandbox.allowed_directory;

    // Spawn process
    let mut child = match TokioCommand::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(sandbox_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Error: Failed to spawn command: {}", e),
    };

    // Send initial message with cancel button
    let initial_text = format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", crate::utils::telegram_markdown::escape_text(command));
    let keyboard = InlineKeyboardMarkup::new([[
        InlineKeyboardButton::callback("Cancel", format!("cancel_cmd:{}", cmd_id)),
    ]]);

    let msg = match self.bot.send_message(chat_id, &initial_text)
        .reply_markup(keyboard)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            let _ = child.kill().await;
            return format!("Error: Failed to send command message: {}", e);
        }
    };

    // Set up cancel channel
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Register in running_commands
    {
        let mut map = self.running_commands.lock().await;
        map.insert(cmd_id.clone(), RunningCommand { cancel_tx });
    }

    // Set up output streaming
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();

    // Spawn stdout reader
    tokio::spawn(async move {
        if let Some(mut stdout) = child_stdout {
            let mut buf = vec![0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 { break; }
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                if output_tx.send(chunk).await.is_err() { break; }
            }
        }
    });

    // Spawn stderr reader
    tokio::spawn(async move {
        if let Some(mut stderr) = child_stderr {
            let mut buf = vec![0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 { break; }
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                if output_tx.send(chunk).await.is_err() { break; }
            }
        }
    });

    // Main loop: wait for output, completion, or cancel
    let mut output_buffer = String::new();
    let mut last_edit = Instant::now();
    let mut final_result: Option<String> = None;

    // Remove from registry when done
    let cmd_id_for_cleanup = cmd_id.clone();
    let running_commands = self.running_commands.clone();

    let cleanup = |running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>, cmd_id: &str| {
        let id = cmd_id.to_string();
        async move {
            let mut map = running_commands.lock().await;
            map.remove(&id);
        }
    };

    loop {
        tokio::select! {
            Some(line) = output_rx.recv() => {
                output_buffer.push_str(&line);

                // Update message every 500ms
                if last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                    let body = if output_buffer.is_empty() {
                        "⏳ Starting...".to_string()
                    } else {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        format!("```\n{}\n```", capped)
                    };
                    let text = format!("💻 Running: `{}`\n\n{}", crate::utils::telegram_markdown::escape_text(command), body);
                    self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();
                    last_edit = Instant::now();
                }
            }
            status = child.wait() => {
                // Process completed
                let exit_code = status.code().unwrap_or(-1);
                let body = if output_buffer.is_empty() {
                    "Command completed with no output.".to_string()
                } else {
                    let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                    format!("```\n{}\n```", capped)
                };
                let header = if exit_code == 0 {
                    format!("✅ Completed: `{}`\n\n", crate::utils::telegram_markdown::escape_text(command))
                } else {
                    format!("❌ Failed (exit code {}): `{}`\n\n", exit_code, crate::utils::telegram_markdown::escape_text(command))
                };
                let text = format!("{}{}", header, body);
                self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();

                // Build result string for LLM
                let mut result = String::new();
                if !output_buffer.is_empty() {
                    result.push_str(&output_buffer);
                    result.push('\n');
                }
                result.push_str(&format!("Exit code: {}", exit_code));

                // Remove trailing newline for cleaner tool result
                let result = result.trim_end().to_string();
                final_result = Some(result);
                break;
            }
            _ = cancel_rx => {
                // User cancelled — kill process
                child.kill().await.ok();
                child.wait().await.ok(); // reap zombie

                cancel_running_commands(&running_commands, &cmd_id_for_cleanup).await;

                let body = if output_buffer.is_empty() {
                    String::new()
                } else {
                    let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                    format!("```\n{}\n```", capped)
                };
                let text = format!("❌ Cancelled: `{}`\n\n{}", crate::utils::telegram_markdown::escape_text(command), body);
                self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();

                final_result = Some("⚠️ User cancelled the command".to_string());
                break;
            }
        }
    }

    // Cleanup registry
    cleanup(running_commands, &cmd_id_for_cleanup).await;

    final_result.unwrap_or_else(|| "Error: command execution failed".to_string())
}
```

Wait — there's a subtlety with the cancel branch. `cancel_rx` is consumed by the `select!` but `running_commands` is also moved into the cleanup. Let me fix the cancel branch to not use `cancel_running_commands` (which doesn't exist). Instead, the cleanup after the loop does it.

Actually, let me simplify. The `cleanup` closure is defined but not used in the cancel branch correctly. Let me restructure:

```rust
async fn execute_command_interactive(
    &self,
    arguments: &serde_json::Value,
    _user_id: &str,
    chat_id: ChatId,
) -> String {
    use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
    use tokio::io::AsyncReadExt;
    use std::time::Instant;

    let command = match arguments["command"].as_str() {
        Some(c) => c,
        None => return "Error: Missing 'command' argument".to_string(),
    };

    let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());
    let sandbox_dir = &self.config.sandbox.allowed_directory;

    // Spawn process
    let mut child = match TokioCommand::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(sandbox_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Error: Failed to spawn command: {}", e),
    };

    let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

    // Send initial message with cancel button
    let keyboard = InlineKeyboardMarkup::new([[
        InlineKeyboardButton::callback("Cancel", format!("cancel_cmd:{}", cmd_id)),
    ]]);

    let msg = match self.bot.send_message(chat_id,
        &format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd))
        .reply_markup(keyboard)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            let _ = child.kill().await;
            return format!("Error: Failed to send command message: {}", e);
        }
    };

    // Set up cancel channel
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Register in running_commands
    {
        let mut map = self.running_commands.lock().await;
        map.insert(cmd_id.clone(), RunningCommand { cancel_tx });
    }

    // Capture Arc for cleanup
    let running_commands = self.running_commands.clone();
    let cmd_id_clone = cmd_id.clone();

    // Output streaming
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();

    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Some(mut stream) = child_stdout.as_mut() {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(String::from_utf8_lossy(&buf[..n]).to_string()).await.is_err() { break; }
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Some(mut stream) = child_stderr.as_mut() {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(String::from_utf8_lossy(&buf[..n]).to_string()).await.is_err() { break; }
                }
            }
        }
    });

    // Helper to update the Telegram message
    let update_msg = |bot: &Bot, chat_id: ChatId, msg_id: teloxide::types::MessageId, icon: &str, label: &str, body: &str| {
        let text = if body.is_empty() {
            format!("{} {}: `{}`", icon, label, escaped_cmd)
        } else {
            format!("{} {}: `{}`\n\n{}", icon, label, escaped_cmd, body)
        };
        async move {
            bot.edit_message_text(chat_id, msg_id, &text).await.ok();
        }
    };

    // Main select loop
    let mut output_buffer = String::new();
    let mut last_edit = Instant::now();

    let result = loop {
        tokio::select! {
            Some(chunk) = output_rx.recv() => {
                output_buffer.push_str(&chunk);
                if last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                    let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                    update_msg(&self.bot, chat_id, msg.id, "💻", "Running", &format!("```\n{}\n```", capped)).await;
                    last_edit = Instant::now();
                }
            }
            status = child.wait() => {
                let exit_code = status.code().unwrap_or(-1);
                let (icon, label) = if exit_code == 0 { ("✅", "Completed") } else { ("❌", "Failed") };
                let body = if output_buffer.is_empty() {
                    "Command completed with no output.".to_string()
                } else {
                    let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                    format!("```\n{}\n```", capped)
                };
                update_msg(&self.bot, chat_id, msg.id, icon, label, &body).await;

                let mut result = String::new();
                if !output_buffer.is_empty() {
                    result.push_str(output_buffer.trim_end());
                    result.push('\n');
                }
                result.push_str(&format!("Exit code: {}", exit_code));
                break result;
            }
            _ = &mut cancel_rx => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let body = if output_buffer.is_empty() {
                    String::new()
                } else {
                    let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                    format!("```\n{}\n```", capped)
                };
                update_msg(&self.bot, chat_id, msg.id, "❌", "Cancelled", &body).await;
                break "⚠️ User cancelled the command".to_string();
            }
        }
    };

    // Cleanup registry
    let mut map = running_commands.lock().await;
    map.remove(&cmd_id_clone);

    result
}
```

- [ ] **Step 2: Add `"execute_command"` arm in `execute_tool`**

Find the `execute_tool` method (line 2476). Before the MCP catch-all (`_ if self.mcp.is_mcp_tool(name)`), add:

```rust
"execute_command" => {
    self.execute_command_interactive(arguments, user_id, chat_id).await
}
```

- [ ] **Step 3: Add missing imports**

Ensure these imports are present at the top of `agent.rs`:

```rust
use std::collections::HashMap;
use tokio::sync::oneshot;
use tokio::process::Command as TokioCommand;
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check 2>&1`
Expected: No errors

- [ ] **Step 5: Run existing tests**

Run: `cargo test 2>&1`
Expected: All existing tests pass

- [ ] **Step 6: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add interactive command execution with cancel button"
```

---

### Task 4: Wire cancel callback in Telegram handler (Feature 2)

**Files:**
- Modify: `src/platform/telegram.rs` — add `cancel_cmd:*` handler to `handle_model_callback`

- [ ] **Step 1: Add cancel command handler to `handle_model_callback`**

In `handle_model_callback` (line 1141), after the `model_select:cancel` branch (around line 1199), add:

```rust
// Handle command cancellation
if let Some(cmd_id) = data.strip_prefix("cancel_cmd:") {
    let mut map = agent.running_commands.lock().await;
    if let Some(cmd) = map.remove(cmd_id) {
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

- [ ] **Step 2: Verify compilation**

Run: `cargo check 2>&1`
Expected: No errors

- [ ] **Step 3: Run tests**

Run: `cargo test 2>&1`
Expected: All existing tests pass

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: wire cancel command callback in Telegram handler"
```

---

### Task 5: Retroactive entity formatting for split streaming messages (Feature 3a)

**Files:**
- Modify: `src/platform/telegram.rs` — track `sent_msg_ids` in `stream_handle`, re-edit on final flush

- [ ] **Step 1: Track sent message IDs during streaming**

In `stream_handle` (line 958), add a new tracking vec:

```rust
// After the existing let statements (line 964-966):
let mut sent_msg_ids: Vec<(teloxide::types::ChatId, teloxide::types::MessageId)> = Vec::new();
```

At the split point (line 981-995), when a split message is sent, track it:

```rust
// Replace the split-send block (lines 981-995):
if buffer.len() > TELEGRAM_STREAM_SPLIT {
    if let Some(msg_id) = current_msg_id {
        // Edit existing message (this is fine — we already track it)
        if let Err(e) = stream_bot
            .edit_message_text(stream_chat_id, msg_id, &buffer)
            .await
        {
            tracing::warn!(error = %e, "stream_handle: edit failed at split");
        }
    } else {
        // Send as new message and track it
        if let Ok(sent) = stream_bot.send_message(stream_chat_id, &buffer).await {
            sent_msg_ids.push((stream_chat_id, sent.id));
        } else {
            tracing::warn!("stream_handle: send failed at split");
        }
    }
    buffer.clear();
    current_msg_id = None;
    last_action = Instant::now();
    continue;
}
```

Also track the initial message when it's first sent (line 1005-1009):

```rust
// Replace the first-send block (lines 1005-1010):
} else {
    match stream_bot.send_message(stream_chat_id, &buffer).await {
        Ok(sent) => {
            current_msg_id = Some(sent.id);
            // Also track in sent_msg_ids so it gets retroactive formatting
            sent_msg_ids.push((stream_chat_id, sent.id));
        }
        Err(e) => tracing::warn!(error = %e, "stream_handle: initial send failed"),
    }
}
```

- [ ] **Step 2: Update final flush to re-edit all tracked messages**

Replace the final flush block (lines 1019-1049) with:

```rust
// Final: flush whatever is left in the buffer.
if !buffer.is_empty() {
    const MAX_UTF16: usize = 4090;
    let (plain_text, entities) = markdown_to_entities(&buffer);
    let chunks = split_entities(&plain_text, &entities, MAX_UTF16);

    // Track the span that the current in-progress message covers
    // (it was edited during streaming with plain text)
    if let Some(msg_id) = current_msg_id {
        // This message was already sent — ensure it's in sent_msg_ids
        if !sent_msg_ids.iter().any(|(cid, mid)| *cid == stream_chat_id && *mid == msg_id) {
            sent_msg_ids.push((stream_chat_id, msg_id));
        }
    }

    for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
        if i < sent_msg_ids.len() {
            // Re-edit existing split message with proper entities
            let (cid, mid) = sent_msg_ids[i];
            stream_bot
                .edit_message_text(cid, mid, chunk_text)
                .entities(chunk_entities.clone())
                .await
                .ok();
        } else {
            // Overflow chunks beyond what we tracked: send as new messages
            stream_bot
                .send_message(stream_chat_id, chunk_text)
                .entities(chunk_entities.clone())
                .await
                .ok();
        }
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1`
Expected: No errors

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1`
Expected: All existing tests pass. Note: `test_final_flush_uses_entity_based_conversion` and `test_stream_handle_does_not_require_placeholder_send` are source-inspection tests that should still pass.

- [ ] **Step 5: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: retroactively format split streaming messages with entities"
```

---

### Task 6: Entity-based command responses with splitting (Feature 3b)

**Files:**
- Modify: `src/platform/telegram.rs` — convert `/start`, `/clear`, `/verbose`, `/queryrewrite`, `/skills`, error messages to entity-based formatting with splitting
- Modify: `src/platform/telegram.rs` — update `/tools` to use entity-based + grouping

- [ ] **Step 1: Create helper function for entity-based message sending**

Before `is_verbose_enabled` (around line 219), add a helper:

```rust
/// Send a markdown string as entity-formatted message(s), splitting if needed.
/// Returns Ok if at least one message was sent successfully.
async fn send_markdown_message(
    bot: &Bot,
    chat_id: ChatId,
    markdown: &str,
) -> ResponseResult<()> {
    const MAX_UTF16: usize = 4090;
    let (plain_text, entities) = markdown_to_entities(markdown);
    let chunks = split_entities(&plain_text, &entities, MAX_UTF16);

    if chunks.is_empty() {
        // Empty output — send something so the user knows the command ran
        bot.send_message(chat_id, "Done.").await?;
        return Ok(());
    }

    for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
        if i == 0 {
            bot.send_message(chat_id, chunk_text)
                .entities(chunk_entities.clone())
                .await?;
        } else {
            // Best-effort for overflow chunks — ignore send failures
            bot.send_message(chat_id, chunk_text)
                .entities(chunk_entities.clone())
                .await
                .ok();
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Convert `/start` command**

Replace the `/start` handler (lines 592-609):

```rust
if text == "/start" {
    let help = "Hello! I'm your AI assistant. Send me a message and I'll help you.\n\n\
         Commands:\n\
         **/clear** - Clear conversation history\n\
         **/tools** - List available tools\n\
         **/skills** - List loaded skills\n\
         **/update\\-skills** - Re-sync bundled skills/agents (backs up local edits)\n\
         **/verbose** - Toggle tool call progress display\n\
         **/queryrewrite** - Toggle query rewriting for memory search\n\
         **/selfupgrade** - Upgrade the bot (source or release binary)\n\
         **/models** - Browse and change the model";
    return send_markdown_message(&bot, msg.chat.id, help).await;
}
```

- [ ] **Step 3: Convert `/clear` command**

Replace line 583-589:

```rust
if text == "/clear" {
    if let Err(e) = agent
        .clear_conversation("telegram", &user_id.to_string())
        .await
    {
        error!("Failed to clear conversation: {}", e);
    }
    return send_markdown_message(&bot, msg.chat.id, "Conversation archived. Past messages remain searchable.").await;
}
```

- [ ] **Step 4: Convert `/verbose` response**

Replace the response at lines 690-698:

```rust
let reply = if new_value == "true" {
    "🔧 **Tool call UI enabled.** I'll show you what I'm working on."
} else {
    "🔇 **Tool call UI disabled.** I'll respond silently."
};
return send_markdown_message(&bot, msg.chat.id, reply).await;
```

- [ ] **Step 5: Convert `/queryrewrite` response**

Replace the response at lines 727-735:

```rust
let reply = if new_value == "true" {
    "🔍 **Query rewriting enabled.** Follow\\-up questions will be rewritten before memory search."
} else {
    "🔍 **Query rewriting disabled.** Messages will be searched as\\-is."
};
return send_markdown_message(&bot, msg.chat.id, reply).await;
```

- [ ] **Step 6: Convert `/skills` command**

Replace lines 626-643:

```rust
if text == "/skills" {
    let skills_guard = agent.skills.read().await;
    let skills = skills_guard.list();
    if skills.is_empty() {
        return send_markdown_message(&bot, msg.chat.id, "No skills loaded.").await;
    }
    let mut skill_list = String::from("**Loaded skills:**\n\n");
    for skill in &skills {
        skill_list.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
    }
    return send_markdown_message(&bot, msg.chat.id, &skill_list).await;
}
```

- [ ] **Step 7: Convert `/tools` with grouping**

Replace lines 611-623:

```rust
if text == "/tools" {
    let all_tools = agent.all_tool_definitions();
    let mut builtin = Vec::new();
    let mut mcp_servers: std::collections::BTreeMap<String, Vec<&ToolDefinition>> = std::collections::BTreeMap::new();

    for tool in &all_tools {
        if let Some(rest) = tool.function.name.strip_prefix("mcp_") {
            if let Some(sep) = rest.find('_') {
                let server = rest[..sep].to_string();
                mcp_servers.entry(server).or_default().push(tool);
            } else {
                // Unknown MCP format — treat as builtin-like
                builtin.push(tool);
            }
        } else {
            builtin.push(tool);
        }
    }

    let mut tool_list = format!("📦 **Built-in tools ({})**\n", builtin.len());
    for tool in &builtin {
        tool_list.push_str(&format!("  - `{}`: {}\n", tool.function.name, tool.function.description));
    }
    tool_list.push('\n');

    for (server, tools) in &mcp_servers {
        tool_list.push_str(&format!("🔧 **MCP: {} ({})**\n", server, tools.len()));
        for tool in tools {
            tool_list.push_str(&format!("  - `{}`: {}\n", tool.function.name, tool.function.description));
        }
        tool_list.push('\n');
    }

    return send_markdown_message(&bot, msg.chat.id, &tool_list).await;
}
```

Add `use std::collections::BTreeMap;` at the top of the file.

- [ ] **Step 8: Convert error messages**

Replace lines 1113-1117:

```rust
if let Err(e) = process_result {
    warn!(error = %e, "Agent processing failed");
    return send_markdown_message(&bot, msg.chat.id, &format!("**Error:** {}", e)).await;
}
```

- [ ] **Step 9: Verify compilation**

Run: `cargo check 2>&1`
Expected: No errors

- [ ] **Step 10: Run tests**

Run: `cargo test 2>&1`
Expected: Tests pass. The test `test_command_responses_use_escape_text` (line 1396) will FAIL because it asserts `escape_text` appears in the source, but we're replacing it with `send_markdown_message`. Update this test.

- [ ] **Step 11: Fix the test**

Replace the `test_command_responses_use_escape_text` test (line 1396) with:

```rust
#[test]
fn test_command_responses_use_entity_formatting() {
    // Command responses now use send_markdown_message (entity-based) instead of
    // escape_text + ParseMode::MarkdownV2.
    let source = include_str!("telegram.rs");
    assert!(
        source.contains("send_markdown_message"),
        "Command responses must use send_markdown_message for entity-based formatting"
    );
}
```

- [ ] **Step 12: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: convert command responses to entity-based formatting with splitting"
```

---

### Task 7: Extend markdown_to_entities with blockquote and table support (Feature 3c)

**Files:**
- Modify: `src/utils/markdown_entities.rs` — add `Tag::BlockQuote`, `Tag::Table`, `Tag::TableHead`, `Tag::TableRow`, `Tag::TableCell` handling

- [ ] **Step 1: Write failing tests**

In `src/utils/markdown_entities.rs`, add to the `mod tests` block:

```rust
#[test]
fn test_blockquote_prefixes_with_gt() {
    let (text, _) = markdown_to_entities("> This is a quote");
    assert!(
        text.contains("> "),
        "blockquote must be prefixed with '> ': {text}"
    );
    assert!(
        text.contains("This is a quote"),
        "blockquote text must be present: {text}"
    );
}

#[test]
fn test_table_renders_columns() {
    let input = "| A | B |\n|---|---|\n| 1 | 2 |";
    let (text, _) = markdown_to_entities(input);
    assert!(text.contains('A'), "column A must be in output: {text}");
    assert!(text.contains('B'), "column B must be in output: {text}");
    assert!(text.contains('1'), "row 1 col 1 must be in output: {text}");
    assert!(text.contains('2'), "row 1 col 2 must be in output: {text}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rustfox -- utils::markdown_entities::tests --test test_blockquote 2>&1`
Expected: FAIL — blockquote/table not handled

- [ ] **Step 3: Add `StackTag` variants**

In the `StackTag` enum (line 317), add:

```rust
enum StackTag {
    Bold,
    Italic,
    Strikethrough,
    Link(String),
    Heading,
    CodeBlock(Option<String>),
    BlockQuote,    // <-- add
    TableRow,      // <-- add
}
```

- [ ] **Step 4: Handle `Tag::BlockQuote` in `Event::Start`**

In the `Event::Start(tag)` match (around line 84), add:

```rust
Tag::BlockQuote => {
    stack.push((StackTag::BlockQuote, plain_utf16_len));
}
Tag::Table(_) => {
    // Start of a table — no entity needed, just track state
    // We don't push to stack; TableRow handles individual rows
}
Tag::TableHead => {
    stack.push((StackTag::TableRow, plain_utf16_len));
}
Tag::TableRow => {
    stack.push((StackTag::TableRow, plain_utf16_len));
}
Tag::TableCell => {
    // Push a marker so we can add spacing between cells
    // We need to track cell boundaries for padding
    stack.push((StackTag::Bold, plain_utf16_len)); // table cells render as bold
}
```

Wait, the table approach is more complex. Let me keep it simpler: just render table cells with text content, separated by spaces. No need for complex alignment.

Actually, a simpler approach for tables: don't add entities, just extract the plain text from cells with separators.

```rust
Tag::BlockQuote => {
    // Blockquote: prefix with "> " — handled on End
    stack.push((StackTag::BlockQuote, plain_utf16_len));
}
Tag::TableHead | Tag::TableRow => {
    // Each row starts — push a marker
    stack.push((StackTag::TableRow, plain_utf16_len));
}
Tag::TableCell => {
    // Record where this cell starts, so we can add a separator on End
    stack.push((StackTag::Bold, plain_utf16_len)); // just a position marker
}
```

- [ ] **Step 5: Handle `TagEnd` in `Event::End`**

In the `Event::End(tag_end)` match (around line 119), add:

```rust
TagEnd::BlockQuote => {
    if let Some((StackTag::BlockQuote, start)) = stack.pop() {
        // Prefix all lines in the range with "> "
        // We need to modify the plain text retroactively
        // Simple approach: add "> " at the start and after each newline in range
        // But modifying existing plain text would mess up offsets...
        // Better: append "> " to the start of the quote text
        // Since pulldown-cmark gives us the text content between Start/End,
        // we can just prepend "> " to each line we've accumulated.
    }
}
TagEnd::TableHead | TagEnd::TableRow => {
    stack.pop(); // discard TableRow marker
    plain.push('\n');
    plain_utf16_len += 1;
}
TagEnd::TableCell => {
    // Pop the Bold marker we pushed as a position tracker
    stack.pop();
    // Add a spacer between cells (but not after the last cell)
    plain.push_str("  ");
    plain_utf16_len += 2;
}
```

Actually, this blockquote approach is wrong. We can't retroactively modify the plain text because entity offsets would be wrong.

Let me take a different approach for blockquotes — instead of trying to modify the text retrospectively, I'll handle it at the text level:

For blockquotes, we need to inject "> " before each text event inside the blockquote. The simplest way is to add a flag:

```rust
// Before the parser loop:
let mut in_blockquote = false;

// In Event::Start:
Tag::BlockQuote => {
    in_blockquote = true;
}

// In Event::End:
TagEnd::BlockQuote => {
    in_blockquote = false;
}

// In Event::Text:
Event::Text(text) => {
    if in_blockquote {
        // Prefix each line with "> "
        let quoted: String = text.lines()
            .map(|line| format!("> {}", line))
            .collect::<Vec<_>>()
            .join("\n");
        plain.push_str(&quoted);
        plain_utf16_len += quoted.encode_utf16().count();
    } else {
        plain.push_str(&text);
        plain_utf16_len += text.encode_utf16().count();
    }
}
```

And for tables, similarly handle `in_table` / `in_table_row` / `in_table_cell` flags:

```rust
let mut in_blockquote = false;
let mut in_table_cell = false;
let mut cell_texts: Vec<String> = Vec::new();
let mut is_table_head = false;

// In Event::Start:
Tag::BlockQuote => { in_blockquote = true; }
Tag::Table(_) => { cell_texts.clear(); }
Tag::TableHead => { is_table_head = true; }
Tag::TableRow => { cell_texts.clear(); }
Tag::TableCell => { in_table_cell = true; }

// In Event::End:
TagEnd::BlockQuote => { in_blockquote = false; }
TagEnd::TableHead | TagEnd::TableRow => {
    let row_text = cell_texts.join("  ");
    if is_table_head && matches!(tag_end, TagEnd::TableHead) {
        // Add a separator line under the header
        plain.push_str(&row_text);
        plain.push_str("\n");
        plain.push_str(&"-".repeat(row_text.encode_utf16().count()));
        is_table_head = false;
    } else {
        plain.push_str(&row_text);
    }
    plain.push('\n');
    plain_utf16_len += (row_text.encode_utf16().count() + 1) as usize;
    // ... but this is getting complex because we're in the Event::Text handler...

```

This is getting really complex for tables. Let me take a simpler approach:

**Blockquotes**: Use a simple flag approach as shown above. Prefix each line with "> ".

**Tables**: Since proper table rendering with alignment is complex and prone to bugs, and Telegram doesn't support tables natively, let's just render each cell on a new line with a bold prefix-like marker. Actually, the simplest approach: collect the text from all cells sequentially, separated by spaces, terminated by newline per row.

Hmm, this is getting complex in the spec. Let me simplify for the plan — just handle blockquotes, and for tables, add a basic fallback that joins cell content with spaces. The implementation details will be worked out during coding.

Let me write a simpler version:

- [ ] **Step 3: Add blockquote and table state tracking**

Add these variables before the parser loop (line 56):

```rust
let mut in_blockquote = false;
```

- [ ] **Step 4: Handle blockquotes**

In `Event::Start`, add:

```rust
Tag::BlockQuote => {
    in_blockquote = true;
}
```

In `Event::End`, add:

```rust
TagEnd::BlockQuote => {
    in_blockquote = false;
    // Ensure blockquote ends with a newline
    if !plain.ends_with('\n') {
        plain.push('\n');
        plain_utf16_len += 1;
    }
}
```

In `Event::Text`, modify to:

```rust
Event::Text(text) => {
    if in_blockquote {
        let quoted: String = text.lines()
            .map(|line| format!("> {}", line))
            .collect::<Vec<_>>()
            .join("\n");
        plain.push_str(&quoted);
        plain_utf16_len += quoted.encode_utf16().count();
    } else {
        plain.push_str(&text);
        plain_utf16_len += text.encode_utf16().count();
    }
}
```

- [ ] **Step 5: Handle tables**

For tables, add state tracking:

```rust
let mut in_table_cell = false;
let mut table_cell_texts: Vec<String> = Vec::new();
```

In `Event::Start`, add:

```rust
Tag::Table(_) => {
    table_cell_texts.clear();
}
Tag::TableHead => {}
Tag::TableRow => {
    table_cell_texts.clear();
}
Tag::TableCell => {
    in_table_cell = true;
}
```

In `Event::End`, add:

```rust
TagEnd::TableHead => {
    let row = table_cell_texts.join(" │ ");
    plain.push_str(&row);
    plain_utf16_len += row.encode_utf16().count();
    // Add separator row
    let sep = "─".repeat(5);
    let separators: Vec<&str> = (0..table_cell_texts.len()).map(|_| &sep[..]).collect();
    let sep_line = separators.join("─┼─");
    plain.push('\n');
    plain.push_str(&sep_line);
    plain.push('\n');
    plain_utf16_len += sep_line.encode_utf16().count() + 2;
    table_cell_texts.clear();
}
TagEnd::TableRow => {
    let row = table_cell_texts.join(" │ ");
    plain.push_str(&row);
    plain.push('\n');
    plain_utf16_len += row.encode_utf16().count() + 1;
    table_cell_texts.clear();
}
TagEnd::TableCell => {
    in_table_cell = false;
}
```

And in `Event::Text`, accumulate cell text:

```rust
Event::Text(text) => {
    if in_blockquote {
        // ... blockquote handling
    } else if in_table_cell {
        table_cell_texts.push(text.to_string());
        // Don't add to plain yet — we'll join on row end
    } else {
        plain.push_str(&text);
        plain_utf16_len += text.encode_utf16().count();
    }
}
```

Wait, this has a problem — table cells need their text to go through to plain text, not just be accumulated. Otherwise entity offsets break. Let me simplify: just add cell text directly to plain text, with a " | " separator. Don't try to retroactively create an aligned table.

Simplest approach for tables — append cell content directly to plain text with separators:

For `Event::Start`:
```rust
Tag::Table(_) | Tag::TableHead | Tag::TableRow => {
    // No special handling needed
}
Tag::TableCell => {
    in_table_cell = true;
}
```

For `Event::Text`:
```rust
Event::Text(text) => {
    if in_table_cell {
        // Directly append cell text; separator added on End
        plain.push_str(&text);
        plain_utf16_len += text.encode_utf16().count();
    } else if in_blockquote {
        // ... blockquote
    } else {
        plain.push_str(&text);
        plain_utf16_len += text.encode_utf16().count();
    }
}
```

For `Event::End`:
```rust
TagEnd::TableCell => {
    in_table_cell = false;
    plain.push_str(" | ");
    plain_utf16_len += 3;
}
TagEnd::TableRow | TagEnd::TableHead => {
    // Remove trailing " | " and add newline
    if plain.ends_with(" | ") {
        plain.truncate(plain.len() - 3);
        plain_utf16_len = plain_utf16_len.saturating_sub(3);
    }
    plain.push('\n');
    plain_utf16_len += 1;
}
TagEnd::TableBody => {
    // No-op
}
```

This is simple but produces readable output like:
```
Col1 | Col2
A | B
```

OK, this is good enough. Let me write it as a cleaner step in the plan.

Actually, I realize this approach is getting very detailed and complex for a plan document. Let me simplify the plan step to just say "Add blockquote and table handling" with the key changes shown, rather than trying to write every single line.

Let me rewrite this task more concisely:<｜end▁of▁thinking｜>

<｜｜DSML｜｜tool_calls>
<｜｜DSML｜｜invoke name="edit">
<｜｜DSML｜｜parameter name="filePath" string="true">/home/kan/workspaces/playground/myplay/RustBot/docs/superpowers/plans/2026-07-07-streaming-cmd-ui.md