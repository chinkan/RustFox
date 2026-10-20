# ADR-0038: Scheduler human-gate Telegram copy + inline retry/cancel

- **Status:** Proposed
- **Date:** 2026-10-21
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0013 (dead-letter / human-gate / full replay), ADR-0024 (scheduler run timeout → human-gate once)
- **Related:** #240 (tracking); Slice B true resume / checkpoint **out**
- **Issue:** #240

## Context

Kan: Scheduler human-gate Telegram copy is awkward (timeout shows raw
`900s` / “DID NOT auto-retry” / “half-done” / “run timeout” / queue id).
Operators need clearer stop wording and a low-friction Retry / Cancel
path without changing gate semantics.

Today (tip ~`4b7ec3c`):

- First-run wall-clock timeout (ADR-0024) and sibling human-gate DMs
  (re-run timeout, iteration stall, failed twice) share awkward copy and
  ask the operator to reply with bare `retry` / `cancel` text.
- Transient 429/5xx first death still auto-queues (ADR-0013) — notify-only
  hiccup copy, **not** a human-gate.
- Gate-expired / abandoned notify is separate.
- `RerunQueue::retry` / `cancel` already exist; retry is a **full replay**
  of the scheduled job (ADR-0013 — agent loop state is not serialisable).

Product lock (PO grill on #240, 2026-10-21 HKT; Round 1 Q1–Q5=A; Round 2
Q6=B, Q7–Q11=A) is authoritative for the Decision below. This ADR is
**Slice A only** (copy + inline/text gate UX). Slice B (true resume /
checkpoint) is explicitly out.

## Decision

### A. Human-gate copy skeleton (Round 1 Q1–Q5 A)

1. **First-run timeout** (ADR-0024 `enqueue_manual` path) uses this PO
   skeleton (Markdown as today):

   ```
   **Scheduled task stopped — time limit reached**

   It ran for 15 minutes and didn’t finish. I did **not** retry
   automatically, because some work may already have happened.

   Reply:
   • `retry` — run it once more
   • `cancel` — leave it stopped

   (id: `…`)
   ```

2. **Readable duration:** format whole minutes when the configured
   `scheduler.run_timeout_secs` divides evenly by 60 (e.g. default
   `900` → `15 minutes`). Non-whole-minute values may keep a clear
   seconds form; do **not** leave bare `900s` in operator-facing copy.
   Duration text comes from the configured timeout (or the elapsed bound
   used for that gate), not a hard-coded string.

3. **Retry wording:** always **“run it once more”** — never
   “automatically” on the human-gate retry line (avoids implying the bot
   will auto-retry after the gate).

4. **Same skeleton for siblings** (swap only the title / reason
   sentence; keep the “did not retry automatically / side effects /
   Reply / id” shape):
   - **Re-run timeout** (timeout while `rerun_id` is set)
   - **Iteration stall** (max-iterations → `enqueue_manual` /
     `awaiting_user`)
   - **Failed twice** (second death → ask human)

5. **Out of the skeleton (leave existing family or notify-only):**
   - **Transient hiccup** (first-death 429/5xx auto re-fire queued) —
     still auto; not a human-gate.
   - **Gate expired** / abandoned ageing notify — not this copy family.

### B. Inline primary + text backup (Round 2 Q6–Q11)

6. **Surface (Q6 B):** human-gate messages use **Telegram inline buttons
   as primary** (Retry / Cancel) **plus** text backup in the same
   message body so operators without callback UX can still act.

7. **Callback data (Q7 A):** mirror `model_select:` style, keep
   `callback_data` **≤ 64 bytes**:
   - `rerun:retry:<uuid>`
   - `rerun:cancel:<uuid>`
   where `<uuid>` is the `pending_reruns.id`.

8. **On callback (Q8 A):** `answer_callback_query`, then **edit** the
   gate message: **remove the inline buttons** and append / replace with
   a short result line — one of **Retried** / **Cancelled** /
   **Already done** (exact English wording may vary slightly; meaning
   locked).

