# ADR-0024: Scheduled-run wall-clock timeout + zombie reconcile

- **Status:** Accepted
- **Date:** 2026-10-12
- **Issue:** #175 (canonical; #174 closed as duplicate)
- **Depends on:** ADR-0013 (human-gate / dead-letter), ADR-0020 slice 1b (TurnGate `begin_scheduled` / `finish`)
- **Related:** #240 / ADR-0039 (timeout transcript continue; Slice B); ADR-0038 / #242 (human-gate copy + inline; Slice A — separate)
- **Amends:** none (scheduler-side analogue of #58 CLI unbound timeout)

## Context

The scheduled job runner is a **single-threaded** `mpsc` consumer. One hung
`process_message_outcome` await blocks every later fire while heartbeats still
tick — silent full-day outage of user automations. `scheduled_task_runs` rows
stuck at `running` are never reconciled after crash/kill. Portal/CLI already
gained bounds via #58; the cron path did not.

Product lock (v1.0.5 **must**, grill 2026-10-12): wall-clock timeout, cancel on
expiry, human-gate once, startup clear of zombie `running` rows, stale-run
watchdog; **no** concurrent runner in this slice. `#166` timezone yields.

## Decision

1. **Wall-clock timeout (config)**  
   - Key: `scheduler.run_timeout_secs` (u64).  
   - **Default: `900` (15 minutes).**  
   - Wrap the scheduled agent call (`run_one_scheduled_job` / equivalent) in
     `tokio::time::timeout`.  
   - **Floor: `60`.** Values `1..=59` are rejected (or clamped to `60` at load —
     prefer reject with a clear config error). `0` is **refused** / treated as
     default (no unbound path — same spirit as fixing #58's "0 = forever").

2. **On timeout**  
   - **Cancel** the in-flight agent (same cancel-token path as user `/stop` for
     that `session_key`), best-effort wait for the cancel to land.  
   - `update_run(..., status="failed", error="run timeout")`.  
   - Enqueue **ADR-0013 human-gate once** via `enqueue_manual` (born
     `awaiting_user`, `attempts=1`) — hang may have side effects; **no** auto
     re-fire. DM / schedule reply uses the existing retry/cancel gate copy,
     stating run timeout.  
   - **Always** call `TurnGate::finish` for the key so waiting scheduled tickets
     and pending user messages (1b) are not stranded; release the job runner
     loop to the next `job_rx` item.

3. **`/stop` cancels an active scheduled turn**  
   - When the active `TurnKind` is `Scheduled` for that session key, `/stop`
     cancels the running scheduled agent (same cancel token) **and** clears the
     user pending queue (existing 1a behaviour).  
   - Waiting scheduled **tickets** behind the active turn remain (1b: clear does
     not drop `waiting_scheduled`). After cancel + `finish`, the next waiting
     ticket (if any) may proceed.

4. **Startup reconcile**  
   - On boot, before accepting new jobs: every `scheduled_task_runs` row with
     `status='running'` → `failed` with reason `interrupted (process restart)`
     (or equivalent). Local single-process: no live cross-process run to
     preserve.

5. **Stale-run watchdog**  
   - Periodic sweep (reuse ADR-0013 system-level interval style; default
     **hourly** is enough, or the same tick as the dead-letter watchdog): any
     `running` row whose age exceeds `run_timeout_secs` → `failed` + owner
     warning. Defence in depth if a future code path forgets `update_run`.

6. **Concurrency (explicit non-goal for this ADR)**  
   - Runner stays single-consumer. Bounded concurrent runners are a later ADR
     if multi-bot starvation matters.

## Rejected alternatives

- **Timeout without cancel:** orphan LLM/tool work keeps burning; TurnGate may
  stay held → rejected.  
- **Auto-retry on timeout:** same side-effect hazard as max-iterations
  (ADR-0013 amendment) → human-gate only.  
- **Startup reconcile only age > N:** after kill -9 every `running` row is a
  zombie on a single-binary host → full clear is safer.  
- **Concurrent runner in v1:** larger blast radius; timeout alone unblocks the
  queue for personal local use.

## Consequences

- ✅ One hung schedule can no longer wedge the day; next jobs run after at most
  ~`run_timeout_secs`.  
- ✅ Zombie `running` rows cleared on restart and by watchdog.  
- ⚠️ Long legitimate jobs (>15m) need a higher `run_timeout_secs` in config.  
- ⚠️ `/stop` during a scheduled turn now cancels that schedule (product change);
  document in `/help` / schedule docs.  
- ⚠️ Timeout still serialises behind TurnGate for that user — correct per ADR-0020
  Q8; other users' jobs still wait on the global single consumer until timeout
  releases the loop.

## Tests

- Unit: timeout path marks run failed, enqueues manual gate, calls `finish`.  
- Unit: `/stop` while `TurnKind::Scheduled` cancels + clear pending; waiting
  scheduled ticket survives clear.  
- Unit: startup reconcile flips all `running` → `failed`.  
- Unit: watchdog flips stale `running` older than timeout.  
- Manual: force a long schedule (or test hook), confirm later crons still fire
  after timeout; confirm human-gate DM; confirm `/stop` mid-schedule.

## Out of scope

- #166 timezone impl  
- #173 verifier sandbox tools  
- Concurrent / sharded job runners  
- Portal UI for timeout config (config.toml / docs enough for v1)

## Cross-reference (#240 / ADR-0039)

Writing a **transcript checkpoint** on this ADR’s timeout →
`enqueue_manual` path, and Retry continuing from that checkpoint when
present, is locked in **ADR-0039** (Proposed; Slice B). This ADR’s
wall-clock timeout, cancel-on-expiry, human-gate-once (no timeout
auto-retry), startup reconcile, and stale-run watchdog locks are
unchanged. Slice A copy/inline is **ADR-0038** (separate).

## Cross-reference (#240 / ADR-0038)

Human-gate **copy** for the timeout DM (and sibling gates) plus inline /
`/retry` `/cancel` UX is locked in **ADR-0038**. This ADR’s wall-clock
timeout, cancel-on-expiry, `enqueue_manual` human-gate-once, startup
reconcile, and stale-run watchdog locks are unchanged. No timeout
auto-retry; Slice B resume out.
