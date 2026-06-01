# Multi-Directory Skill Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support two skill layers — instance (custom/writable, `~/.rustfox/skills/`) and bundled (read-only templates, `./skills/`) — with instance shadowing bundled on name collision.

**Architecture:** `SkillRegistry` gains two internal maps (`instance_skills` + `bundled_skills`) and a `skill_base_dirs` lookup. `load_skills_from_dir` takes a `SkillSource` enum to tag skills. Agent tools (`read_skill_file`, `write_skill_file`, etc.) resolve via the registry's source tracking. Config adds `bundled_directory` fields for skills and agents.

**Tech Stack:** Rust, tokio, serde, `PathBuf` path resolution

---

### Task 1: Add `SkillSource` and refactor `SkillRegistry`

**Files:**
- Modify: `src/skills/mod.rs`

- [ ] **Step 1: Add `SkillSource` enum and refactor `SkillRegistry`**

Add `SkillSource` and replace the single `skills: HashMap` with two maps plus a `skill_base_dirs` lookup:

```rust
// Add to src/skills/mod.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Instance,
    Bundled,
}

#[derive(Debug, Clone)]
pub struct SkillRegistry {
    instance_skills: HashMap<String, Skill>,
    bundled_skills: HashMap<String, Skill>,
    /// Maps skill name → absolute base directory for read_skill_file path resolution.
    /// Uses the source directory (e.g. ~/.rustfox/skills/ or /project/skills/).
    skill_base_dirs: HashMap<String, PathBuf>,
}
```

- [ ] **Step 2: Update `SkillRegistry` methods**

```rust
impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            instance_skills: HashMap::new(),
            bundled_skills: HashMap::new(),
            skill_base_dirs: HashMap::new(),
        }
    }

    /// Register a skill with its source and base directory.
    pub fn register(&mut self, skill: Skill, source: SkillSource, base_dir: PathBuf) {
        let name = skill.name.clone();
        match source {
            SkillSource::Instance => {
                self.instance_skills.insert(name.clone(), skill);
            }
            SkillSource::Bundled => {
                self.bundled_skills.insert(name.clone(), skill);
            }
        }
        self.skill_base_dirs.insert(name.clone(), base_dir);
        info!("Registered skill: {} ({:?})", name, source);
    }

    /// Instance shadows bundled.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.instance_skills
            .get(name)
            .or_else(|| self.bundled_skills.get(name))
    }

    /// Returns the source directory for a skill (used by read_skill_file).
    pub fn base_dir(&self, name: &str) -> Option<&Path> {
        self.skill_base_dirs.get(name).map(|p| p.as_path())
    }

    /// All unique skills (instance shadows bundled, so only instance names appear for duplicates).
    pub fn list(&self) -> Vec<&Skill> {
        let mut all: Vec<&Skill> = self.instance_skills.values().collect();
        for skill in self.bundled_skills.values() {
            if !self.instance_skills.contains_key(&skill.name) {
                all.push(skill);
            }
        }
        all
    }

    pub fn len(&self) -> usize {
        let mut names = std::collections::HashSet::new();
        for name in self.instance_skills.keys() {
            names.insert(name);
        }
        for name in self.bundled_skills.keys() {
            names.insert(name);
        }
        names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instance_skills.is_empty() && self.bundled_skills.is_empty()
    }
}
```

Add the `use std::path::PathBuf;` import at the top of the file.

- [ ] **Step 3: Update `build_context` and `build_agents_context` to iterate both maps**

The `build_context` and `build_agents_context` methods iterate `self.skills.values()`. Change them to iterate all unique skills using `self.list()`:

```rust
pub fn build_context(&self) -> String {
    let unique_skills = self.list();
    if unique_skills.is_empty() {
        return String::new();
    }
    // ... rest of the method using `unique_skills` instead of `self.skills.values()`
}

pub fn build_agents_context(&self) -> String {
    let unique_agents = self.list();
    // ... same change
}
```

- [ ] **Step 4: Update tests**

The existing test helper `make_skill` stays unchanged. Update all `registry.register(make_skill(...))` calls to pass the new `source` and `base_dir` parameters:

