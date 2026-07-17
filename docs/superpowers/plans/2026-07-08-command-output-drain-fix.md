# Command Output Drain Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the race condition in `execute_command_interactive` where `child.wait()` resolves before pipe readers have delivered all output, causing the LLM to see only `"Exit code: 0"` with no command output.

**Architecture:** Capture `JoinHandle`s from the spawned stdout/stderr reader tasks. After the `tokio::select!` loop exits (either via `child.wait()` or cancel), await both handles in sequential code, then drain the remaining mpsc channel data with `try_recv()`. This guarantees the output buffer is complete before the result string is built, with no borrow conflicts or cancel-safety issues.

**Tech Stack:** Rust, Tokio (`JoinHandle`, `mpsc::Receiver::try_recv`, `tokio::join!`)

---

### Task 1: Capture JoinHandles and restructure the select loop

**Files:**
- Modify: `src/agent.rs:2558-2654`

The spawned reader tasks return `JoinHandle`s. We store them, then restructure the select loop to **only determine the exit reason** — the final result is built after the loop, after awaiting handles.

- [ ] **Step 1: Capture JoinHandle from stdout reader**

Current (lines 2558-2574):
```rust
tokio::spawn(async move {
```

Replace with:
```rust
let stdout_handle = tokio::spawn(async move {
```

- [ ] **Step 2: Capture JoinHandle from stderr reader**

Current (lines 2576-2592):
```rust
tokio::spawn(async move {
```

Replace with:
```rust
let stderr_handle = tokio::spawn(async move {
```

- [ ] **Step 3: Replace the select loop with a post-loop drain**

The select loop should no longer build and return the result inline. Instead, it breaks out of the loop with just the exit status info. Replace lines 2594-2654.

Remove:
```rust
        let result = loop {
```

Replace entire block from line 2594 (`// Main select loop`) through line 2654 (`};`) with:

```rust
        // Cap accumulated output to prevent unbounded memory growth
        const MAX_BUFFER_CHARS: usize = 100_000;

        // Main select loop — only determines exit reason
        let mut exit_code: Option<i32> = None;
        let mut cancelled = false;
        tokio::pin!(cancel_rx);

        loop {
            tokio::select! {
                Some(chunk) = output_rx.recv() => {
                    output_buffer.push_str(&chunk);
                    if output_buffer.chars().count() > MAX_BUFFER_CHARS {
                        output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
                    }
                    if last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        let body = format!("```\n{}\n```", capped);
                        let text = format!("💻 Running: `{}`\n\n{}", escaped_cmd, body);
                        self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();
                        last_edit = Instant::now();
                    }
                }
                status = child.wait() => {
                    exit_code = Some(status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1));
                    let (icon, label) = if exit_code == Some(0) { ("✅", "Completed") } else { ("❌", "Failed") };
                    let body = if output_buffer.is_empty() {
                        "Command completed with no output.".to_string()
                    } else {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        format!("```\n{}\n```", capped)
                    };
                    let text = format!("{} {}: `{}`\n\n{}", icon, label, escaped_cmd, body);
                    self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();
                    break;
                }
                _ = &mut cancel_rx => {
                    cancelled = true;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let body = if output_buffer.is_empty() {
                        String::new()
                    } else {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        format!("```\n{}\n```", capped)
                    };
                    let text = if body.is_empty() {
                        format!("❌ Cancelled: `{}`", escaped_cmd)
                    } else {
                        format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, body)
                    };
                    self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();
                    break;
                }
            }
        }

        // Post-loop: wait for readers to finish, drain remaining output
        // Timeout is a safety net — readers finish promptly after pipe EOF.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            async { let _ = tokio::join!(stdout_handle, stderr_handle); },
        ).await;
        while let Ok(chunk) = output_rx.try_recv() {
            output_buffer.push_str(&chunk);
        }
        // Re-cap buffer after drain (defensive — drain may push past limit)
        if output_buffer.chars().count() > MAX_BUFFER_CHARS {
            output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
        }

        // Build the final result with complete output
        let result = if cancelled {
            // Update display with final (complete) output
            let body = if output_buffer.is_empty() {
                String::new()
            } else {
                let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                format!("```\n{}\n```", capped)
            };
            let text = if body.is_empty() {
                format!("❌ Cancelled: `{}`", escaped_cmd)
            } else {
                format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, body)
            };
            self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();
            "⚠️ User cancelled the command".to_string()
        } else if let Some(code) = exit_code {
            // Update display with final (complete) output
            let (icon, label) = if code == 0 { ("✅", "Completed") } else { ("❌", "Failed") };
            let body = if output_buffer.is_empty() {
                "Command completed with no output.".to_string()
            } else {
                let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                format!("```\n{}\n```", capped)
            };
            let text = format!("{} {}: `{}`\n\n{}", icon, label, escaped_cmd, body);
            self.bot.edit_message_text(chat_id, msg.id, &text).await.ok();

            let mut result = String::new();
            if !output_buffer.is_empty() {
                result.push_str(output_buffer.trim_end());
                result.push('\n');
            }
            result.push_str(&format!("Exit code: {}", code));
            result
        } else {
            "Error: command exited with unknown state".to_string()
        };
```

- [ ] **Step 4: Verify the code compiles**

Run: `cargo check 2>&1`

Expected: No errors. If borrow-checker errors occur around `child.wait()` in select vs `child.wait()` in cancel, verify the `&mut` borrows are non-overlapping (they should be — select branches are mutually exclusive).

---

### Task 2: Run clippy and tests

- [ ] **Step 1: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1`

Expected: No new warnings. If any warnings about unused variables or dead code, fix them.

- [ ] **Step 2: Run existing tests**

Run: `cargo test 2>&1`

Expected: All tests pass.

---

### Task 3: Commit

- [ ] **Step 1: Stage and commit**

```bash
git add src/agent.rs
git commit -m "fix: drain pipe reader JoinHandles after child.wait() to capture all command output

The select! loop in execute_command_interactive races child.wait() against
the spawned pipe reader tasks. When the child exits quickly, child.wait()
resolves before the readers have flushed pipe data through the mpsc
channel, producing a result of just \"Exit code: 0\" with no output.

Fix: capture JoinHandle from each spawned reader, then after the select
loop exits, await both handles and drain remaining channel data with
try_recv() before building the result. This guarantees the output buffer
is complete, with no borrow conflicts or cancel-safety issues."
```
