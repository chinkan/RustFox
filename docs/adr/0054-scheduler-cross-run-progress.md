# ADR-0054: Same-day scheduled progress and stall checkpoints

- **Status:** Accepted
- **Date:** 2026-11-01
- **Issue:** Closes #331
- **Amends:** [ADR-0039](./0039-scheduler-timeout-transcript-continue.md) Decision 7 and Decision 11 (checkpoint write timing)
- **Related:** [ADR-0022](./0022-schedule-local-timezone.md) (calendar day), [ADR-0024](./0024-scheduler-run-timeout.md) (timeout gate), [ADR-0053](./0053-scheduler-completion-not-model-speech.md) (guard rewrite is not `completed`)
- **Does not block:** v1.0.8 tag. No version bump. No tag.

## Context

A scheduled run that stops short of a verified result leaves almost nothing for the next run. ADR-0039 writes a transcript checkpoint only on the wall-clock timeout human-gate. Max-iterations and guard-rewrite gates open the same manual Retry path and then full-replay, because no checkpoint row exists. There is also no task-level file, so the next cron fire the same day starts a new conversation and repeats work already done.

Product lock (2026-11-01): do A and B from #331. Do not do C (plan read-back). Do not raise `max_iterations`.

## Decision

1. **Daily progress file.** Path `{sandbox workspace}/_runs/<task_id>-YYYY-MM-DD.json`. The date is the task's IANA calendar day (ADR-0022). Unknown timezones use the UTC day. The task id is the scheduled-task id. The body is `task_id`, `date`, `last_phase`, `next_phase`, `evidence`, `attempt`, and optional `updated_at`. A file written with `next` instead of `next_phase` still loads.

2. **Inject on the next fresh run the same day.** If that file exists and Retry did not load a transcript checkpoint, the scheduled prompt gains an instruction to continue from `next_phase` and not redo completed work. A transcript continue is left as the checkpoint; the file is not also prepended.

3. **When the file is kept or removed.**

   | Stop | Daily file | Transcript checkpoint |
   | --- | --- | --- |
   | Wall-clock timeout | upsert | save (already ADR-0039) |
   | Max-iterations | upsert | save on the human-gate queue id |
   | Guard rewrite (unverified) | upsert | save on that queue id |
   | Other unverified (missing artifact, short clarify/menu) | upsert | no |
   | Verified `completed` | delete | no |
   | Explicit hard failure (agent `Err`, including failed-twice) | delete | no |
   | Transient provider error (hourly auto re-fire) | leave unchanged | no |

   Upsert keeps `last_phase` / `next_phase` already on disk, appends one evidence line, and increments `attempt`. Rust does not invent a phase name. Failed-twice stays full replay (no new checkpoint).

4. **Checkpoint shape is unchanged.** Messages, iteration, dangling tool ids. Retry still continues when a row exists and full-replays when it does not. The stored iteration is the real loop count. Continue budget stays `max(max_iterations - iteration, 1)`. This ADR does not raise `max_iterations`. A cap-exhausted checkpoint therefore resumes with one model turn; the daily file is what the next fresh scheduled run uses, with a full budget.

5. **Out of scope.** Plan read-back and archiving `.plans/` (#331 C). Idempotent-tool registry. Portal UI for these files. Deleting progress files from other days.

## Consequences

- The next fresh run of the same task on the same calendar day is told where the previous run left off, including when the agent never called a tool to write the file itself (timeout, max-iterations, guard rewrite).
- Human Retry on a max-iterations or guard-rewrite gate continues from the transcript instead of replaying the scheduled prompt from scratch.
- A verified completion or a hard error removes today's file so a later run is not told to resume a finished or crashed attempt.
- Operators who Retry a max-iterations gate still get only the remaining iteration budget (at least one turn), not a higher cap.
