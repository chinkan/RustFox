# ADR-0013: Dead-letter re-run queue + hourly watchdog (scheduled tasks)

- **Status:** Accepted
- **Date:** 2026-09-26
- **Branch:** `fix/429-backup-model`
- **Depends on:** ADR-0012 (chat already tries fallbacks; queue catches what still dies)

## Context

11 failed task runs / 7 days, all final 429 errors *after* ADR-0009 retries *and* (once #0012
lands) fallback attempts. Congestion windows are ~10 min+; the useful action is **re-fire the
whole job later**, not resend now. Kan's Q3 is the crux: an agent run cannot be resumed
mid-flight (LLM loop state is not serialisable), so a re-run replays side effects (a Threads
job that posted 3 of 6 replies would post them again). Decision:

- **First death → automatic single re-fire** (attempts=1), no human in the loop.
- **Second death → ask Kan** (DM with `retry` / `cancel`), never auto again.
- **Re-fire succeeds → output is exactly a normal run** (no "♻️ delayed" marker; Q4). Whether
  queue events appear in the morning report is a *skill* concern (dream D2), not code.
- **Watchdog is system-level** (Q5): tokio interval in main, hourly; not a user `scheduled_task`
  row (can't be edited/deleted by accident, doesn't pollute the user list).

## Decision

1. **Table `pending_reruns`** (new, alongside — not inside — `scheduled_task_runs`, which stays
   append-only evidence):

   ```sql
   CREATE TABLE pending_reruns (
     id                TEXT PRIMARY KEY,
     task_id           TEXT NOT NULL,        -- FK scheduled_tasks
     original_run_id   TEXT NOT NULL,        -- FK scheduled_task_runs (the failed row)
     fail_reason       TEXT NOT NULL,
     attempts          INTEGER NOT NULL DEFAULT 0,  -- re-fires already done
     state             TEXT NOT NULL DEFAULT 'queued', -- queued|awaiting_user|abandoned|done|superseded
     next_eligible_at  TEXT NOT NULL,        -- datetime('now', '+30 minutes')
     created_at        TEXT NOT NULL DEFAULT (datetime('now')),
     updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
   );
   ```

2. **Enqueue** (main runner, on `Err(e)` where `is_transient_llm_error(e)`):
   insert `queued, attempts=0, next_eligible=now+30m`. Non-LLM errors never queue. If a live
   queue row already exists for the same task → mark the old row `superseded`, insert new
   (latest prompt wins; task definitions are edited via portal/tools).

3. **Watchdog, hourly tick** (interval in main, first tick skipped like OAuth refresher):
   - `queued AND next_eligible_at <= now` → load task row (must be live: `status='active'`,
     not soft-deleted), rebuild `IncomingMessage` from `chat_id/prompt/platform/user_id`
     exactly like `build_fire_closure`, set `rerun_id = Some(row.id)`, dispatch on `job_tx`,
     bump `attempts += 1, next_eligible_at += 30m`.
   - `awaiting_user` older than **7 days** → `abandoned` + DM one-liner (don't nag forever).

4. **Runner outcome, when `req.rerun_id.is_some()`** (Q3 two-strike):
   - success → queue row `done`; Telegram send path identical to a normal run.
   - failure → queue row `awaiting_user`; DM: task name, both failures, `[retry <id>] /
     [cancel <id>]` instructions. **No** further auto attempts.

5. **Human gate = `rerun` builtin tool** (agent-visible, so Kan can reply in natural chat and
     the model can call it; also usable from portal later):
   - `retry <id|task description>` → reset to `queued, attempts=0` (second auto attempt still
     bound by the two-strike rule: if it dies again it re-asks).
   - `cancel <id>` → `abandoned`.
   - `list` → active rows (queued/awaiting_user) with id, task, attempts, age.

6. **Crash safety:** boot runs `UPDATE pending_reruns SET attempts=0, next_eligible_at=… WHERE
   state='queued' AND attempts>0` — a re-fire dispatched but not persisted would otherwise
   strand at attempts=1 and silently upgrade to "ask on second death" for a run that never ran.
   (Dispatch → tick persists attempts *before* send; the crash window is one tick.)

7. **Retention:** `done`/`abandoned`/`superseded` rows older than 30 days deleted at watchdog
   tick (queue is control state, not history — history lives in `scheduled_task_runs`).

## Rejected alternatives

- **Celery-style external broker / Redis:** single-binary self-host constraint; SQLite row queue
  is all we need for ~dozens of tasks/user.
- **Reusing `scheduled_tasks.retry_max`:** that column belongs to tokio-cron job plumbing, not
  dead-letter semantics; mixing them makes portal display ambiguous.
- **Auto re-fire up to N times:** rejected by Q3 — second death *asks*, because side effects.
- **Persisting the agent-loop mid-state for true resume:** research-grade, not PR-shaped.

## Consequences

- ⚠️ Re-fire is a **full replay** — safe for idempotent-ish jobs (briefings re-check vault state
  per skills) but a job that dies *after* publishing may duplicate if it dies again *before*
  queueing rules out. Accepted deliberately: first strike auto because most deaths are turn-1
  LLM errors (zero side effects); second strike asks a human to judge side effects.
- ⚠️ Watchdog delay ≤1h by design (Q4). 30m eligibility + hourly tick ≈ worst-case ~90m gap —
  still vastly better than "failed until next day".
- ✅ `scheduled_task_runs` untouched → dream Phase W audit trail semantics unchanged.

## Tests (in-memory SQLite + pure logic seams)

Enqueue only on transient error (helper predicate). Supersede on duplicate live rows. Watchdog
select boundary (`next_eligible_at <= now`), skips soft-deleted/inactive tasks. attempts=1
death → awaiting_user (no 3rd attempt). Success → done + normal send. retry → requeue attempts
reset; cancel → abandoned; 7-day awaiting_user → abandoned. Crash reset boot migration. Schema
idempotency (open twice).

## Amendment (2026-09-26): max-iterations joins the dead-letter path

Recon found a **second silent failure class** with the same root shape: a scheduled run that
hits `max_iterations` returns `Ok(...)` with a "please rephrase" string. The runner recorded it
`completed` and delivered a shrug — the task looked successful, and nothing told Kan it had
stalled mid-task.

Decision: the agent loop now reports a **typed stop reason** (`RunStop::{FinalResponse,
Cancelled, MaxIterations, Llm}` via `RunOutcome`), and the runner treats `MaxIterations` as a
failure:

- run row → `failed` with reason `Reached max iterations (N) without a final response`;
- queue row → **`enqueue_manual`**: born `awaiting_user` with `attempts=1`, so the watchdog can
  **never** auto-fire it (a budget-exhausted run may already have side effects);
- DM explains *why* it was not retried, and offers the same `retry` / `cancel` gate.

`process_message` stays the text-returning wrapper every other caller uses; only the
scheduled-task runner needs the stop reason. This keeps chat/portal/subagent behaviour
byte-identical.

**Why notify-only and not auto-retry:** a max-iterations run stopped *between* tool calls, not
before them. Unlike a turn-1 429 (zero side effects), replaying it can duplicate whatever it
already did — exactly the Threads-duplicate hazard Q3 exists to prevent.
