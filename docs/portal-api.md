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

## Agents (runtime status)

### GET /api/agents
→ `200 [{"id":"main","name":"RustFox","model":"<current model>","status":"idle|running","lastActive":"<ISO8601|null>","subagents":n}]` — status from `is_processing("web"|"telegram")`; `subagents` = count of loaded agent definitions.

### GET /api/agents/skills
→ `200 [{"name","description","path","instruction":true|false}]` from the live `SkillRegistry`.

### POST /api/agents/reload
→ `200 {"skillsLoaded": n, "agentsLoaded": n}` — calls `Agent::reload_skills_and_agents`. (Writes below auto-reload; this endpoint is for out-of-band edits, e.g. via the agent's own tools or OpenCode.)

## Skills & Agents control plane (ADR 0011 / 0011a)

Full CRUD over the two editable-behavior surfaces: `~/.rustfox/skills/` and `~/.rustfox/agents/`. Shared semantics:

- **Provenance** (every list/detail item): `"bundled"` (shipped in the binary, re-seeded on update — editable, never deletable), `"installed"` (GitHub installer, recorded in `installed-skills.json`), `"user"` (created by hand / agent tools / portal "new"). Classification order: ledger > bundled > user.
- **Optimistic lock (R4)**: `GET detail` returns `hash` (dir hash for dir-form entries, file sha256 for standalone) and per-file `files[].hash`. `PUT .../file` may carry `baseHash`; mismatch/missing-since-read → `409 changed_since_read`. Omit `baseHash` → force-write (CLI parity).
- **Quarantine, never hard-remove (R3)**: `DELETE` moves the entry into `<root>/.trash/<name>-<timestamp>` (structural: the dot-dir is invisible to the loader scan; the loader additionally skips hidden dirs). Bundled → `403 bundled_readonly` with fork advice. The installed-ledger record is removed on quarantine.
- **Writes auto-reload** the skill/agent registry; responses carry `skillsLoaded`/`agentsLoaded` so the UI can show the new count without a second round-trip.
- **Name/content gates**: `invalid_name` (slug rules from `validate_skill_name`), `invalid_path` (escape/symlink-outside), `empty_primary_file`, `frontmatter_name_mismatch` (a `name:` that disagrees with the directory — this is what revives ghost skills).
- **Agent tool whitelist (T4)**: writing an `AGENT.md` whose `tools:` list contains names the running agent doesn't have → `400 unknown_tools` (lists them). Escape hatch: `?allowMissing=1` (query) saves anyway with a `warnings` entry.

### GET /api/skills?kind=skills|agents|all
→ `200 [{"name","kind":"skill|agent","provenance","modified":bool,"deletable":bool}]` — merged listing of both roots (dir-form entries + loader-supported standalone `<name>.md`). `modified` = current hash drifted from the bundled lock-file / installed-ledger baseline.

### GET /api/skills/{name} · GET /api/agents/{name}
→ `200 {"name","kind","provenance","deletable","modified","hash","standalone","fileHash","content","files":[{"path","size","hash"}],"sourceRepo","commitSha","installedAt"}` (last three non-null only for installed entries). Agent details add `"tools":[...]` and `"model"` parsed from frontmatter. 404 `skill not found` / `agent not found`.

### GET /api/skills/{name}/file?path=SKILL.md · GET /api/agents/{name}/file?path=AGENT.md
→ `200 {"path","content","size"}`. Standalone `.md` skills expose only their primary file (anything else → `400 invalid_path`).

### PUT /api/skills/{name}/file · PUT /api/agents/{name}/file
Body `{"path","content","baseHash"?}` + optional `?allowMissing=1`. **Update-only**: the entry must already exist (404 otherwise — creation is a separate intent, R5). Primary-file writes re-run the content gates; `SKILL.md`/`AGENT.md` naming is enforced by `validate_primary_content`.
→ `200 {"ok":true,"name","path","bytes","provenance","skillsLoaded"|"agentsLoaded","warnings":[]}`. `.bak` written before overwrite. Files > 512 KB → `400 file_too_large`.

### POST /api/skills?allowMissing=1 — create skill
Body `{"name","content"}` (content = the full `SKILL.md`). Creates `<skills>/<name>/SKILL.md` via the same gate chain as writes; rolls the directory back if a gate refuses. Existing name → `409 already_exists`.
→ `200 {"ok":true,"name","kind":"skill","path","bytes","provenance":"user","skillsLoaded":n,"agentsLoaded":n,"warnings":[]}`

### POST /api/agents?allowMissing=1 — create subagent (structured, R5)
Body `{"name","content":<instructions body — no frontmatter>,"description"?,"model"?,"tools"?:[...],"maxIterations"?:n,"skipBootstrap"?:bool}`. The server **renders** `AGENT.md` frontmatter from the structured fields (guaranteed-valid YAML; `name` forced to the directory name), then runs the shared gates incl. the tool whitelist.
→ same shape as skill create.

### DELETE /api/skills/{name} · DELETE /api/agents/{name}
→ `200 {"ok":true,"name","kind","quarantinedTo":"<name>-<ts>","skillsLoaded":n,"agentsLoaded":n}` — quarantine, see above. Bundled → 403.

## GitHub skill installer (ADR 0011 B)

### POST /api/skills/install
Body `{"source":"owner/repo[:subpath][@ref]","dryRun":bool,"acknowledgedWarnings"?:[...],"force"?:bool}`. Browser-pasted `https://github.com/...` prefixes are accepted and stripped. Native Rust fetcher (GitHub contents API, recursive tree), no `npx`.

Caps: 10 skills/request, 20 files/skill, 512 KB/file, 2 MB/skill, dir depth 3. Skipped dirs: `.git`/`node_modules`/`__pycache__` etc. **Hard refusals**: secret-shaped content (`GOCSPX-`, `sk-`, `AKIA`, PEM blocks, `1//…` refresh tokens…), executable-extension files (`.sh`/`.exe`/…), binary content — the skill is knocked out of the plan entirely. **Warnings** (soft): bundled-name collision, file/size caps.

Two-step flow (R2 — reject *before* it scores):
1. `dryRun:true` → `200 {"dryRun":true,"source":{"owner","repo","ref":sha},"skills":[{"name","description","files":[{"path","size"}],"sizeBytes"}],"verdict":{"refused":[{"skill","file","rule","detail"}],"warnings":[{"skill","file","rule","detail"}],"notes":[]}}`
2. Real install (`dryRun:false`) must echo every warning's exact key `[rule] skill: file — detail` in `acknowledgedWarnings`; any unacknowledged warning → `409 warnings_unacknowledged` and **nothing is written**.

Existing same-name entry → skipped with reason (unless `force:true`, which quarantines first). Successful installs land in `~/.rustfox/installed-skills.json` with `sourceRepo`, commit sha, git ref, `installedAt`, and install-time content hash (drift detection). GitHub rate limit → `400 github_rate_limited` with honest wait advice (no silent token use — `[github]` token support deferred with the OAuth-storage decision).
→ `200 {"dryRun":false,"installed":["<skill-name>",…],"skipped":[{"name","reason"}],"verdict":{...},"reload":{"skillsLoaded":n,"agentsLoaded":n}}`

### GET /api/skills/installed
→ `200 {"skills":{"<name>":{"sourceRepo","commitSha","ref","installedAt","contentHash"}},"agents":{...}}` — the provenance ledger, straight off `installed-skills.json`.

## Memory

### GET /api/memory/search?q=<text>&kind=<fact|knowledge|conversation>&limit=50
- `kind=fact|knowledge`: `MemoryStore::search_knowledge` (knowledge entries; `fact` and `knowledge` currently alias — reserved for the fact store).
- `kind=conversation`: `MemoryStore::search_messages`.
- `kind` omitted: both, interleaved, score-ordered.
→ `200 [{"id","kind","text","score","createdAt"}]`. `q` empty → most recent entries.

## Tasks (from `ScheduledTaskStore`) — full CRUD (ADR 0011a R6)

Write paths go through `AgentOps::arm_task`/`disarm_task` against the **live** scheduler (the old `restartToSchedule` fib is gone). Create/update/enable share ONE arming path with restore + the Telegram tool (`build_fire_closure`). `triggerType` is immutable (delete + recreate); in-flight runs are untouched by edits/disarm (already-spawned jobs naturally survive — the UI doesn't pretend otherwise).

### GET /api/tasks
→ `200 [{"id","name","cron","enabled","nextRun","prompt","triggerType","triggerValue","status","platform"}]` — active + paused (completed/cancelled one-shots hidden; soft-deleted always hidden). `name`=description (or first 40 chars of prompt), `cron`=trigger_value when recurring (`"once"` for one-shot), `enabled`=`status=="active"`. Full editable state included so the edit form needs no second fetch.

### POST /api/tasks — create + arm immediately
Body `{"name"?,"prompt","triggerType":"recurring|one_shot","triggerValue":"<6-field cron | ISO-8601 local>"}`.
Validation: non-empty prompt; recurring values run through the **exact** croner config the tokio-cron-scheduler uses internally (`invalid_cron` otherwise — portal gate == scheduler reality, no "accepted here, silently dead there"); one-shot parses to a future datetime (`invalid_trigger`, and `bad_request` if in the past).
→ `201 {"id","name","schedulerJobId","nextRun"}`. If arming fails the DB row is **rolled back** (`arm_failed`) — a task that exists in the DB but never fires is the worst outcome.

### PUT /api/tasks/{id} — edit + re-arm (partial)
Body any subset of `{"name"?,"prompt"?,"triggerValue"?,"triggerType"?}`; `triggerType` present-but-different → `400 trigger_type_immutable`. Active tasks: disarm → update → re-arm. Paused tasks: DB-only edit (stays disarmed). Re-arm failure surfaces as `arm_failed` (row saved but honestly flagged).
→ `200 {"id","updated":{...},"rearmed":bool,"schedulerJobId":null|"<id>","nextRun"}`

### DELETE /api/tasks/{id} — soft delete, history preserved
Disarms the live job and stamps `deleted_at`; the row and **all** `scheduled_task_runs` history survive (`{"ok":true,"id","softDeleted":true,"historyPreserved":true}`) — run history is the evidence base for replay/noise-floor work (RRSI), never burned with the definition. Hard purge is not exposed.

### GET /api/tasks/{id}/runs?limit=20
→ `200 [{"id","runAt","status":"running|completed|failed","error"?:null,"response"?:null}]` newest first (truncated to 400 chars).

### POST /api/tasks/{id}/enable
Re-arms against the live scheduler → `200 {"ok":true,"id","enabled":true,"schedulerJobId","nextRun"}`. One-shot whose time already passed → `400 trigger_passed` (edit the time first); status change rolls back on `arm_failed`.

### POST /api/tasks/{id}/disable
Disarm live job + `status → paused` → `200 {"ok":true,"id","enabled":false,"jobRemoved":bool}`. Telegram-created tasks are listed and toggle-able; they run wherever they were created.

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
