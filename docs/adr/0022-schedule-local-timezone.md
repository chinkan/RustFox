# ADR-0022: Evaluate schedules in a per-task timezone (default: system local, config override)

- **Status:** Accepted (TL lock 2026-10-06; PO locks 2026-10-06, including legacy rows = option C;
  TL lock on legacy one-shot rows 2026-10-06 and PO Product GO on it 2026-10-06, see Decision 3)
- **Date:** 2026-10-06
- **Issue:** #166
- **Branch:** `docs/adr-schedule-timezone` (decision); implementation in a follow-up PR
- **Related:** ADR-0021 / #155 / #167 (5-field pad, temporary UTC hint), #132 (croner 4,
  `parse_scheduler_cron`), #109 / #111 (arm path, `next_run_at`)

## Context

Every recurring schedule is evaluated in **UTC** today:

- `Scheduler::add_cron_job` (`src/scheduler/mod.rs`) calls `Job::new_async`, which in
  tokio-cron-scheduler 0.15 is `new_async_tz(schedule, Utc, ..)`.
- `next_cron_occurrence` / `compute_next_run_at` (`src/scheduler/reminders.rs`) compute in
  `DateTime<Utc>`.

Users mean **local time**. `0 9 * * *` from a Hong Kong user (UTC+8) fires at **17:00 HKT**.
The PO classed this as a P1 product bug (#166). ADR-0021 deliberately left it alone and added
temporary copy that says so:

| Where | Temporary UTC copy (from #167) |
|---|---|
| Portal `cronHint`, `web/src/locales/en.json` | `Times are in UTC for now.` |
| Portal `cronHint`, `web/src/locales/zh-HK.json` | `時間而家係 UTC。` |
| `schedule_task` `trigger_value` description, `src/scheduling_tools.rs` | `Evaluated in UTC.` |

Related facts found while writing this ADR:

1. **croner 4 is timezone-aware.** `Cron::find_next_occurrence` is generic over the
   datetime's zone. With a `DateTime<chrono_tz::Tz>` it walks the **wall clock** of that
   zone, and for fixed-time patterns (single second, minute and hour value, e.g.
   `0 0 9 * * *`) it already does what the PO locked: a time in a DST gap runs at the first
   real instant after the gap; an ambiguous time runs once, at the earlier instant. For
   other patterns (lists, ranges, steps, wildcards in the hour) croner skips gap times and
   runs both ambiguous instants.
2. **tokio-cron-scheduler 0.15.1 cannot follow DST.** `new_async_tz` captures
   `time_offset_seconds` once, at job creation, and the scheduler loop
   (`scheduler.rs`) computes every later tick in that `FixedOffset`. It also parses with
   croner **3**, not the croner 4 we use for `next_run_at`. A job created in winter in a DST
   zone would fire an hour off after the spring change and would never apply the PO DST
   rules. 0.15.1 is the latest release.
3. **One-shot naive datetimes already use process local time.** `parse_one_shot_target`
   (`src/agent.rs`) reads `%Y-%m-%dT%H:%M:%S` with `chrono::Local`; RFC 3339 values carry
   their own offset.
4. **The system prompt gives the model UTC.** `Current date and time: … UTC` (`src/agent.rs`).
   A model told "UTC now" plus "crons are local" will convert wrongly.
5. **"System local" is unreliable in Docker.** A container without `/etc/localtime` and
   without `TZ` reports UTC and reproduces the 09:00 → 17:00 HKT bug silently.
6. `chrono-tz` 0.10 is already in `Cargo.lock` (pulled in by tokio-cron-scheduler); it only
   needs to become a direct dependency.

Entry points that create or change schedules: portal `POST /api/tasks`, portal
`PUT /api/tasks/{id}`, the `schedule_task` agent tool, and config crons
(`[memory] summarize_cron`, `[learning] user_model_cron`, plus the built-in heartbeat) in
`scheduler/tasks.rs::register_builtin_tasks`.

## Decision

### 1. Timezone type and the effective default timezone

- All schedule timezone handling uses **`chrono_tz::Tz`** (IANA names). No hand-rolled
  offsets, no `FixedOffset`, no `chrono::Local` in schedule code. `Tz` gives both the DST
  rules and a name we can show.
- **Effective default timezone**, resolved once at startup, in this order:
  1. **Config override:** `[general] timezone = "Asia/Hong_Kong"` (new, optional). Parsed
     with `chrono_tz::Tz::from_str`. An unknown name **fails startup** and names the key
     (`[general] timezone: unknown IANA timezone 'Hongkong/Asia'`), the same fail-fast rule
     ADR-0021 uses for config crons.
  2. **System local:** `TZ` if it is set to a valid IANA name; otherwise the OS zone via
     `iana-time-zone` (already a chrono dependency), parsed into `Tz`.
  3. **Fallback:** `UTC`, when detection fails or returns a name `chrono-tz` does not know.
     Logged at `warn` (see 2).
- The resolved value is kept as `ScheduleTimezone { tz: Tz, source: Config | Env | System |
  Fallback }` in shared state. Changing the config value takes effect on the next start.

### 2. Boot log and portal display (PO AC)

- At startup log at `info`:
  `Schedule timezone: Asia/Hong_Kong (UTC+08:00, source: config)`.
- When the source is `Fallback`, or the system zone resolves to `UTC` with no config
  override, also log at `warn`:
  `Schedule timezone is UTC. If this is a container, set [general] timezone or TZ (e.g. Asia/Hong_Kong).`
- **The portal Tasks page always shows the effective default timezone** (name and current
  UTC offset), even when there are no tasks. A boot log alone is not enough: non-technical
  users do not read logs. New endpoint:
  `GET /api/schedule/timezone` → `200 {"timezone":"Asia/Hong_Kong","utcOffset":"+08:00","source":"config"}`.

### 3. Per-task `timezone` column (PO lock, option C)

- New column `scheduled_tasks.timezone TEXT` holding an IANA name.
- **Every new row stores an explicit timezone.** Default = the effective default timezone
  (1). Write paths never leave it empty.
- **Boot migration** (same pattern as `migrate_unscoped_schedules`: add the column if
  missing, then backfill only `NULL`s, so it is idempotent):
  - existing **recurring** rows → `timezone = 'UTC'`. Their fire times stay exactly what
    they are today. Nothing fires early or late without the user's action.
  - existing **one-shot** rows → the **system local** zone at migration time: the zone
    `chrono::Local` uses on that host (`TZ`, else the OS zone; 1.2 / 1.3), **ignoring the
    `[general] timezone` override**. By the form of the stored `trigger_value`:
    - **Timezone-naive** wall time (`%Y-%m-%dT%H:%M:%S`, no offset) → system local. Today
      this value is read with `chrono::Local` (Context fact 3), so stamping the same zone
      keeps the fire instant. Stamping `UTC` would be wrong: on a Hong Kong host it would
      delay the fire by 8 hours.
    - **Absolute time** (RFC 3339 with `Z` or an explicit offset, i.e. a UTC instant) →
      system local as well, for display consistency. The instant does not depend on the
      zone, so here `timezone` is **display-only**.
  - **Bottom line (PO): after migration, no task's fire time may change.** This covers
    recurring and one-shot rows alike. The rules above are chosen so that it holds on every
    host: recurring rows keep `UTC` (how they are evaluated today), naive one-shots keep the
    zone `chrono::Local` reads them in today.
  - Detection must use the same source as `chrono::Local`, so the stamped zone and today's
    reading agree. If the system zone cannot be resolved to an IANA name (fallback `UTC`
    while `chrono::Local` is not UTC), the migration logs `warn` naming the affected
    naive one-shot tasks so the user can check them in the portal.
  - Log at `info`: `Assigned timezone UTC to {n} existing recurring task(s); edit a task in the portal to change it.`
    and, for one-shots, `Assigned timezone {system_tz} to {n} existing one-shot task(s) ({w} naive, {a} absolute).`
- **No automatic rewrite of cron fields.** Old UTC rows are fixed by the user, by changing
  the task's timezone in the portal (4) or by recreating it.
- A row whose stored timezone `chrono-tz` cannot parse is handled like an invalid cron
  today: `error!` naming the task and the value, row left unarmed. It is shown in the portal
  so the user can fix it.

### 4. Write paths and API

- **Portal create** `POST /api/tasks`: optional `timezone` (IANA). Missing → effective
  default. Invalid → `400 invalid_timezone`. The 201 body echoes `timezone`.
- **Portal edit** `PUT /api/tasks/{id}`: `timezone` joins the editable fields
  (`name`, `prompt`, `triggerValue`, `timezone`). Changing it on an active task follows the
  existing disarm → update → re-arm path and refreshes `next_run_at`. Invalid →
  `400 invalid_timezone`.
- **List** `GET /api/tasks`: each item gains `"timezone"`. The array shape is unchanged.
- **Portal UI:** the Tasks list shows each task's timezone next to its cron (and next run).
  The create/edit form has a timezone field, prefilled with the effective default on create
  and with the row's value on edit. Rows still on `UTC` while the default is something else
  are visibly marked as UTC so the user can see which ones to fix.
- **`schedule_task` tool:** new optional `timezone` argument (IANA). Missing → effective
  default. The stored row gets the resolved value. Success text:
  `Task scheduled! ID: … — {description} ({normalised cron}, {timezone})`.
- **Config crons** (`summarize_cron`, `user_model_cron`, heartbeat) have no row. They are
  evaluated in the **effective default timezone**. The default `0 0 2 * * *` summarisation
  therefore moves from 02:00 UTC to 02:00 local. These are maintenance jobs; the shift is
  intended and is noted in the changelog.
- **One-shot naive datetimes** are read in **the row's own `timezone`**. After migration
  `parse_one_shot_target` takes the row's `Tz` and must not use a hard-coded
  `chrono::Local`. This matters when `[general] timezone` differs from the system zone
  (e.g. a Docker host on UTC with config `Asia/Hong_Kong`): a new naive one-shot gets
  `Asia/Hong_Kong` and fires at that wall time, while a migrated legacy one-shot keeps the
  system zone it was stamped with and fires at the same instant as before. New rows get the
  effective default (1). One-shot RFC 3339 values keep their own offset; for them the row's
  timezone is only shown.

### 5. Evaluation: one function, croner 4, the row's `Tz`

- Replace `next_cron_occurrence(expr, after)` with
  `next_fire(expr: &str, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>>`:
  parse with `parse_scheduler_cron`, call
  `find_next_occurrence(&after.with_timezone(&tz), false)`, return the instant in UTC.
  `compute_next_run_at` passes the row's `Tz`; config crons pass the default.
- **`next_run_at` and the live job use the same function**, so the #111 / #132 invariant
  ("the value we persist is what the job will do") holds.

### 6. DST rules (PO locks)

Hong Kong has no DST; many users do. For any expression in a DST zone:

- **Skipped local time** (e.g. 02:30 when clocks jump 02:00 → 03:00): the run **fires once,
  immediately after the jump** (at the first real instant after the gap). Reminders must not
  disappear. If several skipped times map to that same instant (e.g. `*/15` across the gap),
  they **coalesce into one** run.
- **Ambiguous local time** (clocks fall back and 01:30 happens twice): the run **fires
  once, at the first occurrence**. The second pass of that wall-clock time does not fire.
- These rules apply to **every** pattern, not only fixed-time ones. croner 4 already gives
  this for fixed-time patterns. For other patterns (lists such as `0 30 2,14 * * *`, ranges,
  steps, hour wildcards) `next_fire` adds a thin wrapper around croner: drop a candidate
  that is the later instant of an ambiguous wall time, and when a gap lies between `after`
  and the candidate, check whether the pattern matched any wall time inside the gap and if
  so return the first instant after the gap. Gap and fold detection use `Tz`
  (`from_local_datetime` → `Single` / `Ambiguous` / `None`), not computed offsets.

### 7. Arming: do not use `Job::new_async_tz`

Because of Context fact 2, recurring jobs are **not** armed with tokio-cron-scheduler's cron
jobs at all. `Scheduler::add_cron_job` keeps its signature (plus a `Tz`) but arms a
**self-rescheduling one-shot**: compute `next_fire(expr, tz, now)`, arm a one-shot at that
instant; when it fires, run the task and arm the next one from `next_fire(expr, tz, fired_at)`.

- The scheduler keeps a **stable handle** per recurring job (the `Uuid` returned to callers
  and stored as `scheduler_job_id`) mapped to the current inner one-shot, so `disarm_task`,
  enable/disable and edit keep working unchanged for callers.
- Long sleeps are fine: each wait is for one absolute instant computed with the correct
  offset, so DST changes between arm and fire are handled.

### 8. Copy changes (implementation PR must do all of them)

- **Remove** the temporary portal hint from #167: `Times are in UTC for now.` (`en.json`)
  and `時間而家係 UTC。` (`zh-HK.json`). Replace with a hint that the time is in the task's
  timezone.
- **Replace** `Evaluated in UTC.` in the `schedule_task` `trigger_value` description with:
  `Evaluated in the task's timezone (argument 'timezone', IANA name; default {default_tz}).`
  `{default_tz}` is filled from the effective default when the tool is registered. The model
  must stop treating crons as UTC.
- `timezone` argument description: `IANA timezone for this task, e.g. 'Asia/Hong_Kong'. Omit
  to use the default ({default_tz}).`
- **System prompt:** `Current date and time: 2026-10-06 11:52:00 HKT (Asia/Hong_Kong,
  UTC+08:00)` in the effective default timezone, so the model's idea of "now" matches how
  crons are read.
- Any skill or prompt text that says crons are UTC gets the same update.

### 9. Docker

Document in `config.example.toml` (next to `[general] location`) and the user guide:
containers often have no `/etc/localtime` and no `TZ`, so the system zone is UTC. Docker
users should set `[general] timezone` and/or the `TZ` environment variable (e.g.
`TZ=Asia/Hong_Kong`). The boot log and the portal Tasks page show which zone is in effect.

## Rejected alternatives

- **(A) Keep existing rows as they are and read them in the new local timezone.** A row a
  user already wrote in UTC (e.g. `0 0 1 * * *` for 09:00 HKT) would silently fire 8 hours
  early. Rejected by PO.
- **(B) One-time boot rewrite of UTC cron fields into local fields.** Cannot be done
  correctly in general: shifting the hour across midnight changes day-of-week and
  day-of-month (`MON` → `TUE`, `L`, ranges, steps), half-hour and 45-minute offsets
  (e.g. India, Nepal) change the minute field, and in a DST zone a fixed UTC time is not a
  fixed local time at all. Rejected by PO.
- **Single global timezone with no per-row column.** Any later change of system zone or
  config would silently move every existing schedule, and the migration problem above
  repeats each time.
- **Hand-rolled offsets (store `+08:00`).** Wrong for DST zones and loses the zone name the
  portal needs to show.
- **`chrono::Local` as the runtime type.** No IANA name to display or store, and it reads
  process state instead of the row.
- **`Job::new_async_tz` with a `Tz`.** Freezes a `FixedOffset` at job creation and parses
  with croner 3 (Context fact 2); breaks DST and the `next_run_at` invariant.
- **Re-create tokio-cron-scheduler jobs at each DST transition.** Needs a transition
  calendar per zone and still does not implement the gap/fold rules.
- **Skip DST-gap runs or fire both ambiguous runs (cron/croner interval behaviour).**
  Contradicts the PO locks (reminders must not disappear; fire once).

## Consequences

- ✅ New schedules fire at the local time the user meant. `0 9 * * *` from Hong Kong fires
  at 09:00 HKT.
- ✅ No existing task's fire time changes on migration (PO bottom line). Recurring rows are
  stored as `UTC`; all legacy one-shot rows get the system local zone (the zone
  `chrono::Local` reads naive values in today; for absolute values it is display-only). The
  portal shows which rows are UTC so the user can fix them deliberately.
- ✅ Naive one-shots are read in the row's own timezone, so a config override (e.g. Docker on
  UTC with `[general] timezone = "Asia/Hong_Kong"`) applies to new one-shots without moving
  legacy ones.
- ✅ Users can see the timezone in use (default and per task) without reading logs.
- ✅ One evaluation function for the live job and `next_run_at`, on croner 4.
- ⚠️ Arming changes from tokio-cron-scheduler cron jobs to chained one-shots. The stable
  handle mapping must be covered by tests (disarm, edit, enable/disable, restart).
- ⚠️ Config crons move from UTC to the effective default timezone (e.g. nightly
  summarisation at 02:00 local). Intended; noted in the changelog.
- ⚠️ In a DST zone, a run in the skipped hour happens right after the jump, and an
  ambiguous-hour run happens only in the first pass. Interval patterns lose their ticks in
  the second pass of a repeated hour.
- ⚠️ In Docker without `TZ` or config, behaviour stays UTC; mitigated by the warn log and
  the portal display.
- ⚠️ New dependency surface: `chrono-tz` becomes direct (already in the lock file);
  `iana-time-zone` is used directly.

## Tests

Unit (`next_fire`, `src/scheduler/reminders.rs` or a new `scheduler/timezone.rs`):

- `0 0 9 * * *` in `Asia/Hong_Kong`, after `2026-10-06T00:00:00Z` → `2026-10-06T01:00:00Z`
  (09:00 HKT), **not** `09:00Z`. Same expression in `UTC` → `09:00Z`.
- `0 0 9 * * MON` in `Asia/Hong_Kong` across a UTC/local day boundary picks the local Monday.
- DST gap, `Europe/London` 2027-03-28 (01:00 → 02:00): `0 30 1 * * *` → fires once at
  `01:00:00Z` (02:00 BST); next day back to 01:30 local. Same for a list
  `0 30 1,13 * * *`. `0 */15 1 * * *` in the gap → one run at the jump instant, not four.
- DST fold, `Europe/London` 2026-10-25 (02:00 → 01:00): `0 30 1 * * *` → fires once at the
  first 01:30 (`00:30:00Z`), not at `01:30:00Z`. Same for `0 30 1,13 * * *` and
  `0 */15 1 * * *` (first pass only).
- `America/New_York` gap and fold cases mirroring the above.
- Effective default resolution: config beats `TZ` beats system; invalid config name → error
  naming `[general] timezone`; undetectable system zone → `UTC` with `source = Fallback`.

Migration (in-memory SQLite):

- Old schema with recurring + one-shot rows → column added; recurring rows `UTC`; one-shot
  rows, naive and absolute → the system local zone (not `UTC`, and not the config
  override); second run changes nothing. Rows written after migration keep their explicit
  value.
- **Fire time unchanged (PO bottom line):** for every migrated row, the fire instant after
  migration equals the one before. Recurring row `0 0 9 * * *` stamped `UTC` → `next_run_at`
  unchanged. One-shot `2099-01-01T09:00:00Z` (absolute) and one-shot `2099-01-01T09:00:00`
  (naive) → same UTC fire instant as the pre-migration `chrono::Local` reading. Run with
  the process zone set to `Asia/Hong_Kong` (e.g. `TZ`): the naive row is stamped
  `Asia/Hong_Kong` and fires at `2099-01-01T01:00:00Z`, not `09:00:00Z` (which is what a
  `UTC` stamp would give, 8 hours late).

`parse_one_shot_target` (row timezone):

- Naive `2099-01-01T09:00:00` with row timezone `Asia/Hong_Kong` → `01:00:00Z`; with `UTC`
  → `09:00:00Z`; result does not depend on the process zone (run with `TZ=UTC` and
  `TZ=Asia/Hong_Kong`).
- Config override ≠ system zone (process `TZ=UTC`, `[general] timezone = "Asia/Hong_Kong"`):
  a new naive one-shot is stored with `Asia/Hong_Kong` and fires at 09:00 HKT; a migrated
  legacy naive one-shot stamped `UTC` (the system zone there) still fires at its old instant.
- RFC 3339 values ignore the row timezone.
- The function has no `chrono::Local` call left (code review / grep).

QA (manual, required before merge of the implementation PR):

- On a host whose system zone is not UTC (e.g. `Asia/Hong_Kong`), create **one legacy
  naive one-shot** task (e.g. via `schedule_task` with `%Y-%m-%dT%H:%M:%S`) on the
  pre-migration build and note its fire time. Upgrade, restart, and confirm the fire time
  (portal next run, and the actual fire) is **unchanged**, and that the assigned
  `timezone` is the system zone (`Asia/Hong_Kong`), **not** `UTC`. Record the stored
  `trigger_value` and the assigned `timezone` in the QA note.
- Same check for one legacy recurring task: assigned `UTC`, next run unchanged.
- After upgrade, with `[general] timezone` set to a zone different from the system zone,
  create a new naive one-shot and confirm it fires at the wall time in the config zone.
- Row with an invalid stored timezone → not armed, error logged, still listed.

Integration (fake scheduler where they exist today):

- Portal `POST /api/tasks` with `0 9 * * *` and no `timezone` → row has the effective
  default; `next_run_at` = next 09:00 in that zone; response echoes `timezone`.
  With `timezone: "America/New_York"` → stored and used. With `timezone: "Mars/Base"` →
  `400 invalid_timezone`.
- Portal `PUT` changing `timezone` on an active task → disarm, update, re-arm; `next_run_at`
  moves accordingly.
- `GET /api/tasks` items include `timezone`; `GET /api/schedule/timezone` returns the
  default, offset and source.
- `schedule_task` with and without `timezone` → stored value correct; success text names it;
  tool schema no longer contains `Evaluated in UTC`.
- Config crons registered with the effective default.
- Chained one-shot arming: fire → next armed; `disarm_task` on the stable handle stops the
  chain; restart restores from rows.
- Web (vitest): Tasks page shows the effective default and per-task timezone; form field
  prefilled; locale files no longer contain `Times are in UTC for now.` / `時間而家係 UTC。`.

## Follow-ups

- Implementation PR for #166 (this ADR). Update `docs/portal-api.md` (`timezone` fields,
  `GET /api/schedule/timezone`, `invalid_timezone`), `config.example.toml`, the user guide
  (Docker note), and the changelog (config crons now local).
- Optional later: a portal action to bulk-change legacy `UTC` rows to the default zone
  (explicit user action only; never automatic).
- Optional later: feed `[general] timezone` into the `location` prompt context.