```rust
// In tests:
registry.register(
    make_skill("my-skill", "Does things", "content", None),
    SkillSource::Instance,
    PathBuf::from("/tmp/instance-skills"),
);
```

Add an import for `SkillSource` and `PathBuf` in the test module.

- [ ] **Step 5: Run tests to verify compilation and existing tests pass**

Run: `cargo test test_build_context -v`
Expected: Tests pass (after fixing all `register` calls)

- [ ] **Step 6: Commit**

```bash
git add src/skills/mod.rs
git commit -m "feat(skills): add SkillSource and refactor SkillRegistry with instance/bundled layers"
```

---

### Task 2: Update `load_skills_from_dir` to accept `SkillSource`

**Files:**
- Modify: `src/skills/loader.rs`

- [ ] **Step 1: Add `SkillSource` and `base_dir` parameter**

```rust
use super::{Skill, SkillRegistry, SkillSource};
use std::path::{Path, PathBuf};

pub async fn load_skills_from_dir(
    dir: &Path,
    source: SkillSource,
    base_dir: PathBuf,
) -> Result<SkillRegistry> {
    let mut registry = SkillRegistry::new();
    // ... existing logic, but pass source and base_dir to register:
    // registry.register(skill, source, base_dir.clone());
    // ... rest unchanged
}
```

The `base_dir` parameter is the root skills directory (e.g. `~/.rustfox/skills/` or `/project/skills/`). This is what gets stored in `skill_base_dirs` for `read_skill_file` resolution.

- [ ] **Step 2: Update `register` call inside `load_skills_from_dir`**

Change:
```rust
registry.register(skill)
```
To:
```rust
registry.register(skill, source, base_dir.clone())
```

- [ ] **Step 3: Update all callers of `load_skills_from_dir`**

These are in:
- `src/agent.rs` lines 148 and 152 (`reload_skills_and_agents`)
- `src/agent.rs` lines 1741 and 1901 (`reload_skills` / `reload_agents` tool handlers)
- `src/main.rs` lines 133 and 137 (startup loading)
- `src/platform/telegram.rs` line 247 (`reload_skills_and_agents` after update — this one goes through `reload_skills_and_agents` which is handled separately)

But actually, `reload_skills_and_agents` is a single method in `agent.rs` that calls `load_skills_from_dir`. And the tool handlers (`reload_skills`, `reload_agents`) also call it directly.

We'll fix all callers in a later task. For now just update the function signature.

- [ ] **Step 4: Update the test in loader.rs that calls `load_skills_from_dir`**

```rust
let skills = load_skills_from_dir(dir.path(), SkillSource::Instance, dir.path().to_path_buf()).await.unwrap();
```

- [ ] **Step 5: Run tests**

Run: `cargo test test_skill_with_supervisor_block_loads_workflow_hint -v`
Expected: Fails because callers not yet updated (expected; we'll fix them next)

- [ ] **Step 6: Commit**

```bash
git add src/skills/loader.rs
git commit -m "feat(skills): add SkillSource and base_dir to load_skills_from_dir"
```

---

### Task 3: Add `bundled_directory` config fields

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Add `bundled_directory` to `SkillsConfig` and `AgentsConfig`**

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct SkillsConfig {
    #[serde(default)]
    pub directory: PathBuf,
    /// Bundled skills directory (read-only templates, default CWD-relative ./skills/).
    #[serde(default = "default_bundled_skills_dir")]
    pub bundled_directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default)]
    pub directory: PathBuf,
    /// Bundled agents directory (read-only templates, default CWD-relative ./agents/).
    #[serde(default = "default_bundled_agents_dir")]
    pub bundled_directory: PathBuf,
}
```

Add default functions:
```rust
fn default_bundled_skills_dir() -> PathBuf {
    PathBuf::from("skills")
}

fn default_bundled_agents_dir() -> PathBuf {
    PathBuf::from("agents")
}
```

- [ ] **Step 2: Update default constructors**

```rust
fn default_skills_config() -> SkillsConfig {
    SkillsConfig {
        directory: PathBuf::new(),
        bundled_directory: default_bundled_skills_dir(),
    }
}

