use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOrigin {
    Default,
    Absolute,
    RelativeLegacy,
}

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
        let got = resolve_home(None, Some(Path::new("rel/cfg")), Some(Path::new("/os/home"))).unwrap();
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
    fn unset_path_resolves_under_home() {
        let (path, origin) =
            resolve_data_path(Path::new(""), Path::new("/h/.rustfox"), "rustfox.db");
        assert_eq!(path, PathBuf::from("/h/.rustfox/rustfox.db"));
        assert_eq!(origin, PathOrigin::Default);
    }

    #[test]
    fn absolute_path_used_verbatim() {
        let (path, origin) =
            resolve_data_path(Path::new("/data/rustfox.db"), Path::new("/h/.rustfox"), "rustfox.db");
        assert_eq!(path, PathBuf::from("/data/rustfox.db"));
        assert_eq!(origin, PathOrigin::Absolute);
    }

    #[test]
    fn relative_path_is_legacy() {
        let (path, origin) =
            resolve_data_path(Path::new("rustfox.db"), Path::new("/h/.rustfox"), "rustfox.db");
        assert_eq!(path, PathBuf::from("rustfox.db"));
        assert_eq!(origin, PathOrigin::RelativeLegacy);
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
}
