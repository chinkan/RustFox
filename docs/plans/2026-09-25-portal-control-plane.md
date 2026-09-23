# Portal Control Plane (M3-first) — Skills CRUD, GitHub Installer, Task CRUD, Agent Editor

Date: 2026-09-25 · Status: APPROVED (grill rounds: order = Skills → Tasks → Agents; installer = native Rust, option B)

## Decisions locked (from grilling)

1. **Order**: Skills control plane (editor + installer together) → Task CRUD → Agent editor.
2. **Installer**: native Rust (reqwest → GitHub API), **no** `npx skills` shell-out.
3. Branch: stacked on `feat/web-portal-impl` (PR #57). Draft PR targets #57's branch.
4. Provenance: new `installed-skills.json` (name → { source_repo, commit_sha, installed_at, content_hash }). `skills-lock.json` (bundled semantics) untouched.
5. Skill/Agent deletion of *bundled* entries: **refused** (binary re-seeds on every update; deleting is a lie). `.bak`/`*.bak` dirs are hidden from lists.

## Existing assets (audit, 2026-09-25)

| Asset | File | Reuse |
|---|---|---|
| name/path validators | `src/skill_tools.rs` `validate_skill_name` / `validate_skill_path` | make `pub(crate)`, portal handlers call them |
| dir hash | `src/skills/seed.rs::hash_skill_dir` (pub) | provenance + modified-detection |
| lock read/write | `src/skills/update.rs` (private) | portal install record uses own file, same style |
| write pattern | `src/portal/settings.rs` `.bak` + atomic tmp+rename | all portal writes |
| reload | `AgentOps::reload_skills_and_agents` | after every write/delete/install |
| error envelope | `src/portal/error.rs` | all new endpoints |
| task fire builder | `agent.rs::restore_scheduled_tasks` | extract → `AgentOps::arm_task` (fixes `restartToSchedule` gap) |
| fake ops harness | `tests/portal_api.rs` (29 tests, FakeAgent) | extend, don't rewrite |

## Tickets (tracer bullets; each: failing test → implement → green)

### T1 — Skill file read/write API (editor backend)
- `GET /api/skills/{name}` → `{ name, provenance: bundled|installed|user, modified: bool, files: [{path,size}], content /*SKILL.md*/, description }`
- `GET /api/skills/{name}/file?path=rel` → `{ path, content }` (≤512 KB, validated path)
- `PUT /api/skills/{name}/file` `{ path, content }` → validates name+path, non-empty content for `SKILL.md`, YAML frontmatter `name:` (if present) must match dir, `.bak` on overwrite, atomic rename. Returns `{ ok, bytes }`.
- `DELETE /api/skills/{name}` → refuse `bundled` (403 `bundled_skill_readonly` — detect: dir exists in embedded `include_dir!` list AND hash matches lock); refuse names with `/`/`..`; move dir to `<name>.deleted-<ts>`; drop installed-skills.json entry; auto-reload registry.
- Provenance: `installed` (in installed-skills.json) > `bundled` (in lock map/embedded) > `user` (else). `modified` = hash_skill_dir(dir) != lock entry.
- Files: `src/portal/control.rs` (new), routes in `mod.rs`, tests in `tests/portal_api.rs`.

### T2 — Native GitHub installer
- `src/portal/install.rs`:
  - `GitHubFetcher` trait (`get_json`, `get_bytes`) so tests use a fake; reqwest impl w/ User-Agent `rustfox-portal`, 30 s timeout.
  - `parse_source("owner/repo[:subpath]@ref")` → validated owner/repo (chars ⊆ [A-Za-z0-9._-]).
  - Resolve ref: `GET /repos/{o}/{r}` default_branch (or explicit `?ref=`), then `GET /repos/{o}/{r}/git/trees/{sha}?recursive=1`.
  - Skill discovery: every `**/SKILL.md` (≤ depth 3, `.git`/`node_modules`/`dist`/`__pycache__` excluded); skill name = parent dir name, must pass `validate_skill_name`.
  - Dry run: `POST /api/skills/install { source, dryRun: true }` → `{ skills: [{name, description, files, sizeBytes}], verdicts }` — **no writes**.
  - Real install (`dryRun:false`): per-file raw.githubusercontent fetch w/ caps: ≤20 files/skill, ≤512 KB/file, ≤2 MB/skill, ≤10 skills/request. **Refuse** executable-looking targets by extension (.sh/.py/.js/.ts/.pl/.rb/.exe/.dll/.dylib/.so/.bin).
  - **Secret scan** of every text file (GOCSPX-, sk-[A-Za-z0-9]{20,}, AKIA[0-9A-Z]{16}, xox[baprs]-, google_oauth_refresh `1//[A-Za-z0-9_-]{20,}`, PEM headers) → skill **refused** naming the file+rule (mirrors RRSI leak screening: reject before it scores).
  - Collision with existing dir → 409 unless `?force` (then `.deleted-<ts>` first). Bundled names → refused (same rule as DELETE).
  - Write provenance + `POST reload` done server-side in one step. Response: `{ installed:[…], skipped:[…], warnings:[…], reload:{skillsLoaded,agentsLoaded} }`.
- Routes: `GET /api/skills/installed` (provenance list), `POST /api/skills/install`.
- Tests: fake fetcher fixtures (tree JSON + raw contents); cover: discovery, depth cap, executable refusal, secret refusal, size cap, collision 409, force, provenance round-trip, dry-run writes nothing, bundled-name refusal.

### T3 — Task CRUD + live re-arm
- `ScheduledTaskStore`: add `update_task_fields(id, prompt, trigger_type, trigger_value, description)`, `delete_task(id)` (runs have FK → `PRAGMA foreign_keys=ON` + `ON DELETE CASCADE` — verify schema first; if missing, DELETE runs manually in same txn).
- `agent.rs`: extract the fire-closure builder from `restore_scheduled_tasks` into a shared helper; add `pub async fn arm_task(&self, task: &ScheduledTask, bot: Arc<Bot>) -> anyhow::Result<Uuid>` + `disarm_task(&self, uuid)`. New `AgentOps` methods: `arm_task`, `disarm_task` (tests' FakeAgent records calls instead of scheduling).
  - `PortalState` gains `bot: Arc<teloxide::Bot>` (main.rs passes the live bot; test fixtures use `Bot::new("42:dummy")` — constructing a Bot does no network I/O).
  - Store: `update_task_fields` must also refresh `next_run_at` (one_shot) so the UI shows the new fire time.
- Endpoints (protected):
  - `POST /api/tasks` `{ name, prompt, triggerType: recurring|one_shot, triggerValue }` → validate: description+prompt non-empty; recurring → `validate_cron_expr` (6-field); one_shot → parse future ISO → store `create()` + `arm_task`. One-shot `nextRunAt` set like scheduling_tools does.
  - `PUT /api/tasks/{id}` partial `{ name?, prompt?, triggerValue? }` (triggerType immutable; changing type = delete+create). Re-arm if status active.
  - `DELETE /api/tasks/{id}` → disarm + DB delete (204/404).
  - `POST /api/tasks/{id}/enable` → set active + real re-arm (replaces `restartToSchedule` stub).
- Tests: create→fires list+scheduler fake; invalid cron → 400 `invalid_cron`; past one-shot → 400; update re-arms (fake records job); delete removes; enable no longer lies. Fake `AgentOps` gets `reschedule` counter.

### T4 — Agent editor (thin slice of T1)
- `GET /api/agents/{name}` → parsed AGENT.md frontmatter (`model`, `tools`, `max_iterations`, `skip_bootstrap`, description) + content.
- `PUT /api/agents/{name}/file` → same write gates as skills + **tool whitelist check**: every declared tool must exist in `all_tool_definitions()` names else 400 listing missing (RRSI evidence: warn on tools not available at runtime — here we hard-gate, it's a save-time action not a search heuristic). Allow explicit `?ignoreMissing=true` escape hatch → saves with warning list.
- `POST /api/agents { name, content }` create-from-scratch with `AGENT.md`; names reserved: reject existing.
- `DELETE /api/agents/{name}` → refuse bundled (seeded default agents), else quarantine.
- Tests: frontmatter round-trip; tool gate (fake registry w/ 2 tools; declaring 3rd → 400; escape hatch → 200+warning).

### T5 — Frontend: Skills page + install dialog, Task editor, Agent editor
- `web/src/api/client.ts`: 12 new fetch fns mirroring handlers exactly.
- `types.ts`: SkillDetail, InstalledRecord, InstallRequest/Response, TaskCreate/Update, AgentDetail.
- `routes/_auth/skills.tsx` (replace inline skill list on agents page w/ dedicated page; agents page keeps main-agent status + link):
  - table: name · kind · provenance badge · modified dot · reload count; row click → **editor drawer**: file list → content textarea (plain <textarea>, no editor dep) → Save (PUT) / Reload button / Delete w/ confirm.
  - **Install dialog**: source input (`owner/repo`), Dry-run button → verdicts table (would install / skipped + reason) → Install button (same payload dryRun:false). Errors surfaced w/ exact reason codes.
- `routes/_auth/tasks.tsx`: Edit (inline form: name, prompt textarea, cron or ISO field by triggerType), Create (+), Delete w/ confirm. Enable now shows real next-run (no `restartToSchedule` note).
- i18n: en + zh-HK keys for all new strings (parity test guards).
- vitest: client fn shapes; install verdict rendering; task form validation (6-field cron); provenance badges.
- Keep bundle lean (no Monaco/CodeMirror — plain textarea + monospace is fine for markdown; revisit if it stings).

### T6 — Docs + ADR
- `docs/adr/0011-skills-control-plane-and-provenance.md` (status: accepted) — decision record for this whole change:
- `docs/portal-api.md`: new endpoint rows + error codes.
- `docs/portal-access.md` note: portal write surface = same trust as Telegram tool access; keep bind local/tailnet.
- README portal paragraph: one line for skills control plane.

## Verification gate (before PR ready)
- `cargo test --release` green (lib + portal_api + web build present via build-all)
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
- `npm run build` + `vitest run` in web/
- mutation-checks: revert `validate_skill_path` → traversal tests fail; drop secret-scan → fixture test fails; remove bundled guard → refuses tests fail.

## Out of scope (parked)
- Telegram auto-login (M3-first, separate). Skill *marketplace browsing* UI (search GitHub API). Bundled-skill delete (refused). Version pinning/auto-update of installed skills (record exists; update engine later). Editor syntax highlighting.
