# Fix Telegram Streaming No-Response Bug Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix the bug where the Telegram bot receives a valid LLM reply but delivers no message to the user.

**Architecture:** The `stream_handle` task currently sends a `\u{200B}` zero-width-space placeholder immediately on spawn, then edits it as tokens arrive. Telegram rejects `\u{200B}` as an empty message (400), causing the task to return early, dropping the receiver, making every `tx.send()` in `agent.rs` fail, so `process_message` returns `Ok` while nothing was ever sent. The fix removes the placeholder entirely: accumulate tokens and send the **first real message** lazily — either after 500 ms of content or on channel close — then edit that message for subsequent updates. A final fallback `send_message` covers the complete response if no intermediate message was sent.

**Tech Stack:** Rust 2021, Tokio async, teloxide 0.12, `tokio::sync::mpsc`, `std::time::Instant`

---

### Task 1: Reproduce & confirm root cause

**Files:**
- Read: `src/platform/telegram.rs:229-273`

**Step 1: Read the stream_handle task to confirm the placeholder line**

Open `src/platform/telegram.rs` and locate line ~233:
```rust
let Ok(stream_msg) = stream_bot.send_message(stream_chat_id, "\u{200B}").await else {
    return;
};
```
Confirm this pattern exists exactly. If `send_message` fails, the task returns — dropping `stream_token_rx` — with no logging and no fallback.

**Step 2: Confirm agent.rs exits streaming loop on send error**

Open `src/agent.rs` and locate the streaming block (line ~404–415):
```rust
if let Some(ref tx) = stream_token_tx {
    ...
    if tx.send(piece).await.is_err() {
        break;
    }
    ...
}
```
Confirm that if the receiver was dropped, the very first `send()` returns `Err` and the loop breaks — causing `process_message` to return `Ok(content)` without the content ever reaching Telegram.

**Step 3: Confirm telegram.rs treats Ok as "already delivered"**

Open `src/platform/telegram.rs` and locate the post-process block (line ~305–310):
```rust
if let Err(e) = process_result {
    ...
    bot.send_message(msg.chat.id, format!("Error: {:#}", e)).await?;
}
// Success: response already delivered via streaming
```
Confirm there is no send in the `Ok` branch. Root cause confirmed.

---

### Task 2: Write the failing test (TDD)

**Files:**
- Modify: `src/platform/telegram.rs` — `#[cfg(test)] mod tests` block at the bottom

**Step 1: Add a unit test that documents the broken behaviour**

In the `#[cfg(test)] mod tests` block at the bottom of `src/platform/telegram.rs`, add:

```rust
#[test]
fn test_stream_handle_does_not_require_placeholder_send() {
    // If the initial send fails, the stream handle must NOT silently swallow
    // all tokens. This test documents that the placeholder approach is fragile;
    // the implementation plan removes it entirely.
    // After the fix, a failed initial-send path no longer exists, so this test
    // verifies the new code compiles correctly without the \u{200B} literal.
    let source = include_str!("telegram.rs");
    assert!(
        !source.contains(r#""\u{200B}""#),
        "Zero-width-space placeholder must be removed from stream_handle"
    );
}
```

**Step 2: Run the test to see it fail**

```bash
cargo test -p rustfox test_stream_handle_does_not_require_placeholder_send -- --nocapture 2>&1 | tail -20
```

Expected output: `FAILED` — assertion fails because `\u{200B}` is still present.

**Step 3: Commit the failing test**

```bash
git add src/platform/telegram.rs
git commit -m "test: failing test documents \u{200B} placeholder bug"
```

---

### Task 3: Rewrite stream_handle with lazy first-send

**Files:**
- Modify: `src/platform/telegram.rs:229-273`

**Step 1: Replace the stream_handle spawn block**

Find the current spawn block (lines ~229–273) and replace it entirely:

