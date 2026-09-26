# ADR 0011a: Control-Plane Grill Resolutions (Round 1–3)

## Status
Accepted — supplements [ADR 0011](0011-skills-control-plane-and-provenance.md); where this
file and the 2026-09-25 plan disagree, **this file wins**.

## Date
2026-09-25 (grilling with Kan, three rounds, all decisions user-ratified)

## Context
ADR 0011 locked the architecture (native installer, provenance ledger, quarantine deletes).
The grill rounds resolved the *behavioural* questions the plan left open: injection-gate
enforcement, the quarantine ghost-revival bug, concurrency semantics, task-deletion history
policy, rate-limit UX, and the system-prompt editing surface.

---

## Resolutions

### R1 — Scope order (Round 0/1, confirmed)
Skills control plane (T1) → Git installer (T2) → Task CRUD (T3) → Agent editor (T4).
Tasks CRUD stays in scope (reframe was additive, not a removal).
Prompt editing enters as its own slice (see R7/R8).

### R2 — Injection screening: soft gate + hard acknowledge
- Secret scan stays a **hard refuse** (0011 B — unchanged).
- A separate **injection-shape scan** (MUST/NEVER override phrasing, hidden HTML comments,
  invisible unicode, URL-shortener spam) produces **warnings**, not refusals — 0011's trust
  model says these gates reduce foot-guns, they are not a sandbox.
- But warnings require **acknowledgement**: `dryRun:false` install must send
  `acknowledgedWarnings: [ ... ]` covering every warning emitted by the dry run;
  server re-scans and returns **409 `warnings_unacknowledged`** listing any uncovered
  warning. Rationale: RRSI's leak-screening principle — surface before it "scores" —
  combined with owner sovereignty (Kan can override; he cannot *miss* it).
- The same warning list is stored per-skill in `installed-skills.json` at install time.

### R3 — Quarantine ghost-revival: fix = option C (belt and braces)
**Bug** (found in audit): the loader registers any top-level dir containing
`SKILL.md`/`AGENT.md`, and `load_skill_file` prefers the frontmatter `name:` over the dir
name. A quarantine rename to `<name>.deleted-<ts>` therefore keeps the skill *live* in the
registry under its original name after reload — delete becomes rename-revival.
(The earlier claim "loader ignores `*.deleted-*`" was wrong; corrected here.)

**Fix (both layers):**
1. **Structural**: quarantine moves the dir to `skills/.trash/<name>-<ts>` (dot-dir is
   never a loadable top-level entry — inert by construction). No restore endpoint in v1;
   `mv` out of `.trash` is the restore path (documented).
2. **Defensive**: loader skips entries matching `is_hidden_entry` (`.deleted-*`, `.bak*`,
   dot-prefixed) so even hand-made quarantine dirs can't revive.
3. **Regression test**: quarantine → reload → `GET /api/skills` contains neither the dir
   name *nor* the frontmatter name of the quarantined skill.

### R4 — Concurrency: optimistic locking + auto-reload
- `GET detail` already returns `hash` — that is the ETag.
- `PUT .../file` accepts `baseHash` (body) or `If-Match` header; mismatch →
  **409 `changed_since_read`** (protects Kan-vs-OpenCode concurrent edits; last-write-wins
  silently eats files).
- Successful primary write or new-entry creation **auto-reloads**; response carries
  `{skillsLoaded, agentsLoaded}`. Aux-only edits skip reload (already implemented).

### R5 — PUT/POST semantics (Round 3 Q9)
Creation moves to **`POST /api/skills`** and **`POST /api/agents`** (structured body →
rendered file, frontmatter guaranteed valid, name = dir name enforced; agents POST carries
`{name, description, model, tools[], maxIterations, skipBootstrap, instructions}`).
`PUT .../file` becomes **update-only → 404 when the entry doesn't exist** (existing
PUT-create fall-through removed; "create" is an intent worth its own endpoint + gate).
`Duplicate` = client-side GET → POST with new name.

### R6 — Task deletion: soft delete, history is evidence
- Tasks get `deleted_at` (soft). `scheduled_task_runs` are **never cascade-deleted** —
  they are the raw material for the RRSI Stage-1 noise-floor/replay-set work
  (vault: `ideas/fleeting/rrsi-regularized-harness-evolution.md`).
- List endpoints filter deleted by default; `?includeDeleted=true` shows them.
- Editing a task while a run is in flight: allowed; the already-spawned run finishes
  untouched (disarm only prevents *future* fires). `nextRunAt` in responses is a
  **computed** field, not stored truth.

### R7 — System prompt: file pointer, not inline config (Round 3 Q8 = option b)
- New config field `system_prompt_file` (path, resolved under `RUSTFOX_HOME`).
- Precedence: **file > inline `system_prompt` > built-in default**; when both file and
  inline are set, API responses carry a warning (divergence trap).
- Portal edits the prompt through the existing `PUT /soul` whitelist-write machinery
  (new allow-listed entry), **not** through config.toml PATCH — the toml round-trip
  destroys comments and mangles multi-line strings.
- The hard-coded guardrail sections (Verification Protocol, Soul Files) remain
  **non-configurable**: the most important gate must not ship an off switch.

### R8 — Rate limits & file-level deletes (Rounds 2, Q6/Q7)
- GitHub rate-limit hits → 400 `github_rate_limited` with an honest message
  ("wait ~N min or install manually"); **no** GitHub-token config field yet — one more
  plaintext secret in config.toml is a surface we agreed not to grow piecemeal
  (secret-storage policy rides with the OAuth-token discussion).
- No per-aux-file delete in v1. Undo a bad aux file via editor (overwrite) or
  quarantine-whole-skill + reinstall.

### R9 — T4 whitelist gate naming (carried from plan)
Tool-whitelist hard-gate + `?allowMissing=1` escape hatch: **ratified as implemented**
(`write_file_kind` — 400 `unknown_tools` listing missing names).

---

## Ticket deltas caused by this ADR
| Ticket | Delta |
|---|---|
| T1 | R3 loader filter + `.trash` move + regression test; R4 `baseHash`/409; R5 PUT-create removal → POST endpoints |
| T2 | R2 acknowledgedWarnings enforcement; R3 shares quarantine impl; R8 error code |
| T3 | R6 soft delete (schema migration `deleted_at`), drop the CASCADE plan, `?includeDeleted` |
| T4 | R5 POST create with structured render |
| NEW T7 | System-prompt file pointer (R7): config field + loader precedence + `PUT /soul` entry + settings UI textarea + precedence warning |

## Out of scope (re-confirmed)
Restore-from-trash endpoint, per-file delete, GitHub token auth, editor syntax
highlighting, marketplace browsing UI, configurable guardrail appends.
