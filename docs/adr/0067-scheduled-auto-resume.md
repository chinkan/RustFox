# ADR-0067: Auto-resume incomplete scheduled tasks

- **Status:** Accepted
- **Date:** 2026-11-15
- **Issue:** #412 (v1.0.10 Must). Does not block or retag v1.0.9.
- **Amends:** [ADR-0039](./0039-scheduler-timeout-transcript-continue.md) (mid-run persist), [ADR-0024](./0024-scheduler-run-timeout.md) Decisions 2 and 4, the [ADR-0013](./0013-dead-letter-rerun-queue.md) max-iterations amendment, [ADR-0054](./0054-scheduler-cross-run-progress.md) Decision 4 only as stated below.

## Context

Scheduled timeout and max-iterations stops opened a human Retry gate and did not continue until someone tapped Retry. A process restart failed leftover `running` rows and did not continue. Checkpoints were written only on those human-gate stops, so a crash mid-run had nothing to continue from.

Kan locked heartbeat-or-startup resume without a Retry click, continue-from-checkpoint rather than a full replay, and a synchronous checkpoint after each completed tool. Product Soft lock fixed the remaining questions: the restore trigger, notify-only copy, the iteration budget, unverified dead-letters, one active run, queue ids, `reset_inflight_on_boot`, and the stale-running watchdog.

## Decision

1. **Scheduled tasks only.** Interactive Retry is unchanged. `TurnGate`'s one-shot interactive max-iterations auto-continue stays as-is. No interactive wall-clock timeout is added.

2. **When.** Auto-resume runs on process start (the startup path, after `fail_all_running`) and on a dedicated restore tick every 60 seconds. The builtin liveness cron named `heartbeat` (`0 0 * * * *`, log line `Heartbeat: bot is alive`) is not the trigger. The hourly system watchdog is not the trigger, and its skipped first tick is not the boot path. A timeout or max-iterations stop in a living process dispatches continue immediately (the next tick is only the backup, at most 60 seconds). It does not wait for the task's next cron.

3. **What resumes.** Timeout, max-iterations / IterationStall, and process-restart interrupt of scheduled work. Continue from the checkpoint. Do not full-replay when a checkpoint exists. If none exists, do not full-replay as auto-resume.

4. **Mid-run persist (amends ADR-0039).** Write synchronously after each completed tool, before the next tool starts. Keep the human-gate checkpoint writes (timeout, max-iterations, guard-rewrite). Do not use a timer as the primary cadence. Do not write a resumable checkpoint mid-tool or mid-LLM-stream. Unfinished in-flight tools stay stubbed and are not rerun.

5. **No human Retry gate for those stops (amends ADR-0024 Decision 2 and the ADR-0013 max-iterations amendment).** Do not leave `awaiting_user` plus a Retry button for timeout or max-iterations auto-resume. One informational message may say the scheduled task timed out or hit max iterations and will auto-continue. The user does not have to tap Retry.

6. **Iteration budget (ADR-0054 Decision 4, unchanged formula).** Auto-resume of `CheckpointStopKind::MaxIterations` gets a full configured `max_iterations`. Timeout and process-restart (`MidRun`) continues get `max(configured cap − stored iteration, 1)`. Auto-resume does not grant a full cap to timeout or process-restart continues.

7. **Fresh wall-clock timeout.** Each `run_one_scheduled_job` invocation already starts a new timeout. No extra timer reset.

8. **Unverified stays a human dead-letter.** `HumanGateKind::UnverifiedResult` (guard-rewrite, missing artifact, short clarify/menu, `UnverifiedOther`) is not in the auto-resume set. If a run is both incomplete and unverified, unverified wins: keep the human gate.

9. **One active run.** While auto-resume is in flight, or a human gate is open, the next same-day cron must not start a second run. A human Retry during an in-flight auto-resume must not start a second copy. After a successful auto-resume, the queue row is cleared and cron may fire at its next due time.

10. **Queue id.** Continue stays keyed by a `pending_reruns` id when one exists. On process restart, look up the latest mid-run checkpoint for that `task_id` (and the interrupted `run_id` if known). If no row exists, create one keyed to that task plus checkpoint so continue uses the same path as Retry-from-checkpoint.

11. **`reset_inflight_on_boot` stays the transient 429/5xx claim heal (+30 minutes).** Timeout, max-iterations, and process-restart auto-resume use the startup plus 60-second path and do not inherit that delay. The hourly `due()` scan does not dispatch auto-resume rows.

12. **Stale-running watchdog (ADR-0024 Decision 4).** It still owner-warns and fails the stale row. It is not itself a second auto-resume trigger. After that row is failed, the normal startup / 60-second path may continue from a mid-run checkpoint if one exists and no live worker still holds the run. If no checkpoint exists, stay owner-warn only.

13. **Out of this decision.** Gate `/cancel` (abandon, clear the checkpoint, do not auto-resume), first-run hard `Err`, `FailedTwice`, transient 429/5xx auto-queue, and `/stop` mid-schedule.

## Consequences

- An incomplete scheduled task continues from its checkpoint without a Retry click, including after a process restart once a tool has completed.
- Timeout and process-restart continues still have only the remaining iteration budget. Max-iterations continue still gets a full cap.
- Unverified results, hard failures, and cancelled gates still wait for a person.
- The builtin heartbeat log line and the hourly watchdog keep their existing jobs.
