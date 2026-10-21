# ADR-0040: Human Retry eligibility now + sync claim/dispatch

- **Status:** Accepted
- **Date:** 2026-10-21
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0013 (dead-letter / two-strike / full replay / hourly watchdog), ADR-0038 (human-gate copy + inline Retry/Cancel)
- **Amends:** ADR-0013 (human `retry` eligibility + dispatch timing), ADR-0038 (Retry UX implies immediate replay)
- **Related:** ADR-0039 (Slice B transcript continue — out)
- **Issue:** #246

## Context

After ADR-0038 Slice A (#245 → main `775179f`), Kan tapped inline **Retry**
on a timeout human-gate DM. Telegram correctly edited the message to
**Retried**, but the scheduled task did **not** re-run immediately.

Root cause: `RerunQueue::retry` reset the row to `queued` / `attempts = 0`
but set `next_eligible_at = now + 30m` — the same `ELIGIBILITY_DELAY_MINUTES`
used by first-strike auto `enqueue` for 429 congestion. `due()` only returns
rows with `next_eligible_at <= now`, so human Retry was silently deferred
~30 minutes. The hourly watchdog (ADR-0013) could add up to another ~1h
before dispatch even after eligibility elapsed.

Operator copy (ADR-0038) says **run it once more** — that implies
**immediate** full replay, not a congestion delay.

Product locks (PO grill on #246, 2026-10-21 HKT; Q1–Q8 = A) are
authoritative for the Decision below.

## Decision

1. **Human Retry eligibility = now (Q1/Q5 A):** `RerunQueue::retry` sets
   `next_eligible_at = datetime('now')` (or equivalent). It does **not**
   apply the +30m congestion delay.

2. **Auto enqueue unchanged (Q8 A):** first-strike auto `enqueue` still uses
   `+30 minutes`. The hourly watchdog sweep interval is **unchanged**.

3. **Sync claim+dispatch after Retried (Q1/Q5 A):** after a successful
   human Retry (`GateOutcome::Retried`), the bot **synchronously** runs the
   same per-row path as the watchdog for that one row:
   load task → abandon if missing/inactive → `mark_dispatched` (claim
   first) → `Agent::build_rerun_request`. This is **not** notify/wake-only
   and must not wait for the next hourly tick.

4. **Both human surfaces kick (Q6 A):** inline callback and `/retry <id>`
   both go through `apply_gate_action`; both kick after `Retried`.
   **Cancel does not kick.**

5. **Kick failure vs UI (Q7 A):** if claim/send fails after eligibility was
   set to now, the operator UI still shows **Retried**; log a warn. Claim
   semantics follow the watchdog (`mark_dispatched` before send).
   Crash/send-fail recovery continues to rely on existing boot
   `reset_inflight_on_boot` and the hourly sweep.

6. **Primary UX (product lock):** Retry button (callback carries uuid) is
   primary; `/retry <id>` is text backup. Bare `/retry` (no id) is **out**
   of this slice.

7. **Stuck-row soft/ops only:** one-off stuck Kan rows are soft/ops, not a
   product path in this ADR.

8. **Full replay unchanged:** Retry remains ADR-0013 full replay. Slice B
   transcript continue stays ADR-0039 (out).

## Non-goals

- Changing hourly auto sweep cadence or enqueue 30m delay.
- Bare `/retry` without id.
- Slice B resume / checkpoint (ADR-0039).
- Portal human-gate UI.
- Merge/tag from this issue (four gates on the impl PR; no tag).

## Consequences

- Human Retry becomes eligible immediately and is claim+dispatched on the
  same path as the watchdog, matching “run it once more” copy.
- Auto first-strike congestion behaviour (30m + hourly) is preserved.
- Kick errors are best-effort relative to UI; existing boot/hourly recovery
  still heals stranded claims.

## Tests

- `retry()` → row in `due()` immediately; eligibility offset ~0.
- `enqueue` still ~+30m and not due immediately.
- Kick wiring: claim bumps `attempts`, removes row from `due()`, and
  enqueues a `ScheduledJobRequest` with `rerun_id` (in-memory / channel;
  no Telegram network).
- Cancel does not leave a due queued row.

## Related

- ADR-0013 — dead-letter queue, two-strike, full replay, hourly watchdog.
- ADR-0038 — human-gate copy + inline Retry/Cancel UX.
- ADR-0039 — timeout transcript continue (Slice B; out).
- #246 — tracking / closes on impl PR.
