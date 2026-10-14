//! OS sandbox for `execute_command` (ADR-0025 Phase 1).
//!
//! `sandbox` mode confines commands with bubblewrap (Linux) or Seatbelt
//! (macOS). Windows has no OS sandbox in Phase 1 — commands are refused.
//! Missing / unset mode ⇒ `trusted` (upgrade-safe default).

pub mod install;
mod probe;
mod wrap;

pub use install::{
    can_offer_install_button, detect_install_command, format_install_failed, install_bwrap,
    install_ux_for_probe, InstallOutcome, InstallUx, MSG_INSTALLING, MSG_INSTALL_BUTTON,
    MSG_INSTALL_FAILED_PREFIX, MSG_UNSUPPORTED_PACKAGE_MANAGER,
};
pub use probe::{probe_sandbox, ProbeFailure, ProbeOutcome};
pub use wrap::{prepare_sandboxed_command, SandboxStartError};

use serde::{Deserialize, Serialize};

/// Canonical user-facing copy (ADR-0025 §7). Tests and runtime match exactly.
pub const MSG_WINDOWS_SANDBOX: &str = "Running commands is not available in safe mode on Windows yet. You can switch to trusted mode in the portal.";
pub const MSG_SANDBOX_FAILED: &str = "Running commands is not available in safe mode on this computer right now. You can switch to trusted mode in the portal. If you run RustFox in Docker, allow sandboxing in your Docker settings.";
pub const MSG_BLOCKED_BY_SYSTEM: &str =
    "This computer's system settings block safe mode, so RustFox will run in trusted mode.";
pub const MSG_SAFE_MODE_ON: &str =
    "Safe mode is on. Commands can now only use your workspace folder.";

/// `[sandbox] mode` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    /// OS sandbox (bwrap / Seatbelt). Windows: execute_command refused.
    Sandbox,
    /// Current behaviour: cwd = allowed_directory only.
    #[default]
    Trusted,
}

impl SandboxMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Trusted => "trusted",
        }
    }

    /// Parse a config string; unknown values fall back to trusted.
    pub fn parse_config(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("sandbox") => Self::Sandbox,
            Some("trusted") | None | Some("") => Self::Trusted,
            Some(_) => Self::Trusted,
        }
    }
}

/// Effective mode when the key is missing or empty ⇒ trusted.
pub fn effective_mode(mode: SandboxMode) -> SandboxMode {
    mode
}

/// Choose the mode a new install should write, based on a probe result.
pub fn wizard_mode_from_probe(outcome: &ProbeOutcome) -> (SandboxMode, Option<&'static str>) {
    match outcome {
        ProbeOutcome::Success => (SandboxMode::Sandbox, None),
        ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings) => {
            (SandboxMode::Trusted, Some(MSG_BLOCKED_BY_SYSTEM))
        }
        ProbeOutcome::Failed(ProbeFailure::WindowsUnavailable) => (SandboxMode::Trusted, None),
        ProbeOutcome::Failed(ProbeFailure::BwrapMissing)
        | ProbeOutcome::Failed(ProbeFailure::OtherFailure(_)) => (SandboxMode::Trusted, None),
    }
}

/// Probe once and return the mode a new install should write, plus optional UX message.
pub fn choose_wizard_sandbox_mode() -> (SandboxMode, Option<&'static str>, crate::sandbox::InstallUx)
{
    #[cfg(windows)]
    {
        return (
            SandboxMode::Trusted,
            None,
            crate::sandbox::InstallUx::NotApplicable,
        );
    }
    #[cfg(not(windows))]
    {
        let outcome = probe_sandbox(None);
        let ux = crate::sandbox::install_ux_for_probe(&outcome);
        let (mode, msg) = wizard_mode_from_probe(&outcome);
        (mode, msg, ux)
    }
}

/// Test-only helpers shared by every test that reads or sets the process-wide
/// `RUSTFOX_BWRAP_PATH` override (directly or via a probe / `which_bwrap`).
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard};

    static BWRAP_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Serialise access to `RUSTFOX_BWRAP_PATH` across the whole lib test binary.
    pub(crate) fn bwrap_env_lock() -> MutexGuard<'static, ()> {
        BWRAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Holds the lock and forces `RUSTFOX_BWRAP_PATH` until dropped.
    pub(crate) struct ForcedBwrapPath {
        _lock: MutexGuard<'static, ()>,
    }

    impl ForcedBwrapPath {
        pub(crate) fn set(path: &str) -> Self {
            let lock = bwrap_env_lock();
            std::env::set_var("RUSTFOX_BWRAP_PATH", path);
            Self { _lock: lock }
        }
    }

    impl Drop for ForcedBwrapPath {
        fn drop(&mut self) {
            std::env::remove_var("RUSTFOX_BWRAP_PATH");
        }
    }
}

// Live bwrap / Seatbelt tests; nothing here applies to Windows.
#[cfg(all(test, unix))]
mod confinement_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_mode_is_trusted() {
        assert_eq!(SandboxMode::parse_config(None), SandboxMode::Trusted);
        assert_eq!(SandboxMode::parse_config(Some("")), SandboxMode::Trusted);
        assert_eq!(SandboxMode::default(), SandboxMode::Trusted);
    }

    #[test]
    fn parses_sandbox_and_trusted() {
        assert_eq!(
            SandboxMode::parse_config(Some("sandbox")),
            SandboxMode::Sandbox
        );
        assert_eq!(
            SandboxMode::parse_config(Some("trusted")),
            SandboxMode::Trusted
        );
    }

    #[test]
    fn canonical_strings_match_adr() {
        assert_eq!(
            MSG_WINDOWS_SANDBOX,
            "Running commands is not available in safe mode on Windows yet. You can switch to trusted mode in the portal."
        );
        assert_eq!(
            MSG_SANDBOX_FAILED,
            "Running commands is not available in safe mode on this computer right now. You can switch to trusted mode in the portal. If you run RustFox in Docker, allow sandboxing in your Docker settings."
        );
        assert_eq!(MSG_INSTALL_BUTTON, "Install for me");
        assert_eq!(
            MSG_INSTALLING,
            "Installing the safety tool. Your computer may ask for your password; RustFox never sees it."
        );
        assert_eq!(
            MSG_BLOCKED_BY_SYSTEM,
            "This computer's system settings block safe mode, so RustFox will run in trusted mode."
        );
        assert_eq!(
            MSG_SAFE_MODE_ON,
            "Safe mode is on. Commands can now only use your workspace folder."
        );
        assert!(MSG_INSTALL_FAILED_PREFIX.starts_with(
            "We could not install the safety tool. You can install it yourself by running:"
        ));
        assert_eq!(
            MSG_UNSUPPORTED_PACKAGE_MANAGER,
            "We couldn't install the safety tool automatically on this computer. You can install \"bubblewrap\" with your system's software installer, or switch to trusted mode in the portal."
        );
        assert!(!MSG_UNSUPPORTED_PACKAGE_MANAGER.contains("sudo apt install"));
    }
}
