# Interactive Command Terminal: Fix Missing Output

**Date:** 2026-07-08
**Branch:** `feat/streaming-cmd-ui`
**Status:** Design — awaiting review

## Problem

When executing a command via the interactive terminal UI, the tool result returned to the LLM contains only `"Exit code: 0"` — the actual stdout/stderr output is missing. The LLM cannot see command results (file listings, build output, git status, etc.), making the `execute_command` tool effectively useless for context-dependent decisions.

**Observed log:**

```json
{
  "inputs": { "arguments": { "command": "ls -la /home/kan/" } },
  "outputs": { "result": "Exit code: 0" },
  "metadata": { "ls_run_depth": 1 }
}
```

## Root Cause

`tokio::select!` races `child.wait()` against `output_rx.recv()` on every loop iteration (`src/agent.rs:2599-2654`). When a command finishes quickly:

1. The child process writes output to pipes and exits
2. `child.wait()` resolves immediately — kernel reports the child exited
3. **BUT:** the spawned stdout/stderr reader tasks may not have been polled yet, or may still have data in-flight in the mpsc channel
4. The `break result` fires before `output_buffer` contains any data
5. The result is just `"Exit code: 0"`

The spawned reader tasks eventually read the pipe data and send it to the channel, but nobody is receiving anymore — the loop already broke.

## Solution: Join Reader Tasks Before Building Result

Replace the race-condition-prone `tokio::select!` with a **guarded exit**: only build the result after both `child.wait()` AND all pipe reader `JoinHandle`s have completed.

### Mechanism: `JoinHandle` await

`tokio::spawn` returns a `JoinHandle` that resolves when the spawned task finishes. The reader tasks complete naturally when their pipe reaches EOF (child exits, write end closes). By `await`ing the handles after `child.wait()` resolves, we guarantee all pipe data has been consumed and delivered to the mpsc channel before building the result.

```
                        ┌─────────────────┐
                        │  Child Process   │
                        │  (writes output) │
                        └────────┬────────┘
                                 │
                    ┌────────────┼────────────┐
                    │ stdout     │ stderr     │
                    ▼            ▼            │
             ┌──────────┐ ┌──────────┐       │
             │ Reader 1 │ │ Reader 2 │       │
             │ (spawn)  │ │ (spawn)  │       │
             │ returns  │ │ returns  │       │
             │ JoinHandle│ │ JoinHandle│     │
             └─────┬────┘ └─────┬────┘       │
                   │            │            │
                   │   child.wait()          │
                   │   resolves              │
                   ▼            ▼            │
             join!(handle1, handle2) ────────┘
                         │
                         ▼
                  ┌──────────────┐
                  │ Output is    │
                  │ 100% complete│
                  └──────────────┘
```

**Flow:**

1. Spawn two reader tasks (stdout/stderr), capturing their `JoinHandle`s
2. Readers read from pipes in a loop, sending chunks to a shared `mpsc::channel`
3. On EOF (`read()` returns `Ok(0)`), the reader task loop exits naturally
4. Main select loop races: `mpsc::Receiver::recv()` vs `child.wait()` vs cancel
5. When `child.wait()` fires, the main task enters a **drain phase**:
   - Awaits both `JoinHandle`s via `tokio::join!` (readers already finished or finishing due to pipe EOF)
   - Drains remaining channel data with `try_recv()` in a loop
   - Builds the final result from `output_buffer + exit code`
6. If cancel fires: kill child, await JoinHandles (250ms timeout), drain channel, return cancellation message

### Code: Before vs After

**Before** (`src/agent.rs:2558-2654`):
```rust
tokio::spawn(async move { /* read stdout, send to channel */ });
tokio::spawn(async move { /* read stderr, send to channel */ });

loop {
    tokio::select! {
        Some(chunk) = output_rx.recv() => { /* buffer + display */ }
        status = child.wait() => {
            // ⚠️ RACE: child exited but readers may not have flushed
            let exit_code = ...;
            break result; // output_buffer may be empty
        }
        _ = &mut cancel_rx => { /* kill + break */ }
    }
}
```

