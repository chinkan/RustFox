# Multi-Directory Skill Loading — Design

**Date:** 2026-06-01
**Status:** Approved (design phase)
**Worktree:** `telegram-plan-tool-visuals`

## Problem

RustFox skills are loaded from a single instance directory (`config.skills.directory`,
default `~/.rustfox/skills/`). Bundled skills (shipped with the project at `./skills/`)
are seed-copied into this directory on first run, mixing predefined templates with
user/custom skills in one flat namespace. This creates several issues:

1. **No read-only source of truth.** Once seeded, bundled skills are indistinguishable
   from user-created skills. `/update-skills` must use content-hash lock files to
   detect which skills are "original" vs modified, adding complexity.
2. **Custom skills can be overwritten.** If a future bundled release adds a skill
   with the same name as a user-created one, `/update-skills` backs it up as `*.bak`
   but the user's work is displaced.
3. **No way to restore a deleted bundled skill.** If a user deletes a bundled skill
   from the instance dir, there's no mechanism to recover it short of re-seeding.
4. **CWD-dependent bundled path.** The bundled path `PathBuf::from("skills")` is
   relative to the process working directory, which can differ from the project root
   when running as a systemd service or from a different directory.

## Goals

1. Keep bundled skills as a **read-only fallback layer** — always available, never
   accidentally modified or deleted.
2. Allow the agent to create **custom skills** in the instance directory that shadow
   bundled ones of the same name.
3. Provide a **single lookup API** that checks instance → bundled without per-tool
   fallback logic.
4. Fix the CWD-dependent bundled path issue.
5. Apply the same design to agents (`read_agent_file` / `write_agent_file`).

## Non-Goals

- Changing the SKILL.md file format or frontmatter schema.
- Changing the skill/agent loading from markdown files.
- Auto-migration of existing instance-only skills.
- Supporting more than two skill layers (instance + bundled).

## Design

### Layer model

Skills are resolved from two directories in priority order:

| Priority | Layer | Path | Writable | Purpose |
|----------|-------|------|----------|---------|
| 1 (high) | Instance | `config.skills.directory` → `~/.rustfox/skills/` | ✅ Yes | User/custom skills created by the agent |
| 2 (low)  | Bundled  | `<cwd>/skills/` | ❌ No  | Read-only templates shipped with the project |

**Shadow semantics:** Instance layer shadows bundled. A skill named `"thread-writer"`
in the instance dir completely replaces one with the same name in the bundled dir.
The bundled original remains on disk but is invisible to the agent.

### SkillRegistry changes

```rust
pub enum SkillSource {
    Instance,
    Bundled,
}

pub struct SkillRegistry {
    instance_skills: HashMap<String, Skill>,
    bundled_skills: HashMap<String, Skill>,
    /// Maps skill name → absolute base directory for read_skill_file resolution.
    skill_base_dirs: HashMap<String, PathBuf>,
}

impl SkillRegistry {
    /// Instance shadows bundled.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.instance_skills.get(name)
            .or_else(|| self.bundled_skills.get(name))
    }

    /// Returns the source directory for a skill (used by read_skill_file).
    pub fn base_dir(&self, name: &str) -> Option<&Path> {
        self.skill_base_dirs.get(name).map(|p| p.as_path())
    }

    /// All unique skills (instance names shadow bundled).
    pub fn list(&self) -> Vec<&Skill> { /* ... */ }
}
```

### Config changes

```rust
pub struct SkillsConfig {
    /// Instance skills directory (user/custom, writable).
    pub directory: PathBuf,
    /// Bundled skills directory (read-only templates).
    /// Defaults to CWD-relative "./skills/". Can be overridden in config.toml.
    pub bundled_directory: PathBuf,
}
```

Defaults:
- `directory`: empty → resolved to `<home>/skills/` (unchanged)
- `bundled_directory`: `"./skills"` — CWD-relative, same as current `PathBuf::from("skills")`

Resolution in `Config::resolve()`: `bundled_directory` is preserved as-is (CWD-relative),
matching the existing convention. Users who run from a non-project CWD can set it explicitly:

```toml
[skills]
bundled_directory = "/opt/RustFox/skills"
```

### Agent tool resolution

**`read_skill_file(skill_name, relative_path)`:**
1. Validate skill name and relative path (unchanged `validate_skill_name` + `validate_skill_path`)
2. Look up `skill_name` in `skill_base_dirs` → get the absolute base directory
3. Construct target: `<base_dir>/<skill_name>/<relative_path>`
4. Canonicalize target + `starts_with(base_dir)` escape check
5. Read and return content
6. If skill is not in registry (e.g., race condition), fall back to checking instance dir first, then bundled dir

**`write_skill_file(skill_name, relative_path, content)`:**
1. Validate skill name and relative path (unchanged)
2. Target path: `<config.skills.directory>/<skill_name>/<relative_path>` (always instance dir)
3. Create parent dirs, write file
4. Reload the single skill into the instance layer of the registry

**`reload_skills`:**
1. `load_skills_from_dir(instance_dir, SkillSource::Instance)`
2. `load_skills_from_dir(bundled_dir, SkillSource::Bundled)`
3. Merge into registry (instance shadows bundled)

### Startup and update flow

**Startup (`main.rs`):**
1. `seed_dir_if_empty(bundled_path, instance_path)` — unchanged, seeds bundled skills into instance on first run
2. Load instance skills → load bundled skills → merge into `SkillRegistry`
3. Lock files: seed both `skills-lock.json` and `agents-lock.json`

**`/update-skills`:**
1. `update_skills(bundled_skills, instance_skills, lock_path)` — unchanged sync logic
2. `update_skills(bundled_agents, instance_agents, lock_path)` — unchanged
3. Reload both layers

**Custom skills created by agent:**
- Written directly to instance dir via `write_skill_file`
- Reloaded individually into `instance_skills` map
- Never touched by seeding or update (only bundled→instance sync touches instance)

### Agents (parallel design)

The same layering applies to agents:

| Layer | Path | Writable |
|-------|------|----------|
| Instance | `config.agents.directory` → `~/.rustfox/agents/` | ✅ Yes |
| Bundled  | `<cwd>/agents/` | ❌ No |

`read_agent_file`, `write_agent_file` follow identical logic using the agent registry.

## Files changed

| File | Change |
|------|--------|
| `src/skills/mod.rs` | `SkillSource` enum, `SkillRegistry` gains `instance_skills` / `bundled_skills` / `skill_base_dirs` |
| `src/skills/loader.rs` | `load_skills_from_dir` takes `SkillSource` parameter; returns skills keyed by source |
| `src/config.rs` | `SkillsConfig.bundled_directory`, `AgentsConfig.bundled_directory` fields |
| `src/home.rs` | No change (bundled path is CWD-relative, not home-resolved) |
| `src/agent.rs` | `read_skill_file` resolves via `skill_base_dirs`; `write_skill_file` unchanged; `reload_skills` loads both layers |
| `src/agent.rs` | `read_agent_file` / `write_agent_file` parallel changes |
| `src/main.rs` | Load bundled skills at startup; pass both layers to Agent |
| `src/learning.rs` | `patch_skill` uses instance dir (unchanged) |
| `src/platform/telegram.rs` | `/update-skills` reloads both layers after sync |

## Testing

- **Unit tests:** `test_skill_registry_instance_shadows_bundled`, `test_read_skill_file_resolves_from_instance_first`, `test_write_skill_file_always_writes_to_instance`, `test_bundled_skills_untouched_by_write`
- **Integration test:** Verify that after `/update-skills`, bundled skills remain in bundled dir and instance skills are untouched
