# Tasks

Tasks (schedule) lists seeded recurring and one-shot jobs, opens a runs history panel, and can disable/enable a row through the live arm/disarm API path (preview AgentOps stubs succeed).

## Sub-features

- `tasks-list` renders `Daily weather briefing`, cron `0 2 * * 0`, and `Inbox sweep`.
- `tasks-once-chip` shows a `once` chip for one-shot triggers.
- `tasks-runs` opens Runs and lists `completed` + `failed` (timeout after 60s).
- `tasks-disable-enable` flips a row to `paused` then back to `enabled` with `Re-armed live`.

## How to get to it (user POV)

- After login, open Tasks in the nav.
- Deep-link `/tasks`.

## Driving it with portal_preview harness

Preconditions:

- Doctor HEALTHY; preview task fixtures present.
- Authenticated session.

- **List.** `page.goto(BASE + '/tasks')` (`e2e-verify.mjs` §7). Body includes weather briefing, cron, Inbox sweep, and `once`.
- **Runs panel.** Click role button `Runs` (exact) on the first row. Body includes `completed`, `failed`, and `timeout after 60s`.
- **Disable.** Click `Disable` on the first enabled row. Badge shows `paused`; row count unchanged; `Enable` appears.
- **Enable.** Click `Enable`. Body includes `Re-armed live` and `enabled` (not the restart-only notice).
- **API alternate.** `GET /api/tasks`, `GET /api/tasks/{id}/runs`, `POST /api/tasks/{id}/disable|enable` with auth.
- **Proof.** Screenshot `pw-06-tasks.png` under `$EVIDENCE/tasks/` plus optional API JSON.

## Gotchas

- Toggle waits ~1.2s for mutation + refetch in the e2e script — assert after that, not immediately.
- Preview `arm_task` / `disarm_task` always succeed; a full fox-home can return `arm_failed` / `trigger_passed`.
- Soft-deleted tasks stay hidden; do not expect them in `GET /api/tasks`.
