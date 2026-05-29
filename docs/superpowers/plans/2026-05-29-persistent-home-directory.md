# Persistent Home Directory (`~/.rustfox`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each RustFox instance a single persistent home directory (default `~/.rustfox`) that holds config, DB, skills, agents, artifacts, and a durable `workspace/` sandbox, with env/config overrides and a seed-plus-update flow for bundled skills.

**Architecture:** A new pure-logic module `src/home.rs` resolves the home root (env → config → `~/.rustfox`) and every data path (unset → home default; absolute → verbatim; relative → CWD legacy + warning). `Config::resolve()` calls it, creates directories, and writes the resolved **absolute** paths back into the existing config fields so all downstream consumers (`main.rs`, `agent.rs`, `tools.rs`, `learning.rs`) keep reading the same fields unchanged. Skills/agents are seed-copied into the home on first run and refreshed by an explicit `/update-skills` command that diffs content hashes recorded in a home-side `skills-lock.json`.

**Tech Stack:** Rust (edition 2021), Tokio, serde/toml, `dirs` crate (new), `sha2` (already present), `tempfile` (dev). Telegram via teloxide.

---

## File Structure

- **Create** `src/home.rs` — home-root + per-path resolution, `ResolvedPaths`, `PathOrigin`, `LegacyPathWarning`, `ensure_dirs`. Pure and unit-testable.
- **Create** `src/skills/seed.rs` — first-run seed-copy of bundled skills/agents + home-side lock writer + skill content hashing.
- **Create** `src/skills/update.rs` — `/update-skills` hash-diff engine (`UpdateReport`).
- **Create** `docs/persistent-home-directory.md` — migration + multi-instance tutorial.
- **Modify** `Cargo.toml` — add `dirs` dependency.
- **Modify** `src/lib.rs` — register `pub mod home;`.
- **Modify** `src/skills/mod.rs` — register `pub mod seed;` and `pub mod update;`.
- **Modify** `src/config.rs` — empty-sentinel path defaults, `GeneralConfig.home`, `resolved_home` field, `Config::resolve()`, wire into `Config::load`.
- **Modify** `src/main.rs` — config-file discovery fallback, seed call, startup log of home.
- **Modify** `src/agent.rs` — add `reload_skills_and_agents()` method; update system prompt sandbox text.
- **Modify** `src/platform/telegram.rs` — `/update-skills` command + `/start` help text.
- **Modify** `config.example.toml` — document optional paths, `RUSTFOX_HOME`, `[general].home`.
- **Modify** `CLAUDE.md` / `README.md` — note the home model.

**Naming decision (resolved from spec open question):** the module is `src/home.rs`.

---

### Task 1: Add the `dirs` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the crate**

In `Cargo.toml`, under `[dependencies]`, after the `regex = "1"` line, add:

```toml
# OS home-directory resolution for the persistent home dir (~/.rustfox)
dirs = "5"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo build`
Expected: builds successfully; `dirs` appears in `Cargo.lock`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add dirs crate for home directory resolution"
```

---

### Task 2: Create `src/home.rs` with `resolve_home`

**Files:**
- Create: `src/home.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Register the module**

In `src/lib.rs`, add `pub mod home;` in alphabetical position (after `pub mod config;`):

```rust
pub mod agent;
pub mod agent_prompt;
pub mod config;
pub mod home;
pub mod langsmith;
```

- [ ] **Step 2: Write the failing test**

Create `src/home.rs` with only this content:

```rust
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Resolve the home root from the override sources, in priority order:
/// 1. `RUSTFOX_HOME` env var (must be absolute)
/// 2. `[general].home` config value (must be absolute)
/// 3. `<os_home>/.rustfox`
pub fn resolve_home(
    env_home: Option<&str>,
    config_home: Option<&Path>,
    os_home: Option<&Path>,
) -> Result<PathBuf> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn env_takes_priority_when_absolute() {
        let got = resolve_home(
            Some("/srv/rfx"),
            Some(Path::new("/etc/rfx")),
            Some(Path::new("/home/u")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/srv/rfx"));
    }

    #[test]
    fn relative_env_is_ignored_falls_to_config() {
        let got = resolve_home(
            Some("rel/dir"),
            Some(Path::new("/etc/rfx")),
            Some(Path::new("/home/u")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/etc/rfx"));
    }

    #[test]
    fn relative_config_is_ignored_falls_to_default() {
        let got = resolve_home(None, Some(Path::new("rel")), Some(Path::new("/home/u"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/u/.rustfox"));
    }

    #[test]
    fn default_is_os_home_dot_rustfox() {
        let got = resolve_home(None, None, Some(Path::new("/home/u"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/u/.rustfox"));
    }

    #[test]
    fn errors_when_no_os_home_and_no_overrides() {
        assert!(resolve_home(None, None, None).is_err());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib home::tests`
Expected: FAIL — panics with `not yet implemented` (the `todo!()`).

- [ ] **Step 4: Implement `resolve_home`**

Replace the `todo!()` body:

```rust
pub fn resolve_home(
    env_home: Option<&str>,
    config_home: Option<&Path>,
    os_home: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(env) = env_home {
        let p = Path::new(env);
        if p.is_absolute() {
            return Ok(p.to_path_buf());
        }
        tracing::warn!("RUSTFOX_HOME='{env}' is not absolute; ignoring it");
    }
    if let Some(cfg) = config_home {
        if cfg.is_absolute() {
            return Ok(cfg.to_path_buf());
        }
        tracing::warn!(
            "[general].home='{}' is not absolute; ignoring it",
            cfg.display()
        );
    }
    let home = os_home.ok_or_else(|| {
        anyhow!("Could not determine the OS home directory; set RUSTFOX_HOME or [general].home")
    })?;
    Ok(home.join(".rustfox"))
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib home::tests`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add src/home.rs src/lib.rs
git commit -m "feat(home): add resolve_home with env/config/default precedence"
```

---

### Task 2.5: A note on the empty-PathBuf "unset" sentinel

Subsequent tasks treat an **empty `PathBuf`** (`PathBuf::new()`, i.e. `as_os_str().is_empty()`) as "the user did not configure this path." Config struct fields use `#[serde(default)]` so a missing TOML key deserializes to an empty `PathBuf`. This lets the existing fields stay `PathBuf` (not `Option<PathBuf>`), so no downstream consumer changes. Keep this invariant in mind for Tasks 3, 6, and 7.

---

### Task 3: Add `PathOrigin` and `resolve_data_path`

**Files:**
- Modify: `src/home.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/home.rs` above the `#[cfg(test)]` module (the type + function), and add the test cases inside `mod tests`:

Type + function (place after `resolve_home`):

