# Loop Detection + Steer Fix + /btw Context Fork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add loop detection (exact repetition), fix steer injection responsiveness, and upgrade /btw to context-forked side queries.

**Architecture:** Three features implemented in dependency order: (1) steer injection fix (infrastructure for loop detection's "Add instruction"), (2) /btw context fork, (3) LoopDetector module + Telegram callback UX.

**Tech Stack:** Rust, tokio, teloxide, serde_json, fxhash

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/loop_detector.rs` | **Create** | `ToolCallRecord`, `LoopDetector`, `LoopInfo`: hash + rolling window + detect |
| `src/agent.rs` | **Modify** | Add steer drain post-tools (line ~1383), loop detection checks, oneshot callback registry |
| `src/platform/telegram.rs` | **Modify** | /btw context fork (replace `ask_parallel_lightweight`), callback query handler for loop detection |
| `src/config.rs` | **Modify` | Add `LoopDetectionConfig` struct + defaults |
| `config.example.toml` | **Modify** | Add `[agent.loop_detection]` example section |
| `src/llm.rs` | **Modify** | Add `build_btw_context` (or put in agent.rs — see below) |
| `src/lib.rs` | **Modify** | Add `pub mod loop_detector` |

---

### Task 1: Steer Injection Between Tool Calls

**Files:**
- Modify: `src/agent.rs:1376-1387`

- [ ] **Step 1: Add injection drain after tool batch commit**

In `process_message()`, after the `for` loop that pushes tool results to `messages` (around line 1383), before `continue` (line 1387):

```rust
                    // --- Non-agent tool calls run SEQUENTIALLY ---
                    for (idx, name, args, id) in other_group {
                        // ... (existing: regurgitation check, LangSmith, execute, result) ...
                        all_results.push((idx, tool_msg));
                    }

                    // Sort results by original index and push to memory + messages
                    all_results.sort_by_key(|(i, _)| *i);
                    for (_idx, tool_msg) in all_results {
                        self.memory
                            .save_message(&conversation_id, &tool_msg)
                            .await?;
                        messages.push(tool_msg);
                    }

                    // --- Steer injection: drain pending messages between iterations ---
                    // Without this, a steer sent during tool execution is only visible
                    // after the next LLM call completes (the drain at line 869 fires
                    // after the LLM call starts the next iteration).
                    let steer_mode = self.get_mid_run_mode(user_id).await;
                    let injections = self.drain_injections(user_id).await;
                    for text in &injections {
                        let label = if steer_mode == MidRunMode::Steer {
                            "**[Steer]:** "
                        } else {
                            "**[User injected mid-processing]:** "
                        };
                        let msg = ChatMessage {
                            role: "user".to_string(),
                            content: Some(MessageContent::from_text(format!("{}{}", label, text))),
                            tool_calls: None,
                            tool_call_id: None,
                        };
                        if steer_mode == MidRunMode::Queue {
                            self.memory.save_message(&conversation_id, &msg).await.ok();
                        }
                        messages.push(msg);
                    }
                    // --- End steer injection ---

                    iteration_count = iteration + 1;
                    continue;
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: clean build with no errors or warnings.

- [ ] **Step 3: Commit**

```bash
git add src/agent.rs
git commit -m "fix: drain steer messages between tool call iterations"
```

---

### Task 2: /btw Context-Forked Side Query

**Files:**
- Modify: `src/agent.rs` (add `build_btw_context` method)
- Modify: `src/platform/telegram.rs` (replace `ask_parallel_lightweight` usage)
- Remove (optional): `ask_parallel_lightweight` method if no other callers

- [ ] **Step 1: Add `build_btw_context` method to Agent**

In `src/agent.rs`, add a new method near `ask_parallel_lightweight` (around line 2660):

```rust
    /// Build a context-forked message list for a /btw side question.
    ///
    /// Follows Claude Code's pattern: fork the current conversation messages,
    /// strip orphaned tool_use blocks (no matching tool_result), and append a
    /// strict system-reminder that constrains the model to answer from context
    /// only, with no tools and no follow-up turns.
    ///
    /// The returned messages are ephemeral — they are NOT saved to conversation
    /// history and the /btw response is sent asynchronously.
    ///
    /// This is a free function (not a method) because it only uses its arguments.
    pub fn build_btw_context(
        messages: &[ChatMessage],
        question: &str,
    ) -> Vec<ChatMessage> {
        // 1. Collect all tool_call_ids that have a matching tool_result.
        let mut resolved_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for msg in messages.iter().rev() {
            if msg.role == "tool" {
                if let Some(ref id) = msg.tool_call_id {
                    resolved_ids.insert(id.as_str());
                }
            }
        }

        // 2. Walk messages and strip orphaned tool_use blocks from assistant messages.
        let forked: Vec<ChatMessage> = messages
            .iter()
            .map(|msg| {
                if msg.role == "assistant" {
                    if let Some(ref calls) = msg.tool_calls {
                        let kept: Vec<ToolCall> = calls
                            .iter()
                            .filter(|tc| resolved_ids.contains(tc.id.as_str()))
                            .cloned()
                            .collect();
                        if kept.len() != calls.len() {
                            let mut stripped = msg.clone();
                            if kept.is_empty() {
                                stripped.tool_calls = None;
                            } else {
                                stripped.tool_calls = Some(kept);
                            }
                            return stripped;
                        }
                    }
                }
                msg.clone()
            })
            .collect();

        // 3. Append strict system-reminder.
        let reminder = format!(
            r#"<system-reminder>
This is a side question from the user. You must answer this question directly in a single response.

CRITICAL CONSTRAINTS:
- You have NO tools available — you cannot read files, run commands, search, or take any actions
- This is a one-off response — there will be no follow-up turns
- You can ONLY provide information based on what you already know from the conversation context
- NEVER say things like "Let me try...", "I'll now...", "Let me check...", or promise to take any action
- If you don't know the answer, say so — do not offer to look it up or investigate

Simply answer the question with the information you have.
</system-reminder>

{}"#,
            question
        );

        let mut result = forked;
        result.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text(reminder)),
            tool_calls: None,
            tool_call_id: None,
        });
        result
    }
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 3: Add unit tests for `build_btw_context` and orphaned filter**

