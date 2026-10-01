use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

const DEFAULT_SOUL: &str = "\
---
name: soul
version: 1
---
# Soul

## Who I Am
I'm RustFox, a Telegram AI assistant. I use tools to help the user.

## My Values
- Be genuinely helpful, not performatively helpful
- Have opinions; disagree when warranted
- Earn trust through competence

## My Boundaries
- Private things stay private
- Never send half-baked replies
- I'm a guest in the user's life

## Continuity
Each session I wake up fresh. These files _are_ my memory.
I read them at start, update them at end.
";

const DEFAULT_AGENTS: &str = "\
---
name: agents
version: 1
---
# Agent Memory

## What I've Learned
(Updated by the AI after each session.)

## Repeated Patterns
(Observed workflows, preferences, habits.)
";

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

/// Resolve the config file path to use at startup.
///
/// Lookup order:
/// 1. `env_config_path` if `Some`, used verbatim (no existence check — the
///    caller decides what to do with a missing file).
/// 2. `<cwd>/config.toml` if it exists.
/// 3. `<home>/config.toml` where `home` comes from `RUSTFOX_HOME` (absolute)
///    or `<os_home>/.rustfox`. Only returned if the candidate file exists.
/// 4. Falls back to `<cwd>/config.toml` (the wizard treats a non-existent
///    candidate as "no config found" and writes the first-run config there).
///
/// The function takes `cwd` and `os_home` explicitly (rather than reading
/// `std::env::current_dir()` internally) so it can be unit-tested without
/// mutating process state and without tests racing on a shared CWD.
pub fn resolve_config_path(
    env_config_path: Option<&str>,
    cwd: &Path,
    os_home: Option<&Path>,
) -> PathBuf {
    if let Some(p) = env_config_path {
        return PathBuf::from(p);
    }
    let cwd_candidate = cwd.join("config.toml");
    if cwd_candidate.exists() {
        return cwd_candidate;
    }
    if let Some(home) = default_home(None, os_home) {
        let home_candidate = home.join("config.toml");
        if home_candidate.exists() {
            return home_candidate;
        }
    }
    cwd_candidate
}

/// Where a resolved data path came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOrigin {
    /// Field was empty → materialized under the home root.
    Default,
    /// Absolute override → used verbatim.
    Absolute,
    /// Relative override → **rebased onto the home root** so it can never
    /// silently depend on the process CWD (issue #111, Part B). A
    /// `LegacyPathWarning` is emitted so the user can pin it explicitly.
    RebasedRelative,
}

