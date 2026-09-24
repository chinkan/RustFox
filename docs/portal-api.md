# RustFox Portal API Contract (MVP)

Base URL: `http://127.0.0.1:<portal.port>`. All `/api/*` routes except `/api/auth/login` require auth:

- **Cookie**: `rustfox_session=<payload>.<hmac>` (`HttpOnly`, `SameSite=Strict`) obtained from `/api/auth/login`. **Stateless HMAC-signed cookie (ADR 0008A)** — survives restarts; server-side invalidation via a generation counter (`gen`) stored in the SQLite `kv` table, bumped on global-logout/token rotation. Expiry in payload (`expires_at`, default 30 days).
- **Bearer**: `Authorization: Bearer <portal token>` (raw token, hashed-compare against `token_sha256`) for scripted clients.

Errors: `{"error": {"code": "<snake_case>", "message": "<human>"}}` with status `401` (bad/missing auth), `400` (validation), `403` (role), `404`, `409` (busy).

Field naming on the wire is **camelCase** (matches `web/src/api/types.ts`), snake_case internally.

## Auth

### POST /api/auth/login
Request `{"token": "<portal token>"}` → `200 {"username": "<portal.user_name>", "role": "admin"}` + `Set-Cookie`. Wrong token → `401 invalid_credentials`. First login with no `portal_secret.key` present generates + persists it next to the state DB (0600).

### POST /api/auth/logout → 204, clears cookie. Optional body `{"everywhere": true}` bumps the `gen` counter, invalidating all issued cookies (ADR 0008A).

### GET /api/auth/me
→ `200 {"authenticated": true, "username": "kan", "role": "admin"}` (or `{"authenticated": false}` when cookie missing/invalid — used by the SPA guard).

## Chat (ADR 0005)

### GET /api/chat/history
Active web conversation (`platform="web"`, `user_id=portal.user_name`).
→ `200 {"conversationId": "<uuid>", "messages": [{"id","role":"user|assistant","content","createdAt"}]}`

### GET /api/chat/threads
Conversations for the web user, newest first.
→ `200 [{"id","title","messageCount","updatedAt"}]` — `title` = first user message, truncated to 60 chars.

### POST /api/chat
Request `{"text": "...", "attachments"?: [{"name","path"}]}` (attachments = files already placed in the sandbox dir; MVP: none).
Response `200 text/event-stream`:

| event | data |
|---|---|
| `token` | `{"delta": "<incremental assistant text>"}` |
| `tool` | `{"name": "<tool>", "status": "started|completed", "success"?: true}` |
| `done` | `{"messageId","content","conversationId"}` — final full answer (also persisted in history) |
| `error` | `{"message"}` — generation failed mid-stream |

One generation per portal user at a time; concurrent `POST` → `409 chat_in_progress`. Server-side: builds `IncomingMessage{platform:"web", user_id:<user_name>, chat_id:<user_name>, text}`, calls `process_message` with `tool_event_tx`/`stream_token_tx` piped into the SSE writer, `ToolUiMode::Verbose`.

### POST /api/chat/cancel
→ `200 {"cancelled": true|false}` (false when nothing is running). Uses `register_cancel_token`/`cancel_processing` on the web identity.

## Agents (read-only)

### GET /api/agents
→ `200 [{"id":"main","name":"RustFox","model":"<current model>","status":"idle|running","lastActive":"<ISO8601|null>","subagents":n}]` — status from `is_processing("web"|"telegram")`; `subagents` = count of loaded agent definitions.

### GET /api/agents/skills
→ `200 [{"name","description","path","instruction":true|false}]` from the live `SkillRegistry`.

### POST /api/agents/reload
→ `200 {"skillsLoaded": n, "agentsLoaded": n}` — calls `Agent::reload_skills_and_agents`.

## Memory

### GET /api/memory/search?q=<text>&kind=<fact|knowledge|conversation>&limit=50
- `kind=fact|knowledge`: `MemoryStore::search_knowledge` (knowledge entries; `fact` and `knowledge` currently alias — reserved for the fact store).
- `kind=conversation`: `MemoryStore::search_messages`.
- `kind` omitted: both, interleaved, score-ordered.
→ `200 [{"id","kind","text","score","createdAt"}]`. `q` empty → most recent entries.

## Tasks (from `ScheduledTaskStore`)

### GET /api/tasks
→ `200 [{"id","name","cron","enabled","nextRun"}]` — `name`=description, `cron`=trigger_value when `trigger_type="recurring"` (one-shot tasks show `"once"`), `enabled`= `status=="active"`.

### GET /api/tasks/{id}/runs?limit=20
→ `200 [{"id","runAt","status":"running|completed|failed","error"?:null,"response"?:null}]` newest first (truncated to 400 chars).

### POST /api/tasks/{id}/enable · POST /api/tasks/{id}/disable
→ `200 {"ok": true}` — `set_status("active"|"paused")` + scheduler pause/resume. Deleting/pausing non-web-owned tasks (Telegram-created) is allowed; they run wherever they were created.

## Dashboard

### GET /api/health
**Public (no auth)** — the SPA's restart detector (ADR 0008B).
→ `200 {"cpuPercent","memUsedGb","memTotalGb","diskUsedPercent","uptimeHours","bootId":"<random 16-hex per process>"}` — read from `/proc` (Linux); non-Linux returns zeros with `uptimeHours` from process start. SPA polls every 15 s while visible; `bootId` change ⇒ restart happened ⇒ re-`GET /api/auth/me` (cookie re-authenticates transparently) + invalidate all query caches; if an SSE stream had died, reconcile via `GET /api/chat/history` (never auto-replay `POST /api/chat`).

### GET /api/stats
→ `200 {"model","providers":["name",…],"messageCount":n,"conversationCount":n,"activeTasks":n,"skills":n,"portalEnabled":true}`

## Settings (ADR 0006 — whitelisted projection, never raw TOML)

### GET /api/settings
→ `200 {"editable": {"model","generalLocation","defaultAutonomyMode","portalPort"}, "masked": {"openrouterApiKey":"sk-or-…abcd","telegramBotToken":"123…456","providers":[{"name","model","baseUrl","apiKeyMasked":true}], "embeddingApiKey"?}, "restartRequired": ["portalPort"]}`

### PATCH /api/settings
Body may contain any subset of `{"model","generalLocation","defaultAutonomyMode","portalPort"}`.
Validation: `model` non-empty → `Agent::set_model` (live, no restart); `portalPort` 1–65535; `defaultAutonomyMode` ∈ {`default`,`autopilot`,`plan`}.
Before write: copy current file to `config.toml.bak`. Response:
`200 {"updated":["model","portalPort"], "restartRequired":["portalPort"], "applied":{"model":"<new>"}}`

### GET /api/soul · PUT /api/soul
Markdown soul files, whitelisted names only (`SOUL.md`, `USER.md`, `AGENTS.md`).
GET → `200 {"name","content","mtime"}`; PUT body `{"name","content"}` → backup `*.bak` then write; fires the `soul_updated` notification so the agent re-reads identity files.

## Static hosting
- `GET /` → SPA `index.html` (embedded); unknown non-`/api` paths → same `index.html` (client-side routing); `/assets/*` hashed files with long cache.
- When built without `web/dist` present, only `/api/*` is served and startup logs `WARN portal: no embedded frontend`.