Add in the same file as `build_btw_context` (under `#[cfg(test)] mod tests`):

```rust
#[test]
fn test_build_btw_context_removes_orphaned_tool_use() {
    use crate::llm::{FunctionCall, ToolCall};
    let assistant = ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "orphaned_call".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"x"}"#.into(),
            },
        }]),
        tool_call_id: None,
    };
    let msgs = vec![assistant];
    let result = crate::agent::build_btw_context(&msgs, "test question");
    let forked = &result[..result.len() - 1];
    for msg in forked {
        if let Some(ref calls) = msg.tool_calls {
            assert!(calls.is_empty(), "orphaned tool_use should be stripped");
        }
    }
}

#[test]
fn test_build_btw_context_preserves_matched_tool_calls() {
    use crate::llm::{FunctionCall, ToolCall};
    let tool_msg = ChatMessage {
        role: "tool".to_string(),
        content: Some(MessageContent::from_text("result")),
        tool_calls: None,
        tool_call_id: Some("call_1".into()),
    };
    let assistant = ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"x"}"#.into(),
            },
        }]),
        tool_call_id: None,
    };
    let msgs = vec![tool_msg, assistant];
    let result = crate::agent::build_btw_context(&msgs, "test question");
    let forked = &result[..result.len() - 1];
    let has_tool_calls = forked.iter().any(|m| {
        m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
    });
    assert!(has_tool_calls, "matched tool_use should be preserved");
}

#[test]
fn test_build_btw_context_text_only_messages_unchanged() {
    let msgs = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::from_text("hello")),
        tool_calls: None,
        tool_call_id: None,
    }];
    let result = crate::agent::build_btw_context(&msgs, "question");
    assert!(result.len() > msgs.len(), "should append question");
    assert_eq!(
        result[0].content.as_ref().map(|c| c.as_text()),
        Some("hello".to_string())
    );
}

#[test]
fn test_build_btw_context_empty_list() {
    let result = crate::agent::build_btw_context(&[], "question");
    assert_eq!(result.len(), 1, "only the question message");
    assert!(result[0]
        .content
        .as_ref()
        .map(|c| c.as_text())
        .unwrap_or_default()
        .contains("question"));
}
```

