# Stop, BTW, and Steer/Inject Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add /stop (cooperative cancel), /btw (parallel subagent question), and steer/inject (user messages injected mid-processing) to RustFox.

**Architecture:** Three features sharing per-user CancellationToken registry + pending injection queue on Agent. Cancellation checks at iteration boundaries in the agentic loop. Injection drains between tool execution and next LLM call. BTW spawns isolated ad-hoc subagent via `run_subagent`.

**Tech Stack:** Rust, tokio, tokio-util (CancellationToken), teloxide

---

### Task 1: Add tokio-util dependency

**Files:**
- Modify: `Cargo.toml:9`

- [ ] **Step 1: Add tokio-util to dependencies**

Edit `Cargo.toml`, add right after the `tokio` line (line 8):

```toml
# Async runtime
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7" }
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 3: Commit**

```
git add Cargo.toml Cargo.lock
git commit -m "chore: add tokio-util dependency for CancellationToken"
```

---

### Task 2: Add cancel token registry + pending injection queue to Agent

**Files:**
- Modify: `src/agent.rs:3-5` (imports)
- Modify: `src/agent.rs:54-76` (Agent struct)
- Modify: `src/agent.rs:115-154` (Agent::new)

- [ ] **Step 1: Add import**

Add `use tokio_util::sync::CancellationToken;` to the imports at the top of `agent.rs` (insert after `use std::sync::{Arc, Weak};`):

```rust
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
```

- [ ] **Step 2: Add two new fields to `Agent` struct**

After `running_commands` (search for `pub running_commands:`), add:

```rust
    pub running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>,
    /// Per-user CancellationTokens for /stop — created at process_message entry,
    /// removed on exit. Checked at each iteration boundary.
    pub cancel_token_registry: Arc<tokio::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// Per-user pending injection messages (Steer/Inject), max 10 per user.
    /// When a non-command message arrives while processing is active, it's queued here.
    pub pending_injections: Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
```

- [ ] **Step 3: Initialize new fields in `Agent::new`**

In the `Self { ... }` block (after `running_commands` at line 152), add:

```rust
            running_commands: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cancel_token_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_injections: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
