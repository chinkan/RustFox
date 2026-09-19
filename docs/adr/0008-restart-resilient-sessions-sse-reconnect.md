# ADR 0008: Restart-Resilient Sessions and Frontend Auto-Reconnect

## Status
Accepted

## Date
2026-09-19

## Context
Settings editing follows "restart to apply" (ADR 0006: no in-process hot-reload of `config.toml` — lock-ordering races aren't worth the MVP risk). That makes process restarts a *routine* part of portal usage, and two MVP behaviors break badly:

1. Sessions live in an in-memory map (ADR 0006) → every restart kicks the user back to the token login screen. On a phone this is the worst possible friction (typing a 64-char token via Telegram WebView after every settings change).
2. An SSE chat stream dies silently mid-generation on restart; the SPA shows a frozen partial answer with no explanation.

## Decision

**A. Stateless signed session cookie (survives restart).**
Replace the in-memory session map with a HMAC-signed cookie:
`rustfox_session = base64(username|issued_at|expires_at|gen) . HMAC-SHA256(server_secret)`
- `server_secret`: 32 random bytes generated on first login-config run, persisted next to the state DB (`portal_secret.key`, 0600) — independent of the portal token itself.
- `gen` (generation counter) stored in SQLite `kv` table; `POST /api/auth/logout` and token rotation bump `gen`, globally invalidating all issued cookies. Verification = signature check + expiry + `gen` match. No session table, restart-proof by construction.
- Bearer-token auth (raw token) is unaffected and remains the scripted-client path.

**B. Frontend reconnect protocol.**
- `GET /api/health` (public, no auth) returns `{ "bootId": "<random per process>", "startedAt": ... }` — the *only* unauthenticated API besides login.
- SPA keeps a `bootId` watch (poll every 15 s while the tab is visible, using Page Visibility API to sleep in background): when `bootId` changes, a restart happened → re-`GET /api/auth/me` (stateless cookie transparently re-authenticates), then invalidate all TanStack Query caches so dashboards show fresh state.
- SSE client: on stream close without a `done`/`error` frame → show inline **"連唔住了⋯ 緊連"** banner, retry `GET /api/chat/history` with backoff (1 s → 2 s → 5 s, max 30 s) until the server answers, then reconcile: the finished assistant message is already persisted by `process_message` (it outlives the broken stream), so history fetch recovers the full answer. If generation was cut mid-way by the restart, history shows the last persisted turn and the banner offers a one-tap **重试** of the failed prompt (kept in memory).
- Never auto-replay a `POST /api/chat` (side effects); recovery is always read-then-manual-retry.

**C. Restart UX stays manual, with a helper.** `restart_required: true` in settings PATCH responses renders a sticky banner in the SPA with the exact command (`sudo systemctl restart rustfox`) and a copy button. No self-restart endpoint in MVP — spawning a process-restart from inside itself needs sudoers surgery and a watchdog to be safe; revisit if manual restarts prove annoying.

## Rationale
- Stateless cookies are the standard single-binary trick: no new table, no cleanup job, and expiry lives in the payload.
- `bootId` is deliberately dumber than watching PID: it also fires on container restarts and binary upgrades, and costs one public one-line JSON endpoint.
- Reconciling from history (server truth) instead of trying to resume a stream (impossible — generation task is gone) keeps the recovery model honest: the persisted conversation *is* the source of truth, SSE is best-effort presentation.

## Consequences
- `web/src/api/sse.ts` grows a small state machine (streaming → reconnecting → reconciled); covered by a Vitest test with a mocked fetch that drops the stream.
- Logout semantics change: single-cookie logout still works client-side; server-side global invalidation now has real teeth via `gen` bump (previously a restart covered it).
- `docs/portal-api.md` updates: health bootId, cookie format note, history-reconcile flow diagram.
- A restart during an *active generation* loses the in-flight assistant turn's partial output (final message persisted only when the turn completes) — acceptable; documented in the UI banner copy.
