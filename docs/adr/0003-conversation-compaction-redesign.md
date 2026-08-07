# ADR 0003: Conversation Compaction Redesign

## Status
Accepted (implemented)

## Date
2026-08-06

## Context

The agent "forgets what the user asked" and answers follow-up questions with
predictions of what the user "is going to ask". Root cause analysis:

- `compact_messages` (src/conversation.rs) triggers on raw character count
  against a hardcoded `context_window = 128_000` (src/loop_runner.rs:97),
  excluding tool-call arguments. 128K chars ≈ 22K tokens ≈ 17% of a real
  128K-token window — compaction fires far too early.
- The preserved tail is "newest 8 messages" (conversation.rs:211), not the
  last user turn. The oldest non-system message — usually the user's current
  request — is summarized first, mid-task, because compaction runs inside the
  agent loop gated by loop-iteration gap (loop_runner.rs:108-118).
- The summary is injected as `role: "user"` with `tool_call_id: "summary"`
  (conversation.rs:227-232). The model treats the compacted history as the
  current user turn and answers its content — hence "I know what you're going
  to ask".
- Each compaction re-summarizes raw history from scratch; the previous summary
  is itself re-summarized next pass → cumulative information loss.
- Sync fallback truncates every message to 200 chars (conversation.rs:300) —
  deletion, not summarization.

Industry research (Claude Code 4-tier cascade + compaction sub-agent,
OpenCode prune-then-summarize + nested compression, OpenClaw compaction vs
pruning, Codex session-memory/server-side compact, LangChain/LangGraph
summarize-then-extend): all converge on token-based triggers with a reserve
buffer, running/nested summaries, system-role injection, compaction at turn
boundaries, and full-history persistence.

## Decision

### Q1 — Compaction cadence (Accepted)
Move compaction out of the agent loop. Routine compaction runs **once per
user turn** in `process_message` (after `add_user_turn`, before the loop).
Long-running tool loops are handled by delegation first, then by a
threshold-triggered compact as last resort — never by a per-iteration check.
The loop keeps Tier 1/2 masking as an emergency safety net only.
`ConversationMeta`/`should_auto_compact`/loop compaction plumbing becomes dead
code and is deleted.

### Q2 — Summary representation (Accepted)
Adopt a **running summary** carried on `ConversationManager` (`summary:
Option<String>`). Each compaction EXTENDS the previous summary ("Extend the
previous summary with the new messages above"), layered under a
"Previously compacted context" heading. Injected as a **system message**
before history, never as a user-role message. The per-message marker-line
format is retired to the sync fallback only.

### Q3 — Trigger metric (Accepted)
Token-based trigger at **85% of the real provider window**, not chars:
`estimate_tokens = (latin + other chars) / 4 + CJK chars × 1`. The window
comes from `ProviderConfig.context_window` via `registry.resolve_model(
current_model)`, replacing the hardcoded `128_000`. Estimation unifies with
`estimate_prompt_bytes` (which already counts tool args) — a single
`estimate_tokens` used for both trigger and prompt budget. The
`COMPACT_TRIGGER_PCT 0.70` / `OBSERVATION_MASK_PCT` / `COLLAPSE_PCT` char
ladder is removed.

### Q4 — Preserve policy (Accepted)
The protected verbatim zone is the **latest user intent**: the last **two**
user turns' user messages verbatim plus the active exchange (last user
message → end). Tool traffic in older turns is summarized, never kept raw.
The preserved tail is capped at ≤ 20% of the window. There is no
first-request anchor — the running summary carries the original request
forward, per assistant (not coding-task) semantics.

### Q5 — Durable memory flush (Accepted)
Before the running summary is written, one flush turn extracts durable facts
(preferences, standing intents, project state) from the to-be-summarized
range and writes them to **USER.md** (home root) — not a new internal file.
`user_model.md` is legacy; `config.rs` already migrates it to `USER.md`.
USER.md wins because it is injected into the system prompt every message
(agent.rs:309), has the validated write path in `learning.rs`
(frontmatter check prevents prompt injection, `.bak` backup before
overwrite, merge-not-remove, 500-word cap), is agent-editable via
`update_soul_file`, and is already cron-updated weekly. Implementation:
refactor `update_user_model_inner` (learning.rs:489) to accept snippets as
a parameter; the flush passes the to-be-summarized range, the cron keeps
passing `search_messages` results. Same validation + backup + write tail
shared.

### Q6 — Flush gating (Accepted)
Run the flush only when the summarized range contains ≥ 1 **user-authored**
message (tool traffic alone cannot contain durable facts), and skip when the
range was already covered by a recent flush — `last_flush_turn` tracked on
`ConversationManager`.

### Q7 — Sync fallback (Accepted, with change)
The 200-char-per-message truncation (conversation.rs:300) is removed. Fallback
chain:
1. Summary succeeds → running-summary compact.
2. Summary fails → **defer**: skip this turn's compact; the 85% trigger
   leaves 15% slack; retry next turn.
3. Hard ceiling hit (emergency mask) → **oldest-first truncation of
   non-protected messages only** — the protected tail (last 2 user turns +
   active exchange) is never touched; dropped traffic is replaced by a
   one-line marker (precedent: Codex v0.118 no-LLM session-memory compact).

Every summary failure is logged (`warn!`) with reason, model, and message
range so failures are visible without being user-facing noise.

### Q8 — Running-summary persistence (Accepted)
The running summary is persisted in the database, keyed by conversation id
(e.g. a `summary` role row in the existing message store). Reloaded in the
conversation load path; extension continues seamlessly across restarts.
`ConversationManager` remains the in-memory view; the DB row is the
source of truth.

### Q9 — Summarizer model (Accepted)
New config key `compaction_model` (empty default = current model). Summary
and flush turns run on the configured model via `registry.resolve_model`,
letting users pin a cheap fast model. Precedent: OpenClaw `compaction.model`,
Claude Code compaction sub-agent.

## Decision complete — ready for implementation planning

## Consequences
- The active user request and recent turns survive compaction verbatim.
- The model never sees compacted history as its current user turn.
- Information loss per compaction is bounded (layered extension, not
  re-summarization).
- Compaction cost moves off the hot path; each user turn pays at most one
  summarizer call.