```rust
// Spawn receiver task: edits Telegram message as tokens arrive
let stream_bot = bot.clone();
let stream_chat_id = msg.chat.id;
let stream_handle = tokio::spawn(async move {
    use std::time::{Duration, Instant};

    let mut buffer = String::new();
    let mut current_msg_id: Option<teloxide::types::MessageId> = None;
    let mut last_action = Instant::now();
    let mut rx = stream_token_rx;

    while let Some(token) = rx.recv().await {
        buffer.push_str(&token);

        // When buffer exceeds split threshold, send a NEW message and reset
        if buffer.len() > TELEGRAM_STREAM_SPLIT {
            match stream_bot.send_message(stream_chat_id, &buffer).await {
                Ok(new_msg) => {
                    current_msg_id = Some(new_msg.id);
                    buffer.clear();
                }
                Err(e) => {
                    tracing::error!(error = %e, "stream_handle: send_message failed at split");
                    break;
                }
            }
            last_action = Instant::now();
            continue;
        }

        // Every 500 ms: send first message or edit existing one
        if last_action.elapsed() >= Duration::from_millis(500) {
            if let Some(msg_id) = current_msg_id {
                stream_bot
                    .edit_message_text(stream_chat_id, msg_id, &buffer)
                    .await
                    .ok();
            } else {
                match stream_bot.send_message(stream_chat_id, &buffer).await {
                    Ok(sent) => current_msg_id = Some(sent.id),
                    Err(e) => tracing::warn!(error = %e, "stream_handle: initial send failed"),
                }
            }
            last_action = Instant::now();
        }
    }

    // Final: flush whatever is left in the buffer
    if !buffer.is_empty() {
        if let Some(msg_id) = current_msg_id {
            stream_bot
                .edit_message_text(stream_chat_id, msg_id, &buffer)
                .await
                .ok();
        } else {
            // No intermediate message was sent — deliver the complete response now
            stream_bot
                .send_message(stream_chat_id, &buffer)
                .await
                .ok();
        }
    }
});
```

Key changes vs old code:
- **No `\u{200B}` placeholder send** — nothing is sent until real content exists.
- `current_msg_id` starts as `None`; first real send sets it.
- Errors on `send_message` (split threshold) are **logged** (`tracing::error!`).
- Initial-send failures are logged as `warn` and the loop continues accumulating.
- Final block: if `current_msg_id` is still `None`, falls back to a direct `send_message`.

**Step 2: Run the failing test to verify it now passes**

```bash
cargo test -p rustfox test_stream_handle_does_not_require_placeholder_send -- --nocapture 2>&1 | tail -10
```

Expected: `PASSED`

**Step 3: Run all tests**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass, no regressions.

**Step 4: Run clippy and fmt**

```bash
cargo fmt && cargo clippy -- -D warnings 2>&1 | tail -30
```

Expected: no warnings, no errors.

**Step 5: Commit the fix**

```bash
git add src/platform/telegram.rs
git commit -m "fix: replace \u{200B} placeholder with lazy first-send in stream_handle

Telegram rejects messages containing only zero-width space (U+200B),
causing stream_handle to return early and drop the receiver. This made
every tx.send() in agent.rs fail, breaking the streaming loop so
process_message returned Ok while nothing was ever delivered to the user.

Remove the placeholder send. Instead, accumulate tokens and:
- Send the first real message after 500ms of content (or at channel close).
- Edit that message for subsequent updates.
- Fall back to a direct send_message at the end if no intermediate
  message was sent (covers short responses < 500ms token delivery).

Errors on send are now logged via tracing::error/warn instead of
being silently swallowed."
```

---

### Task 4: Push and verify

**Step 1: Push to feature branch**

```bash
git push -u origin claude/chat-history-rag-telegram-T4Jmo
```

**Step 2: Manual smoke-test checklist**

Start the bot locally and verify each scenario:

| Scenario | Expected |
|---|---|
| Send "Hi" | Bot replies with full response (no blank message, no placeholder) |
| Send a long prompt triggering 3800+ char response | Response split across multiple messages |
| Send message while verbose mode ON | Tool notifier still works alongside streaming |
| Send `/clear` then message | Fresh conversation, streaming works |

**Step 3: Confirm no `\u{200B}` remains in codebase**

```bash
grep -r '\\u{200B}' src/ && echo "FOUND - revert" || echo "CLEAN"
```

Expected: `CLEAN`