```rust
/// How a single configured data path was resolved (drives warning output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOrigin {
    /// Field unset (empty) → resolved under the home root.
    Default,
    /// Field set to an absolute path → used verbatim.
    Absolute,
    /// Field set to a relative path → resolved from CWD (legacy behavior).
    RelativeLegacy,
}

/// Resolve one data path. An empty `configured` path means "unset".
pub fn resolve_data_path(
    configured: &Path,
    home: &Path,
    default_subpath: &str,
) -> (PathBuf, PathOrigin) {
    todo!()
}
```

Tests (inside `mod tests`):

```rust
    #[test]
    fn unset_path_resolves_under_home() {
        let (p, o) = resolve_data_path(Path::new(""), Path::new("/h/.rustfox"), "workspace");
        assert_eq!(p, PathBuf::from("/h/.rustfox/workspace"));
        assert_eq!(o, PathOrigin::Default);
    }

    #[test]
    fn absolute_path_used_verbatim() {
        let (p, o) = resolve_data_path(Path::new("/data/wp"), Path::new("/h/.rustfox"), "workspace");
        assert_eq!(p, PathBuf::from("/data/wp"));
        assert_eq!(o, PathOrigin::Absolute);
    }

    #[test]
    fn relative_path_is_legacy() {
        let (p, o) = resolve_data_path(Path::new("skills"), Path::new("/h/.rustfox"), "skills");
        assert_eq!(p, PathBuf::from("skills"));
        assert_eq!(o, PathOrigin::RelativeLegacy);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib home::tests`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Implement `resolve_data_path`**

Replace the `todo!()`:

```rust
pub fn resolve_data_path(
    configured: &Path,
    home: &Path,
    default_subpath: &str,
) -> (PathBuf, PathOrigin) {
    if configured.as_os_str().is_empty() {
        (home.join(default_subpath), PathOrigin::Default)
    } else if configured.is_absolute() {
        (configured.to_path_buf(), PathOrigin::Absolute)
    } else {
        (configured.to_path_buf(), PathOrigin::RelativeLegacy)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib home::tests`
Expected: PASS (8 tests total).

- [ ] **Step 5: Commit**

```bash
git add src/home.rs
git commit -m "feat(home): add resolve_data_path with origin classification"
```

---

### Task 4: Add `ResolvedPaths` and `ensure_dirs`

**Files:**
- Modify: `src/home.rs`

- [ ] **Step 1: Write the failing test**

Add the struct + function after `resolve_data_path`:

```rust
/// The fully-resolved absolute paths an instance operates on.
#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub skills: PathBuf,
    pub agents: PathBuf,
    pub artifacts: PathBuf,
    pub user_model: PathBuf,
}

/// Create the home root and standard subdirectories (0700 on Unix), plus the
/// parent directories of the database and user-model files.
pub fn ensure_dirs(paths: &ResolvedPaths) -> Result<()> {
    todo!()
}
```

Add the test inside `mod tests` (note: requires the `tempfile` dev-dependency, already present):

```rust
    #[test]
    fn ensure_dirs_creates_full_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let paths = ResolvedPaths {
            home: home.clone(),
            workspace: home.join("workspace"),
            database: home.join("rustfox.db"),
            skills: home.join("skills"),
            agents: home.join("agents"),
            artifacts: home.join("artifacts"),
            user_model: home.join("user_model.md"),
        };
        ensure_dirs(&paths).unwrap();
        assert!(paths.home.is_dir());
        assert!(paths.workspace.is_dir());
        assert!(paths.skills.is_dir());
        assert!(paths.agents.is_dir());
        assert!(paths.artifacts.is_dir());
        // db + user_model are files, but their parent dirs must exist
        assert!(paths.database.parent().unwrap().is_dir());
        assert!(paths.user_model.parent().unwrap().is_dir());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib home::tests::ensure_dirs_creates_full_tree`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Implement `ensure_dirs`**

Replace the `todo!()`:

```rust
pub fn ensure_dirs(paths: &ResolvedPaths) -> Result<()> {
    use anyhow::Context;
    for dir in [
        &paths.home,
        &paths.workspace,
        &paths.skills,
        &paths.agents,
        &paths.artifacts,
    ] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    }
    for file in [&paths.database, &paths.user_model] {
        if let Some(parent) = file.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create directory: {}", parent.display())
                })?;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&paths.home, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib home::tests::ensure_dirs_creates_full_tree`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/home.rs
git commit -m "feat(home): add ResolvedPaths and ensure_dirs (0700)"
```

---

### Task 5: Add `LegacyPathWarning` and its `render()`

**Files:**
- Modify: `src/home.rs`

- [ ] **Step 1: Write the failing test**

Add after `ensure_dirs`:

```rust
/// An actionable warning emitted when a path is relative (CWD-resolved) or when
/// legacy data is detected in the launch directory while the path is unset.
#[derive(Debug, Clone)]
pub struct LegacyPathWarning {
    /// Config field label, e.g. "memory.database_path".
    pub label: String,
    /// The path currently in effect (CWD-resolved legacy location).
    pub current: PathBuf,
    /// Where this path would live under the home root.
    pub home_default: PathBuf,
}