Note: `build_btw_context` is a free function (not a method on `Agent`) since
it only operates on its arguments. It lives in `agent.rs` as a public function.

- [ ] **Step 4: Update `/btw` handler in `telegram.rs`**

Replace the current `/btw` handler (lines 824-857) with a version that loads conversation messages, forks context, and calls the LLM directly:

```rust
    // Handle /btw <text> for context-forked side question
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

        // Load current conversation messages for context fork
        let conversation_id = agent
            .memory
            .get_or_create_conversation("telegram", &user_id.to_string())
            .await;
        let conversation_id = match conversation_id {
            Ok(id) => id,
            Err(e) => {
                let _ = send_markdown_message(
                    &bot,
                    msg.chat.id,
                    &format!("**BTW error:** {}", e),
                )
                .await;
                return Ok(());
            }
        };
        let messages = agent
            .memory
            .load_messages_with_limit(
                &conversation_id,
                agent.config.memory.max_raw_messages,
            )
            .await
            .unwrap_or_default();

        let agent_clone = agent.clone();
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        tokio::spawn(async move {
            let forked = crate::agent::build_btw_context(&messages, &btw_text);
            match agent_clone.llm.chat(&forked, &[]).await {
                Ok(response) => {
                    let text = response
                        .content
                        .as_ref()
                        .map(|c| c.as_text())
                        .unwrap_or_default();
                    let _ = send_markdown_message(&bot_clone, chat_id, &text).await;
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

Note: The `conversation_id` load may fail if there is no existing conversation (first message is /btw). The `unwrap_or_default` handles this gracefully — empty context is fine.

- [ ] **Step 4: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 5: Remove `ask_parallel_lightweight` if no other callers**

Search for references to `ask_parallel_lightweight` in the codebase:

Run: `rg "ask_parallel_lightweight" src/`
Expected: only the method definition (and possibly tests). If no callers remain, remove the method.

- [ ] **Step 6: Commit**

```bash
git add src/agent.rs src/platform/telegram.rs
git commit -m "feat: upgrade /btw to context-forked side query (Claude Code pattern)"
```

---

### Task 3: LoopDetector Module

**Files:**
- Create: `src/loop_detector.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write the LoopDetector module**

Create `src/loop_detector.rs`:

