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
}
