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

## Design: 4-Tier Progressive Pipeline

Replace `compact_tool_heavy_history()` with a graduated pipeline:

```
prepare_messages_for_llm():
  1. Estimate context bytes %
  2. If >50%  → Tier 1: observation masking (mask old tool results, 0 LLM cost)
  3. If >70%  → Tier 2: context collapse (drop oldest tool groups, 0 LLM cost)
  4. If >85%  → Tier 3: auto compact (LLM summarization, 1 LLM call)
  5. If >95%  → Tier 4: reactive (keep last 4 messages, summarize rest)
```

### Tier 1: Observation Masking

Replace old tool result content with `[previous tool result — masked]` marker. Preserves the message structure and tool_use record. The LLM knows it made the call, but doesn't carry the bulky payload.

- **Trigger:** estimated bytes > 50% of hard cap (50,000/100,000)
- **Scope:** tool results older than `PRESERVED_TOOL_GROUPS` (2)
- **Cost:** 0 LLM calls

### Tier 2: Context Collapse

Remove entire oldest assistant-with-tool-calls groups and their tool results. These messages are **gone**, not compacted.

- **Trigger:** still > 70% after Tier 1
- **Scope:** oldest 50% of non-preserved tool groups
- **Cost:** 0 LLM calls

### Tier 3: Auto Compact (Core)

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
- Estimated prompt bytes > 40,000
- Last compact was ≥ 5 turns ago (anti-loop guard)

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
Use <summary> tags to wrap your output.
```

**Post-Compact Reconstruction (after LLM returns summary):**
1. Strip `<summary>` tags from LLM output (`format_compact_summary()`)
2. Create compact boundary marker message (system role)
3. Create summary message (system role)
4. Append preserved recent messages
5. Append `recovery_nudge_for()` message
6. Return new message list

**Cost:** 1 LLM call (returns ~500-2000 tokens of summary, saves potentially 50K+ tokens)

**Integration Note:** `prepare_messages_for_llm()` is currently sync. Tier 3 requires an async LLM call, so the trigger logic moves to `agent.rs`:
1. `agent.rs` processing loop calls `should_auto_compact(messages, turn_count)` to check thresholds
2. If true, calls `async auto_compact_conversation(messages, llm_client)` → returns new message list with summary
3. Then calls sync `prepare_messages_for_llm()` (which now does only Tiers 1-2 masking/collapse) on the already-compacted messages
4. Tiers 1-2 remain sync inside `prepare_messages_for_llm()` as they are 0-cost

### Tier 4: Reactive Compact

Emergency path triggered when the API returns a 413 / prompt-too-long error.

- Keep only the last 4 messages
- Summarize everything else using Tier 3's prompt
- One-attempt guard to prevent retry loops

## Anti-Loop Guards

- **Recursion guard:** Set `query_source = "compact"` when calling LLM for summary; Tier 3 checks this and skips itself
- **Turn counter:** Track `last_compact_turn` per conversation; skip if < 5 turns since last compact
- **Backward compat:** Keep `is_compacted_regurgitation()` as safety net, but new system should never trigger it

## Threshold Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `OBSERVATION_MASK_THRESHOLD` | 50,000 bytes (50%) | Trigger Tier 1 |
| `COLLAPSE_THRESHOLD` | 70,000 bytes (70%) | Trigger Tier 2 |
| `COMPACT_THRESHOLD` | 85,000 bytes (85%) | Trigger Tier 3 |
| `REACTIVE_THRESHOLD` | 95,000 bytes (95%) | Trigger Tier 4 |
| `PROMPT_HARD_CAP_BYTES` | 100,000 bytes (unchanged) | Absolute limit |
| `COMPACT_TURN_GAP` | 5 | Minimum turns between compacts |
| `PRESERVED_TOOL_GROUPS` | 2 | Groups to keep verbatim (unchanged) |

## Files to Change

| File | Changes |
|------|---------|
| `src/agent_prompt.rs` | Replace `compact_tool_heavy_history()` with 4-tier pipeline. Add `format_compact_summary()`. Add `auto_compact_conversation()`. Add `reactive_compact()`. |
| `src/agent.rs` | Wire LangSmith logging for new compaction events. Update `is_compacted_regurgitation()` docs. |
| `src/llm.rs` | Add `is_tool_call()` helper to ChatMessage (needed by pipeline). |

## Test Plan

### Unit Tests (in `agent_prompt.rs`)

1. **Tier 1 masking:** Old tool results replaced with marker, recent tool results preserved
2. **Tier 2 collapse:** Oldest tool groups removed entirely, structure preserved (system → user → compact → recent)
3. **Tier 3 auto compact:** 15+ messages over threshold → replaced with summary + recent messages
4. **Tier 3 no-trigger:** Under threshold → passthrough unchanged
5. **Tier 3 anti-loop:** Second compact within 5 turns → skipped
6. **Summary format:** `format_compact_summary()` strips `<analysis>` blocks, formats `<summary>` correctly
7. **Progressive pipeline:** All 4 tiers execute in correct order with correct thresholds
8. **Regurgitation elimination:** After Tier 3, no `[RustFox compacted:` markers remain in context

### Integration Tests

1. Run a real conversation through `prepare_messages_for_llm()` with all 4 tiers
2. Verify message count is significantly reduced after Tier 3
3. Verify the LLM doesn't repeat tool calls after compact
