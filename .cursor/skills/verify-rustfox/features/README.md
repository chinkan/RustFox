# RustFox portal verification map

This directory is the maintained source for verifying user-facing behavior of the **RustFox embedded web portal** (primary agent drive surface). Telegram and CLI exist but are out of scope for these recipes unless a feature file says otherwise.

## Baseline preconditions

- Launch via `.cursor/skills/verify-rustfox/helpers/launch.sh` (or manual `portal_preview` on `127.0.0.1:8123`).
- Token defaults to `preview-token` (`PORTAL_PREVIEW_TOKEN`).
- Run `.cursor/skills/verify-rustfox/helpers/doctor.sh` and require HEALTHY before driving.
- Never drive `/home/kan/.rustfox`, live Telegram, or any instance you did not start.
- Evidence goes under `/tmp/verify-rustfox-<run>/` (or `VERIFY_EVIDENCE_DIR`); cleanup must not remove it.
- Seeded preview fixtures (from `examples/portal_preview.rs`): knowledge facts (`The Death of Portia`, RustFox project blurb), a web conversation mentioning Rothfuss / Portia, tasks `Daily weather briefing` + `Inbox sweep` with run history, agents/skills `preview-agent` / `preview-skill`.

## Driving conventions

- Start every recipe from the baseline state unless its preconditions say otherwise.
- Prefer routes (`/chat`, `/settings`, …), ARIA roles/names, and `/api/*` paths from `docs/portal-api.md` over brittle CSS.
- Treat every command as literal. Keep quoted tokens and flags unchanged.
- Browser actions: Playwright via root `e2e-verify.mjs` or a one-off script resolving `playwright` from `web/node_modules`.
- HTTP actions: `curl` with cookie jar from `POST /api/auth/login` or `Authorization: Bearer <token>`.
- Restore mutable preview state only when a journey mutates shared fixtures and a later step depends on them; never delete proof artifacts in cleanup.

## Proof and skip reporting

- Capture the user action and the resulting state (e.g. SSE frames **and** history re-fetch).
- UI proof: screenshot under the evidence feature dir + body text / ARIA where useful.
- API proof: request, response body, and HTTP status on disk.
- Mutation proof: a second read (history reload, list refresh) of the stored value.
- Record `feature_id` and entry point in `PROOF.txt`.
- Report an unreachable path with the attempted command and unmet precondition — do not claim a different entry verified it.

## Feature entry contract

Each feature file starts with an H1 and one paragraph of user-visible behavior, then exactly four H2s in order:

1. `Sub-features`
2. `How to get to it (user POV)`
3. `Driving it with portal_preview harness`
4. `Gotchas`

## Features

- [Portal home & chat](./portal-home-chat.md) — login, dashboard, chat SSE + persistence.
- [Settings](./settings.md) — deep-link `/settings`, auth'd settings surface.
- [Memory](./memory.md) — browse/search knowledge and conversations.
- [Tasks](./tasks.md) — schedule list, runs panel, enable/disable.
- [Agents](./agents.md) — main + subagent rows, skills, reload.
