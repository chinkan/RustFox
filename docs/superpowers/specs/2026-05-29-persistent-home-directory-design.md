# Persistent Home Directory (`~/.rustfox`) — Design

**Date:** 2026-05-29
**Status:** Approved (design phase)
**Worktree:** `telegram-plan-tool-visuals`

## Problem

RustFox today scatters its working state and resolves most paths relative to the
process launch directory:

- The sandbox (`[sandbox].allowed_directory`) is a **required** path that users
  typically point at an ephemeral location like `/tmp/rustfox-sandbox`. Files
  the LLM writes there (scripts, programs, notes) are wiped on reboot, so they
  cannot be reused over the long term.
- `rustfox.db`, `skills/`, `agents/`, `supervisor/artifacts`, and the learning
  `user_model.md` all default to **relative** paths, resolved from the current
  working directory. State location therefore depends on where the bot was
  launched, and is easy to lose or duplicate.
- There is no first-class notion of an isolated *instance home*, so running two
  RustFox instances on one machine for different purposes requires careful
  manual path juggling.

## Goals

1. Give each RustFox instance a single, persistent **home directory**
   (default `~/.rustfox`) that survives reboots.
2. Make the sandbox a **durable workspace** so the LLM can accumulate reusable
   scripts, programs, and markdown files over time.
3. Keep **secrets** (`config.toml`, OAuth tokens) and the **SQLite DB**
   structurally **outside** the LLM-writable sandbox.
4. Store **instance-created skills/agents** under the home so the work
   environment is isolated per instance.
5. Support **multiple instances on one machine**, each with its own home, with
   minimal friction (ideally one environment variable).
6. Do not break existing installs; never move user data automatically.

## Non-Goals

- OS-level sandboxing (containers, seccomp, namespaces). The sandbox remains a
  path-canonicalization boundary, as today.
- Changing the skill/agent *file format* or the agentic loop.
- Auto-migrating or relocating any existing user files on disk.

---

## Reference research

Surveyed how comparable AI assistants and the platform convention handle
persistent working directories:

- **Claude Code / Codex CLI** use a single home dot-directory (`~/.claude`,
  `~/.codex`) holding config, skills, agents, memory, and project state.
  Simple, discoverable, easy to back up or wipe. (Claude Code is criticized for
  *not* following XDG, which informed our decision to keep a single root but
  allow overrides.) Claude Code also treats the **working directory** as the
  agent's primary context and operation boundary, and supports additional
  writable roots via `--add-dir`.
- **XDG Base Directory Specification** splits state by purpose
  (`$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_STATE_HOME`, `$XDG_CACHE_HOME`).
  It is the "correct" Linux convention and is env-var overridable, but spreads
  files across four locations, which makes per-instance isolation and "delete
  everything" harder.