/// Resolve a single configured data path.
///
/// - empty → `<home>/<default_subpath>` ([`PathOrigin::Default`])
/// - absolute → used verbatim ([`PathOrigin::Absolute`])
/// - relative → **rebased onto `home`** ([`PathOrigin::RebasedRelative`]).
///
/// The relative case used to return the value unchanged, which made it
/// relative to the process CWD — so running the binary from a different
/// directory silently opened a *different* database / workspace. Rebasing
/// onto `home` makes the location stable regardless of where the process was
/// launched from (issue #111, Part B).
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
        (home.join(configured), PathOrigin::RebasedRelative)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub skills: PathBuf,
    pub agents: PathBuf,
    pub artifacts: PathBuf,
    pub soul: PathBuf,       // SOUL.md
    pub agents_md: PathBuf,  // AGENTS.md
    pub user_model: PathBuf, // USER.md
}

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
    // Soul files live directly in the home dir; their parent is `home`, which
    // is already created above. Only the database file lives in a separate
    // subdirectory we need to make sure exists.
    for file in [
        &paths.database,
        &paths.soul,
        &paths.agents_md,
        &paths.user_model,
    ] {
        if let Some(parent) = file.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            }
        }
    }
    // Write default soul files if missing
    for (path, content) in [
        (&paths.soul, DEFAULT_SOUL),
        (&paths.agents_md, DEFAULT_AGENTS),
    ] {
        if !path.exists() {
            std::fs::write(path, content)
                .with_context(|| format!("Failed to write default {}", path.display()))?;
            tracing::info!("Created default soul file: {}", path.display());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&paths.home, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Emitted when a config path is **relative**. Since issue #111 such paths are
/// rebased onto the home root (they used to bind to the process CWD), so this
/// is a migration notice: where the path now lives, and how to pin it.
#[derive(Debug, Clone)]
pub struct LegacyPathWarning {
    /// Config field label, e.g. "memory.database_path".
    pub label: String,
    /// The relative value exactly as written in config.toml.
    pub configured: PathBuf,
    /// The absolute path it now resolves to (home-rebased).
    pub resolved: PathBuf,
    /// Where the *old* (CWD-relative) behavior would have resolved it, when a
    /// CWD is available. This is where pre-#111 data may still live, so the
    /// operator can move it. `None` if the CWD can't be read.
    pub legacy_cwd: Option<PathBuf>,
}

impl LegacyPathWarning {
    /// Multi-line, copy-pasteable migration hint.
    pub fn render(&self) -> String {
        let legacy_line = match &self.legacy_cwd {
            Some(p) if p != &self.resolved => format!(
                "\n  Previously (CWD-relative) it resolved to: {}\n                   If real data lives there, move it:  mv {} {}",
                p.display(),
                p.display(),
                self.resolved.display(),
            ),
            _ => String::new(),
        };
        format!(
            "Relative path in config for `{label}`: '{configured}'.\n  \
             It now resolves to: {resolved}{legacy_line}\n  \
             (Relative paths are home-relative, not CWD-relative — issue #111.)\n  \
             To make it explicit, pin the absolute path in config.toml under its section:\n    \
             {label} = \"{resolved}\"",
            label = self.label,
            configured = self.configured.display(),
            resolved = self.resolved.display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn env_takes_priority_when_absolute() {
        let got = resolve_home(
            Some("/abs/env/home"),
            Some(Path::new("/abs/cfg/home")),
            Some(Path::new("/os/home")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/abs/env/home"));
    }

    #[test]
    fn relative_env_is_ignored_falls_to_config() {
        let got = resolve_home(
            Some("rel/env"),
            Some(Path::new("/abs/cfg/home")),
            Some(Path::new("/os/home")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/abs/cfg/home"));
    }

    #[test]
    fn relative_config_is_ignored_falls_to_default() {
        let got = resolve_home(
            None,
            Some(Path::new("rel/cfg")),
            Some(Path::new("/os/home")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/os/home/.rustfox"));
    }

    #[test]
    fn default_is_os_home_dot_rustfox() {
        let got = resolve_home(None, None, Some(Path::new("/os/home"))).unwrap();
        assert_eq!(got, PathBuf::from("/os/home/.rustfox"));
    }

    #[test]
    fn errors_when_no_os_home_and_no_overrides() {
        let got = resolve_home(None, None, None);
        assert!(got.is_err());
    }

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
        assert_eq!(
            default_home(Some("rel"), Some(Path::new("/home/u"))),
            Some(PathBuf::from("/home/u/.rustfox"))
        );
    }

    #[test]
    fn unset_path_resolves_under_home() {
        let (path, origin) =
            resolve_data_path(Path::new(""), Path::new("/h/.rustfox"), "rustfox.db");
        assert_eq!(path, PathBuf::from("/h/.rustfox/rustfox.db"));
        assert_eq!(origin, PathOrigin::Default);
    }

    #[test]
    fn absolute_path_used_verbatim() {
        let (path, origin) = resolve_data_path(
            Path::new("/data/rustfox.db"),
            Path::new("/h/.rustfox"),
            "rustfox.db",
        );
        assert_eq!(path, PathBuf::from("/data/rustfox.db"));
        assert_eq!(origin, PathOrigin::Absolute);
    }

    /// Regression (issue #111, Part B): a relative override must be rebased
    /// onto the home root, *not* left relative to the process CWD.
    #[test]
    fn relative_path_is_rebased_onto_home() {
        let (path, origin) = resolve_data_path(
            Path::new("rustfox.db"),
            Path::new("/h/.rustfox"),
            "rustfox.db",
        );
        assert_eq!(path, PathBuf::from("/h/.rustfox/rustfox.db"));
        assert_eq!(origin, PathOrigin::RebasedRelative);
        assert!(
            path.is_absolute(),
            "resolved path must never be CWD-relative"
        );
    }

    #[test]
    fn nested_relative_path_is_rebased_onto_home() {
        let (path, origin) = resolve_data_path(
            Path::new("sub/data/skills"),
            Path::new("/h/.rustfox"),
            "skills",
        );
        assert_eq!(path, PathBuf::from("/h/.rustfox/sub/data/skills"));
        assert_eq!(origin, PathOrigin::RebasedRelative);
    }

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
            soul: home.join("SOUL.md"),
            agents_md: home.join("AGENTS.md"),
            user_model: home.join("USER.md"),
        };
        ensure_dirs(&paths).unwrap();
        assert!(paths.home.is_dir());
        assert!(paths.workspace.is_dir());
        assert!(paths.skills.is_dir());
        assert!(paths.agents.is_dir());
        assert!(paths.artifacts.is_dir());
        // Soul files live directly in the home dir (parent == home);
        // the database lives in home/ as well, so its parent must also exist.
        assert!(paths.database.parent().unwrap().is_dir());
        assert!(paths.soul.parent().unwrap().is_dir());
        assert!(paths.agents_md.parent().unwrap().is_dir());
        assert!(paths.user_model.parent().unwrap().is_dir());
    }

    #[test]
    fn warning_render_includes_paths_and_commands() {
        let w = LegacyPathWarning {
            label: "memory.database_path".to_string(),
            configured: PathBuf::from("rustfox.db"),
            resolved: PathBuf::from("/h/.rustfox/rustfox.db"),
            legacy_cwd: Some(PathBuf::from("/work/rustfox.db")),
        };
        let s = w.render();
        assert!(s.contains("memory.database_path"));
        assert!(s.contains("rustfox.db"));
        assert!(s.contains("/h/.rustfox/rustfox.db"));
        assert!(s.contains("/work/rustfox.db"), "legacy CWD path surfaced");
        assert!(s.contains("mv "), "migration command surfaced");
        assert!(s.contains("#111"));
    }
    // ── resolve_config_path tests ───────────────────────────────────

    #[test]
    fn resolve_config_path_prefers_env_when_set() {
        // env var wins regardless of whether the file exists — the caller
        // decides what to do with a missing path.
        let got = resolve_config_path(
            Some("/etc/rustfox/override.toml"),
            Path::new("/work"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(got, PathBuf::from("/etc/rustfox/override.toml"));
    }

    #[test]
    fn resolve_config_path_env_to_nonexistent_path_is_returned_verbatim() {
        // Caller (e.g. wizard) is expected to handle "exists == false" — the
        // resolution itself must not silently swap in the home default.
        let got = resolve_config_path(
            Some("/tmp/does-not-exist.toml"),
            Path::new("/work"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(got, PathBuf::from("/tmp/does-not-exist.toml"));
    }

    #[test]
    fn resolve_config_path_uses_cwd_config_when_present() {
        // Set up a CWD-style dir with a config.toml — should beat <home>.
        let tmp = tempfile::tempdir().unwrap();
        let cwd_config = tmp.path().join("config.toml");
        std::fs::write(&cwd_config, b"[telegram]\nbot_token = \"x\"\n").unwrap();

        // Set up a "home" candidate that must be ignored in this scenario.
        let home = tmp.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            b"[openrouter]\napi_key = \"home\"\n",
        )
        .unwrap();

        let got = resolve_config_path(None, tmp.path(), Some(tmp.path()));
        assert_eq!(got, cwd_config);
    }

    #[test]
    fn resolve_config_path_falls_back_to_home_when_cwd_missing() {
        // CWD has no config.toml; <home>/config.toml exists → use it.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        let home_config = home.join("config.toml");
        std::fs::write(&home_config, b"[telegram]\nbot_token = \"home\"\n").unwrap();

        // cwd is an empty sibling dir; home is its child. With cwd passed
        // explicitly, tests are independent and don't share process state.
        let cwd = tmp.path().join("empty-cwd");
        std::fs::create_dir_all(&cwd).unwrap();

        let got = resolve_config_path(None, &cwd, Some(tmp.path()));
        assert_eq!(got, home_config);
    }

    #[test]
    fn resolve_config_path_falls_back_to_cwd_when_nothing_exists() {
        // Neither CWD nor <home> have a config.toml → return the CWD candidate
        // so the wizard has a deterministic "first run" target to write to.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("empty-cwd");
        std::fs::create_dir_all(&cwd).unwrap();

        // Home is provided but its .rustfox dir doesn't exist on disk.
        let got = resolve_config_path(None, &cwd, Some(tmp.path()));
        assert_eq!(got, cwd.join("config.toml"));
    }

    #[test]
    fn resolve_config_path_falls_back_to_cwd_when_no_home_provided() {
        // No env, no os_home → return CWD candidate unconditionally.
        let tmp = tempfile::tempdir().unwrap();
        let got = resolve_config_path(None, tmp.path(), None);
        assert_eq!(got, tmp.path().join("config.toml"));
    }
}
