# ADR-0039: Scheduler timeout transcript continue (Slice B)

- **Status:** Proposed
- **Date:** 2026-10-21
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0013 (dead-letter / human-gate / full replay default), ADR-0024 (scheduler run timeout → human-gate once)
- **Related:** #240 (tracking); ADR-0038 / #242 (Slice A copy + inline Retry/Cancel — separate)
- **Issue:** #240

## Context

Kan: after a scheduled run hits the wall-clock timeout human-gate
(ADR-0024), Retry today is always a **full replay** of the scheduled job
(ADR-0013 — agent loop state is not serialisable). That re-runs side
effects already performed before the timeout (the Threads-duplicate
hazard ADR-0013 Q3 exists to prevent).

Slice A (ADR-0038) only clarifies operator-facing gate copy and adds
inline `/retry`/`/cancel` UX; Retry still means full replay. Slice B
needs a restart-safe **continue** path that restores the conversation
transcript without auto-replaying interrupted tools.

Product lock (PO grill on #240 Slice B, 2026-10-21 HKT; Q2=D, Q3=A,
Q4=A, Q5–Q10=A) is authoritative for the Decision below. This ADR is
**Slice B only**. Slice A (copy + inline) remains ADR-0038 / #242.

Today (tip ~`4b7ec3c`):

- First-run wall-clock timeout → cancel in-flight agent, mark run
  failed, `enqueue_manual` human-gate once (ADR-0024). No transcript
  checkpoint is written.
- Operator Retry → `RerunQueue::retry` → full replay from the scheduled
  prompt (ADR-0013).
- Iteration stall / failed-twice human-gates share the same Retry =
  full-replay path; they are **out of first scope** here (Q4 A).

## Decision

### A. Persist transcript continue (Q2 D)

1. **Checkpoint shape:** on the in-scope timeout gate, persist a
   **transcript checkpoint**: conversation **messages** (including
   completed **tool results**), current **iteration** counter, and
   **dangling tool call ids** (tool calls that were in flight or
   unanswered when the timeout cancelled the turn).

2. **Retry = restart-safe continue:** when a checkpoint is present for
   the queue / run id, Retry **continues** from that transcript (OpenClaw-
   style continue — Q6 A): restore messages + iteration, then let the
   agent loop proceed. Do **not** auto-replay side-effecting tools that
   were interrupted.

3. **Not soft summary-only (old A):** do not replace the transcript with
   a compacted summary as the resume payload for this slice.

4. **Not full Rust loop / future serialize (old B):** do not serialize
   Tokio tasks, cancel tokens, open HTTP bodies, or other live Rust
   futures. Transcript + iteration + dangling ids only.

### B. Isolation from ADR-0013 default (Q3 A)

5. **This ADR isolates Slice B.** Until this lands in impl, ADR-0013
   **full replay remains the default** Retry behaviour for every
   human-gate (including timeout).

6. After impl: the **same** Retry control (inline button / `/retry`
   from Slice A when present, or existing gate Retry) chooses:
   - **checkpoint present** → transcript continue (this ADR);
   - **no checkpoint** → full replay (ADR-0013).

### C. First scope = timeout human-gate only (Q4 A)

7. **Write and consume checkpoints only for the wall-clock timeout
   human-gate** (ADR-0024 `enqueue_manual` path on first-run / re-run
   timeout). Iteration stall and failed-twice gates stay full-replay
   until a later ADR / amend.

### D. Checkpoint content, dangling, storage, write timing (Q5–Q10 A)

8. **Content (Q5 A):** messages + iteration + dangling tool ids (as in
   Decision 1). Enough for a coherent LLM continue turn; not a process
   snapshot.

9. **Continue semantics (Q6 A):** OpenClaw-style continue — restore
   transcript and resume the agent loop at the next model turn. Dangling
   interrupted tool calls are **not** treated as success and are **not**
   re-executed automatically (Q7 A). Impl synthesizes a safe interrupted /
   cancelled tool-result (or equivalent) so the message list stays
   well-formed for the provider; exact string is an impl detail.