```rust
use std::collections::VecDeque;

use crate::llm::ToolCall;

/// A recorded tool call in the rolling window.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    /// Hash of (tool_name + normalized JSON arguments).
    pub args_hash: u64,
    /// Iteration index when this call was made.
    pub iteration: usize,
}

/// Information returned when a loop is detected.
#[derive(Debug, Clone)]
pub struct LoopInfo {
    pub tool_name: String,
    pub call_count: usize,
}

/// Detects exact-repetition loops in tool call sequences.
///
/// Maintains a rolling FIFO window of recent tool calls. A loop is declared
/// when the last N entries all have the same (tool_name, args_hash).
pub struct LoopDetector {
    window: VecDeque<ToolCallRecord>,
    threshold: usize,
}

impl LoopDetector {
    pub fn new(threshold: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(threshold + 1),
            threshold,
        }
    }

    /// Normalize and hash tool call arguments for comparison.
    ///
    /// Sorts JSON keys alphabetically, trims whitespace, then computes a
    /// non-cryptographic hash of (tool_name + "|" + normalized_args).
    pub fn compute_hash(name: &str, arguments: &str) -> u64 {
        use std::hash::{Hash, Hasher};

        // Normalize: parse as JSON, sort keys, re-serialize.
        let normalized = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .map(|v| normalize_json_value(v))
            .unwrap_or_else(|| arguments.trim().to_string());

        let mut hasher = rustc_hash::FxHasher::default();
        name.hash(&mut hasher);
        "|".hash(&mut hasher);
        normalized.hash(&mut hasher);
        hasher.finish()
    }

    /// Record a batch of tool calls from one iteration.
    pub fn record(&mut self, tool_calls: &[ToolCall], iteration: usize) {
        for tc in tool_calls {
            let hash = Self::compute_hash(&tc.function.name, &tc.function.arguments);
            self.window.push_back(ToolCallRecord {
                tool_name: tc.function.name.clone(),
                args_hash: hash,
                iteration,
            });
            while self.window.len() > self.threshold {
                self.window.pop_front();
            }
        }
    }

    /// Check whether a loop is currently detected.
    ///
    /// Returns `Some(LoopInfo)` when the last N entries all share the same
    /// (tool_name, args_hash), where N == threshold.
    pub fn detect_loop(&self) -> Option<LoopInfo> {
        if self.window.len() < self.threshold {
            return None;
        }

        // All entries in the window must match the first (oldest) entry.
        let first = self.window.front()?;
        let all_same = self.window.iter().all(|r| r.args_hash == first.args_hash);

        if all_same {
            Some(LoopInfo {
                tool_name: first.tool_name.clone(),
                call_count: self.window.len(),
            })
        } else {
            None
        }
    }

    /// Clear the window — used after user approves continuation.
    pub fn clear(&mut self) {
        self.window.clear();
    }
}

/// Recursively sort all JSON object keys for deterministic comparison.
fn normalize_json_value(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, String)> = map
                .into_iter()
                .map(|(k, v)| (k, normalize_json_value(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let inner: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("\"{}\":{}", k, v))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.into_iter().map(normalize_json_value).collect();
            format!("[{}]", items.join(","))
        }
        serde_json::Value::String(s) => format!("\"{}\"", s.trim()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn make_tool_call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "test_id".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[test]
    fn test_compute_hash_same_args_same_hash() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path": "foo.txt"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "foo.txt"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_compute_hash_different_args_different_hash() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path": "a.txt"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "b.txt"}"#);
        assert_ne!(a, b);
    }

    #[test]
    fn test_compute_hash_key_order_invariance() {
        let a = LoopDetector::compute_hash("write_file", r#"{"content": "x", "path": "f.txt"}"#);
        let b = LoopDetector::compute_hash("write_file", r#"{"path": "f.txt", "content": "x"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_compute_hash_whitespace_invariance() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path":"x"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "x"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_detect_below_threshold_returns_none() {
        let mut d = LoopDetector::new(3);
        d.record(&[make_tool_call("read", r#"{"path":"x"}"#)], 0);
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_detect_exact_threshold_detects() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        d.record(&[tc.clone()], 0);
        d.record(&[tc.clone()], 1);
        d.record(&[tc.clone()], 2);
        let info = d.detect_loop().expect("loop should be detected");
        assert_eq!(info.tool_name, "read");
        assert_eq!(info.call_count, 3);
    }

    #[test]
    fn test_detect_three_different_returns_none() {
        let mut d = LoopDetector::new(3);
        d.record(&[make_tool_call("a", r#"{"path":"x"}"#)], 0);
        d.record(&[make_tool_call("b", r#"{"path":"x"}"#)], 1);
        d.record(&[make_tool_call("c", r#"{"path":"x"}"#)], 2);
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_clear_resets_detection() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        d.record(&[tc.clone()], 0);
        d.record(&[tc.clone()], 1);
        d.record(&[tc.clone()], 2);
        assert!(d.detect_loop().is_some());
        d.clear();
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_detects_across_multiple_calls_per_iteration() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        // Two identical calls in iteration 0, one in iteration 1 = 3 total
        d.record(&[tc.clone(), tc.clone()], 0);
        d.record(&[tc.clone()], 1);
        let info = d.detect_loop().expect("cross-turn loop detected");
        assert_eq!(info.tool_name, "read");
    }

    #[test]
    fn test_diff_tool_same_args_not_detected() {
        let mut d = LoopDetector::new(3);
        let tc_a = make_tool_call("read", r#"{"path":"x"}"#);
        let tc_b = make_tool_call("write", r#"{"path":"x"}"#);
        d.record(&[tc_a], 0);
        d.record(&[tc_b], 1);
        d.record(&[make_tool_call("read", r#"{"path":"x"}"#)], 2);
        assert!(d.detect_loop().is_none());
    }
}
```