9. **Idempotency (Q9 A):** if the queue row is already handled
   (`done` / `abandoned` / `superseded` / no longer `awaiting_user`),
   clear the buttons the same way and show **Already done** (or
   equivalent). Do **not** error-spam or double-dispatch.

10. **Text backup only (Q10 A):** deterministic allowlisted commands
    **`/retry <id>`** and **`/cancel <id>`** only. **No** LLM / chat
    history / natural-language matching for the gate (replaces reliance
    on bare `retry`/`cancel` via the model + `rerun` tool for this
    path). Impl may keep or retire the agent-visible `rerun` tool
    separately; the **operator gate** path for Slice A is the inline +
    `/retry`/`/cancel` pair.

11. **Keep human-gate (Q11 A):** timeout (and the sibling human-gate
    reasons above) stay **human-gate only** — **no** timeout auto-retry.
    First-death transient auto re-fire (ADR-0013) is unchanged.

### C. Retry / cancel semantics

12. **Full replay:** Retry and Cancel call existing
    `RerunQueue::retry` / `RerunQueue::cancel`. Retry remains a **full
    replay** of the scheduled job (ADR-0013). This ADR does **not**
    introduce mid-run resume.

13. **Release / process:** target **v1.0.7+**. Docs-only this PR; impl
    after merge + TL assign. Does **not** close #240 — leave open for
    impl. GitHub English only. Four gates on the **impl** PR. No tag
    from this issue.

## Non-goals

- **Slice B** true resume / checkpoint / serialisable agent-loop state.
- Portal human-gate UI / Settings.
- Auto-retry on timeout, max-iterations, or second death.
- Changing `scheduler.run_timeout_secs` default (`900`) or floor.
- Rewriting transient hiccup (429 auto) or gate-expired notify copy
  beyond leaving them out of this skeleton.
- Changing ADR-0013 two-strike / watchdog / `enqueue_manual` birth
  rules, or ADR-0024 cancel-on-timeout / TurnGate `finish` behaviour —
  only the operator-facing gate **copy** and **Retry/Cancel UX**.
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Implementing Rust in this PR.

## Acceptance criteria

- Docs state: first-run timeout DM uses the PO skeleton (title, readable
  duration, did not auto-retry, side effects, Reply `retry`/`cancel`,
  `(id: …)`); siblings share the skeleton with swapped reason sentence.
- Docs state: retry line says **“run it once more”** (not
  “automatically”); transient hiccup and gate expired are out of this
  family.
- Docs state: inline buttons primary + text backup; callback
  `rerun:retry:<uuid>` / `rerun:cancel:<uuid>` (≤64 bytes); on press
  answer_callback + edit (remove buttons + Retried/Cancelled/Already
  done); already-handled is idempotent Already done.
- Docs state: text backup is only `/retry <id>` and `/cancel <id>`
  (deterministic; no LLM/history matching for the gate).
- Docs state: human-gate kept (no timeout auto-retry); Retry/Cancel =
  existing queue APIs = full replay (ADR-0013); Slice B out.
- CHANGELOG Unreleased note for this docs slice; shipping release note
  on impl.
- This docs PR does **not** close #240.

## Consequences

- Docs-only first (this ADR + short cross-refs on ADR-0013 / ADR-0024).
  Tech lead assigns RustFox Rust Developer for impl after merge
  (`fix/240-scheduler-human-gate` or similar). Leave #240 open for
  impl — this PR is a docs slice only.
- Operators get clearer stop wording and one-tap Retry/Cancel without
  changing side-effect / human-gate policy.
- Full-replay retry stays honest in copy (“run it once more”) so
  operators do not expect silent auto-retry or true resume.

## Related

- ADR-0013 — dead-letter queue, two-strike, full replay, human-gate.
- ADR-0024 — wall-clock timeout → human-gate once (no auto re-fire).
- #240 — tracking issue (stays open through impl).