**Takeaway adopted:** a single discoverable root (Claude/Codex style) for
isolation and simplicity, plus an environment-variable + per-path override
escape hatch (the spirit of XDG's overridability) for multi-instance and
power-user needs.

---

## Design

### 1. Directory model (Hybrid)

**Home root resolution order (first match wins):**

1. `RUSTFOX_HOME` environment variable (must be an absolute path).
2. `[general].home` in `config.toml` (absolute path).
3. Default: `~/.rustfox` (expanded from the OS home directory).

**Default layout under the home root:**

```
~/.rustfox/
  config.toml          # optional config search location (secrets) — OUTSIDE sandbox
  rustfox.db           # SQLite memory/embeddings                   — OUTSIDE sandbox
  skills/              # skill source of truth (seeded + writable)
  agents/              # agent source of truth (seeded + writable)
  workspace/           # THE SANDBOX — file/command tools confined here
  artifacts/           # supervisor artifacts
  user_model.md        # learning user model
```

**Per-path override semantics.** Each data path
(`sandbox.allowed_directory`, `memory.database_path`, `skills.directory`,
`agents.directory`, `supervisor.artifacts_dir`, `learning.user_model_path`)
becomes **optional**. Resolution per path:

- **Unset** → resolved under the home root using the default subpath above.
- **Set, absolute** → used verbatim (full backward compatibility).
- **Set, relative** → resolved from the current working directory (legacy
  behavior) and a one-time **actionable startup warning** is emitted (see §5).

**Config file discovery.** `Config::load` keeps its current behavior (CLI arg,
else `./config.toml`) and additionally falls back to `<home>/config.toml` when
neither is present. The explicit CLI path always wins.

**Multi-instance.** A second isolated instance is one line:

```bash
RUSTFOX_HOME="$HOME/.rustfox-work" cargo run
```

This instance gets its own `workspace/`, `skills/`, `agents/`, `rustfox.db`,
and artifacts — fully isolated from the default instance.

### 2. Home resolution module

Introduce a small, focused unit responsible solely for resolving the home root
and the per-path defaults. It has one clear job: given the environment, the
config, and the OS home, produce the set of absolute resolved paths plus the
list of legacy-relative-path warnings to print.

- **Input:** `RUSTFOX_HOME` env, `Config` (raw, with `Option<PathBuf>` fields),
  OS home dir.
- **Output:** a `ResolvedPaths` struct with absolute paths for home, workspace
  (sandbox), db, skills, agents, artifacts, user_model; plus a
  `Vec<LegacyPathWarning>`.
- **Side effect (explicit, not hidden):** `ensure_dirs()` creates the home and
  its standard subdirectories with `0700` permissions on Unix (mirroring XDG's
  guidance for user-private dirs).

This keeps `config.rs` parsing dumb (just `Option<PathBuf>` fields) and isolates
all path-resolution policy in one testable place.

### 3. Sandbox boundary

`file_read`, `file_write`, and `run_command` (and any other filesystem/command
tool in `src/tools.rs`) remain confined by `validate_sandbox_path`, but the
sandbox root is now the resolved **`workspace/`** path under the home.

- `[sandbox].allowed_directory` becomes optional; default `<home>/workspace`.
- Because `config.toml`, `rustfox.db`, and OAuth token state live **above**
  `workspace/`, they are structurally unreachable by file/command tools — even
  under prompt injection.
- Skills/agents are written through their **dedicated** tool path
  (`write_skill_file` + the learning extractor), not the raw sandbox file
  tools, so they intentionally live outside `workspace/`.

### 4. Workspace as durable LLM scratch space

`workspace/` persists across restarts (no more `/tmp` wipe). The LLM may
accumulate reusable scripts, programs, and markdown notes there. The layout is
**freeform** — RustFox does not impose or enforce subdirectories; the LLM
organizes the space as it sees fit. The system prompt's sandbox description is
updated to reflect that the workspace is persistent and reusable.

### 5. Skills/agents sourcing: seed + explicit update

The instance `skills/` directory is the **single load source** at runtime
(`load_skills_from_dir`). Agents follow the identical model with `agents/`.

**Seeding (first run / empty dir).** When the resolved `skills/` (resp.
`agents/`) directory does not exist or is empty, RustFox seed-copies the
**bundled** skills shipped with the installation into it. The bundled source is
the directory adjacent to the executable / repo (the existing `skills/` and
`agents/` folders). Seeding also copies `skills-lock.json` into the home as
`skills-lock.json` so future updates can diff.

**Explicit update (`/update-skills` Telegram command — no auto-sync).** Running
`/update-skills` re-syncs bundled → instance using content hashes from
`skills-lock.json`:

- **Bundled, unchanged locally** (instance file hash matches the lock) →
  overwrite with the new bundled version.
- **Bundled, locally modified** (hash differs from the lock) → back up the
  instance copy to `<skill>/SKILL.md.bak` (and other changed files to `*.bak`),
  then write the new bundled version. Never silent data loss.
- **Instance-created** (skill name absent from `skills-lock.json`) → never
  touched.

The command reports a summary: updated / backed-up / skipped counts. After
sync it hot-reloads the registries (reusing the existing `reload_skills` /
`reload_agents` machinery). `/update-skills` is registered alongside the other
text commands in `src/platform/telegram.rs`.

### 6. Migration & compatibility (no auto-move)

- **No automatic file moves.** RustFox never relocates an existing
  `./rustfox.db`, `./skills`, etc.
- **Explicit config paths honored** verbatim (absolute) — existing deployments
  that pin paths are unaffected.
- **Actionable startup warning.** When any path is unset *and* a legacy file is
  detected in the launch CWD (e.g. `./rustfox.db`, `./skills/`), or when a path
  is set to a relative value, print a clear warning that includes:
  - the old resolved path vs the new default path, and
  - the exact shell command to copy data
    (e.g. `cp ./rustfox.db ~/.rustfox/rustfox.db`), and
  - the exact `config.toml` line to pin the old path instead.
- **Start-fresh path.** Doing nothing and letting RustFox use the new
  `~/.rustfox` defaults is itself the "start fresh" option (a brand-new empty
  workspace, freshly seeded skills, a new DB). This is documented explicitly.
- **Tutorial doc.** Add `docs/persistent-home-directory.md` covering:
  1. The new home layout and what each subdir holds.
  2. How to **migrate existing data** into `~/.rustfox` (DB, skills, agents,
     artifacts, user model) with copy commands.
  3. How to **start fresh** instead (and how to keep the old location by
     pinning paths in `config.toml`).
  4. How to run **multiple instances** via `RUSTFOX_HOME`.

### 7. Config & example updates

- `SandboxConfig.allowed_directory`: `PathBuf` → `Option<PathBuf>`.
- `MemoryConfig.database_path`, `SkillsConfig.directory`,
  `AgentsConfig.directory`, `SupervisorConfig.artifacts_dir`,
  `LearningConfig.user_model_path`: make optional (or keep the field but resolve
  through the home module, preferring an explicit value when present).
- Add `GeneralConfig.home: Option<PathBuf>`.
- Update `config.example.toml`: comment out the now-optional paths, document the
  `~/.rustfox` defaults, `RUSTFOX_HOME`, and `[general].home`.

---

## Data flow (resolution at startup)

```
RUSTFOX_HOME / [general].home / ~/.rustfox
        │
        ▼
  resolve home root ──► ensure_dirs() (0700)
        │
        ▼
  for each path field:
     unset      → <home>/<default-subpath>
     absolute   → use verbatim
     relative   → use as-is (CWD) + queue legacy warning
        │
        ▼
  ResolvedPaths { home, workspace, db, skills, agents, artifacts, user_model }
        │
        ├─► seed skills/ + agents/ if empty
        ├─► print queued legacy warnings (with copy/pin commands)
        └─► hand resolved absolute paths to MemoryStore, sandbox tools,
            skill/agent loaders, supervisor, learning
```

## Error handling

- `RUSTFOX_HOME` set but not absolute → log a warning and ignore it (fall back
  to config/`~/.rustfox`), consistent with XDG's "ignore relative" rule.
- OS home directory undeterminable → fail fast with a clear error (the home
  root cannot be resolved).
- Directory creation failure (permissions) → fail fast with the offending path,
  as the bot cannot operate without its home.
- `/update-skills` with no bundled source available → report the error to the
  user; do not modify the instance dir.
- Seeding is best-effort per file; a failed copy logs a warning and continues,
  leaving any successfully copied files in place.

## Testing strategy

Unit tests (in the home-resolution module and `config.rs`):

- Home resolution precedence: env > config > default.
- Relative `RUSTFOX_HOME` is ignored.
- Per-path override: unset → default subpath; absolute → verbatim; relative →
  used as-is + warning queued.
- `ensure_dirs` creates the expected tree.

Integration tests (`tests/`):

- Sandbox confinement still rejects paths outside the resolved `workspace/`
  (extend existing sandbox tests against the new default root).
- Seeding populates an empty `skills/`/`agents/` from a fixture bundle.
- `/update-skills` hash logic: unchanged → overwritten; modified → `.bak`
  created + updated; instance-only → untouched. Drive via the update function
  with a temp home + temp bundle + temp lock file.
- Two distinct `RUSTFOX_HOME` values yield fully isolated resolved path sets.

## File structure (created / modified)

- **Create:** `src/home.rs` (or `src/paths.rs`) — home root + per-path
  resolution, `ResolvedPaths`, `ensure_dirs`, legacy-warning collection.
- **Create:** `docs/persistent-home-directory.md` — migration & multi-instance
  tutorial.
- **Modify:** `src/config.rs` — optional path fields, `[general].home`,
  resolution hook in `Config::load` (or a new `Config::resolve_paths`).
- **Modify:** `src/main.rs` — call home resolution, ensure dirs, seed skills/
  agents, print warnings, pass resolved paths downstream.
- **Modify:** `src/tools.rs` — sandbox root sourced from resolved `workspace/`.
- **Create/Modify:** skill-update logic (likely `src/skills/loader.rs` or a new
  `src/skills/update.rs`) — seed + hash-diff update using `skills-lock.json`.
- **Modify:** `src/platform/telegram.rs` — register `/update-skills` command
  and surface it in `/start` help text.
- **Modify:** `config.example.toml` — document optional paths, `RUSTFOX_HOME`,
  `[general].home`, persistent workspace.
- **Modify:** `CLAUDE.md` / `README.md` — note the new home model.

## Open questions

None blocking. (Whether the home-resolution unit lives in `src/home.rs` vs
`src/paths.rs` is a naming detail to settle during planning.)