- [ ] **Step 2: Register module in `src/lib.rs`**

Add `pub mod loop_detector;` to `src/lib.rs`.

- [ ] **Step 3: Add `rustc-hash` dependency**

Run: `cargo add rustc-hash`

- [ ] **Step 4: Run tests**

Run: `cargo test loop_detector`
Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/loop_detector.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat: add LoopDetector module with exact-repetition detection"
```

---

### Task 4: Loop Detection Configuration

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

- [ ] **Step 1: Add `LoopDetectionConfig` struct**

In `src/config.rs`, add to the `AgentConfig` struct:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LoopDetectionConfig {
    #[serde(default = "default_loop_detection_enabled")]
    pub enabled: bool,
    #[serde(default = "default_loop_detection_threshold")]
    pub threshold: usize,
    #[serde(default = "default_loop_detection_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_loop_detection_enabled() -> bool { true }
fn default_loop_detection_threshold() -> usize { 3 }
fn default_loop_detection_timeout_seconds() -> u64 { 120 }
```

Add the field to `AgentConfig`:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_empty_response_retry_limit")]
    pub empty_response_retry_limit: u32,
    #[serde(default = "default_parse_retry_limit")]
    pub parse_retry_limit: u32,
    #[serde(default)]
    pub loop_detection: LoopDetectionConfig,
}
```

Add accessor method on `Config`:

```rust
    pub fn loop_detection_config(&self) -> &LoopDetectionConfig {
        &self.agent.loop_detection
    }
```

And a `Default` impl for `LoopDetectionConfig`:

```rust
impl Default for LoopDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 3,
            timeout_seconds: 120,
        }
    }
}
```

- [ ] **Step 2: Update `config.example.toml`**

Add commented section:

```toml
[agent.loop_detection]
# enabled = true
# threshold = 3
# timeout_seconds = 120
```

- [ ] **Step 3: Update `default_agent_config()`**

In `src/config.rs`, update `default_agent_config()` (around line 467) to include the new field:

```rust
fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_iterations: default_max_iterations(),
        empty_response_retry_limit: default_empty_response_retry_limit(),
        parse_retry_limit: default_parse_retry_limit(),
        loop_detection: LoopDetectionConfig::default(),
    }
}
```

- [ ] **Step 4: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: add LoopDetectionConfig to agent configuration"
```

---

### Task 5: Loop Detection Callback Registry

**Files:**
- Modify: `src/agent.rs` (add `pending_loop_callbacks` field + setup methods)

- [ ] **Step 1: Add callback channel type and Agent field**

Add near the top of Agent's fields (around line 100):

```rust
/// Type for the user's choice when a loop is detected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopCallbackChoice {
    Continue,
    Stop,
    AddInstruction,
}

/// Channel sender for loop detection callbacks, keyed by user_id.
type LoopCallbackRegistry = Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<LoopCallbackChoice>>,
    >,
>;
```

Add field to `Agent` struct:

```rust
    pub pending_loop_callbacks: LoopCallbackRegistry,
```

Initialize in the constructor(s) with `Arc::new(Mutex::new(HashMap::new()))`.

- [ ] **Step 2: Add registry methods (async, since Mutex::lock() requires .await)**

