# ADR-0021: Accept 5-field cron by padding seconds (store 6-field only)

- **Status:** Accepted (TL lock 2026-10-06; PO may amend)
- **Date:** 2026-10-06
- **Issue:** #155
- **Related:** #132 (croner 4 migration, `parse_scheduler_cron`), #109 / #111 (arm path,
  `next_run_at`)

## Context

`validate_cron_expr` (`src/agent.rs`) rejects anything that is not exactly 6 fields, and
`parse_scheduler_cron` uses `croner` with `Seconds::Required` (the same settings
tokio-cron-scheduler uses internally). The classic crontab form `min hour dom month dow`,
which users and LLMs write most often, is a hard error:

```
POST /api/tasks  triggerValue = "0 9 * * *"
→ 400 invalid_cron: "Cron expression must have 6 fields (sec min hour day month weekday), got 5: '0 9 * * *'"
```

A 5-field row already in the DB (hand-inserted or from an old version) fails at boot in
`restore_scheduled_tasks` with `Failed to create cron job` and is never armed.

Write entry points that reach `validate_cron_expr` today:

| Entry point | Code |
|---|---|
| Portal create | `POST /api/tasks` → `portal/tasks_admin.rs::task_create` |
| Portal edit | `PUT /api/tasks/{id}` (partial update; #155 calls it PATCH) → `tasks_admin.rs` (recurring + `triggerValue`) |
| Agent tool | `schedule_task` → `scheduling_tools.rs` (Telegram chat creates schedules only through this tool) |
| Config | `[memory] summarize_cron`, `[learning] user_model_cron` → `scheduler/tasks.rs::register_builtin_tasks` → `add_cron_job` (not validated; a bad value fails startup via `?`) |

Plus the portal UI's client-side gate: `CRON_6_FIELD` in `web/src/api/types.ts`, used by
`triggerLooksValid` in `web/src/routes/_auth/tasks.tsx`. Hint strings are in
`web/src/locales/{en,zh-HK}.json` (`cronExpr`, `cronHint`, `triggerInvalid`).

**Timezone (restated, not changed):** every cron is evaluated in **UTC**. `Scheduler::add_cron_job`
calls `Job::new_async`, which in tokio-cron-scheduler 0.15 is `new_async_tz(schedule, Utc, ..)`.
`next_cron_occurrence` also computes in `DateTime<Utc>`. So `0 0 9 * * *` fires at 09:00 UTC
(17:00 HKT). This ADR does not touch timezone behaviour.

## Decision

1. **One normaliser: `normalize_cron_expr(expr: &str) -> anyhow::Result<String>`** in
   `src/agent.rs`, next to `parse_scheduler_cron`. It **replaces** `validate_cron_expr` at
   every write path.
   - Split on ASCII whitespace.
   - **5 fields** → return `"0 " + fields.join(" ")` (seconds = `0`).
     `0 9 * * *` → `0 0 9 * * *`; `*/15 * * * 1-5` → `0 */15 * * * 1-5`.
   - **6 fields** → return the input **trimmed and otherwise byte-for-byte unchanged** (no
     whitespace collapsing, so existing rows and API echoes stay identical).
   - Anything else → `Err`: `Cron expression must have 5 fields (min hour day month weekday)
     or 6 fields (sec min hour day month weekday), got {n}: '{expr}'`.
   - Values starting with `@` (`@daily`, `@hourly`, …) → `Err`: `Cron macros like '@daily'
     are not supported; use 5 or 6 fields, e.g. '0 9 * * *'`.
   - The result is then checked with `parse_scheduler_cron`. A parse error →
     `Invalid cron expression '{original}' (normalised to '{normalised}'): {e}`.
   - Keep `validate_cron_expr` as a thin `normalize_cron_expr(..).map(|_| ())` wrapper only if
     some caller still needs it. Callers that **store** must use the returned string.

2. **Write paths store the normalised 6-field string only.**
   - `task_create`: normalise `body.trigger_value` and put the result in
     `ScheduledTask.trigger_value`. Error → `400 invalid_cron` (code unchanged).
   - `PUT /api/tasks/{id}`: when `triggerValue` is present on a recurring task, normalise it
     and pass the **normalised** value to `update_task_fields` and to the re-arm.
   - `schedule_task` tool: normalise and store the result. The success text shows the stored
     form: `Task scheduled! ID: … — {description} ({normalised})`.
   - Config crons: run `summarize_cron` / `user_model_cron` through `normalize_cron_expr`
     before `add_cron_job` in `register_builtin_tasks`. A 5-field config value now works.
     An invalid value keeps today's fail-fast startup, but the error names the config key
     and the 5-or-6-field rule. Config files are not rewritten.
   - Nothing else writes `scheduled_tasks.trigger_value` for recurring rows.

3. **Restore repairs 5-field rows.** In `restore_scheduled_tasks`, for each
   `trigger_type == "recurring"` row, run `normalize_cron_expr` before `arm_task`:
   - changed (5 → 6) → `update_task_fields(id, None, Some(&normalised), None, None)`, then arm
     using the normalised value. Log at `info`: `Normalised 5-field cron for task {id}: '{old}'
     → '{new}'`. The next boot sees a clean row.
   - invalid (4/7 fields, macro, parse error) → keep today's `error!` log and leave the row
     unarmed. The message now names the real cause instead of `Failed to create cron job`.
   - The DB write is best-effort. If it fails, still arm with the normalised value and `warn!`.

4. **Still rejected (out of scope):** 4-field, 7-field (year), and `@`-macros. All return
   `invalid_cron` with the messages above.

5. **LLM-facing text says 5 or 6 fields.**
   - `schedule_task` `trigger_value` description: `ISO 8601 (one_shot) or cron (recurring): 5
     fields 'min hour day month weekday' or 6 fields 'sec min hour day month weekday'.
     Evaluated in UTC. Stored as 6 fields (seconds added as 0).`
   - Any system prompt or skill text that tells the model to emit 6 fields gets the same
     update. Today only the tool schema mentions it.

6. **Portal UI.** `CRON_6_FIELD` → `CRON_5_OR_6_FIELD = /^(\S+\s+){4,5}\S+$/`. Rename
   `triggerLooksValid` to match. Update the `cronExpr` / `cronHint` / `triggerInvalid`
   strings in `en.json` and `zh-HK.json` to say 5 or 6 fields, with a 5-field example. The
   server parser stays the real gate.

7. **API echoes the stored form.** `GET /api/tasks` already returns the stored
   `triggerValue`. `PUT /api/tasks/{id}` returns `updated.triggerValue`, which must be the
   normalised value. `POST /api/tasks` adds `triggerValue` (normalised) to its 201 body. So
   the user sees `0 0 9 * * *` after entering `0 9 * * *`.

## Rejected alternatives

- **Store what the user typed and normalise only at arm time.** Two dialects in the DB;
  `next_cron_occurrence`, restore and every reader would need to normalise. Store one
  canonical form instead.
- **Switch croner to `Seconds::Optional`.** tokio-cron-scheduler parses with
  `Seconds::Required` internally, so the job would still reject 5 fields. Padding before both
  parsers keeps them in agreement (the #132 invariant).
- **Accept `@daily` / 7-field now.** No demand in #155. Each adds a dialect question
  (`@reboot`, year ranges). Out of scope until asked.
- **Leave 5-field rows failing on restore.** Silent unarmed schedules are worse than a
  one-line rewrite.

## Consequences

- ✅ `0 9 * * *` works from the portal, the tool and config. Rows are always 6-field, so the
  scheduler, `next_run_at` and restore logic do not change.
- ✅ Existing 5-field rows heal on the next boot.
- ⚠️ A 5-field cron always fires at second 0. That is crontab semantics, so it is expected.
- ⚠️ Times are UTC. A user who means 09:00 Hong Kong time must write `0 1 * * *`. This is
  existing behaviour, now stated in the tool description (see open question).

## Tests

Unit (`src/agent.rs`):

- `normalize_cron_expr("0 9 * * *") == "0 0 9 * * *"`;
  `"*/15 * * * 1-5" → "0 */15 * * * 1-5"`; `"  0 9 * * *  "` (padded) → `"0 0 9 * * *"`.
- 6-field passthrough: `"0 0 9 * * *"`, `"0 1/10 * * * *"` (sloppy step), `"0 0 9 * * MON"`
  unchanged.
- Reject: 4 fields (`"9 * * *"`), 7 fields (`"0 0 9 * * * 2027"`), `"@daily"`, empty string,
  5 fields with garbage (`"x 9 * * *"`). Messages name the field count or macro.
- Update `next_cron_occurrence_pins_6_field_and_rejects_5_field` (`scheduler/reminders.rs`).
  It stays true for the raw parser. Add a case where `normalize_cron_expr` + parse gives the
  next 09:00:00 UTC.

Integration (in-memory SQLite, fake scheduler where they exist today):

- Portal `POST /api/tasks` with `0 9 * * *` → 201, row `trigger_value == "0 0 9 * * *"`,
  response echoes it, `next_run_at` = next 09:00 UTC.
- Portal `PUT` with a 5-field `triggerValue` → stored normalised. 4-field → `400
  invalid_cron`.
- `schedule_task` tool with a 5-field value → stored normalised; success text shows the
  6-field form.
- Restore: seed a 5-field active recurring row → after `restore_scheduled_tasks` the row is
  rewritten to 6 fields and armed. A 6-field row is untouched (no write). A 7-field row is
  logged and not armed.
- Web: `triggerLooksValid` accepts 5 and 6 fields and rejects 4 and 7 (vitest).

## Open question for PO

- **"Fires at 09:00" in the #155 acceptance criteria means 09:00 UTC** under current
  behaviour. If the intent is local time (HKT), that is a separate timezone ADR, not this one.
