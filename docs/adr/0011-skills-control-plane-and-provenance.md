# ADR 0011: Skills Control Plane, Provenance Ledger, and Native GitHub Installer

## Status
Accepted

## Date
2026-09-25

## Context
The portal today exposes skills/agents read-only (`GET /api/agents/skills`) plus a blanket reload. All real editing goes through the agent's own tools (`write_skill_file`) or the filesystem. Kan's RRSI analysis (see vault note `ideas/fleeting/rrsi-regularized-harness-evolution.md`) established that a self-editing agent needs a **governable edit surface**: every harness mutation should be visible, attributable, and reversible. The skills directory is ~80% of RustFox's editable harness (91 SKILL.md files + agent definitions).

Second problem: community skills are distributed via `npx skills` (Vercel Labs skills-cli) against the shared Agent Skills format — SKILL.md with name/description frontmatter, optional aux files. RustFox's format is already the same spec, so third-party skill repos are directly consumable. But shelling out to the CLI would: (a) add a Node runtime dependency, (b) write through `.claude/`/`.agents/` path conventions not ours, (c) create a second provenance system (`.skills.json`) colliding with `skills-lock.json` (which has bundled seed/update semantics — see `src/skills/update.rs`).

Third problem: bundled (cargo-embedded, `include_dir!`) skills are re-seeded on every update. A "delete" button on one would be a lie — it returns next boot.

## Decision

**A. Skills control plane in the portal** (order approved in grill: Skills → Tasks → Agents):
- `GET /api/skills/{name}` — detail: provenance (`bundled` | `installed` | `user`), `modified` flag (dir hash vs lock record), file list, SKILL.md content.
- `PUT /api/skills/{name}/file` — write aux/SKILL.md through the *existing* validators (`validate_skill_name`/`validate_skill_path` promoted from `skill_tools.rs`), `.bak` backup + atomic tmp+rename (same pattern as settings/soul writes), refuse empty SKILL.md, refuse frontmatter `name:` mismatch.
- `DELETE /api/skills/{name}` — quarantine to `<name>.deleted-<utc>` (never hard-remove), then auto-reload. **Refused for bundled skills** (403 `bundled_skill_readonly`).

**B. Native Rust GitHub installer** (grill Q1b = option B). No `npx`.
- `reqwest` against the GitHub REST API: `GET /repos/{o}/{r}` → default branch, `/git/trees/{sha}?recursive=1` → discover every `SKILL.md` at depth ≤ 3, then `raw.githubusercontent.com` per file. Behind a `GitHubFetcher` trait so tests fake the network.
- Two-phase: `dryRun: true` returns the plan + safety verdicts with **zero writes**; `dryRun: false` installs.
- Hard caps: ≤10 skills/request, ≤20 files/skill, ≤512 KB/file, ≤2 MB/skill.
- Safety gates (RRSI-inspired, enforced *before* anything lands on disk):
  - **Secret scan** — every fetched file checked for credential shapes (`GOCSPX-`, `sk-…`, `AKIA…`, `xox…`, PEM headers, Google refresh tokens `1//…`); a hit refuses the whole skill naming file + rule. Rationale: a leaked credential inside a skill becomes a live system-prompt injection vector — reject at the gate, not after it "scores".
  - **Executable refusal** — files with executable-code extensions (.sh/.py/.js/.ts/.pl/.rb/.exe/.dll/.dylib/.so/.bin) are not installed; SKILL.md-only + docs/markdown aux files only.
  - **Bundled-name collision** — installing over a bundled skill name is refused (same lie-prevention as DELETE).
  - Existing non-bundled dir → 409 unless `force` (which quarantines first).

**C. Provenance ledger = new file `~/.rustfox/installed-skills.json`**
`{ version, skills: { name: { sourceRepo, commitSha, ref, installedAt, contentHash } } }`
Separate from `skills-lock.json` on purpose: the lock tracks *bundled* sync state (seed/update engine contract); this file tracks *externally installed* skills with their git origin. Provenance resolution order: installed-ledger > bundled-lock/embedded > user-created. `contentHash` = `hash_skill_dir()` so an installed skill that drifts from its record can be flagged `modified` too.

**D. Trust model**: the portal's write surface is granted to exactly the same principal as the agent's own skill tools (token-authenticated owner on LAN/Tailnet — ADR 0006/0007). The gates above reduce accidents and supply-chain foot-guns; they are *not* a sandbox. Docs must say so.

## Consequences
- Editing/reloading skills becomes a 5-second loop instead of a Telegram chat round-trip; install-then-curator workflow (dry-run → read verdicts → install) is inspectable.
- Quarantine + `.bak` + provenance = every harness mutation attributable and reversible — the ledger precondition the RRSI gap analysis named.
- GitHub API unauthenticated rate limit (60/hr/IP) is acceptable for personal use; token support parked for later.
- `*.deleted-*` / `*.bak` dirs are hidden from listings (loader ignores them; filter in API).
- The installer deliberately cannot execute anything it downloads; skills with real code deps must be installed manually (message says so).

## Rejected options
- **Shell out to `npx skills`** (Q1b option A): Node dependency, path-convention mismatch, rival provenance file.
- **URL-only import of single SKILL.md** (Q1b option C): fails on multi-file skills, which are most of the useful public catalog.
- **Installing into a staging dir with an activate step**: more moving parts than the two-phase dry-run already provides at this trust level; revisit when a skill *update* engine lands.
- **Deleting bundled skills** (returns on update → lie) and **editing them in place** (would diverge from every future release): both refused.