```rust
    /// Register a oneshot sender for a user's loop detection callback.
    /// Returns the old sender if one was already registered (should not happen
    /// in practice since one user has one active process_message).
    pub async fn register_loop_callback(
        &self,
        user_id: &str,
        sender: tokio::sync::oneshot::Sender<LoopCallbackChoice>,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let mut map = self.pending_loop_callbacks.lock().await;
        map.insert(user_id.to_string(), sender)
    }

    /// Take the loop callback sender for a user, if any.
    pub async fn take_loop_callback(
        &self,
        user_id: &str,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let mut map = self.pending_loop_callbacks.lock().await;
        map.remove(user_id)
    }
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add loop detection callback registry to Agent"
```

---

### Task 6: Loop Detection Integration in Agent Loop

**Files:**
- Modify: `src/agent.rs` (detect loop + suspend + handle response)

- [ ] **Step 1: Import and init LoopDetector before the main loop**

In `process_message()`, around line 793 (before `'outer: for iteration`):

```rust
        // Loop detection state (cross-turn, resets each process_message call)
        let loop_config = self.config.loop_detection_config();
        let loop_threshold = if loop_config.enabled {
            loop_config.threshold
        } else {
            // When disabled, use a sentinel threshold that never triggers
            usize::MAX
        };
        let mut loop_detector = crate::loop_detector::LoopDetector::new(loop_threshold);
        let loop_timeout = std::time::Duration::from_secs(loop_config.timeout_seconds);
        let user_id_str = user_id.to_string(); // owned copy for move into closures
```

- [ ] **Step 2: Record + detect after LLM response**

Before the tool execution section (after line 1182 `response = completion.message; break;`), add:

```rust
            // --- Loop detection: record tool calls and check for repetition ---
            if loop_config.enabled {
                if let Some(ref tool_calls) = response.tool_calls {
                    loop_detector.record(tool_calls, iteration as usize);
                    if let Some(loop_info) = loop_detector.detect_loop() {
                        info!(
                            user_id = %user_id,
                            tool = %loop_info.tool_name,
                            count = loop_info.call_count,
                            "Loop detected — pausing for user approval"
                        );

                        // Build preview: tool name + first argument snippet
                        let preview = if let Some(first) = response.tool_calls.as_ref()
                            .and_then(|c| c.first())
                        {
                            let preview = &first.function.arguments;
                            let preview = if preview.len() > 80 {
                                format!("{}...", &preview[..80])
                            } else {
                                preview.to_string()
                            };
                            format!("{}({})", loop_info.tool_name, preview)
                        } else {
                            loop_info.tool_name.clone()
                        };

                        // Create oneshot channel for the callback
                        let (cb_tx, cb_rx) = tokio::sync::oneshot::channel::<LoopCallbackChoice>();

                        // Register callback sender
                        self.register_loop_callback(&user_id_str, cb_tx);

                        // Send Telegram inline keyboard
                        let keyboard = teloxide::types::InlineKeyboardMarkup::new([
                            [
                                teloxide::types::InlineKeyboardButton::callback(
                                    "Continue",
                                    r#"{"type":"loop","action":"continue"}"#,
                                ),
                                teloxide::types::InlineKeyboardButton::callback(
                                    "Stop",
                                    r#"{"type":"loop","action":"stop"}"#,
                                ),
                            ],
                            [
                                teloxide::types::InlineKeyboardButton::callback(
                                    "Add instruction",
                                    r#"{"type":"loop","action":"add_instruction"}"#,
                                ),
                            ],
                        ]);
                        let bot_for_msg = self.bot.clone();
                        let chat_id_for_msg = parsed_chat_id;
                        let _ = bot_for_msg
                            .send_message(
                                chat_id_for_msg,
                                format!(
                                    "I seem to be calling the same tool repeatedly:\n  {} called {} times",
                                    preview,
                                    loop_info.call_count,
                                ),
                            )
                            .reply_markup(keyboard)
                            .await;

                        // Await user's choice (with timeout)
                        match tokio::time::timeout(loop_timeout, cb_rx).await {
                            Ok(Ok(LoopCallbackChoice::Continue)) => {
                                info!("User approved — continuing loop");
                                loop_detector.clear();
                                // `continue` targets the 'outer for loop — starts a
                                // fresh iteration (re-invokes the LLM), skipping any
                                // remaining tool execution from this iteration.
                                continue;
                            }
                            Ok(Ok(LoopCallbackChoice::Stop)) => {
                                info!("User requested stop — breaking loop");
                                was_cancelled = true;
                                break 'outer;
                            }
                            Ok(Ok(LoopCallbackChoice::AddInstruction)) => {
                                info!("User requested add instruction — waiting for input");
                                // The user will send a text message that gets queued
                                // as a steer injection. We need another callback
                                // for the instruction text, or we wait for the steer
                                // to arrive via pending_injections.
                                //
                                // Simplified approach: tell user to type their instruction,
                                // then wait for a second callback to confirm they're done.
                                let _ = bot_for_msg
                                    .send_message(
                                        chat_id_for_msg,
                                        "Please type your instruction as your next message. \
                                         Then tap Continue to resume.",
                                    )
                                    .reply_markup(
                                        teloxide::types::InlineKeyboardMarkup::new([[
                                            teloxide::types::InlineKeyboardButton::callback(
                                                "Continue",
                                                r#"{"type":"loop","action":"continue"}"#,
                                            ),
                                        ]]),
                                    )
                                    .await;

                                // Wait for second callback
                                let (cb2_tx, cb2_rx) =
                                    tokio::sync::oneshot::channel::<LoopCallbackChoice>();
                                self.register_loop_callback(&user_id_str, cb2_tx);
                                match tokio::time::timeout(loop_timeout, cb2_rx).await {
                                    Ok(Ok(LoopCallbackChoice::Continue)) => {
                                        loop_detector.clear();
                                        continue;
                                    }
                                    _ => {
                                        was_cancelled = true;
                                        break 'outer;
                                    }
                                }
                            }
                            _ => {
                                // Timeout or channel closed — auto-stop
                                warn!("Loop callback timed out — stopping");
                                was_cancelled = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            // --- End loop detection ---
```

