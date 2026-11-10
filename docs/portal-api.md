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
No query: the active web conversation (`platform="web"`, `bot_id="default"`, `user_id=portal.user_name`), get-or-created.
→ `200 {"conversationId": "<uuid>", "messages": [{"id","role":"user|assistant","content","createdAt"}]}`

`?conversationId=<uuid>` loads that web conversation for the same portal user, including an archived thread. It does not get-or-create and does not unarchive. A wrong id, another platform, or another user → `404 {"error":{"code":"conversation_not_found","message":"Conversation not found"}}`. The response shape is the same.

### GET /api/chat/threads
Web conversations for the portal user only (never Telegram). The single non-archived row is first; archived rows follow by stored `conversations.updated_at` descending. A first visit still get-or-creates the active row.
→ `200 [{"id","title","messageCount","updatedAt","archived":false}]` — `title` = first user message, truncated to 60 chars, or the English fallback `"New chat"`. `updatedAt` is that stored timestamp (RFC3339). `archived` is false on the active row.

### POST /api/chat/threads
Empty body or `{}`. No client id or title. If a generation is in progress → `409 chat_in_progress` and nothing is archived. Otherwise the active web conversation is resolved. Zero visible user/assistant messages → `200` of that same row with `"created": false` and `"previousId": null` (no empty archived row). Otherwise that web conversation is archived by id, a fresh active UUID is created, and mid-run mode for `(default, portal.user_name)` is reset. Messages are not deleted. Telegram `/clear` is not involved.
→ `200 {"id","title","messageCount","updatedAt","created":true|false,"previousId":"<uuid>"|null}` — an empty new thread uses title `"New chat"` and `messageCount` 0.

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
→ `200 [{"id":"main"|"agent:<name>","name","model","status":"idle|running|error","lastActive":"<ISO8601|null>","platform":"telegram+web"|"subagent","live_signal":bool,"definition_only":bool}]` — ADR-0050 honesty: main `running` iff web/Telegram `is_processing`; a definition seat `running` iff an in-flight `invoke_agent` / tagged `spawn_agents` name is held in the seat←run flag. `live_signal=false` ⇒ `definition_only=true` and status stays `idle` (never invent busy). Pixel office UI is separate.

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
→ `200 [{"id","kind","text","score","createdAt","evidence","evidenceText"}]`. Knowledge hits include `evidence` (JSON object or array: `{kind, ref, captured_at}`) and `evidenceText`. Unknown or missing evidence sets `evidenceText` to `⚠️ 無來源`. Conversation hits omit those two fields. `q` empty → most recent entries.

## Tasks (from `ScheduledTaskStore`) — full CRUD (ADR 0011a R6)

