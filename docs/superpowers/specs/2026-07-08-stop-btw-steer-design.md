# Stop, BTW, and Steer/Inject — Mid-Processing User Interaction

Date: 2026-07-08

## Problem

Once the agent begins processing a message (`process_message` runs the agentic
loop), the user has no way to interact mid-flight:

- No way to stop processing gracefully (only kill the bot process)
- No way to redirect the agent mid-task ("use JWT not sessions")
- No way to ask a separate question while the agent is busy
- The Telegram handler is blocked for potentially minutes

## Research

**Hermes Agent**: Uses `/stop` to cancel, "send a new message" to interrupt.

**OpenCode PR #32425 (`subagent-interrupt`)**: Three interrupt modes —
`task_steer` (inject guidance frame, continue), `task_cancel` (grace window
then break), `task_abort` (immediate hard stop). Messages are consumed at the
next turn boundary in the run loop.

**OpenCode Issue #21388 (mid-turn messaging)**: Proposes three modes —
Queue+Inject at tool boundary, Preempt (pause stream + inject + resume), Hard
interrupt with context carry-forward.

## Design

Three interconnected features sharing a `per-user processing state tracked via
a CancellationToken registry`:

### /stop — Cooperative Cancellation

When `/stop` is received during active processing, signal a `CancellationToken`
at the next iteration boundary. The loop breaks gracefully preserving
accumulated conversation state.

**Token lifecycle:**
- Created at `process_message` entry, keyed by `user_id`
- Checked at each iteration boundary (before auto-compact, before the LLM retry loop)
- Removed when `process_message` exits (any path)

**Where checks go:**
- Start of `for iteration in 0..max_iterations` (the outer agentic loop)
- Before auto-compact (Tier 3) — skip if cancelled
- Before 413 recovery (Tier 4) — skip if cancelled
- Start of the retry inner loop (before each LLM call attempt)

**In-flight LLM request** is allowed to finish (cooperative, not aborted).

### Steer/Inject — User Messages Injected Mid-Processing

When a non-command Telegram message arrives and the user is currently being
processed (token exists in registry), the message is **queued as a pending
injection** rather than starting a new `process_message`.

**At the next iteration boundary** (between tool execution and LLM call), the
agent drains pending injections and inserts them as `user`-role ChatMessages.
The agent sees the guidance alongside tool results and adapts. Injected
messages are saved to persistent memory via `self.memory.save_message()` so
they survive restarts and conversation reloads.

**Injection format:**
```
**[User injected mid-processing]:** <user text>
```

**Queue storage:** `pending_injections: Arc<Mutex<HashMap<String, Vec<String>>>>`

### /btw — Parallel Question

When `/btw <text>` is received (regardless of whether user is processing), a
background `tokio::spawn` task calls `run_subagent(None, ..., text, ...)` —
an isolated ad-hoc subagent with timestamp/location context. The answer is
sent as a new Telegram message.

**No interaction with main processing** — fully isolated conversation state.

## Architecture

### Agent state additions

**Dependency:** `tokio-util` — provides `CancellationToken` (used instead of
`AtomicBool` because it supports `.cancelled()` async waiter and can be cloned
across tasks).

```rust
use tokio_util::sync::CancellationToken;

pub struct Agent {
    // ...
    /// Per-user CancellationTokens for /stop
    pub cancel_token_registry: Arc<tokio::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// Per-user pending injection messages (Steer/Inject), max 10 per user.
    pub pending_injections: Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
}
```

### process_message loop changes

```
for iteration in 0..max_iterations {
    // CHECK: cancelled?
    if token_registry.is_cancelled(user_id) {
        break;  // return partial response
    }

    // CHECK: pending injections?
    if let Some(msgs) = drain_injections(user_id) {
        for msg in msgs {
            messages.push(ChatMessage { role: "user", content: msg });
        }
    }

    // ...existing LLM call + tool execution...
}
```

### src/platform/telegram.rs handle_message changes

```
"/stop" => {
    if agent.cancel_processing(user_id) {
        send "⏹ Processing cancelled"
    } else {
        send "Nothing is currently processing."
    }
}

if agent.is_processing(user_id) && !text.starts_with('/') {
    if agent.queue_injection(user_id, text) {
        send "📨 Message queued — will inject into current processing"
    } else {
        send "⚠️ Injection queue full (max 10). Please wait for current processing to finish."
    }
    return;
}

if let Some((cmd, arg)) = parse_command("/btw ...") {
    let answer_bot = bot.clone();
    let answer_chat_id = chat_id;
    tokio::spawn(async move {
        match agent.ask_parallel(&arg).await {
            Ok(answer) => { send_message(answer_bot, answer_chat_id, &answer).await; }
            Err(e) => { send_message(answer_bot, answer_chat_id, &format!("BTW error: {}", e)).await; }
        }
    });
    send "⏳ BTW question sent to subagent..."
    return;
}

**Injection queue:** Per-user cap of 10 messages. Oldest message is dropped
when the cap is reached (FIFO).
```

### Commands registered

```
/stop   — Cancel the current processing gracefully
/btw    — Ask a parallel question while the bot is busy
```

- `cancel_processing(user_id) -> bool` — returns false if no processing was
  active (so caller can give different feedback)
- `ask_parallel(question) -> Result<String>` — returns error instead of
  silently failing; spawned task sends error message to user on failure
- Injected messages are saved to persistent memory (`save_message`) so they
  survive restarts

## Edge Cases

- **Double /stop**: Second cancel finds empty registry → reply "No active
  processing"
- **Inject while idle**: Non-command message without active processing →
  process as normal (existing behavior)
- **Inject + /stop simultaneously**: Cancel wins (loop breaks before next
  injection drain)
- **/btw while idle**: Same behavior — subagent answers immediately
- **Partial state on cancel**: Messages accumulated so far are saved to DB,
  user can continue from there
- **Subagent loops**: Same CancellationToken checked at `run_subagent_loop`
  iteration boundaries