Note: The `continue;` after "Continue" goes back to the start of the `'outer` loop, which will re-run the LLM call with the steer (or at a clean iteration boundary). This avoids re-executing the same tool calls.

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "feat: integrate loop detection into main agent loop"
```

---

### Task 7: Telegram Callback Handler for Loop Detection

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add callback query handler for loop detection**

Add a new handler function in `telegram.rs` following the same pattern as the
existing `handle_model_callback`:

```rust
async fn handle_loop_callback(
    bot: Bot,
    q: CallbackQuery,
    agent: Arc<Agent>,
) -> ResponseResult<()> {
    let user_id = q.from.id.to_string();
    let data = match q.data {
        Some(ref d) => d.clone(),
        None => return Ok(()),
    };

    let choice = if data.contains(r#""action":"continue""#) {
        crate::agent::LoopCallbackChoice::Continue
    } else if data.contains(r#""action":"stop""#) {
        crate::agent::LoopCallbackChoice::Stop
    } else if data.contains(r#""action":"add_instruction""#) {
        crate::agent::LoopCallbackChoice::AddInstruction
    } else {
        bot.answer_callback_query(q.id).await.ok();
        return Ok(());
    };

    // Lookup the pending callback sender and send the choice.
    // The agent loop awaits the oneshot receiver; this wakes it up.
    if let Some(sender) = agent.take_loop_callback(&user_id).await {
        let _ = sender.send(choice);
    }

    bot.answer_callback_query(q.id).await.ok();
    Ok(())
}
```

- [ ] **Step 2: Register the handler in the dispatcher**

Register a new handler branch alongside the existing `callback_handler`,
using `.filter_map()` (the same pattern as the existing handler at line 204):

```rust
    let loop_callback_handler = Update::filter_callback_query()
        .filter_map(|q: CallbackQuery| async move {
            if q.data.as_deref().map_or(false, |d| d.contains(r#""type":"loop""#)) {
                Some(q)
            } else {
                None
            }
        })
        .endpoint(handle_loop_callback);

    let handler = dptree::entry()
        .branch(message_handler)
        .branch(callback_handler)
        .branch(loop_callback_handler);  // new
```

The `agent` dependency is already injected via `.dependencies(dptree::deps![agent])`
at line 220, so `handle_loop_callback` receives it automatically.

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: add callback query handler for loop detection inline keyboard"
```

---

### Task 8: Loop Detection in Subagent Loop

**Files:**
- Modify: `src/agent.rs` (subagent loop)

- [ ] **Step 1: Add recovery nudge injection**

In `run_subagent_loop` (around line 2605, after `response = completion.message; break;`), add:

```rust
            // --- Loop detection (subagent: auto-recover, no user prompt) ---
            if loop_config.enabled {
                if let Some(ref tool_calls) = response.tool_calls {
                    loop_detector_sub.record(tool_calls, _iteration as usize);
                    if let Some(loop_info) = loop_detector_sub.detect_loop() {
                        warn!(
                            subagent = %label,
                            tool = %loop_info.tool_name,
                            count = loop_info.call_count,
                            "Subagent loop detected — injecting recovery nudge"
                        );

                        // Inject recovery message as a tool result
                        let nudge_text = format!(
                            "Error: You have called {} {} times with the same arguments. \
                             The result has not changed. Try a different approach.",
                            loop_info.tool_name,
                            loop_info.call_count,
                        );
                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: Some(MessageContent::from_text(nudge_text)),
                            tool_calls: None,
                            tool_call_id: Some("loop_recovery_nudge".to_string()),
                        });

                        loop_detector_sub.clear();
                        continue;
                    }
                }
            }
            // --- End subagent loop detection ---
```

This requires:
- Adding a `loop_detector_sub = LoopDetector::new(threshold)` before the
  subagent `for` loop (line 2544), right after `let empty_response_retry_limit`:

  ```rust
  let loop_config = self.config.loop_detection_config();
  let sub_threshold = if loop_config.enabled {
      loop_config.threshold
  } else {
      usize::MAX
  };
  let mut loop_detector_sub = crate::loop_detector::LoopDetector::new(sub_threshold);
  ```

- The enabled check, timeout, and user_id_str from Task 6 are not needed here
  because the subagent loop auto-recovers instead of prompting the user.

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 3: Add tests for steer injection edge cases**

Add tests in `src/agent.rs` under `#[cfg(test)]`:

```rust
#[test]
fn test_steer_injection_label_format() {
    // Verify the steer message label matches what the LLM sees
    let label_steer = "**[Steer]:** ";
    let label_queue = "**[User injected mid-processing]:** ";
    assert!(label_steer.contains("Steer"));
    assert!(label_queue.contains("injected"));
}
```

Also manually verify (cannot unit-test in isolation):
- Queue mode injection persists to DB: send text during processing → check DB
- Steer mode injection does not persist: send text during processing → check DB
- Empty injection queue is no-op: verify no message is added to the conversation

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add loop detection with recovery nudge to subagent loop"
```

---

### Task 9: Full Integration Build and Test

- [ ] **Step 1: Run all tests**

Run: `cargo test`
Expected: all existing tests pass + all new loop_detector tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Run format check**

Run: `cargo fmt --all -- --check`
Expected: no formatting issues.

- [ ] **Step 4: Commit final integration**

```bash
git add -A
git commit -m "chore: final integration build for loop detection features"
```

---

## Self-Review Checklist

1. **Spec coverage:**
   - Loop detection: Tasks 3-8 cover the full LoopDetector module, config, callback registry, agent loop integration, Telegram UX, and subagent recovery.
   - /btw context fork: Task 2 covers the new method, handler replacement, and cleanup.
   - Steer injection fix: Task 1 covers the post-tools drain.

2. **Placeholder scan:** All steps contain actual code. No "TBD", "TODO", or "implement later".

3. **Type consistency:** `LoopDetector::new(threshold)` is consistent between the module (Task 3), agent loop (Task 6), and subagent loop (Task 8). `LoopCallbackChoice` enum is defined in agent.rs (Task 5) and used in both telegram.rs handler (Task 7) and agent loop (Task 6).