Write paths go through `AgentOps::arm_task`/`disarm_task` against the **live** scheduler (the old `restartToSchedule` fib is gone). Create/update/enable share ONE arming path with restore + the Telegram tool (`build_fire_closure`). `triggerType` is immutable (delete + recreate); in-flight runs are untouched by edits/disarm (already-spawned jobs naturally survive — the UI doesn't pretend otherwise).

### GET /api/schedule/timezone
→ `200 {"timezone":"Asia/Hong_Kong","utcOffset":"+08:00","source":"config|env|system|fallback"}` — effective default schedule timezone (ADR-0022).

### GET /api/tasks
→ `200 [{"id","name","cron","enabled","nextRun","prompt","triggerType","triggerValue","status","platform","timezone"}]` — active + paused (completed/cancelled one-shots hidden; soft-deleted always hidden). `name`=description (or first 40 chars of prompt), `cron`=trigger_value when recurring (`"once"` for one-shot), `enabled`=`status=="active"`, `timezone`=IANA name for the row. Full editable state included so the edit form needs no second fetch.

### POST /api/tasks — create + arm immediately
Body `{"name"?,"prompt","triggerType":"recurring|one_shot","triggerValue":"<6-field cron | ISO-8601 local>","timezone"?}`. Optional `timezone` (IANA); missing → effective default. Invalid → `400 invalid_timezone`.
Validation: non-empty prompt; recurring values run through the **exact** croner config the live evaluator uses (`invalid_cron` otherwise); one-shot parses to a future datetime in the row timezone (`invalid_trigger`, and `bad_request` if in the past).
→ `201 {"id","name","triggerValue","timezone","schedulerJobId","nextRun"}`. If arming fails the DB row is **rolled back** (`arm_failed`) — a task that exists in the DB but never fires is the worst outcome.

### PUT /api/tasks/{id} — edit + re-arm (partial)
Body any subset of `{"name"?,"prompt"?,"triggerValue"?,"triggerType"?,"timezone"?}`; `triggerType` present-but-different → `400 trigger_type_immutable`. Invalid timezone → `400 invalid_timezone`. Active tasks: disarm → update → re-arm. Paused tasks: DB-only edit (stays disarmed). Re-arm failure surfaces as `arm_failed` (row saved but honestly flagged).
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

### GET /api/opencode/status (ADR-0043 Phase 1)
Session-gated on portal (wizard exposes the same path publicly as `/api/opencode/status`). Snake_case JSON:
`{"state":"absent"|"no_creds"|"ready","model_count":n,"models":[…],"message":null|string}`.
`absent` = binary missing; `no_creds` = present but no usable credentials; `ready` = creds OK + `opencode models --pure` list. Always includes `model_count` (0 when not ready) and `models` (empty when not ready). Detect-only — not Active / not chat.

## Settings (ADR 0006 — whitelisted projection, never raw TOML)

### GET /api/settings
→ `200 {"editable": {"model","generalLocation","defaultAutonomyMode","portalPort","voiceEnabled","voiceLanguages","voiceTts"}, "masked": {"openrouterApiKey":"sk-or-…abcd","telegramBotToken":"123…456","providers":[{"name","model","baseUrl","apiKeyMasked":true}], "embeddingApiKey"?}, "switchbot": {"enabled":false,"tokenMasked":null,"secretMasked":null}, "voiceDownload": {"sensevoice":{"status":"absent"|"downloading"|"ready"|"error","message":""}, "ttsCantonese":{"status","message"}, "ttsEnglish":{"status","message"}}, "restartRequired": ["portalPort"]}`

`voiceEnabled` defaults to true when `[voice].enabled` is absent. `voiceLanguages` is ordered `yue, zh, en, ja, ko` (default `["yue","en"]`). `voiceTts` defaults to false. `voiceDownload` is read-only. These fields are not in `restartRequired`. OpenRouter STT is not on this whitelist.

`switchbot.tokenMasked` / `secretMasked` are masks or null. The raw token and secret never appear.

### PATCH /api/settings
Body may contain any subset of `{"model","generalLocation","defaultAutonomyMode","portalPort","switchbotEnabled","switchbotToken","switchbotSecret","voiceEnabled","voiceLanguages","voiceTts"}`.
Validation: `model` non-empty → `Agent::set_model` (live, no restart); `portalPort` 1–65535; `defaultAutonomyMode` ∈ {`default`,`autopilot`,`plan`}. `voiceLanguages` must be a non-empty subset of `yue, zh, en, ja, ko` (duplicates collapse, order is fixed). An empty or unknown list is `400 invalid_voice_languages` and writes nothing. `voiceEnabled` / `voiceTts` are booleans. A save with `voiceEnabled: true` ensures the SenseVoice pack; `voiceTts: true` (and enabled) also ensures Cantonese TTS and the English pack `vits-piper-en_US-lessac-medium` under `<home>/models/tts-english`. Download failure does not fail the save. The response includes `voiceDownload`. Voice fields are live on the next Telegram note and are not listed in `restartRequired`.
`switchbotToken` / `switchbotSecret` are write-only plaintext. The server seals them into SecretStore as `secret:switchbot.api_token` and `secret:switchbot.api_secret` and writes only those refs plus `enabled` under `[switchbot]`. `switchbotEnabled: true` without both secrets resolvable → `400 switchbot_credentials_required`. Empty token or secret → `400 empty_switchbot_credential`. SwitchBot changes are live (not listed in `restartRequired`). Advanced `env:NAME` refs are not written by this PATCH; an operator can set them in `config.toml`.
Before write: copy current file to `config.toml.bak`. Response:
`200 {"updated":["model","portalPort"], "restartRequired":["portalPort"], "applied":{"model":"<new>"}}`


### GET /api/settings — MCP projection (ADR-0041)
`masked.mcpServers` is an array of:
`{"name","kind":"preset"|"custom"|"stdio","preset"?,"enabled","url"?,"authConfigured","headerNames","connected","privacyAccepted"?,"privacyAcceptedAt"?,"configOwned"}`.
Never includes secret values. `kind=preset` for reserved `exa` / `jina`. `configOwned` is true when a hand-written preset row diverges from the pinned URL (portal will not overwrite).

### GET /api/settings/mcp/presets
→ `200 {"presets":[{ "id","label","kind","url","auth","tools",… }]}` — static Exa + Jina catalog.

### PUT /api/settings/mcp/{name}
Upsert Custom or update a preset. Body (camelCase, all optional except Custom `url` on create):
`{"name","url","enabled","bearer","headers","privacyAccepted","clearApiKey"}`.
Plaintext `bearer` / credential `headers` are sealed into SecretStore as `secret:NAME`; `env:NAME` refs are stored as-is. Exa rejects Bearer (use `headers["x-api-key"]`). Response `200 {"server":{…projection…}}`.

### DELETE /api/settings/mcp/{name}
Custom only. Presets → `400 preset_delete_forbidden`.

### POST /api/settings/mcp/{name}/enable · `/disable`
Persist `enabled` and hot-connect / disconnect that server only (no process restart). Exa first enable requires `{"privacyAccepted":true}` when consent is unset (`400 privacy_required`). Config-owned Exa → `400 config_owned`.

### GET /api/mcp/registry/search (ADR-0041 §11)
Session-gated. Proxies Official Registry `GET https://registry.modelcontextprotocol.io/v0.1/servers?version=latest` plus `search` / `cursor` / `limit` when sent.

Query: `q` (empty or omitted = unfiltered list; nonempty must be ≥3 Unicode characters after trim, else `400 search_too_short` and the registry is not called; surrounding whitespace is preserved on the upstream `search` value), `cursor` (opaque, max 512 bytes), `limit` (integer 1–100, else `400 invalid_limit`).

`200` is the registry list (`servers`, `metadata.nextCursor`, `metadata.count`) with an `install` object added on each server record. Upstream down, timeout, or non-success → `502 {"error":{"code":"registry_unavailable","message":"Official MCP Registry is unreachable"}}`. The portal process stays up.

### GET /api/mcp/registry/servers/{urlencodedName}
Session-gated. `{urlencodedName}` is one path segment: encode `/` as `%2F` (`io.github.user%2Fweather`). Proxies `GET /v0.1/servers/{name}/versions/latest`. `200` is that registry record plus `install`. Unknown name → `404`. Invalid name → `400 invalid_name`. Upstream down → `502 registry_unavailable`.

`install` (both routes):

| Field | Meaning |
| --- | --- |
| `registryName` | Full registry `server.name` (UI subtitle). Not persisted. |
| `suggestedSlug` | `[[mcp_servers]].name` to PUT. Last `/` segment, lowercased, invalid chars → `-`. Collision with an existing name or `exa` / `jina` / `google` → `-2` … `-20`, then a short hash. Same Custom URL (no `command`) reuses that row's name. |
| `installable` | True only for a usable `remotes[]` entry with `type=streamable-http`. |
| `blockReason` | `null`, or `sse`, `stdio`, `no_remote`, `unsupported`. |
| `transport` | Chosen remote type, or null. |
| `remoteTypes` | Distinct remote types, registry order. |
| `hasPackages` | Registry `packages[]` is non-empty. |
| `url` | Install URL, or the SSE URL for display when `blockReason=sse`. Null when the URL is templated or contains a credential. |
| `description`, `version`, `title`, `status`, `isLatest` | Copied for the UI. `status` is official registry status (`active` / `deprecated` / `deleted`). |
| `enabled` | Always `true` (default for a new install). |
| `bearer` | `null`, or `{required, placeholders}`. Present when `Authorization` is Bearer or absent-valued. The token is **not** copied. PUT `bearer` is the token only (no `Bearer ` prefix); the upsert seals it. |
| `headers` | Non-Authorization headers: `{name, isSecret, required, mapsTo:"header", placeholders, literal}`. `literal` is set only for a concrete non-secret value. `isSecret` / credential names / `{placeholder}` values are not echoed. |

**Install** is the existing upsert, once per server. There is no batch route. Multi-add is N sequential calls:

`PUT /api/settings/mcp/{suggestedSlug}` with `{url, enabled:true, bearer?, headers?}`.

Copy `literal` headers through. Operator-supplied bearer and secret header values are plaintext in that body; the upsert seals them (ADR-0041 §6) and hot-reloads that server. Omit `bearer` / a header to keep the previous sealed value on re-install. Best-effort: a failed PUT does not roll back earlier names. Do not PUT `exa`, `jina`, or `google`. SSE-only and package-only rows are not installable (`installable:false`). When both `streamable-http` and `sse` exist, `url` is the streamable-http remote.

### GET /api/soul · PUT /api/soul
Markdown files, whitelisted names only: `SOUL.md`, `USER.md`, `AGENTS.md`, `MEMORY.md`, and `system` (ADR 0011 R7 — the system-prompt file).
GET → `200 {"name","content","mtime"}`; PUT body `{"name","content"}` → backup `*.bak` then write; fires the `soul_updated` notification so the agent re-reads identity files.
The `system` entry resolves to `[openrouter] system_prompt_file` (lazily, relative paths under `RUSTFOX_HOME`); with no pointer configured it falls back to `prompts/system.md` so the file can be prepared ahead of enabling. Missing files read as empty content (200), parent dirs are created on write. Empty writes are refused (`empty_soul`).

### GET /api/settings — systemPrompt projection
`GET /api/settings` includes `systemPrompt: {"source","pointer","divergence"}` (ADR 0011 R7):
- `source`: which layer is live — `file` | `inline` | `builtin`. An absent/empty/missing prompt file honestly reports the fallback, never `file`.
- `pointer`: the configured `system_prompt_file` value (null if unset).
- `divergence`: true when the pointer is set AND the inline prompt differs from the built-in default — the file wins, so the inline copy is dead weight (divergence trap). The SPA surfaces a warning.

## Static hosting
- `GET /` → SPA `index.html` (embedded); unknown non-`/api` paths → same `index.html` (client-side routing); `/assets/*` hashed files with long cache.
- When built without `web/dist` present, only `/api/*` is served and startup logs `WARN portal: no embedded frontend`.

## Secrets (secure-store Slice 2)

Pending secret claims for the portal masked form. Claim routes use the opaque one-shot token as the credential (Telegram magic link); create/list require portal auth.

### POST /api/secrets/pending (auth)
Request `{"name":"OPENROUTER_API_KEY"}` → `200 {"id","name","expiresAt","claimUrl","claimToken"}`. `claimToken` is returned **once** to the authenticated client for link construction — never put the raw token in Telegram message *text* (URL path may carry the opaque id). Invalid name → `400 invalid_secret_name`.

### GET /api/secrets/pending (auth)
→ `200 [{"id","name","expiresAt"}]` (no tokens).

### DELETE /api/secrets/pending/{id} (auth)
→ `200 {"cancelled":true}` or `404`.

### GET /api/secrets/claim/{token} (public)
→ `200 {"id","name","expiresAt","status":"pending"}`. Expired / unknown → `404`. Never returns a secret value.

### POST /api/secrets/claim/{token} (public)
Request `{"value":"..."}` → writes via `SecretStore::set`, consumes the one-shot token → `200 {"ok":true,"name","stored":true}`. Empty value → `400 empty_value`. Expired / unknown / already used → `404`.

### POST /api/secrets/claim/{token}/cancel (public)
→ `200 {"cancelled":true}` or `404`. Clears the pending claim without storing a value.

SPA route: `/secrets/claim/$token` — masked password form (no portal login required).
