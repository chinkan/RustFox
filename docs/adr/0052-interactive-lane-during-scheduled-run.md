# ADR-0052: Interactive lane while a scheduled run is active

- **Status:** Accepted
- **Date:** 2026-10-31
- **Issue:** #338 (regression / incomplete isolation of #275)
- **Amends:** [ADR-0020](./0020-mid-run-message-queue.md) Decision 8 (inbound during a scheduled run); [ADR-0024](./0024-scheduler-run-timeout.md) timeout cancel target
- **Related:** #275, #170, #171, ADR-0020 Q8, ADR-0024 Decision 2–3

## Context

ADR-0020 Q8: a scheduled run waits until the interactive turn on the same `session_key` finishes, and messages sent during the scheduled run are not injected into it. #275 implemented the second half by returning `Submit::Deferred` and holding those messages until `TurnGate::finish` started a user turn. The schedule loop already had no steer source.

That hold is the production bug. While a long scheduled job runs (same bot and user), inbound Telegram cannot start a chat turn. The message sits on the schedule's gate until the job ends. Product lock (2026-10-29, confirmed Must for v1.0.8):

- While a scheduler job runs, inbound Telegram continues on an independent interactive thread.
- Those messages are not steers for the scheduler job.
- Steer applies only when the interactive thread itself is busy.

#275 closed the steer-into-schedule hole by serialising chat behind the job. The lanes were not independent. One `active: Option<TurnKind>` cannot run both, and one cancel token per session key means the second `register_cancel_token` overwrites the first.

## Decision

1. **Two lanes per session key.** `KeyState` tracks `user` and `scheduled` separately. Both may be true. `submit` looks only at the interactive lane:
   - interactive idle → `Submit::Start` (even if `scheduled` is true) and spawn the Telegram user turn;
   - interactive busy → `Submit::Accepted` (steer/queue, 👀) or `Submit::Full`.
   `Submit::Deferred` is removed.

2. **The schedule loop is never the steer target.** It still gets no steer source (`attach_steer` is false when `schedule_id` is set). `TurnGate::drain_for(key, Scheduled)` is always empty. `drain` only returns pending for the interactive lane.

3. **A scheduled run still waits to start** until the interactive lane is idle (ADR-0020 Q8, first half). `begin_scheduled` also waits if the scheduled lane is already held. When the interactive lane ends and a schedule ticket is waiting, that ticket still starts before leftover pending (Decision 5.1). Those leftovers are not injected into the schedule; `finish_scheduled` starts them as an interactive turn if the interactive lane is idle.

4. **Lane finish is explicit.** `finish_user` clears only the interactive lane. If a schedule is already running, it does not take that lane's queue and it does not release a further waiting schedule. `finish_scheduled` clears only the scheduled lane. If the interactive lane is running, it returns `Idle` and leaves pending for that lane.

5. **Cancel tokens are per lane.** Interactive keeps `session_key`. Scheduled uses `{session_key}:sched`. Each `process_message` registers and clears only its lane. Tool cancels (`execute_command`) record the lane scope via a task-local set around the agent loop. A schedule timeout calls `cancel_scheduled_run` (scheduled token + that scope). `/stop` still cancels both tokens and `cancel_all` tools (ADR-0024 Decision 3).

6. **Out of scope.** Supervisor tasks stay on the shared session key, so a schedule timeout can still cancel supervisor work for that key. Conversation rows stay on the owning chat (schedule segments are not moved to a second thread). #330 / #333 are not this change.

## Consequences

- A user can send a normal chat message while their scheduled job is running and get an interactive turn, not a steer into that job and not a silent wait.
- 👀 still means "your interactive turn is busy", not "a schedule is running".
- A schedule timeout no longer cancels the interactive loop token or interactive `execute_command` children. `/stop` still stops both.
- The single job-runner consumer is unchanged. A schedule that has not started yet still waits out an interactive turn, and other users' jobs still wait behind this consumer.

## Tests

- Unit: schedule busy + inbound → `Start`; second message → `Accepted`; `drain_for(Scheduled)` empty; `finish_scheduled` does not take the interactive queue; `finish_user` leaves the scheduled lane up.
- Unit: schedule still waits to start behind an interactive turn; Decision 5.1 leftovers are not a schedule steer.
- Unit: queue cap still applies to steers behind a busy interactive lane while a schedule runs.
- Unit: inbound racing `finish_scheduled` is a `Start`, never dropped.
- Unit: `attach_steer` is false for a `schedule_id`; cancel keys differ by lane; `cancel_scope` does not cancel the other lane.
- Integration: `tests/schedule_interactive_isolation.rs` covers the same isolation with a concurrent submit, and the wait-to-start path.