fn default_agents_config() -> AgentsConfig {
    AgentsConfig {
        directory: PathBuf::new(),
        bundled_directory: default_bundled_agents_dir(),
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add bundled_directory to SkillsConfig and AgentsConfig"
```

---

### Task 4: Update agent tools for multi-directory resolution

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add a helper to resolve skill/agent file paths via registry**

Add a method to `Agent` that looks up a skill name in the registry and returns the correct base directory:

```rust
/// Resolve the base directory for a skill/agent by checking the registry.
/// Falls back to the configured directory if not found (for newly-created skills).
fn resolve_skill_base_dir(&self, name: &str, config_dir: &Path, skills_lock: &SkillRegistry) -> PathBuf {
    skills_lock
        .base_dir(name)
        .unwrap_or(config_dir)
        .to_path_buf()
}
```

- [ ] **Step 2: Update `read_skill_file` tool (lines 1654-1698)**

The current code constructs `target` using `self.config.skills.directory` directly. Change it to use the registry:

```rust
"read_skill_file" => {
    // ... name/path validation unchanged ...

    // Resolve via registry (instance shadows bundled)
    let skills_lock = self.skills.read().await;
    let base_dir = self.resolve_skill_base_dir(&skill_name, &self.config.skills.directory, &skills_lock);
    let target = base_dir.join(&skill_name).join(&relative_path);

    // Canonicalize check against the resolved base dir (not the config dir)
    if let Ok(base_canonical) = base_dir.canonicalize() {
        if let Ok(target_canonical) = target.canonicalize() {
            if !target_canonical.starts_with(&base_canonical) {
                return format!("Access denied: path '{}' resolves outside the skills directory", target.display());
            }
        }
    }

    // ... read unchanged ...
}
```

- [ ] **Step 3: Update `write_skill_file` tool (lines 1700-1738)**

`write_skill_file` always writes to the instance directory. The current code already uses `self.config.skills.directory` which is the instance dir. **No change needed** for the write path. But add a reload step after writing:

```rust
"write_skill_file" => {
    // ... validation and write unchanged ...

    match tokio::fs::write(&target, &content).await {
        Ok(()) => {
            info!("Skill file written: {}", target.display());

            // Reload single skill into registry so it's immediately available
            let skill_dir = target.parent().and_then(|p| p.parent()).unwrap_or(&target);
            if let Ok(skill_registry) = crate::skills::loader::load_skills_from_dir(
                skill_dir,
                crate::skills::SkillSource::Instance,
                self.config.skills.directory.clone(),
            ).await {
                if let Some(skill) = skill_registry.list().into_iter().next() {
                    // Merge this single skill into the existing registry
                    let mut skills = self.skills.write().await;
                    skills.register(skill.clone(), crate::skills::SkillSource::Instance, self.config.skills.directory.clone());
                }
            }

            format!("Written: {}", target.display())
        }
        Err(e) => format!("Failed to write skill file: {}", e),
    }
}
```

Actually, this is overly complex. A simpler approach: after write, call `load_skills_from_dir` on the instance directory to reload the entire instance layer, preserving the bundled layer:

```rust
// After writing, reload just the instance layer
let instance_dir = self.config.skills.directory.clone();
if let Ok(new_instance) = crate::skills::loader::load_skills_from_dir(
    &instance_dir,
    crate::skills::SkillSource::Instance,
    instance_dir,
).await {
    let mut skills = self.skills.write().await;
    // Replace only instance skills, keep bundled
    skills.instance_skills = new_instance.instance_skills;
    // Also update base_dirs for instance skills
    for name in skills.instance_skills.keys() {
        skills.skill_base_dirs.insert(name.clone(), instance_dir.clone());
    }
}
```

- [ ] **Step 4: Update `read_agent_file` tool (lines 1817-1858)**

Same pattern as `read_skill_file` — resolve via agents registry:

```rust
"read_agent_file" => {
    // ... name/path validation unchanged ...

    let agents_lock = self.agents.read().await;
    let base_dir = self.resolve_skill_base_dir(&agent_name, &self.config.agents.directory, &agents_lock);
    let target = base_dir.join(&agent_name).join(&relative_path);

    // ... canonicalize check against base_dir ...
    // ... read unchanged ...
}
```

- [ ] **Step 5: Update `write_agent_file` tool (lines 1860-1898)**

Always writes to instance dir — no path change needed. Add the same reload-after-write pattern as `write_skill_file`.

- [ ] **Step 6: Update `reload_skills` tool (line 1739-1751)**

Change to reload **both** layers:

```rust
"reload_skills" => {
    use crate::skills::loader::load_skills_from_dir;
    use crate::skills::SkillSource;

    let instance_dir = self.config.skills.directory.clone();
    let bundled_dir = self.config.skills.bundled_directory.clone();

    let instance_reg = load_skills_from_dir(&instance_dir, SkillSource::Instance, instance_dir).await;
    let bundled_reg = load_skills_from_dir(&bundled_dir, SkillSource::Bundled, bundled_dir).await;

    let mut skills = self.skills.write().await;
    // Clear and rebuild from both layers
    if let Ok(reg) = instance_reg {
        skills.instance_skills = reg.instance_skills;
        for (k, v) in &reg.skill_base_dirs {
            skills.skill_base_dirs.insert(k.clone(), v.clone());
        }
    }
    if let Ok(reg) = bundled_reg {
        skills.bundled_skills = reg.bundled_skills;
        for (k, v) in &reg.skill_base_dirs {
            // Don't overwrite instance entries
            skills.skill_base_dirs.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    let count = skills.len();
    info!("Skills reloaded: {} skill(s) active ({:?} instance, {:?} bundled)",
        count, skills.instance_skills.len(), skills.bundled_skills.len());
    format!("Skills reloaded. {} skill(s) now active.", count)
}
```

- [ ] **Step 7: Update `reload_agents` tool (line 1899-1909)**

Same pattern as `reload_skills` but using agents config:

```rust
"reload_agents" => {
    use crate::skills::loader::load_skills_from_dir;
    use crate::skills::SkillSource;

    let instance_dir = self.config.agents.directory.clone();
    let bundled_dir = self.config.agents.bundled_directory.clone();

    let instance_reg = load_skills_from_dir(&instance_dir, SkillSource::Instance, instance_dir).await;
    let bundled_reg = load_skills_from_dir(&bundled_dir, SkillSource::Bundled, bundled_dir).await;

    let mut agents = self.agents.write().await;
    // ... same merge logic as reload_skills ...
    // ... format result ...
}
```

- [ ] **Step 8: Update `reload_skills_and_agents` (line 144-157)**

```rust
pub async fn reload_skills_and_agents(&self) -> (usize, usize) {
    use crate::skills::loader::load_skills_from_dir;
    use crate::skills::SkillSource;

    // Skills: load both layers, merge
    let s_instance_dir = self.config.skills.directory.clone();
    let s_bundled_dir = self.config.skills.bundled_directory.clone();
    let s_instance = load_skills_from_dir(&s_instance_dir, SkillSource::Instance, s_instance_dir.clone()).await;
    let s_bundled = load_skills_from_dir(&s_bundled_dir, SkillSource::Bundled, s_bundled_dir.clone()).await;

    {
        let mut skills = self.skills.write().await;
        if let Ok(reg) = s_instance {
            skills.instance_skills = reg.instance_skills;
            for (k, v) in &reg.skill_base_dirs {
                skills.skill_base_dirs.insert(k.clone(), v.clone());
            }
        }
        if let Ok(reg) = s_bundled {
            skills.bundled_skills = reg.bundled_skills;
            for (k, v) in &reg.skill_base_dirs {
                skills.skill_base_dirs.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let s_count = self.skills.read().await.len();

    // Agents: same pattern
    let a_instance_dir = self.config.agents.directory.clone();
    let a_bundled_dir = self.config.agents.bundled_directory.clone();
    let a_instance = load_skills_from_dir(&a_instance_dir, SkillSource::Instance, a_instance_dir.clone()).await;
    let a_bundled = load_skills_from_dir(&a_bundled_dir, SkillSource::Bundled, a_bundled_dir.clone()).await;

    {
        let mut agents = self.agents.write().await;
        if let Ok(reg) = a_instance {
            agents.instance_skills = reg.instance_skills;
            for (k, v) in &reg.skill_base_dirs {
                agents.skill_base_dirs.insert(k.clone(), v.clone());
            }
        }
        if let Ok(reg) = a_bundled {
            agents.bundled_skills = reg.bundled_skills;
            for (k, v) in &reg.skill_base_dirs {
                agents.skill_base_dirs.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let a_count = self.agents.read().await.len();

    (s_count, a_count)
}
```

- [ ] **Step 9: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): update skill/agent tools for multi-directory resolution"
```

---

### Task 5: Update startup to load both layers

**Files:**
- Modify: `src/main.rs`
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Update `main.rs` startup loading (lines 133-138)**

Change:
```rust
let skills = load_skills_from_dir(&config.skills.directory).await?;
let agents = load_skills_from_dir(&config.agents.directory).await?;
```
To:
```rust
use rustfox::skills::SkillSource;

let mut skills = load_skills_from_dir(
    &config.skills.directory,
    SkillSource::Instance,
    config.skills.directory.clone(),
).await?;
let bundled_skills = load_skills_from_dir(
    &config.skills.bundled_directory,
    SkillSource::Bundled,
    config.skills.bundled_directory.clone(),
).await?;
// Merge bundled into skills registry
for skill in bundled_skills.list() {
    skills.register(
        skill.clone(),
        SkillSource::Bundled,
        config.skills.bundled_directory.clone(),
    );
}

let mut agents = load_skills_from_dir(
    &config.agents.directory,
    SkillSource::Instance,
    config.agents.directory.clone(),
).await?;
let bundled_agents = load_skills_from_dir(
    &config.agents.bundled_directory,
    SkillSource::Bundled,
    config.agents.bundled_directory.clone(),
).await?;
for agent in bundled_agents.list() {
    agents.register(
        agent.clone(),
        SkillSource::Bundled,
        config.agents.bundled_directory.clone(),
    );
}
```

- [ ] **Step 2: Verify `Agent::new` call still works**

The `Agent::new` signature hasn't changed (still takes `skills: SkillRegistry` and `agents: SkillRegistry`). The merged registry is passed as-is. No change needed.

- [ ] **Step 3: `src/platform/telegram.rs` — `/update-skills` uses `reload_skills_and_agents` already**

Lines 247-248 call `agent.reload_skills_and_agents().await` which we already updated in Task 4 Step 8. No changes needed in telegram.rs beyond what was already done.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): load bundled skills at startup alongside instance skills"
```

---

### Task 6: Update learning module

**Files:**
- Modify: `src/learning.rs`

- [ ] **Step 1: Update `patch_skill` to always use instance dir**

The `patch_skill` function in `src/learning.rs` uses `config.skills.directory` to find the skill directory. This is already the instance dir, which is correct — `patch_skill` should only modify instance skills, never bundled ones.

Verify at lines 194-261 that the path resolution uses `config.skills.directory` (instance). It does — no change needed.

But after patching, it reloads the skill into the registry. Check that it uses `registry.register()` with the old (single-map) API and update it:

```rust
// In patch_skill / self_patch_skill, change:
// skills.write().await.register(...)  -- old single-map API
// To:
skills.write().await.register(skill, SkillSource::Instance, config.skills.directory.clone());
```

- [ ] **Step 2: Verify test compilation**

Run: `cargo test test_self_patch_skill -v`
Expected: Pass

- [ ] **Step 3: Commit**

```bash
git add src/learning.rs
git commit -m "fix(learning): update patch_skill to use SkillSource::Instance"
```

---

### Task 7: Add tests for multi-directory skills

**Files:**
- Add tests in: `src/skills/mod.rs` (existing test module)
- Add tests in: `src/agent.rs` (existing test module)

- [ ] **Step 1: Add unit test for instance-shadow-bundled**

In `src/skills/mod.rs` tests:

```rust
#[test]
fn test_instance_shadows_bundled() {
    let mut registry = SkillRegistry::new();
    registry.register(
        make_skill("duplicate", "Instance version", "instance content", None),
        SkillSource::Instance,
        PathBuf::from("/instance"),
    );
    registry.register(
        make_skill("duplicate", "Bundled version", "bundled content", None),
        SkillSource::Bundled,
        PathBuf::from("/bundled"),
    );

    // get() should return instance version
    let skill = registry.get("duplicate").unwrap();
    assert_eq!(skill.content, "instance content");
    assert_eq!(skill.description, "Instance version");

    // base_dir should return instance path
    assert_eq!(registry.base_dir("duplicate").unwrap(), Path::new("/instance"));

    // list() should only include the instance version (not duplicate)
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["duplicate"]);
    assert_eq!(registry.len(), 1);
}
```

- [ ] **Step 2: Add unit test for unique-only skills**

```rust
#[test]
fn test_unique_skills_from_both_layers() {
    let mut registry = SkillRegistry::new();
    registry.register(
        make_skill("alpha", "Instance only", "", None),
        SkillSource::Instance,
        PathBuf::from("/instance"),
    );
    registry.register(
        make_skill("beta", "Bundled only", "", None),
        SkillSource::Bundled,
        PathBuf::from("/bundled"),
    );

    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert_eq!(registry.len(), 2);
}
```

- [ ] **Step 3: Add integration test for read_skill_file resolution**

In `src/agent.rs` tests, add a test that verifies `read_skill_file` searches instance first:

```rust
#[tokio::test]
async fn test_read_skill_file_checks_instance_before_bundled() {
    let dir = tempfile::tempdir().unwrap();
    let instance_dir = dir.path().join("instance-skills");
    let bundled_dir = dir.path().join("bundled-skills");

    // Create same-named skill in both dirs with different content
    tokio::fs::create_dir_all(instance_dir.join("my-skill")).await.unwrap();
    tokio::fs::write(instance_dir.join("my-skill/SKILL.md"), "instance content").await.unwrap();
    tokio::fs::create_dir_all(bundled_dir.join("my-skill")).await.unwrap();
    tokio::fs::write(bundled_dir.join("my-skill/SKILL.md"), "bundled content").await.unwrap();

    // Load both into registry
    let mut registry = SkillRegistry::new();
    let inst = load_skills_from_dir(&instance_dir, SkillSource::Instance, instance_dir.clone()).await.unwrap();
    let bund = load_skills_from_dir(&bundled_dir, SkillSource::Bundled, bundled_dir.clone()).await.unwrap();
    for s in inst.list() { registry.register(s.clone(), SkillSource::Instance, instance_dir.clone()); }
    for s in bund.list() { registry.register(s.clone(), SkillSource::Bundled, bundled_dir.clone()); }

    // read_skill_file should find instance version
    let base_dir = registry.base_dir("my-skill").unwrap();
    let target = base_dir.join("my-skill/SKILL.md");
    let content = tokio::fs::read_to_string(&target).await.unwrap();
    assert_eq!(content, "instance content", "instance must shadow bundled");
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add src/skills/mod.rs src/agent.rs
git commit -m "test(skills): add tests for multi-directory shadow semantics"
```

---

### Self-review checklist

- [ ] **Spec coverage:** Does every section of the design doc have a corresponding task?
  - `SkillSource` enum → Task 1
  - `SkillRegistry` refactor → Task 1
  - `load_skills_from_dir` signature change → Task 2
  - Config `bundled_directory` → Task 3
  - Agent tool resolution → Task 4
  - Startup loading → Task 5
  - `patch_skill` → Task 6
  - Tests → Task 7

- [ ] **Placeholder scan:** No TBD, TODO, "implement later", or similar.

- [ ] **Type consistency:** 
  - `SkillSource::Instance` and `SkillSource::Bundled` are used consistently
  - `SkillRegistry::register(skill, source, base_dir)` consistently called
  - `load_skills_from_dir(dir, source, base_dir)` signature matches across all callers
  - Config field `bundled_directory` consistent between `SkillsConfig` and `AgentsConfig`
  - `base_dir()` returns `Option<&Path>` consistently