```

- [ ] **Step 4: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 5: Commit**

```
git add src/agent.rs
git commit -m "feat(agent): add cancel_token_registry and pending_injections fields"
```

---

### Task 3: Add public methods for cancel/inject on Agent

**Files:**
- Modify: `src/agent.rs` (add methods after `set_model` ends ~line 399)

- [ ] **Step 1: Add six new methods after set_model**

After the closing `}` of `set_model` (search for `pub async fn refresh_context_window_cache`), add:

```rust
    /// Register a CancellationToken for the given user_id before processing starts.
    /// Called at the start of process_message. Returns the token for cancellation checks.
    pub async fn register_cancel_token(&self, user_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.cancel_token_registry
            .lock()
            .await
            .insert(user_id.to_string(), token.clone());
        token
    }

    /// Cancel processing for a user. Returns true if there was an active token.
    pub async fn cancel_processing(&self, user_id: &str) -> bool {
        let mut map = self.cancel_token_registry.lock().await;
        if let Some(token) = map.remove(user_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Check if a user has active processing.
    pub async fn is_processing(&self, user_id: &str) -> bool {
        self.cancel_token_registry
            .lock()
            .await
            .contains_key(user_id)
    }

    /// Queue an injection message for a user. Returns false if queue is full (max 10).
    pub async fn queue_injection(&self, user_id: &str, text: &str) -> bool {
        const MAX_INJECTIONS: usize = 10;
        let mut map = self.pending_injections.lock().await;
        let queue = map.entry(user_id.to_string()).or_default();
        if queue.len() >= MAX_INJECTIONS {
            false
        } else {
            queue.push(text.to_string());
            true
        }
    }

    /// Drain all pending injection messages for a user.
    pub async fn drain_injections(&self, user_id: &str) -> Vec<String> {
        let mut map = self.pending_injections.lock().await;
        map.remove(user_id).unwrap_or_default()
    }

    /// Remove cancel token for a user (called on process_message exit).
    pub async fn clear_cancel_token(&self, user_id: &str) {
        self.cancel_token_registry
            .lock()
            .await
            .remove(user_id);
    }
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 3: Commit**

```
git add src/agent.rs
git commit -m "feat(agent): add cancel/inject queue public methods"
```

---

### Task 4: Add cancellation check + injection drain in process_message

**Files:**
- Modify: `src/agent.rs:660-690` (start of agentic loop, register token)
- Modify: `src/agent.rs:672-680` (top of for-iteration loop)
- Modify: `src/agent.rs:907` (early return — 413 recovery)
- Modify: `src/agent.rs:980` (early return — empty response retry)
- Modify: `src/agent.rs:1343` (success return)
- Modify: `src/agent.rs:1362` (max iterations return)

- [ ] **Step 1: Register cancel token before the agentic loop**

After the `soul_updated` reset (line 663), add:

```rust
        // Reset soul-update flag for this session
        self.soul_updated
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Register cancel token for /stop support
        let cancel_token = self.register_cancel_token(user_id).await;
```

- [ ] **Step 2: Add cancellation check + injection drain at the top of the outer loop**

At the start of `for iteration in 0..max_iterations` (right after line 677), add:

```rust
        for iteration in 0..max_iterations {
            debug!(
                "Trying iteration {}: messages length: {}",
                iteration,
                messages.len()
            );

            // CHECK: cancelled by /stop?
            if cancel_token.is_cancelled() {
                info!(
                    user_id = %user_id,
                    iteration,
                    "Processing cancelled by user via /stop"
                );
                break;
            }

            // CHECK: pending injections from user?
            let injections = self.drain_injections(user_id).await;
            for text in &injections {
                let inject_msg = ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::from_text(format!(
                        "**[User injected mid-processing]:** {}",
                        text
                    ))),
                    tool_calls: None,
                    tool_call_id: None,
                };
                // Save to persistent memory
                self.memory
                    .save_message(&conversation_id, &inject_msg)
                    .await
                    .ok();
                messages.push(inject_msg);
            }
```

- [ ] **Step 3: Add cancellation check inside the retry loop**

The inner retry loop (search for `loop {` after `// Tiers 1-2: sync compaction`) runs multiple LLM call attempts per iteration. Add a check before each LLM call:

```rust
            loop {
                // CHECK: cancelled while retrying?
                if cancel_token.is_cancelled() {
                    info!("Cancelled during retry loop — breaking");
                    break;
                }

                // Clone the base prompt for this retry attempt
                let mut prompt = base_prompt.clone();
```

The `break` exits the retry loop, returning to the outer `for iteration` loop which checks `is_cancelled()` again and `break`s out.

- [ ] **Step 4: Clear cancel token before every return path**

There are 4 return/error paths in `process_message`. Add `self.clear_cancel_token(user_id).await;` before each:

**Path 1 — 413 recovery failed (around line 907):**
```rust
                            self.langsmith.end_run(crate::langsmith::EndRunParams {
                                id: chain_run_id,
                                outputs: None,
                                error: Some(err_str),
                                end_time: Self::now_iso8601_static(),
                            });
                            self.clear_cancel_token(user_id).await;
                            return Err(e);
```

**Path 2 — empty response retry exhausted (around line 980):**
```rust
                            self.langsmith.end_run(crate::langsmith::EndRunParams {
                                id: chain_run_id,
                                outputs: None,
                                error: Some(format!(
                                    "Unable to get valid response after {} attempts",
                                    retry_count + 1
                                )),
                                end_time: Self::now_iso8601_static(),
                            });
                            self.clear_cancel_token(user_id).await;
                            return Err(anyhow::anyhow!(...));
```

**Path 3 — success return (around line 1343):**
```rust
            self.clear_cancel_token(user_id).await;

            return Ok(final_content);
```

**Path 4 — max iterations (around line 1362):**
```rust
        self.clear_cancel_token(user_id).await;

        Ok("I've reached the maximum...")
```

- [ ] **Step 5: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 6: Commit**

```
git add src/agent.rs
git commit -m "feat(agent): add cancellation checks and injection drain in process_message loop"
```

---

### Task 5: Add ask_parallel method + make run_subagent pub(crate)

**Files:**
- Modify: `src/agent.rs:2118` (change run_subagent visibility)
- Modify: `src/agent.rs:2460` (add ask_parallel after run_subagent_loop)

- [ ] **Step 1: Make run_subagent pub(crate)**

Change line 2118 from:

```rust
    async fn run_subagent(
```

to:

```rust
    pub(crate) async fn run_subagent(
```

- [ ] **Step 2: Add CancellationToken parameter to run_subagent_loop**

Modify `run_subagent_loop` signature (search for `async fn run_subagent_loop`) to accept an optional `CancellationToken`:

```rust
    async fn run_subagent_loop(
        &self,
        messages: &mut Vec<ChatMessage>,
        subagent_tools: &[ToolDefinition],
        allowed_tools: &[String],
        model: &str,
        max_iter: u32,
        label: &str,
        cancel_token: Option<CancellationToken>,
    ) -> String {
```

Then add a cancellation check at the start of the subagent loop (search for `for _iteration in 0..max_iter`):

```rust
        for _iteration in 0..max_iter {
            // CHECK: cancelled by /stop?
            if let Some(ref token) = cancel_token {
                if token.is_cancelled() {
                    return format!("Subagent '{}' cancelled by user.", label);
                }
            }
```

Also update the two existing callers of `run_subagent_loop` to pass `None`:
- In `run_subagent` (the ad-hoc path), add `None` as the last argument to `run_subagent_loop`.
- In `run_subagent` (the predefined agent path), also add `None`.

- [ ] **Step 3: Add ask_parallel public method**

After the `run_subagent_loop` method (which ends ~line 2459), add:

```rust
    /// Ask a parallel question while the main agent is processing.
    /// Spawns an isolated ad-hoc subagent with timestamp/location context.
    /// Returns the subagent's answer or an error message.
    pub async fn ask_parallel(&self, question: &str) -> Result<String> {
        let answer = self
            .run_subagent(
                None,
                "Answer the user's follow-up question concisely and accurately using your knowledge.",
                question,
                None,
                None,
            )
            .await;
        // Detect error patterns from run_subagent/run_subagent_loop:
        // - "Subagent '...' error: ..." (API error)
        // - "Subagent '...' reached the maximum number of iterations" (max iterations)
        // - "Subagent '...' returned an empty response after ... attempts" (empty response)
        if answer.starts_with("Subagent '") && (answer.contains("error") || answer.contains("reached the maximum") || answer.contains("empty response"))
        {
            Err(anyhow::anyhow!("{}", answer))
        } else {
            Ok(answer)
        }
    }
```

- [ ] **Step 4: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 5: Commit**

```
git add src/agent.rs
git commit -m "feat(agent): add cancel token to subagent loops, ask_parallel for /btw"
```

---

### Task 6: Add /stop command handler in Telegram handler

**Files:**
- Modify: `src/platform/telegram.rs` (supported_commands, handle_message)

- [ ] **Step 1: Register /stop and /btw in supported_commands**

Search for `pub(crate) fn supported_commands`, then add after the `BotCommand::new("models", ...)` line:

```rust
        BotCommand::new("models", "Browse and change the OpenRouter model"),
        BotCommand::new("stop", "Cancel the current processing gracefully"),
        BotCommand::new("btw", "Ask a parallel question while the bot is busy"),
    ]
}
```

- [ ] **Step 2: Add /stop command handler**

In `handle_message`, after the parse_command dispatch block and before the line that says `bot.send_chat_action` (search for `ChatAction::Typing`), add:

```rust
    // Handle /stop command
    if text == "/stop" {
        if agent
            .cancel_processing(&user_id.to_string())
            .await
        {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⏹ **Processing cancelled.** Accumulated state has been saved.",
            )
            .await;
        } else {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "Nothing is currently processing.",
            )
            .await;
        }
    }
```

- [ ] **Step 3: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 4: Commit**

```
git add src/platform/telegram.rs
git commit -m "feat(telegram): add /stop command handler"
```

---

### Task 7: Add user-busy detection + inject queue for non-command messages

**Files:**
- Modify: `src/platform/telegram.rs` (handle_message)

- [ ] **Step 1: Add user-busy check before process_message**

In `handle_message`, after the /stop handler and before the line with `bot.send_chat_action(msg.chat.id, ...)`, add:

```rust
    // CHECK: if user is currently being processed, queue non-command messages as injection
    if !text.starts_with('/') && agent.is_processing(&user_id.to_string()).await {
        if agent
            .queue_injection(&user_id.to_string(), &text)
            .await
        {
            info!("Queued '{}' as injection for user {}", text, user_id);
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "📨 **Message queued** — will inject into current processing at the next step.",
            )
            .await;
        } else {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⚠️ **Injection queue full** (max 10). Please wait for current processing to finish.",
            )
            .await;
        }
    }
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 3: Commit**

```
git add src/platform/telegram.rs
git commit -m "feat(telegram): queue non-command messages as injections when user is busy"
```

---

### Task 8: Add /btw command handler

**Files:**
- Modify: `src/platform/telegram.rs` (handle_message)

- [ ] **Step 1: Add /btw command handler before the parse_command block**

Add before the line with `if let Some((cmd, arg)) = parse_command(&text)` (search for `parse_command`), after the `/query-rewrite` handler:

```rust
    // Handle /btw <text> for parallel question via isolated subagent
    if text == "/btw" || text.starts_with("/btw ") {
        let btw_text = text
            .strip_prefix("/btw")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("What are you doing?")
            .to_string();

        // Reply immediately, then answer in background
        let _ = send_markdown_message(
            &bot,
            msg.chat.id,
            "⏳ **BTW question sent to subagent...**",
        )
        .await;

        let agent_clone = agent.clone();
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        tokio::spawn(async move {
            match agent_clone.ask_parallel(&btw_text).await {
                Ok(answer) => {
                    let _ = send_markdown_message(&bot_clone, chat_id, &answer).await;
                }
                Err(e) => {
                    let _ = send_markdown_message(
                        &bot_clone,
                        chat_id,
                        &format!("**BTW error:** {}", e),
                    )
                    .await;
                }
            }
        });

        return Ok(());
    }
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Success

- [ ] **Step 3: Commit**

```
git add src/platform/telegram.rs
git commit -m "feat(telegram): add /btw command for parallel subagent questions"
```

---

### Design Note: Injection Queue Overflow Behavior

The spec initially described FIFO drop (oldest message silently discarded when
cap reached). During review, this was changed to explicit rejection (user told
"queue full"). Reason: silent drop is confusing — user thinks their message was
accepted but it was dropped. The `queue_injection` method returns `false` when
full, and the Telegram handler sends a warning message.

---

### Task 9: Build, test, and verify

**Files:**
- Test: all modified files

- [ ] **Step 1: Full cargo check**

Run: `cargo check`
Expected: Clean build with no warnings

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: Clean

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All tests pass (including existing agent, telegram, config tests)

- [ ] **Step 4: Format**

Run: `cargo fmt`
Expected: Clean

- [ ] **Step 5: Final commit**

```
git add src/agent.rs src/platform/telegram.rs Cargo.toml
git commit -m "feat: add /stop, /btw, and steer/inject for mid-processing user interaction"
```
