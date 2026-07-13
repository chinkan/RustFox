# Loop Detection Mechanism for RustFox

## Problem

RustFox's agentic loop has no detection for repetitive tool-call patterns. The
only safeguard is `max_iterations` (default 25), which is a blunt instrument —
it kills long legitimate tasks just as readily as stuck ones, and a 3-iteration
tight loop burns 12% of the budget before any guard fires.

Research across opencode ("doom loop"), Claude Code (leaked `query.ts` source),
and community projects (neokai, DedrooM, LangSight) shows that the #1 production
failure mode for LLM agents is calling the same tool with the same arguments
repeatedly. Opencode solves this with exact repetition detection + a permission
prompt. Claude Code relies on `max_turns` + targeted circuit breakers for
specific subsystems (compact, output tokens) but has no general tool-call
loop detector.

## Design

### Detection: exact repetition across turns

The detector compares `(tool_name, normalized_args_hash)` across all tool calls
made since the last user message. If the last N calls (default N=3) are all
identical, a loop is declared.

**Scope:** Cross-message (since last user turn), not scoped to a single
assistant message. This is the fix for opencode bug #25254 — per-message
scoping misses loops that span multiple turns.

**Normalization:** Sort JSON keys alphabetically, strip whitespace, then hash
with a fast non-cryptographic hash (e.g. `std::hash::Hasher` or `fxhash`).

### Action on detection: two paths

| Loop location | Action |
|---|---|
| **Main agent loop** (`process_message`) | Send Telegram inline keyboard: [Continue] [Stop] [Add instruction]. Suspend loop, wait for user callback. |
| **Subagent loop** (`run_subagent_loop`) | Auto-inject recovery nudge into message list, continue immediately. No user interaction. |

### Telegram UX

When a loop is detected, the bot sends a message with inline keyboard buttons:

```
I seem to be calling the same tool repeatedly:
  read_file("src/main.rs") called 3 times
┌──────────┐ ┌──────────┐ ┌──────────────┐
│ Continue │ │   Stop   │ │ Add instruction │
└──────────┘ └──────────┘ └──────────────┘
```

- **Continue:** Detector clears its window, loop resumes normally.
- **Stop:** Cancels the current processing, returns partial result.
- **Add instruction:** Prompts user to type guidance text, which is injected
  as a user message into the conversation, then loop continues.
- **Timeout (120s default):** Auto-stops, returns partial result.

### Subagent recovery nudge

The injected message is a tool result containing:

```
Error: You have called read_file 3 times with the same arguments.
The result has not changed. Try a different approach.
```

This is identical to the neokai pattern — the LLM receives it as a tool result
and adapts.

### Configuration

New section in `config.toml`:

```toml
[agent.loop_detection]
enabled = true
threshold = 3
timeout_seconds = 120
```

### Module: `src/loop_detector.rs`

```
ToolCallRecord { tool_name, args_hash, iteration }
LoopDetector { window: VecDeque<ToolCallRecord>, threshold, config }

fn record(&mut self, tool_calls: &[ToolCall], iteration)
  → hash each call, push to window, evict oldest if over threshold

fn detect_loop(&self) -> Option<LoopInfo>
  → check if last N records all have same args_hash
  → returns tool name + call count if loop detected

fn clear(&mut self)
  → empty the window

fn compute_hash(name: &str, args: &str) -> u64
  → sort JSON keys, trim whitespace, hash
```

### Callback query wiring

When loop is detected in the main loop, `process_message` must suspend and
wait for the user's Telegram inline keyboard response. This requires bridging
the teloxide callback system into the agent loop:

1. **`CallbackData` format** — each button carries a JSON payload:
   ```json
   {"type":"loop","action":"continue|stop|add_instruction"}
   ```

2. **Callback registry** — a shared map on Agent:
   ```rust
   // Arc<Mutex<HashMap<String, oneshot::Sender<LoopCallbackChoice>>>>
   pending_loop_callbacks: ...
   ```