**After**:
```rust
// Capture JoinHandles from spawned readers
let stdout_handle = tokio::spawn(async move {
    // ... read stdout, send to mpsc ...
});

let stderr_handle = tokio::spawn(async move {
    // ... read stderr, send to mpsc ...
});

loop {
    tokio::select! {
        Some(chunk) = output_rx.recv() => { /* buffer + display */ }
        status = child.wait() => {
            let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

            // DRAIN PHASE: wait for readers to finish, then drain channel
            let _ = tokio::join!(stdout_handle, stderr_handle);
            while let Ok(chunk) = output_rx.try_recv() {
                output_buffer.push_str(&chunk);
            }

            // Build result from complete output
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
            // Wait for readers to drain pipe data (with timeout)
            tokio::select! {
                _ = async { let _ = tokio::join!(stdout_handle, stderr_handle); } => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
            while let Ok(chunk) = output_rx.try_recv() {
                output_buffer.push_str(&chunk);
            }
            let _ = child.wait().await; // reap zombie
            break "⚠️ User cancelled the command".to_string();
        }
    }
}
```

### Why JoinHandle and not alternatives

| Approach | Problem |
|----------|---------|
| `try_recv` drain only (no join) | Data might still be in pipe buffer, not yet read by spawned task |
| `tokio::sync::Barrier` | `Barrier::wait()` is **not cancel-safe** — cancelling the future mid-wait (e.g. via `tokio::select!` timeout) corrupts internal state and may hang the reader tasks. JoinHandle has no such issue. |
| `oneshot::channel` per reader | Works, but JoinHandle is the idiomatic tokio primitive for this — no extra channels or synchronization needed |
| Single-threaded `.output()` | Loses streaming UX entirely |

### Why JoinHandle works cleanly with borrows

`JoinHandle::await` takes `&mut Self` (via `Future::poll`). It does NOT borrow `child`, `output_rx`, or any shared state — just the handle itself. This means:
- No borrow conflict with `child.wait()` (branch 2 future) — `JoinHandle` await is in the branch **body**, after the future is dropped
- No borrow conflict with cancel branch (branch 3) — `JoinHandle` values are independent of `child`
- No borrow conflict with `output_rx.try_recv()` in the same body — sequential code, borrows are non-overlapping

### Edge cases

| Case | Handling |
|------|----------|
| Command exits before readers process output | `tokio::join!` blocks until readers finish draining pipe data (which happens promptly after EOF) |
| Readers finish before child exits | `stdout_handle` / `stderr_handle` already resolved; `join!` returns immediately |
| Cancel pressed | Kill child, await handles with 250ms timeout, then drain channel |
| Reader task panics | `JoinHandle::await` returns `Err(JoinError)` — `let _ =` discards it; `output_buffer` contains whatever was captured before the panic |
| Command with no output | `output_buffer` empty, result is `"Exit code: 0"` (same as before) |
| Very large output (100K+ chars) | Already capped by `MAX_BUFFER_CHARS`; `join!` waits for all pipes to drain, so final buffer is complete |
| Outer 300s timeout | Unchanged — the parent agent loop timeout still wraps `execute_tool` |
| stderr-only output | Stderr reader hits EOF, exits; joined alongside stdout reader; main task proceeds with all stderr content in buffer |

## File Changes

| File | Change |
|------|--------|
| `src/agent.rs` | Capture `JoinHandle`s from `tokio::spawn`; add `tokio::join!` in drain phase after `child.wait()`; add join+drain in cancel branch |
| `src/agent.rs` imports | Add `tokio::time::sleep`, no new struct imports needed (`JoinHandle` is already the return type of `tokio::spawn`) |

No new dependencies. No new struct imports.

## Verification

1. **Manual test on Telegram**: Run `ls -la /` (short command, large output) — verify LLM sees file listing + exit code. Run `cargo build` (long command) — verify streaming display + final output.
2. **Race condition torture**: Run 100 iterations of a fast command (`echo hello`) in quick succession via the Telegram `/supervise` command. Verify LLM sees `"hello"` alongside `"Exit code: 0"` every time.
3. **`cargo clippy -- -D warnings`** — no new warnings.
4. **`cargo test`** — all existing tests pass.