10. **Storage (Q8 A):** new **SQLite table** keyed by scheduled **run id
    and/or pending_reruns (queue) id** (exact FK layout is impl; must be
    addressable from the human-gate Retry path). Not Temporal / LangGraph
    node checkpoints; not an external broker.

11. **Write timing (Q9 A):** write the checkpoint **only on the timeout
    human-gate** path (when cancelling for `run timeout` and enqueuing
    the manual gate). Do not checkpoint every tool step or every
    iteration in this slice.

12. **Same Retry button (Q10 A):** one operator Retry control. If a
    checkpoint row is present for that gate → continue; else → full
    replay. Cancel abandons the gate and does not continue; checkpoint
    retention / cleanup follows queue row lifecycle (impl may delete or
    mark consumed on successful continue / abandon — must not leave a
    stale checkpoint that silently continues a cancelled job).

### E. Process / release

13. **Release / process:** target **v1.0.7+**. Docs-only this PR; impl
    after merge + TL assign. Does **not** close #240 — leave open for
    Slice A + Slice B impl. GitHub English only. Four gates on the
    **impl** PR. No tag from this issue.

## Non-goals / iced

- **Temporal / LangGraph-style node checkpoint** frameworks.
- **Mid-tool hard resume** (resume inside an in-flight tool process /
  HTTP call as if never cancelled).
- **Timeout auto-retry** (human-gate stays; ADR-0024 / ADR-0013).
- **Idempotent-tool registry** (Should later — not required to ship
  continue; do not block Slice B on classifying every tool).
- Slice A copy / inline UX (ADR-0038 / #242 — separate).
- Checkpointing iteration stall / failed-twice gates in this slice
  (Q4 A — later).
- Portal human-gate UI / Settings for checkpoints.
- Soft summary-only resume or full Rust future serialization (rejected
  Q2 A/B).
- Changing ADR-0013 two-strike / watchdog / `enqueue_manual` birth
  rules, or ADR-0024 cancel-on-timeout / TurnGate `finish` — only add
  optional transcript continue when a checkpoint exists.
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Implementing Rust in this PR.

## Acceptance criteria

- Docs state: checkpoint = messages + tool results + iteration +
  dangling tool ids; Retry with checkpoint = restart-safe continue; no
  auto-replay of side-effecting / interrupted tools.
- Docs state: not summary-only; not full Rust loop/future serialize.
- Docs state: ADR-0013 full replay remains default until B lands; after
  land, same Retry button → continue if checkpoint else full replay.
- Docs state: first scope = wall-clock timeout human-gate only;
  stall / failed-twice later.
- Docs state: dangling interrupted ≠ success / do not re-run; storage =
  new SQLite table keyed by run/queue id; write checkpoint only on
  timeout gate.
- Docs state: iced — Temporal/LangGraph node checkpoint; mid-tool hard
  resume; timeout auto-retry; idempotent-tool registry (Should later).
- CHANGELOG Unreleased note for this docs slice; shipping release note
  on impl.
- This docs PR does **not** close #240. Slice A is ADR-0038 / #242
  (separate).

## Consequences

- Docs-only first (this ADR + short cross-refs on ADR-0013 / ADR-0024).
  Tech lead assigns RustFox Rust Developer for impl after merge
  (`fix/240-scheduler-transcript-continue` or similar). Leave #240
  open — this PR is a docs slice only.
- Operators who Retry a **timeout** gate after B lands continue from
  progress instead of blindly replaying side effects, while gates
  without a checkpoint stay honest full-replay (ADR-0013).
- Side-effect safety stays explicit: interrupted tools are not
  re-fired; true mid-tool hard resume and idempotent-tool registry
  remain later work.

## Related

- ADR-0013 — dead-letter queue, two-strike, **full replay default**,
  human-gate.
- ADR-0024 — wall-clock timeout → human-gate once (no auto re-fire);
  first checkpoint write site.
- ADR-0038 — Slice A human-gate Telegram copy + inline retry/cancel
  (separate PR #242; not this branch).
- #240 — tracking issue (stays open through Slice A + Slice B impl).
