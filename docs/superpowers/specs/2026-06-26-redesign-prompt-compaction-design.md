# Redesign Prompt Compaction — Design Spec

## Problem

Current compaction (`agent_prompt.rs::compact_tool_heavy_history`) uses **per-message truncation** — each tool call argument and tool result is individually shrunk with a `[RustFox compacted: ...]` marker, but the full message structure (177 messages → 177 messages) and tool-call trajectory are preserved verbatim. This causes the LLM to:

1. **Repeat the same tool-calling pattern** — it sees "I called X→got Y→called Z→got W" and replays the sequence
2. **Regurgitate compaction markers** — `[RustFox compacted: ...]` is output as literal tool call arguments (`is_compacted_regurgitation()` patch)
3. **Waste iterations** — the LLM keeps doing the same things across multiple rounds because the underlying trajectory structure never changes

## Research

### Claude Code's Approach (5-tier cascade, reverse-engineered from leaked source)

See: `https://github.com/claude-code-best/claude-code` & `https://y-agent.github.io/inside-claude-code/04-context-compaction.html`

| Tier | Name | What | Cost |
|------|------|------|------|
| T1 | Microcompact | Rearrange for cache hits | 0ms, no LLM |
| T2 | Snip | LRU archive oldest messages | Async, no LLM |
| T3 | Context Collapse | Staged section summarization | LLM calls |
| T4 | Auto Compact | Full LLM summarization → 9-section structured summary | Sub-agent |
| T5 | Reactive | Emergency 413 recovery (keep last 4 msgs) | 1 attempt |

Key insight from Anthropic's official docs: **"tool result clearing is one of the safest, lightest-touch forms of compaction"** — replacing old tool results with placeholders while keeping the tool_use record.

### Self-Compacting Agents (arXiv 2606.23525)

Research shows that compaction should be **adaptive to trajectory structure**, not fixed-interval. A rubric specifying when to fire (sub-task resolved, trajectory converging) and when to suppress (mid-derivation, stuck) significantly improves outcomes.

## Architecture: 4-Tier Pipeline (Split Across Modules)

Tiers 1-2 live in `agent_prompt.rs` (sync, 0 LLM cost). Tiers 3-4 live in `agent.rs` (async, require LLM call).

```
agent.rs loop:
  │
  ├─ 1. Call should_auto_compact(messages, turn_count)
  │     │
  │     ├─ False → skip to step 2
  │     └─ True  → async auto_compact_conversation(messages, llm)
  │                 └─ LLM generates 8-section summary
  │                 └─ Replaces old messages with summary + recent
  │                 └─ Updates last_compact_turn
  │
  ├─ 2. Call sync prepare_messages_for_llm(messages)          ← Tiers 1-2 only
  │     │
  │     ├─ If >20% → Tier 1: observation masking (mask tool results)
  │     ├─ If >60% → Tier 2: context collapse (drop oldest groups)
  │     └─ Return PreparedPrompt
  │
  └─ 3. Send to LLM API
        │
        └─ On 413 error → Tier 4: reactive_compact (keep last 4, summarize rest)
```

### Tier 1: Observation Masking

Replace old tool result content with `[previous tool result — masked]` marker. Preserves the message structure and tool_use record. The LLM knows it made the call, but doesn't carry the bulky payload.

- **Trigger:** estimated bytes > 20,000 (20% of hard cap; preserves backward compat with old `COMPACTION_PROMPT_BYTE_THRESHOLD`)
- **Scope:** tool results older than `PRESERVED_TOOL_GROUPS` (2)
- **Cost:** 0 LLM calls

Also neutralizes any old `[RustFox compacted:` markers found in tool call arguments by replacing them with a simpler `[compacted]` marker. This prevents the regurgitation problem from persisting through Tiers 1-2.

### Tier 2: Context Collapse

Remove entire oldest assistant-with-tool-calls groups and their tool results. These messages are **gone**, not compacted. A structural boundary marker `[system] ★ earlier conversation collapsed ★` is inserted at the collapse point so the LLM doesn't perceive a confusing jump.

- **Trigger:** still > 60,000 bytes (60%) after Tier 1
- **Scope:** oldest 50% of non-preserved tool groups
- **Cost:** 0 LLM calls

### Tier 3: Auto Compact (Core, Async in agent.rs)

Replace the older portion of conversation (everything except the most recent `PRESERVED_TOOL_GROUPS` tool groups) with a single structured summary message.

**Before:**
```
[system] identity + skills + agents
[user] "research superpowers"
[assistant] tool_call fetch (x4)
[tool] 11001 bytes, 10287 bytes, 10093 bytes, 9054 bytes
[assistant] tool_call write_skill (x10)
[tool] "Written: ..." (x10)
... more messages ...
[assistant] "✅ Done!"
```