3. **Flow**:
   ```
   Loop detected → create oneshot::channel
     → store sender in pending_loop_callbacks[key=user_id]
     → send Telegram inline keyboard
     → await receiver (with timeout)
     → on choice: clear detector, resume/stop
     → on timeout: auto-stop with partial result
   ```

4. **Teloxide callback handler** — registered in the dispatcher at
   `telegram.rs` alongside the existing `callback_handler`:
   ```
   CallbackQuery received
     → parse callback data
     → if type="loop", lookup sender in pending_loop_callbacks[user_id]
     → send choice through oneshot sender
     → answer callback query (remove keyboard loading state)
   ```

5. **Edge case — user sends new message while suspended**: The new message
   hits the injection path at telegram.rs:1060 (since user is processing).
   It gets queued as a steer message. When the callback timeout fires and
   processing resumes, the steer messages are processed normally at line 869.

### Integration points in `src/agent.rs`

| Point | Location | Change |
|---|---|---|
| Before main `for` loop | ~line 793 | Initialize `LoopDetector` on stack |
| After LLM response, before tool exec | ~line 1183 | `detector.record(tool_calls, iteration)` then `detector.detect_loop()` |
| On loop detected (main) | ~line 1185 | Create oneshot channel, register sender, send Telegram keyboard, await with timeout |
| On loop detected (subagent) | ~line 2605 | Inject recovery nudge, `continue` |
| New user message (fresh call) | ~line 571 | Fresh `LoopDetector` on each `process_message` |

### Ownership

The `LoopDetector` lives on the stack within `process_message`. The user's
choice is communicated back via a `oneshot::Sender` stored in a shared
registry on `Agent` (behind `Arc<Mutex<...>>`). On timeout (120s default),
the receiver drops and `process_message` auto-stops.

The subagent loop does not need the callback mechanism — it auto-injects a
recovery nudge and continues.

## Fix 1: `/btw` → Context-Forked Side Query (Claude Code pattern)

### Problem

The current `/btw` implementation at `telegram.rs:824` calls
`ask_parallel_lightweight()` which builds a blank system prompt + user message
with **zero conversation context**. The LLM cannot answer questions like "what
was that config file name?" because it doesn't see the ongoing conversation.

### Design: Context fork with strict constraints

Following Claude Code's leaked `/btw` implementation:

1. **Fork the conversation context** — pass the current `messages` vector
   through a filter that strips orphaned `tool_use` blocks (tool calls without
   corresponding `tool_result`), producing a clean context snapshot.
   Algorithm: collect all `tool_call_id` values from `role: "tool"` messages
   into a set. Walk the messages list; for each `role: "assistant"` message,
   keep only those tool calls whose `id` exists in the set. Messages with
   no remaining tool calls are kept as-is (text-only responses are fine).
2. **Strict system reminder** — inject a `<system-reminder>` message before the
   user question:

   > You must answer this question directly in a single response.
   > CRITICAL CONSTRAINTS:
   > - You have NO tools available
   > - This is a one-off response — there will be no follow-up turns
   > - Answer based on the conversation context provided above
   > - NEVER say "let me try", "I'll now", "let me check"
   > - If you don't know, say so — do not offer to investigate

3. **Single LLM call** — same as current, no agentic loop, no tools passed.
4. **Ephemeral** — response is NOT saved to DB or conversation history.
5. **Parallel** — runs in `tokio::spawn`, does not interrupt main loop.

### Changes required

| File | Change |
|---|---|
| `telegram.rs` (handle_message, ~line 840) | Instead of `agent.ask_parallel_lightweight()`, build forked context + system-reminder, call `agent.llm.chat()` directly with the full context. |
| `agent.rs` | Add method `build_btw_context(messages: &[ChatMessage]) → Vec<ChatMessage>` that filters and constructs the btw prompt. |

### Cleanup

The old `ask_parallel_lightweight` method in `agent.rs` is replaced by this
new implementation. If it has no remaining callers after this change, remove
the method to avoid dead code.

