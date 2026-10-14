//! Probe whether the OS sandbox can start a trivial command (ADR-0025 §3).

use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

#[cfg(not(windows))]
use super::wrap::{self, SandboxStartError};

/// Result of launching a trivial sandboxed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Success,
    Failed(ProbeFailure),
}

/// Why the probe failed — drives wizard / portal UX copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// `bwrap` binary is not on PATH (Linux only).
    BwrapMissing,
    /// Present but blocked (AppArmor userns, Seatbelt policy, etc.).
    BlockedBySystemSettings,
    /// Windows has no Phase-1 OS sandbox.
    WindowsUnavailable,
    /// Any other launch / exit failure.
    OtherFailure(String),
}

/// Launch a trivial sandboxed command (`true` / `echo ok`) and classify the result.
///
/// `workspace` is used as the RW bind target (must exist). When `None`, a temp
/// directory under the system temp dir is created for the probe.
pub fn probe_sandbox(workspace: Option<&Path>) -> ProbeOutcome {
    #[cfg(windows)]
    {
        let _ = workspace;
        return ProbeOutcome::Failed(ProbeFailure::WindowsUnavailable);
    }

    #[cfg(not(windows))]
    {
        probe_unix(workspace)
    }
}

#[cfg(not(windows))]
fn probe_unix(workspace: Option<&Path>) -> ProbeOutcome {
    let owned_ws: PathBuf;
    let ws: &Path = if let Some(p) = workspace {
        p
    } else {
        owned_ws =
            std::env::temp_dir().join(format!("rustfox-sandbox-probe-{}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&owned_ws) {
            return ProbeOutcome::Failed(ProbeFailure::OtherFailure(format!("tempdir: {e}")));
        }
        &owned_ws
    };

    let mut cmd = match wrap::build_probe_command(ws) {
        Ok(c) => c,
        Err(SandboxStartError::BwrapMissing) => {
            return ProbeOutcome::Failed(ProbeFailure::BwrapMissing);
        }
        Err(SandboxStartError::WindowsUnavailable) => {
            return ProbeOutcome::Failed(ProbeFailure::WindowsUnavailable);
        }
        Err(SandboxStartError::Other(msg)) => {
            return ProbeOutcome::Failed(classify_stderr(&msg, wrap::bwrap_on_path()));
        }
    };

    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => return ProbeOutcome::Failed(classify_spawn_error(&e, ws, wrap::bwrap_on_path())),
    };

    if output.status.success() {
        return ProbeOutcome::Success;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");
    ProbeOutcome::Failed(classify_stderr(&combined, wrap::bwrap_on_path()))
}

/// Classify a spawn error. `NotFound` from `Command::spawn` can mean either
/// the program or the working directory is missing, so only report
/// `BwrapMissing` when the bwrap binary itself is absent (#192).
#[cfg(not(windows))]
pub(crate) fn classify_spawn_error(
    e: &std::io::Error,
    workspace: &Path,
    bwrap_present: bool,
) -> ProbeFailure {
    if e.kind() == std::io::ErrorKind::NotFound {
        if cfg!(target_os = "linux") && !bwrap_present {
            return ProbeFailure::BwrapMissing;
        }
        if !workspace.is_dir() {
            return ProbeFailure::OtherFailure(format!(
                "workspace folder not found: {} ({e})",
                workspace.display()
            ));
        }
        return ProbeFailure::OtherFailure(e.to_string());
    }
    classify_stderr(&e.to_string(), bwrap_present)
}

/// Classify stderr / error text into a [`ProbeFailure`].
#[cfg(not(windows))]
pub(crate) fn classify_stderr(text: &str, bwrap_present: bool) -> ProbeFailure {
    let lower = text.to_ascii_lowercase();
    if !bwrap_present
        && (lower.contains("no such file")
            || lower.contains("not found")
            || lower.contains("cannot find"))
    {
        return ProbeFailure::BwrapMissing;
    }
    if looks_blocked_by_system(&lower) {
        return ProbeFailure::BlockedBySystemSettings;
    }
    if cfg!(target_os = "linux") && !bwrap_present {
        return ProbeFailure::BwrapMissing;
    }
    ProbeFailure::OtherFailure(text.trim().to_string())
}

#[cfg(not(windows))]
fn looks_blocked_by_system(lower: &str) -> bool {
    lower.contains("user namespace")
        || lower.contains("uid map")
        || lower.contains("gid map")
        || lower.contains("operation not permitted")
        || lower.contains("permission denied")
        || lower.contains("apparmor")
        || lower.contains("no permissions to create")
        || lower.contains("setgroups")
        || lower.contains("unprivileged")
        || lower.contains("rtm_newaddr")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn classify_apparmor_as_blocked() {
        let f = classify_stderr("bwrap: setting up uid map: Permission denied", true);
        assert_eq!(f, ProbeFailure::BlockedBySystemSettings);
    }

    #[test]
    #[cfg(not(windows))]
    fn classify_missing_bwrap() {
        let f = classify_stderr("No such file or directory", false);
        assert_eq!(f, ProbeFailure::BwrapMissing);
    }

    #[test]
    #[cfg(not(windows))]
    fn spawn_not_found_with_bwrap_present_is_not_bwrap_missing() {
        let missing = std::path::Path::new("/nonexistent/rustfox-missing-ws");
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let f = classify_spawn_error(&e, missing, true);
        assert!(
            matches!(&f, ProbeFailure::OtherFailure(m) if m.contains("workspace folder not found")),
            "{f:?}"
        );
        assert_eq!(
            crate::sandbox::wizard_mode_from_probe(&ProbeOutcome::Failed(f)).0,
            crate::sandbox::SandboxMode::Trusted
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn spawn_not_found_without_bwrap_is_bwrap_missing() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let ws = std::env::temp_dir();
        assert_eq!(
            classify_spawn_error(&e, &ws, false),
            ProbeFailure::BwrapMissing
        );
    }

    /// Real spawn with an existing bwrap binary but a missing cwd must not be
    /// reported as "bwrap missing" (#192).
    #[test]
    #[cfg(target_os = "linux")]
    fn probe_missing_cwd_with_bwrap_present_is_not_bwrap_missing() {
        // Any existing executable stands in for bwrap via the override.
        let _bwrap = crate::sandbox::test_env::ForcedBwrapPath::set("/bin/sh");
        let missing = std::path::Path::new("/nonexistent/rustfox-missing-ws");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("true").current_dir(missing);
        let err = cmd.output().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let f = classify_spawn_error(&err, missing, wrap::bwrap_on_path());
        assert_ne!(f, ProbeFailure::BwrapMissing);
    }

    #[test]
    fn probe_runs_without_panic() {
        let _bwrap = crate::sandbox::test_env::bwrap_env_lock();
        let outcome = probe_sandbox(None);
        match outcome {
            ProbeOutcome::Success => {}
            ProbeOutcome::Failed(ProbeFailure::BwrapMissing)
            | ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings)
            | ProbeOutcome::Failed(ProbeFailure::WindowsUnavailable)
            | ProbeOutcome::Failed(ProbeFailure::OtherFailure(_)) => {}
        }
    }
}