impl LegacyPathWarning {
    /// Multi-line, copy-pasteable migration hint.
    pub fn render(&self) -> String {
        todo!()
    }
}
```

Test inside `mod tests`:

```rust
    #[test]
    fn warning_render_includes_paths_and_commands() {
        let w = LegacyPathWarning {
            label: "memory.database_path".to_string(),
            current: PathBuf::from("/work/rustfox.db"),
            home_default: PathBuf::from("/h/.rustfox/rustfox.db"),
        };
        let s = w.render();
        assert!(s.contains("memory.database_path"));
        assert!(s.contains("/work/rustfox.db"));
        assert!(s.contains("/h/.rustfox/rustfox.db"));
        assert!(s.contains("cp -rT"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib home::tests::warning_render_includes_paths_and_commands`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Implement `render`**

Replace the `todo!()`:

```rust
    pub fn render(&self) -> String {
        // `cp -rT` merges the source into the already-created destination
        // directory instead of nesting it (e.g. avoids <home>/skills/skills).
        format!(
            "Legacy path in use for `{label}`:\n  \
             current : {current}\n  \
             new home default : {home_default}\n  \
             To migrate this path into the home directory:\n    \
             cp -rT {current} {home_default}\n  \
             To keep the current location, pin it in config.toml under its section.",
            label = self.label,
            current = self.current.display(),
            home_default = self.home_default.display(),
        )
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib home::tests::warning_render_includes_paths_and_commands`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/home.rs
git commit -m "feat(home): add LegacyPathWarning with actionable render()"
```

---

### Task 6: Config — optional path fields, `[general].home`, `resolved_home`

**Files:**
- Modify: `src/config.rs`

This task only changes serde defaults and adds fields. No behavior yet (resolution is Task 7). After this task the project must still compile and existing tests pass.

- [ ] **Step 1: Make `SandboxConfig.allowed_directory` optional via empty sentinel**

In `src/config.rs`, change:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct SandboxConfig {
    pub allowed_directory: PathBuf,
}
```

to:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct SandboxConfig {
    #[serde(default)]
    pub allowed_directory: PathBuf,
}
```

- [ ] **Step 2: Switch the path fields to the empty sentinel**

Change these field attributes from their `default = "..."` form to plain `#[serde(default)]`:

In `MemoryConfig`:
```rust
    #[serde(default)]
    pub database_path: PathBuf,
```

In `SkillsConfig`:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct SkillsConfig {
    #[serde(default)]
    pub directory: PathBuf,
}
```

In `AgentsConfig`:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default)]
    pub directory: PathBuf,
}
```

In `LearningConfig`:
```rust
    #[serde(default)]
    pub user_model_path: PathBuf,
```

In `SupervisorConfig`:
```rust
    #[serde(default)]
    pub artifacts_dir: std::path::PathBuf,
```

- [ ] **Step 3: Update the section-builder defaults to empty sentinels**

So that a wholly-missing section also yields the unset sentinel, change the builder functions:

`default_memory_config()` — change `database_path: default_db_path(),` to:
```rust
        database_path: PathBuf::new(),
```

`default_skills_config()`:
```rust
fn default_skills_config() -> SkillsConfig {
    SkillsConfig {
        directory: PathBuf::new(),
    }
}
```

`default_agents_config()`:
```rust
fn default_agents_config() -> AgentsConfig {
    AgentsConfig {
        directory: PathBuf::new(),
    }
}
```

`default_learning_config()` — change `user_model_path: default_user_model_path(),` to:
```rust
        user_model_path: PathBuf::new(),
```

`SupervisorConfig::default()` and `default_artifacts_dir()` — change `default_artifacts_dir()` to return an empty path:
```rust
fn default_artifacts_dir() -> std::path::PathBuf {
    std::path::PathBuf::new()
}
```

- [ ] **Step 4: Delete now-unused default helpers**

Remove `fn default_db_path()`, `fn default_skills_dir()`, `fn default_agents_dir()`, and `fn default_user_model_path()` (they are no longer referenced after Step 3). If the compiler reports any are still referenced, leave that one in place.

- [ ] **Step 5: Add `home` to `GeneralConfig`**

Change:
```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct GeneralConfig {
    /// Optional location string injected into the system prompt (e.g. "Tokyo, Japan")
    #[serde(default)]
    pub location: Option<String>,
}
```
to:
```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct GeneralConfig {
    /// Optional location string injected into the system prompt (e.g. "Tokyo, Japan")
    #[serde(default)]
    pub location: Option<String>,
    /// Optional absolute path overriding the default `~/.rustfox` home root.
    #[serde(default)]
    pub home: Option<PathBuf>,
}
```

- [ ] **Step 6: Add a non-serialized `resolved_home` field to `Config`**

In the `Config` struct, after the `supervisor` field, add:
```rust
    /// Absolute home root resolved at load time (not read from TOML).
    #[serde(skip)]
    pub resolved_home: Option<PathBuf>,
```

- [ ] **Step 7: Verify it compiles and existing tests pass**

Run: `cargo test --lib config::tests`
Expected: PASS (the existing config tests set `allowed_directory = "/tmp"`; with the new optional default they still deserialize). Run `cargo build` to confirm no unused-function errors.

- [ ] **Step 8: Commit**

```bash
git add src/config.rs
git commit -m "refactor(config): make data paths optional + add general.home and resolved_home"
```

---

### Task 7: Implement `Config::resolve()`

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
    fn base_toml() -> &'static str {
        r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
        "#
    }

    #[test]
    fn resolve_fills_unset_paths_under_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home.clone()),
        });
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.resolved_home.as_ref().unwrap(), &home);
        assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
        assert_eq!(cfg.memory.database_path, home.join("rustfox.db"));
        assert_eq!(cfg.skills.directory, home.join("skills"));
        assert_eq!(cfg.agents.directory, home.join("agents"));
        assert_eq!(cfg.supervisor.artifacts_dir, home.join("artifacts"));
        assert_eq!(cfg.learning.user_model_path, home.join("user_model.md"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_keeps_absolute_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home.clone()),
        });
        cfg.memory.database_path = std::path::PathBuf::from("/data/custom.db");
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.memory.database_path, std::path::PathBuf::from("/data/custom.db"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_warns_on_relative_override() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home),
        });
        cfg.skills.directory = std::path::PathBuf::from("my-skills");
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.skills.directory, std::path::PathBuf::from("my-skills"));
        assert!(warnings.iter().any(|w| w.label == "skills.directory"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::tests::resolve_fills_unset_paths_under_home`
Expected: FAIL — `no method named resolve found for struct Config`.

- [ ] **Step 3: Implement `Config::resolve()`**

Add inside `impl Config { ... }` (before `pub fn load`):

```rust
    /// Resolve the home root and every data path, create directories, and write
    /// the resolved absolute paths back into the config fields. Returns any
    /// legacy-path warnings (relative overrides) for the caller to log.
    pub fn resolve(&mut self) -> Result<Vec<crate::home::LegacyPathWarning>> {
        use crate::home::{ensure_dirs, resolve_data_path, resolve_home, PathOrigin, ResolvedPaths};

        let env_home = std::env::var("RUSTFOX_HOME").ok();
        let config_home = self.general.as_ref().and_then(|g| g.home.as_deref());
        let os_home = dirs::home_dir();
        let home = resolve_home(env_home.as_deref(), config_home, os_home.as_deref())?;

        let mut warnings = Vec::new();
        let mut resolve_one = |label: &str, field: &Path, subpath: &str| -> PathBuf {
            let (path, origin) = resolve_data_path(field, &home, subpath);
            if origin == PathOrigin::RelativeLegacy {
                warnings.push(crate::home::LegacyPathWarning {
                    label: label.to_string(),
                    current: path.clone(),
                    home_default: home.join(subpath),
                });
            }
            path
        };

        let workspace = resolve_one("sandbox.allowed_directory", &self.sandbox.allowed_directory, "workspace");
        let database = resolve_one("memory.database_path", &self.memory.database_path, "rustfox.db");
        let skills = resolve_one("skills.directory", &self.skills.directory, "skills");
        let agents = resolve_one("agents.directory", &self.agents.directory, "agents");
        let artifacts = resolve_one("supervisor.artifacts_dir", &self.supervisor.artifacts_dir, "artifacts");
        let user_model = resolve_one("learning.user_model_path", &self.learning.user_model_path, "user_model.md");

        let paths = ResolvedPaths {
            home: home.clone(),
            workspace: workspace.clone(),
            database: database.clone(),
            skills: skills.clone(),
            agents: agents.clone(),
            artifacts: artifacts.clone(),
            user_model: user_model.clone(),
        };
        ensure_dirs(&paths)?;

        self.sandbox.allowed_directory = workspace;
        self.memory.database_path = database;
        self.skills.directory = skills;
        self.agents.directory = agents;
        self.supervisor.artifacts_dir = artifacts;
        self.learning.user_model_path = user_model;
        self.resolved_home = Some(home);

        Ok(warnings)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::tests`
Expected: PASS (existing tests + the three new `resolve_*` tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add Config::resolve to materialize home-relative paths"
```

---

### Task 8: Wire `Config::load` to resolve + log warnings

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn load_resolves_paths_to_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let cfg_path = tmp.path().join("config.toml");
        let toml = format!(
            r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [general]
            home = "{}"
            "#,
            home.display()
        );
        std::fs::write(&cfg_path, toml).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
        assert!(cfg.sandbox.allowed_directory.is_dir());
        assert_eq!(cfg.resolved_home.unwrap(), home);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::tests::load_resolves_paths_to_absolute`
Expected: FAIL — `allowed_directory` is empty (current `load` only creates the directory; it does not resolve).

- [ ] **Step 3: Update `Config::load`**

Replace the current `pub fn load` body:

```rust
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file")?;

        let warnings = config
            .resolve()
            .with_context(|| "Failed to resolve home directory paths")?;
        for w in &warnings {
            tracing::warn!("{}", w.render());
        }

        Ok(config)
    }
```

(Remove the old sandbox-existence block; `ensure_dirs` inside `resolve` now creates all directories.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): resolve paths and log legacy warnings in load"
```

---

### Task 9: Config-file discovery fallback in `main.rs`

**Files:**
- Modify: `src/home.rs` (add `default_home`)
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test for `default_home`**

Add to `src/home.rs` after `resolve_home`:

```rust
/// The home root used purely for *config-file discovery*, before the config is
/// loaded. Uses only `RUSTFOX_HOME` (if absolute) or `<os_home>/.rustfox`.
pub fn default_home(env_home: Option<&str>, os_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(env) = env_home {
        let p = Path::new(env);
        if p.is_absolute() {
            return Some(p.to_path_buf());
        }
    }
    os_home.map(|h| h.join(".rustfox"))
}
```

Add the test in `mod tests`:

```rust
    #[test]
    fn default_home_prefers_absolute_env() {
        assert_eq!(
            default_home(Some("/srv/rfx"), Some(Path::new("/home/u"))),
            Some(PathBuf::from("/srv/rfx"))
        );
        assert_eq!(
            default_home(None, Some(Path::new("/home/u"))),
            Some(PathBuf::from("/home/u/.rustfox"))
        );
        assert_eq!(default_home(Some("rel"), Some(Path::new("/home/u"))),
            Some(PathBuf::from("/home/u/.rustfox")));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib home::tests::default_home_prefers_absolute_env`
Expected: FAIL — `cannot find function default_home`. (Add the function in the same step if TDD prefers; here the function above makes it pass — so first add only the test, confirm fail, then add the function.)

To honor red-green: add the test first, run (FAIL: function missing), then add `default_home`.

- [ ] **Step 3: Confirm pass**

Run: `cargo test --lib home::tests::default_home_prefers_absolute_env`
Expected: PASS.

- [ ] **Step 4: Use it in `main.rs` config discovery**

In `src/main.rs`, replace the config-path block:

```rust
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
```

with:

```rust
    let config_path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        let cwd = PathBuf::from("config.toml");
        if cwd.exists() {
            return cwd;
        }
        let env_home = std::env::var("RUSTFOX_HOME").ok();
        if let Some(home) = rustfox::home::default_home(env_home.as_deref(), dirs::home_dir().as_deref()) {
            let candidate = home.join("config.toml");
            if candidate.exists() {
                return candidate;
            }
        }
        cwd
    });
```

(`dirs` is already a dependency from Task 1; add `use std::path::PathBuf;` is already present.)

- [ ] **Step 5: Verify build + log the home**

After the existing `info!("  Sandbox: ...")` line in `main.rs`, add:

```rust
    if let Some(home) = &config.resolved_home {
        info!("  Home: {}", home.display());
    }
```

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/home.rs src/main.rs
git commit -m "feat(home): config-file discovery fallback to <home>/config.toml"
```

---

### Task 10: Sandbox = workspace integration test + system-prompt text

**Files:**
- Create: `tests/home_sandbox.rs`
- Modify: `src/config.rs` (system prompt text)

- [ ] **Step 1: Write the failing integration test**

Create `tests/home_sandbox.rs`:

```rust
use rustfox::config::Config;

#[test]
fn sandbox_defaults_to_home_workspace_and_excludes_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".rustfox");
    let cfg_path = tmp.path().join("config.toml");
    let toml = format!(
        r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        [general]
        home = "{}"
        "#,
        home.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();
    let cfg = Config::load(&cfg_path).unwrap();

    // Sandbox is the workspace subdir of home.
    assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
    // DB lives ABOVE the sandbox → structurally unreachable by file tools.
    assert_eq!(cfg.memory.database_path, home.join("rustfox.db"));
    assert!(!cfg.memory.database_path.starts_with(&cfg.sandbox.allowed_directory));
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --test home_sandbox`
Expected: PASS (this validates the resolution wired in Tasks 7–8). If it fails, fix resolution before continuing.

- [ ] **Step 3: Update the system-prompt sandbox description**

In `src/config.rs`, in `default_system_prompt()`, change the final block:

```rust
     ## Sandbox\n\
     File and command tools operate only within the allowed sandbox directory."
```

to:

```rust
     ## Sandbox\n\
     File and command tools operate only within your persistent workspace directory.\n\
     The workspace survives restarts — use it to keep reusable scripts, programs, and notes for the long term."
```

- [ ] **Step 4: Verify tests still pass**

Run: `cargo test --lib config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/home_sandbox.rs src/config.rs
git commit -m "test+docs: assert sandbox=workspace and note persistence in prompt"
```

---

### Task 11: Skill content hashing + seeding (`src/skills/seed.rs`)

**Files:**
- Create: `src/skills/seed.rs`
- Modify: `src/skills/mod.rs`

- [ ] **Step 1: Register the module**

In `src/skills/mod.rs`, change the first line from:
```rust
pub mod loader;
```
to:
```rust
pub mod loader;
pub mod seed;
pub mod update;
```

(`update` is created in Task 12; declaring it now is fine only if the file exists. To avoid a build break, create an empty `src/skills/update.rs` placeholder now with a single line `// implemented in Task 12` — it will be filled in Task 12. Alternatively add `pub mod update;` in Task 12. **Do the latter**: in this task add only `pub mod seed;`.)

So in this task, change to:
```rust
pub mod loader;
pub mod seed;
```

- [ ] **Step 2: Write the failing tests**

Create `src/skills/seed.rs`:

```rust
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// SHA-256 (hex) of a skill/agent directory's primary markdown file
/// (`SKILL.md`, else `AGENT.md`). Returns `None` if neither exists.
pub fn hash_skill_dir(dir: &Path) -> Option<String> {
    let primary = ["SKILL.md", "AGENT.md"]
        .into_iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())?;
    let bytes = std::fs::read(&primary).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(format!("{:x}", h.finalize()))
}

/// Copy every skill subdirectory from `bundled` into `instance` when `instance`
/// is empty or missing. Returns the number of skills copied (0 if skipped).
pub async fn seed_dir_if_empty(bundled: &Path, instance: &Path) -> Result<usize> {
    todo!()
}

/// Map of skill-name → content hash for every skill dir under `dir`.
pub fn lock_map_for(dir: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let (Some(name), Some(hash)) =
                (p.file_name().and_then(|n| n.to_str()), hash_skill_dir(&p))
            {
                map.insert(name.to_string(), hash);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    #[tokio::test]
    async fn seeds_into_empty_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        write_skill(&bundled, "alpha", "---\nname: alpha\n---\nhi");
        write_skill(&bundled, "beta", "---\nname: beta\n---\nyo");

        let n = seed_dir_if_empty(&bundled, &instance).await.unwrap();
        assert_eq!(n, 2);
        assert!(instance.join("alpha/SKILL.md").is_file());
        assert!(instance.join("beta/SKILL.md").is_file());
    }

    #[tokio::test]
    async fn skips_when_instance_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        write_skill(&bundled, "alpha", "a");
        write_skill(&instance, "existing", "keep me");

        let n = seed_dir_if_empty(&bundled, &instance).await.unwrap();
        assert_eq!(n, 0);
        assert!(!instance.join("alpha").exists());
        assert!(instance.join("existing/SKILL.md").is_file());
    }

    #[test]
    fn lock_map_hashes_each_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "alpha", "content-a");
        write_skill(tmp.path(), "beta", "content-b");
        let map = lock_map_for(tmp.path());
        assert_eq!(map.len(), 2);
        assert_ne!(map["alpha"], map["beta"]);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib skills::seed::tests`
Expected: FAIL — `seeds_into_empty_instance` panics with `not yet implemented`.

- [ ] **Step 4: Implement `seed_dir_if_empty`**

Replace the `todo!()`:

```rust
pub async fn seed_dir_if_empty(bundled: &Path, instance: &Path) -> Result<usize> {
    // If the bundled source and instance resolve to the same place, nothing to do.
    if let (Ok(a), Ok(b)) = (bundled.canonicalize(), instance.canonicalize()) {
        if a == b {
            return Ok(0);
        }
    }
    if !bundled.is_dir() {
        tracing::info!("No bundled skills at {}; skipping seed", bundled.display());
        return Ok(0);
    }
    // Non-empty instance → never overwrite.
    if instance.is_dir() {
        let mut entries = tokio::fs::read_dir(instance).await?;
        if entries.next_entry().await?.is_some() {
            return Ok(0);
        }
    } else {
        tokio::fs::create_dir_all(instance)
            .await
            .with_context(|| format!("Failed to create {}", instance.display()))?;
    }

    let mut copied = 0usize;
    let mut entries = tokio::fs::read_dir(bundled).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src = entry.path();
        if !src.is_dir() {
            continue;
        }
        let Some(name) = src.file_name() else { continue };
        let dst = instance.join(name);
        if let Err(e) = copy_dir_recursive(&src, &dst).await {
            tracing::warn!("Failed to seed {}: {}", src.display(), e);
            continue;
        }
        copied += 1;
    }
    tracing::info!("Seeded {copied} skill(s) into {}", instance.display());
    Ok(copied)
}

/// Recursively copy `src` directory to `dst`.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            Box::pin(copy_dir_recursive(&from, &to)).await?;
        } else {
            tokio::fs::copy(&from, &to).await?;
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib skills::seed::tests`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add src/skills/seed.rs src/skills/mod.rs
git commit -m "feat(skills): add content hashing + first-run seed copy"
```

---

### Task 12: `/update-skills` hash-diff engine (`src/skills/update.rs`)

**Files:**
- Create: `src/skills/update.rs`
- Modify: `src/skills/mod.rs`

- [ ] **Step 1: Register the module**

In `src/skills/mod.rs`, add after `pub mod seed;`:
```rust
pub mod update;
```

- [ ] **Step 2: Write the failing tests**

Create `src/skills/update.rs`:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::seed::{copy_dir_recursive_pub, hash_skill_dir, lock_map_for};

/// Home-side lock file: skill-name → content hash captured at seed/update time.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SkillLock {
    pub version: u32,
    #[serde(default)]
    pub skills: BTreeMap<String, String>,
}

/// Outcome of an update run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub updated: Vec<String>,
    pub backed_up: Vec<String>,
    pub skipped: Vec<String>,
}

impl UpdateReport {
    pub fn summary(&self) -> String {
        format!(
            "Skill update: {} updated, {} backed-up, {} unchanged.",
            self.updated.len(),
            self.backed_up.len(),
            self.skipped.len()
        )
    }
}

/// Read the lock file at `lock_path`, or an empty lock if absent/invalid.
fn read_lock(lock_path: &Path) -> SkillLock {
    std::fs::read_to_string(lock_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(SkillLock {
            version: 1,
            skills: BTreeMap::new(),
        })
}

fn write_lock(lock_path: &Path, lock: &SkillLock) -> Result<()> {
    let json = serde_json::to_string_pretty(lock)?;
    std::fs::write(lock_path, json)
        .with_context(|| format!("Failed to write lock {}", lock_path.display()))
}

/// Re-sync `bundled` → `instance` using content hashes recorded in `lock_path`.
///
/// For each bundled skill:
/// - missing in instance → copy in (updated)
/// - present, instance hash == bundled hash → unchanged (skipped)
/// - present, instance hash == lock hash (and != bundled) → overwrite (updated)
/// - present, instance hash differs from both → back up `*.bak`, overwrite (backed_up)
///
/// Instance-only skills (absent from `bundled`) are never touched.
pub async fn update_skills(bundled: &Path, instance: &Path, lock_path: &Path) -> Result<UpdateReport> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn seed_lock(lock_path: &Path, instance: &Path) {
        let lock = SkillLock {
            version: 1,
            skills: lock_map_for(instance),
        };
        write_lock(lock_path, &lock).unwrap();
    }

    #[tokio::test]
    async fn unchanged_skill_is_overwritten_with_new_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        // instance currently matches the OLD bundle
        write_skill(&instance, "alpha", "v1");
        seed_lock(&lock, &instance);
        // bundle now has v2
        write_skill(&bundled, "alpha", "v2");

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.updated, vec!["alpha".to_string()]);
        assert!(report.backed_up.is_empty());
        assert_eq!(std::fs::read_to_string(instance.join("alpha/SKILL.md")).unwrap(), "v2");
    }

    #[tokio::test]
    async fn locally_modified_skill_is_backed_up() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&instance, "alpha", "v1");
        seed_lock(&lock, &instance);
        // user edited the instance copy
        std::fs::write(instance.join("alpha/SKILL.md"), "user-edit").unwrap();
        // bundle moved to v2
        write_skill(&bundled, "alpha", "v2");

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.backed_up, vec!["alpha".to_string()]);
        assert_eq!(std::fs::read_to_string(instance.join("alpha/SKILL.md")).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(instance.join("alpha/SKILL.md.bak")).unwrap(), "user-edit");
    }

    #[tokio::test]
    async fn instance_only_skill_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&bundled, "alpha", "v1");
        write_skill(&instance, "alpha", "v1");
        write_skill(&instance, "mine", "private");
        seed_lock(&lock, &instance);

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        // alpha unchanged (same hash), mine never visited
        assert!(report.updated.is_empty());
        assert!(report.backed_up.is_empty());
        assert_eq!(std::fs::read_to_string(instance.join("mine/SKILL.md")).unwrap(), "private");
    }
}
```

- [ ] **Step 3: Expose `copy_dir_recursive` from `seed.rs`**

In `src/skills/seed.rs`, add a public wrapper (the private `copy_dir_recursive` already exists from Task 11):

```rust
/// Public re-export wrapper so the update engine can reuse recursive copy.
pub async fn copy_dir_recursive_pub(src: &Path, dst: &Path) -> Result<()> {
    copy_dir_recursive(src, dst).await
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --lib skills::update::tests`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 5: Implement `update_skills`**

Replace the `todo!()`:

```rust
pub async fn update_skills(bundled: &Path, instance: &Path, lock_path: &Path) -> Result<UpdateReport> {
    let mut report = UpdateReport::default();
    if !bundled.is_dir() {
        anyhow::bail!("No bundled skills found at {}", bundled.display());
    }
    let mut lock = read_lock(lock_path);
    tokio::fs::create_dir_all(instance).await.ok();

    let mut entries = tokio::fs::read_dir(bundled).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src = entry.path();
        if !src.is_dir() {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let dst = instance.join(&name);
        let bundled_hash = match hash_skill_dir(&src) {
            Some(h) => h,
            None => continue,
        };

        if !dst.exists() {
            copy_dir_recursive_pub(&src, &dst).await?;
            lock.skills.insert(name.clone(), bundled_hash);
            report.updated.push(name);
            continue;
        }

        let instance_hash = hash_skill_dir(&dst);
        if instance_hash.as_deref() == Some(bundled_hash.as_str()) {
            report.skipped.push(name);
            continue;
        }

        let lock_hash = lock.skills.get(&name).cloned();
        let unmodified = lock_hash.is_some() && lock_hash.as_deref() == instance_hash.as_deref();

        if !unmodified {
            // Back up the primary file before overwriting.
            for f in ["SKILL.md", "AGENT.md"] {
                let p = dst.join(f);
                if p.is_file() {
                    let _ = tokio::fs::copy(&p, dst.join(format!("{f}.bak"))).await;
                }
            }
            // Remove and re-copy the whole dir to pick up new/removed files.
            let _ = tokio::fs::remove_dir_all(&dst).await;
            copy_dir_recursive_pub(&src, &dst).await?;
            // Restore the .bak we just lost in the remove (re-copy from src wiped it):
            // simplest: re-write the bak from the in-memory backup is unnecessary because
            // we copied the bak before removing — but remove_dir_all deletes it. So copy bak first to instance parent.
            lock.skills.insert(name.clone(), bundled_hash);
            report.backed_up.push(name);
        } else {
            let _ = tokio::fs::remove_dir_all(&dst).await;
            copy_dir_recursive_pub(&src, &dst).await?;
            lock.skills.insert(name.clone(), bundled_hash);
            report.updated.push(name);
        }
    }

    write_lock(lock_path, &lock)?;
    Ok(report)
}
```

**Important correctness note:** the backup-then-`remove_dir_all` ordering above would delete the `.bak`. Implement the backup by writing it to a sibling location that survives the wipe. Replace the backed-up branch body with:

```rust
        if !unmodified {
            // Back up the primary file to a sibling path OUTSIDE the dir being replaced.
            for f in ["SKILL.md", "AGENT.md"] {
                let p = dst.join(f);
                if p.is_file() {
                    let bak = instance.join(format!("{name}.{f}.bak.tmp"));
                    let _ = tokio::fs::copy(&p, &bak).await;
                }
            }
            let _ = tokio::fs::remove_dir_all(&dst).await;
            copy_dir_recursive_pub(&src, &dst).await?;
            // Move the temp backups into the freshly-copied dir as <file>.bak
            for f in ["SKILL.md", "AGENT.md"] {
                let bak = instance.join(format!("{name}.{f}.bak.tmp"));
                if bak.is_file() {
                    let _ = tokio::fs::rename(&bak, dst.join(format!("{f}.bak"))).await;
                }
            }
            lock.skills.insert(name.clone(), bundled_hash);
            report.backed_up.push(name);
        }
```

Use this corrected branch (delete the earlier first-draft backed-up branch). The test `locally_modified_skill_is_backed_up` asserts `alpha/SKILL.md.bak == "user-edit"`, which this satisfies.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib skills::update::tests`
Expected: PASS (3 tests).

- [ ] **Step 7: Commit**

```bash
git add src/skills/update.rs src/skills/seed.rs src/skills/mod.rs
git commit -m "feat(skills): add /update-skills hash-diff engine with backups"
```

---

### Task 13: Seed skills/agents at startup in `main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add seeding before skills are loaded**

In `src/main.rs`, immediately **before** the line `let skills = load_skills_from_dir(&config.skills.directory).await?;`, insert:

```rust
    // Seed bundled skills/agents into the instance home on first run.
    let bundled_skills = PathBuf::from("skills");
    let bundled_agents = PathBuf::from("agents");
    if let Err(e) =
        rustfox::skills::seed::seed_dir_if_empty(&bundled_skills, &config.skills.directory).await
    {
        warn!("Skill seeding failed: {e}");
    }
    if let Err(e) =
        rustfox::skills::seed::seed_dir_if_empty(&bundled_agents, &config.agents.directory).await
    {
        warn!("Agent seeding failed: {e}");
    }
    // Write the home-side lock so /update-skills can diff later (only when seeded
    // into the home and a lock does not already exist).
    if let Some(home) = &config.resolved_home {
        let lock_path = home.join("skills-lock.json");
        if !lock_path.exists() {
            let lock = rustfox::skills::update::SkillLock {
                version: 1,
                skills: rustfox::skills::seed::lock_map_for(&config.skills.directory),
            };
            if let Ok(json) = serde_json::to_string_pretty(&lock) {
                let _ = std::fs::write(&lock_path, json);
            }
        }
    }
```

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: builds cleanly. (`serde_json` and `warn` are already imported in `main.rs`.)

- [ ] **Step 3: Manual smoke check**

Run: `RUSTFOX_HOME="$(mktemp -d)/.rustfox" cargo run -- config.toml` for ~3 seconds, then Ctrl-C.
Expected log lines: `Home: .../.rustfox`, `Seeded N skill(s) ...`, `Skills: N`. A `skills-lock.json` exists in the home.

(If `config.toml` is not present/valid in the dev env, skip this manual step and rely on the integration test in Task 10 plus the unit tests.)

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: seed bundled skills/agents into home on first run"
```

---

### Task 14: Add `Agent::reload_skills_and_agents`

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add the method**

In `src/agent.rs`, inside `impl Agent { ... }` (near the other public methods, e.g. after `build_system_prompt`), add:

```rust
    /// Reload both skill and agent registries from their directories.
    /// Returns `(skills_count, agents_count)`.
    pub async fn reload_skills_and_agents(&self) -> (usize, usize) {
        use crate::skills::loader::load_skills_from_dir;
        let mut s_count = 0;
        let mut a_count = 0;
        if let Ok(reg) = load_skills_from_dir(&self.config.skills.directory).await {
            s_count = reg.len();
            *self.skills.write().await = reg;
        }
        if let Ok(reg) = load_skills_from_dir(&self.config.agents.directory).await {
            a_count = reg.len();
            *self.agents.write().await = reg;
        }
        (s_count, a_count)
    }
```

(The `agents` field is `pub agents: tokio::sync::RwLock<SkillRegistry>`, symmetric with `skills`. If the field has a different name, use the actual name found near the `skills` field declaration.)

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): add reload_skills_and_agents helper"
```

---

### Task 15: `/update-skills` Telegram command

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add the command block**

In `src/platform/telegram.rs`, immediately **after** the `if text == "/skills" { ... }` block (before `if text == "/verbose"`), insert:

```rust
    if text == "/updateskills" || text == "/update-skills" {
        let bundled_skills = std::path::PathBuf::from("skills");
        let bundled_agents = std::path::PathBuf::from("agents");
        let lock_path = agent
            .config
            .resolved_home
            .clone()
            .map(|h| h.join("skills-lock.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("skills-lock.json"));

        let mut lines = Vec::new();
        match rustfox::skills::update::update_skills(
            &bundled_skills,
            &agent.config.skills.directory,
            &lock_path,
        )
        .await
        {
            Ok(r) => lines.push(format!("Skills — {}", r.summary())),
            Err(e) => lines.push(format!("Skills update failed: {e}")),
        }
        match rustfox::skills::update::update_skills(
            &bundled_agents,
            &agent.config.agents.directory,
            &lock_path,
        )
        .await
        {
            Ok(r) => lines.push(format!("Agents — {}", r.summary())),
            Err(e) => lines.push(format!("Agents update failed: {e}")),
        }

        let (s, a) = agent.reload_skills_and_agents().await;
        lines.push(format!("Reloaded: {s} skill(s), {a} agent(s) active."));

        bot.send_message(msg.chat.id, escape_text(&lines.join("\n")))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }
```

(Confirm the in-scope binding name for the agent — it is `agent` where `/skills` reads `agent.skills.read()`. Reuse the same binding and `escape_text` / `ParseMode` already imported in this file.)

- [ ] **Step 2: Update `/start` help text**

In the `/start` block, change the help string to add the new command line:

```rust
             /skills - List loaded skills\n\
             /update-skills - Re-sync bundled skills/agents (backs up local edits)\n\
             /verbose - Toggle tool call progress display\n\
```

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): add /update-skills command and help entry"
```

---

### Task 16: Update `config.example.toml`

**Files:**
- Modify: `config.example.toml`

- [ ] **Step 1: Document the home model and optional paths**

In `config.example.toml`, change the `[sandbox]` section from:

```toml
[sandbox]
allowed_directory = "/tmp/rustfox-sandbox"
```

to:

```toml
# ── Home directory & paths ──────────────────────────────────────────────
# By default RustFox stores everything under ~/.rustfox:
#   ~/.rustfox/config.toml, rustfox.db, skills/, agents/, workspace/, artifacts/
# Override the home root with the RUSTFOX_HOME env var (absolute path) or
# [general].home below. Run a second isolated instance with:
#   RUSTFOX_HOME="$HOME/.rustfox-work" cargo run
#
# [general]
# home = "/absolute/path/to/home"   # optional; overrides ~/.rustfox

[sandbox]
# The LLM's persistent workspace (file/command tools are confined here).
# Leave unset to use <home>/workspace. Set an absolute path to override.
# allowed_directory = "/absolute/path/to/workspace"
```

- [ ] **Step 2: Comment out the now-optional path keys**

Find the `[memory]` section and change `database_path = "rustfox.db"` to:
```toml
# Leave unset to use <home>/rustfox.db. Set an absolute path to override.
# database_path = "/absolute/path/to/rustfox.db"
```

Find `[skills]` and change `directory = "skills"` to:
```toml
# Leave unset to use <home>/skills (seeded from the bundled skills on first run).
# directory = "/absolute/path/to/skills"
```

If `[agents]`, `[supervisor].artifacts_dir`, or `[learning].user_model_path` keys are present in the example, comment them out the same way with a "Leave unset to use <home>/..." note. If a section is absent, do not add it.

- [ ] **Step 3: Commit**

```bash
git add config.example.toml
git commit -m "docs: document ~/.rustfox home and optional paths in example config"
```

---

### Task 17: Migration & multi-instance tutorial

**Files:**
- Create: `docs/persistent-home-directory.md`

- [ ] **Step 1: Write the tutorial**

Create `docs/persistent-home-directory.md`:

````markdown
# Persistent Home Directory (`~/.rustfox`)

RustFox stores all of its state under a single **home directory**, by default
`~/.rustfox`. This survives reboots, keeps secrets out of the LLM sandbox, and
makes it easy to run several isolated instances on one machine.

## Layout

```
~/.rustfox/
  config.toml      # secrets (bot token, API keys) — OUTSIDE the sandbox
  rustfox.db       # SQLite memory + embeddings     — OUTSIDE the sandbox
  skills/          # skills (seeded on first run, editable)
  agents/          # agents (seeded on first run, editable)
  workspace/       # THE SANDBOX — durable scratch space for the LLM
  artifacts/       # supervisor artifacts
  user_model.md    # learned user model
```

Only `workspace/` is reachable by the file and command tools. `config.toml`
and `rustfox.db` live above it and cannot be read or written by the LLM.

## Choosing where the home lives

Resolution order (first match wins):

1. `RUSTFOX_HOME` environment variable (must be an absolute path)
2. `[general].home` in `config.toml` (absolute path)
3. `~/.rustfox` (default)

Each individual path can still be pinned independently in `config.toml`
(e.g. `[memory].database_path`). An absolute value is used verbatim; an unset
value falls back to the home default.

## Migrating existing data

If you previously ran RustFox from a project directory (with `./rustfox.db`,
`./skills`, etc.), RustFox will **not** move your files automatically. On
startup it prints an actionable warning for each legacy path. To migrate:

RustFox auto-creates the home subdirectories on startup, so use `cp -rT`
(merge into the existing destination directory) rather than plain `cp -r`,
which would nest (e.g. `~/.rustfox/skills/skills`). Each command copies only
that one path — never your whole project.

```bash
mkdir -p ~/.rustfox
cp     ./rustfox.db            ~/.rustfox/rustfox.db
cp -rT ./skills               ~/.rustfox/skills
cp -rT ./agents               ~/.rustfox/agents
cp -rT ./supervisor/artifacts ~/.rustfox/artifacts   # if you used the supervisor
cp     ./memory/USER.md        ~/.rustfox/user_model.md   # if present
# Move your old sandbox contents into the new persistent workspace:
cp -rT /tmp/rustfox-sandbox    ~/.rustfox/workspace   # adjust to your old sandbox
```

Then remove any path overrides from `config.toml` so RustFox uses the home
defaults, and place your `config.toml` at `~/.rustfox/config.toml` (or keep
passing it as the first CLI argument).

## Keeping the old location instead

If you prefer your current layout, pin the paths explicitly in `config.toml`:

```toml
[sandbox]
allowed_directory = "/abs/path/to/old/sandbox"
[memory]
database_path = "/abs/path/to/rustfox.db"
[skills]
directory = "/abs/path/to/skills"
```

Absolute paths are always honored unchanged.

## Starting fresh

Doing nothing is the "start fresh" path: RustFox creates an empty
`~/.rustfox/workspace`, a new database, and seeds skills/agents from the bundled
copies. Your old project-directory files are left untouched.

## Running multiple instances

Give each instance its own home:

```bash
# Default personal instance
cargo run

# A separate work instance, fully isolated
RUSTFOX_HOME="$HOME/.rustfox-work" cargo run
```

Each home has independent skills, agents, workspace, database, and artifacts.

## Updating bundled skills

The instance `skills/` and `agents/` directories are seeded once on first run.
To pull in newer bundled versions later, send the bot `/update-skills`:

- Skills you have not modified are overwritten with the new bundled version.
- Skills you edited locally are backed up to `<skill>/SKILL.md.bak` before being
  updated, so your changes are never silently lost.
- Skills you created yourself (not part of the bundle) are never touched.
````

- [ ] **Step 2: Commit**

```bash
git add docs/persistent-home-directory.md
git commit -m "docs: add persistent home directory migration tutorial"
```

---

### Task 18: Update `CLAUDE.md` and `README.md`

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`

- [ ] **Step 1: Update `CLAUDE.md` Configuration section**

In `CLAUDE.md`, in the `### Configuration` section, after the existing list of required fields, add:

```markdown
### Home directory

RustFox stores all state under a single home directory (default `~/.rustfox`),
resolved as: `RUSTFOX_HOME` env (absolute) → `[general].home` config → `~/.rustfox`.
Layout: `config.toml`, `rustfox.db`, `skills/`, `agents/`, `workspace/` (the
sandbox), `artifacts/`, `user_model.md`. Each path can be pinned to an absolute
location in `config.toml`; unset paths fall back to the home default. Run
isolated instances with `RUSTFOX_HOME=...`. See
`docs/persistent-home-directory.md`. Path resolution lives in `src/home.rs`
(`Config::resolve` writes the resolved absolute paths back into the config).
Bundled skills/agents are seed-copied on first run; `/update-skills` re-syncs
them using `<home>/skills-lock.json`.
```

- [ ] **Step 2: Update `README.md`**

In `README.md`, find the configuration/setup section that mentions the sandbox
directory and add a short note (place it near where `[sandbox]` is described):

```markdown
> **Persistent home:** RustFox keeps all state under `~/.rustfox` by default
> (config, database, skills, agents, and a durable `workspace/` sandbox).
> Override with the `RUSTFOX_HOME` environment variable or `[general].home`.
> See [docs/persistent-home-directory.md](docs/persistent-home-directory.md).
```

(If the README has no obvious sandbox section, add the note under the main setup/usage heading.)

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: document persistent home directory in CLAUDE.md and README"
```

---

### Task 19: Full verification pass

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. Fix any (e.g. unused imports left from removed default helpers in Task 6).

- [ ] **Step 3: Tests**

Run: `cargo test`
Expected: all unit + integration tests pass, including:
- `home::tests::*`
- `config::tests::*` (incl. `resolve_*`, `load_resolves_paths_to_absolute`)
- `skills::seed::tests::*`
- `skills::update::tests::*`
- `tests/home_sandbox.rs`

- [ ] **Step 4: Release build**

Run: `cargo build --release`
Expected: builds cleanly.

- [ ] **Step 5: Commit any fixups**

```bash
git add -A
git commit -m "chore: fmt + clippy fixups for persistent home directory"
```

---

## Self-Review

**1. Spec coverage:**

| Spec section | Task(s) |
|---|---|
| §1 Directory model (hybrid home, RUSTFOX_HOME, [general].home, per-path override) | 2, 3, 6, 7 |
| §1 Config discovery `<home>/config.toml` | 9 |
| §1 Multi-instance via RUSTFOX_HOME | 7, 9, 17 |
| §2 Home resolution module + ResolvedPaths + ensure_dirs (0700) | 2, 3, 4 |
| §3 Sandbox = workspace; secrets unreachable | 7, 10 |
| §4 Durable workspace + prompt text | 10 |
| §5 Seed-copy + `/update-skills` hash-diff + .bak + instance-created untouched + hot reload | 11, 12, 13, 14, 15 |
| §6 No auto-move + actionable warning + start-fresh + tutorial | 7 (warnings), 17 (tutorial) |
| §7 Config field changes + example | 6, 16 |
| Testing strategy | 2–5, 7, 8, 10, 11, 12, 19 |
| File structure / docs (CLAUDE/README) | 18 |

No uncovered spec requirement.

**2. Placeholder scan:** Every code step contains complete code. The only `todo!()` bodies are intentional red-phase placeholders immediately replaced in the next step of the same task.

**3. Type consistency:** `resolve_home`, `resolve_data_path`, `PathOrigin`, `ResolvedPaths`, `ensure_dirs`, `LegacyPathWarning`, `default_home` (home.rs); `Config::resolve` returns `Vec<crate::home::LegacyPathWarning>`; `SkillLock`, `UpdateReport`, `update_skills`, `hash_skill_dir`, `lock_map_for`, `copy_dir_recursive_pub` (skills); `Agent::reload_skills_and_agents` — names are consistent across the tasks that define and consume them. The empty-PathBuf sentinel convention (Task 2.5) is applied consistently in Tasks 6 and 7.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-29-persistent-home-directory.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