### The /btw flow (new)

```
User: /btw what config file did we edit?
  → telegram.rs: sends "⏳ BTW..." immediate reply
  → reads current messages: `agent.memory.load_messages_with_limit(conversation_id, limit)`
  → filters: strips orphaned tool_use blocks
  → builds: [filtered_context..., system_reminder, user_question]
  → tokio::spawn { agent.llm.chat(forked_messages, &[]) }
  → sends answer asynchronously
  → answer NOT saved to conversation history
```

## Fix 2: Steer Injection Between Tool Calls

### Problem

The injection drain at `agent.rs:869-892` runs **once per iteration**, before
the LLM call. If a user sends a steer message during a long tool execution in
the `other_group` sequential loop (lines 1305-1375), the steer sits in the
`pending_injections` queue until ALL tools finish AND the next iteration's
LLM call completes — potentially minutes of delay.

### Design

Inject steer messages **after the sorted tool-result batch is committed to
`messages`**, before the `continue` that starts the next iteration. This saves
one full LLM call round-trip compared to the current behavior (which only
drains at the next iteration's heading, after the LLM call).

In the multi-tool case: if multiple tools run sequentially in the batch, the
drain fires once after ALL of them complete. An injection check between each
individual tool is not specified here — it would require restructuring the
batch flow (breaking the sort-and-commit batch into per-tool steps). This
is a future optimization if multi-tool responses are frequent.

### Changes required

In `agent.rs` `process_message()`, after line 1383 (the sorted batch push to
`messages`), before the `continue` at line 1387:

```rust
// Drain and inject pending steer messages before next iteration.
// Without this, steer is not visible until the next LLM call completes
// (the check at line 869 fires after the LLM call starts the next iteration).
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
```

Variable `conversation_id` is in scope (declared at line 586, lives until
function return). `user_id` is also in scope (line 578). The existing
`drain_injections()` method already handles the queue.

### Why not per-tool or during LLM call

For `tokio::select!` based interruption of in-flight LLM calls: adds
architectural complexity for minimal gain (LLM calls are typically 5-15s).
For per-tool draining within the batch: would require restructuring the
sort-and-commit flow. Both are future optimizations.

## Testing strategy

### Unit-testable components

| Component | Test cases |
|---|---|
| `LoopDetector::compute_hash` | Same args produces same hash; different args produces different hash; JSON key order invariance; whitespace invariance |
| `LoopDetector::detect_loop` | Below threshold returns None; exactly at threshold (3 identical) returns Some; 3 different returns None; 2 identical + 1 different returns None |
| `LoopDetector::clear` | After clear, detect_loop returns None regardless of prior calls |
| Orphaned `tool_use` filter | Removes tool calls without matching tool_result; preserves calls with matching result; handles text-only messages; handles empty messages list |
| Steer injection edge case | Injection during Queue mode persists to DB; injection during Steer mode does not persist; injection with empty queue is no-op |

### Integration testing

The loop detection Telegram callback flow is harder to test in isolation
(teloxide dispatcher, real Telegram API). Cover this with manual testing:
1. Send 3 identical tool calls in sequence → verify inline keyboard appears
2. Tap "Continue" → verify loop resumes
3. Tap "Stop" → verify processing cancels
4. Tap "Add instruction" → verify next response considers the new guidance
5. Wait for timeout → verify auto-stop with partial result

### Regression testing

- Verify `/btw` still works (existing test: the immediate reply and async
  answer pattern)
- Verify steer injection still works at iteration boundary (existing behavior)
- Verify `/stop` still cancels processing immediately

## Future extensibility (not in this spec)

- Cycle detection (A→B→A→B pattern)
- Frequency-based detection (same tool N times in M seconds)
- Per-tool configurable thresholds (e.g., `read_file=5`, `execute_command=3`)
- Semantic similarity for near-identical arguments
- Audit log of detected loops