**After:**
```
[system] identity + skills + agents
[user] "research superpowers"
─── COMPACT BOUNDARY ───
[system] ★ COMPACT SUMMARY ★
─── PRESERVED ───
[assistant] "✅ Done!"
[recovery nudge] → "Continue from the user's request above..."
```

**Trigger Conditions (ALL must be true):**
- Message count > 15
- Estimated prompt bytes > 85,000 (matches `COMPACT_THRESHOLD` in constants table)
- Last compact was ≥ 5 turns ago (`COMPACT_TURN_GAP`)
- `query_source != "compact"` (recursion guard)

**Summary Prompt (adapted from Claude Code's `BASE_COMPACT_PROMPT`):**

```
Your task is to create a detailed summary of the conversation so far.

Your summary must include these sections:

1. Primary Request and Intent: What was the user's original request?
2. Key Technical Concepts: Technologies, frameworks, approaches discussed
3. Files and Code Sections: Files read, created, or modified
4. Errors and Fixes: Errors encountered and how they were fixed
5. All User Messages: List ALL user messages verbatim (not tool results)
6. Pending Tasks: What tasks were explicitly requested but not completed
7. Current Work: Exactly what was being worked on before this summary
8. Next Step: The next logical action based on the most recent user request

IMPORTANT: Do NOT call any tools. Respond with text only.
```

**Post-Compact Reconstruction (after LLM returns summary in agent.rs):**
1. Create compact boundary marker message (system role, contains pre-compact stats)
2. Create summary message (system role, contains raw LLM summary text)
3. Append preserved recent messages
4. Append `recovery_nudge_for()` message
5. Update `last_compact_turn` in `ConversationMeta`
6. Return new message list

**Cost:** 1 LLM call (returns ~500-2000 tokens of summary, saves potentially 80K+ tokens)

**Both call sites get Tier 3:** The main agent loop (line 645) and `run_subagent_loop` (line ~1921) both call `should_auto_compact()` + `auto_compact_conversation()`. Subagent loops use the same thresholds and same `ConversationMeta` tracking.

### Tier 4: Reactive Compact

Emergency path, triggered when the API returns a 413 / prompt-too-long error. Lives in `agent.rs` as a catch block around the LLM API call.

- Keep only the last 4 messages
- Summarize everything else using Tier 3's prompt
- One-attempt guard (`has_attempted_reactive_compact` bool) to prevent retry loops
- On failure: surface error to user

## State: ConversationMeta

A new struct to hold per-conversation compaction state. Currently conversation is a bare `Vec<ChatMessage>`. The agent loop wraps it with this metadata:

```rust
pub struct ConversationMeta {
    pub messages: Vec<ChatMessage>,
    pub last_compact_turn: usize,       // turn count when last Tier 3/4 occurred
    pub has_attempted_reactive_compact: bool,  // one-shot guard for Tier 4
}
```

`last_compact_turn` is incremented by the agent loop on every iteration and read by `should_auto_compact()`. It is an in-memory counter, not persisted.

## Anti-Loop Guards

- **Recursion guard:** When calling LLM for Tier 3 summary, set an internal flag (`query_source = "compact"`). `should_auto_compact()` checks this and returns false. Without this, the compaction LLM call would itself trigger compaction → infinite loop.
- **Turn counter:** `last_compact_turn` tracks last Tier 3/4 compact; skip if `current_turn - last_compact_turn < COMPACT_TURN_GAP`.
- **Reactive one-shot:** `has_attempted_reactive_compact` prevents Tier 4 retry loops.
- **Backward compat:** Keep `is_compacted_regurgitation()` as safety net. After Tier 3 fires, no old markers remain in context.

## Threshold Constants

| Constant | Value | Purpose | Notes |
|----------|-------|---------|-------|
| `OBSERVATION_MASK_THRESHOLD` | 20,000 bytes (20%) | Trigger Tier 1 | Same as old `COMPACTION_PROMPT_BYTE_THRESHOLD` for backward compat |
| `COLLAPSE_THRESHOLD` | 60,000 bytes (60%) | Trigger Tier 2 | |
| `COMPACT_THRESHOLD` | 85,000 bytes (85%) | Trigger Tier 3 | |
| `REACTIVE_THRESHOLD` | 95,000 bytes (95%) | Trigger Tier 4 | |
| `PROMPT_HARD_CAP_BYTES` | 100,000 bytes | Absolute limit | Unchanged |
| `COMPACT_TURN_GAP` | 5 | Minimum turns between compacts | |
| `PRESERVED_TOOL_GROUPS` | 2 | Groups to keep verbatim | Unchanged |

Deprecated constants to remove:
| Old Constant | Reason |
|--------------|--------|
| `COMPACTION_MESSAGE_COUNT_THRESHOLD` (10) | Replaced by explicit > 15 in trigger conditions |
| `COMPACTION_PROMPT_BYTE_THRESHOLD` (20,000) | Replaced by `OBSERVATION_MASK_THRESHOLD` |
| `TOOL_ARGUMENT_COMPACT_THRESHOLD` (1,000) | No longer needed — tools are not individually truncated |
| `TOOL_RESULT_COMPACT_THRESHOLD` (2,000) | Replaced by observation masking |
| `TOOL_RESULT_PREVIEW_CHARS` (1,000) | Replaced by masking |
| `COMPACTION_MARKER_PREFIX` | Removed — no new markers created (retain for `is_compacted_regurgitation` backward compat) |

## Files to Change

### `src/agent_prompt.rs`
- Remove `compact_tool_heavy_history()`, `compact_tool_heavy_history_with_preserved_groups()`, `compact_assistant_tool_calls()`, `compact_tool_result()`
- Remove old threshold constants
- Add `observation_mask(messages) → Vec<ChatMessage>` (Tier 1)
  - For tool results older than PRESERVED_TOOL_GROUPS: replace content with `[previous tool result — masked]`
  - For old `[RustFox compacted:` markers in tool call arguments: replace with `[compacted]`
- Add `collapse_context(messages) → Vec<ChatMessage>` (Tier 2)
  - Identify tool groups via existing algorithm
  - Drop oldest 50% of non-preserved groups
  - Insert `[system] ★ earlier conversation collapsed ★` at collapse boundary
- Keep `prepare_messages_for_llm()` as sync function, now calls Tiers 1-2 only
- Keep `recovery_nudge_for()` unchanged
- Keep `estimate_prompt_bytes()` unchanged

### `src/agent.rs`
- Add `pub use agent_prompt::*` imports for new functions
- Add `ConversationMeta` struct definition
- At both call sites (main loop line ~645, subagent loop line ~1921):
  - Before `prepare_messages_for_llm()`:
    1. Call `should_auto_compact(meta)` — checks bytes + turn gap + recursion guard
    2. If true → call `async auto_compact_conversation(messages, llm)` → returns new messages
    3. Update `meta.last_compact_turn`
  - Around LLM API call:
    - Catch 413 error → call `async reactive_compact(messages, llm)`
    - Set `meta.has_attempted_reactive_compact` to prevent retry loop
- Wire LangSmith logging for Tier 3/4 events (tier, pre/post byte counts, message count delta)
- Keep `is_compacted_regurgitation()` as safety net

### `src/llm.rs`
- Add `fn has_tool_calls(&self) -> bool` to `ChatMessage`:
  ```rust
  pub fn has_tool_calls(&self) -> bool {
      self.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
  }
  ```
  Replaces inline `msg.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())` patterns.

### `src/agent_prompt.rs` — New Module Functions

```rust
/// Check whether Tier 3 auto-compact should trigger.
/// Returns false if query_source == "compact" (recursion guard) or
/// if COMPACT_TURN_GAP turns haven't passed since last compact.
pub fn should_auto_compact(messages: &[ChatMessage], meta: &ConversationMeta) -> bool;

/// Async: call LLM to summarize older portion of conversation.
/// Returns new message list with summary + preserved recent messages.
pub async fn auto_compact_conversation(
    messages: Vec<ChatMessage>,
    llm: &LlmClient,
) -> Result<Vec<ChatMessage>>;

/// Async: emergency compact for 413 recovery.
/// Keeps last 4 messages, summarizes the rest.
pub async fn reactive_compact(
    messages: Vec<ChatMessage>,
    llm: &LlmClient,
) -> Result<Vec<ChatMessage>>;
```

## Test Plan

### Unit Tests (in `agent_prompt.rs`, sync)

1. **Tier 1 masking:** Old tool results replaced with `[previous tool result — masked]` marker, recent tool results preserved, old `[RustFox compacted:` markers neutralized
2. **Tier 2 collapse:** Oldest tool groups removed entirely, structure is `[system] → [user] → [collapsed marker] → [recent groups]`
3. **Tier 2 boundary marker:** Collapse boundary has a structural marker, not silent removal
4. **Tiers 1-2 pipeline:** Both tiers execute in correct order with correct thresholds; Tiers 1-2 are purely sync and tested in isolation
5. **should_auto_compact:** Returns true when bytes > 85K, message count > 15, turn gap ≥ 5; false otherwise
6. **should_auto_compact recursion guard:** Returns false when `query_source == "compact"`
7. **should_auto_compact turn gap:** Returns false when turn gap < 5
8. **Summary prompt stripping:** Format output correctly (no stray XML)
9. **No regression:** Old `is_compacted_regurgitation()` still works if called

### Integration Tests

1. Run a full Tier 1-4 conversation through the pipeline: sync Tiers 1-2 produce correct output, then Tier 3 replaces old portion with summary, message count drops significantly
2. Verify 413 error triggers Tier 4 and falls back to last 4 messages
3. Verify the LLM doesn't repeat tool calls after Tier 3 compact
