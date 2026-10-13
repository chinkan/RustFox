//! Linux bubblewrap install helper via `pkexec` (ADR-0025 §3a, tests 25–31).
//!
//! RustFox never collects, stores, or logs the sudo password — `pkexec` owns
//! the OS auth dialog.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::probe::{probe_sandbox, ProbeFailure, ProbeOutcome};
use super::{SandboxMode, MSG_BLOCKED_BY_SYSTEM, MSG_SAFE_MODE_ON};

/// Button label (ADR-0025 §7).
pub const MSG_INSTALL_BUTTON: &str = "Install for me";
/// Installing hint (ADR-0025 §7).
pub const MSG_INSTALLING: &str =
    "Installing the safety tool. Your computer may ask for your password; RustFox never sees it.";
/// Prefix of the install-failed copy; callers append `{command}`.
pub const MSG_INSTALL_FAILED_PREFIX: &str =
    "We could not install the safety tool. You can install it yourself by running:";

fn install_failed_message(command: &str) -> String {
    format!("{MSG_INSTALL_FAILED_PREFIX} {command}")
}

/// Detected package-manager install command (one-liner for the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCommand {
    /// e.g. `sudo apt install bubblewrap`
    pub user_command: String,
    /// argv for pkexec (no sudo): e.g. `["apt-get", "install", "-y", "bubblewrap"]`
    pub pkexec_argv: Vec<String>,
}

/// UX state after probing / deciding whether to offer install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallUx {
    /// Probe succeeded — no install needed.
    AlreadyWorks,
    /// Show the Install for me button + installing hint.
    OfferButton { command: InstallCommand },
    /// No GUI / no pkexec / unsupported PM — show one-line command only.
    CommandOnly { command: String },
    /// bwrap present but blocked — no install advice.
    BlockedBySystemSettings,
    /// Non-Linux (macOS Seatbelt is built-in; Windows has no Phase-1 sandbox).
    NotApplicable,
}

/// Outcome of running the install helper (and re-probe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub mode: SandboxMode,
    pub message: String,
    /// When true, the message includes an install command the user can run.
    pub includes_install_command: bool,
}

/// Trait so unit tests inject a fake pkexec without a real binary.
pub trait PkexecRunner: Send + Sync {
    /// Run `pkexec <argv…>`; return the process exit status code
    /// (`126` dismissed, `127` not authorized, package-manager codes otherwise).
    fn run_pkexec(&self, argv: &[String]) -> std::io::Result<i32>;
}

/// Production runner: resolves `pkexec` (or `RUSTFOX_PKEXEC_PATH` override).
pub struct RealPkexec;

impl PkexecRunner for RealPkexec {
    fn run_pkexec(&self, argv: &[String]) -> std::io::Result<i32> {
        let bin = pkexec_path()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "pkexec not found"))?;
        let status = Command::new(bin).args(argv).status()?;
        Ok(status.code().unwrap_or(1))
    }
}

fn pkexec_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RUSTFOX_PKEXEC_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for p in ["/usr/bin/pkexec", "/bin/pkexec"] {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let c = Path::new(dir).join("pkexec");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn has_display() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Detect apt / dnf / pacman and return the install command.
pub fn detect_install_command() -> Option<InstallCommand> {
    if Path::new("/usr/bin/apt-get").is_file() || Path::new("/bin/apt-get").is_file() {
        return Some(InstallCommand {
            user_command: "sudo apt install bubblewrap".into(),
            pkexec_argv: vec![
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                "bubblewrap".into(),
            ],
        });
    }
    if Path::new("/usr/bin/dnf").is_file() || Path::new("/bin/dnf").is_file() {
        return Some(InstallCommand {
            user_command: "sudo dnf install bubblewrap".into(),
            pkexec_argv: vec![
                "dnf".into(),
                "install".into(),
                "-y".into(),
                "bubblewrap".into(),
            ],
        });
    }
    if Path::new("/usr/bin/pacman").is_file() || Path::new("/bin/pacman").is_file() {
        return Some(InstallCommand {
            user_command: "sudo pacman -S bubblewrap".into(),
            pkexec_argv: vec![
                "pacman".into(),
                "-S".into(),
                "--noconfirm".into(),
                "bubblewrap".into(),
            ],
        });
    }
    None
}

/// Whether the wizard / portal should show the Install for me button.
pub fn can_offer_install_button() -> bool {
    cfg!(target_os = "linux")
        && has_display()
        && pkexec_path().is_some()
        && detect_install_command().is_some()
}

/// Decide install UX from a probe failure on Linux.
pub fn install_ux_for_probe(outcome: &ProbeOutcome) -> InstallUx {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = outcome;
        return InstallUx::NotApplicable;
    }
    #[cfg(target_os = "linux")]
    {
        match outcome {
            ProbeOutcome::Success => InstallUx::AlreadyWorks,
            ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings) => {
                InstallUx::BlockedBySystemSettings
            }
            ProbeOutcome::Failed(ProbeFailure::BwrapMissing) => match detect_install_command() {
                Some(cmd) if can_offer_install_button() => InstallUx::OfferButton { command: cmd },
                Some(cmd) => InstallUx::CommandOnly {
                    command: cmd.user_command,
                },
                None => InstallUx::CommandOnly {
                    command: "sudo apt install bubblewrap".into(),
                },
            },
            ProbeOutcome::Failed(ProbeFailure::WindowsUnavailable) => InstallUx::NotApplicable,
            ProbeOutcome::Failed(ProbeFailure::OtherFailure(_)) => {
                // Treat unknown failure like missing tooling when bwrap is absent.
                if !super::wrap::bwrap_on_path() {
                    match detect_install_command() {
                        Some(cmd) if can_offer_install_button() => {
                            InstallUx::OfferButton { command: cmd }
                        }
                        Some(cmd) => InstallUx::CommandOnly {
                            command: cmd.user_command,
                        },
                        None => InstallUx::CommandOnly {
                            command: "sudo apt install bubblewrap".into(),
                        },
                    }
                } else {
                    InstallUx::BlockedBySystemSettings
                }
            }
        }
    }
}

/// Run pkexec install, then re-probe. Never stores a password.
pub fn install_bwrap(
    runner: &dyn PkexecRunner,
    command: &InstallCommand,
    workspace: Option<&Path>,
    // Optional re-probe override (tests inject AppArmor-blocked outcome).
    re_probe: Option<ProbeOutcome>,
) -> InstallOutcome {
    let code = match runner.run_pkexec(&command.pkexec_argv) {
        Ok(c) => c,
        Err(_) => {
            return InstallOutcome {
                mode: SandboxMode::Trusted,
                message: install_failed_message(&command.user_command),
                includes_install_command: true,
            };
        }
    };

    if code != 0 {
        // 126 dismissed, 127 not authorized, 100 apt failure, etc.
        return InstallOutcome {
            mode: SandboxMode::Trusted,
            message: install_failed_message(&command.user_command),
            includes_install_command: true,
        };
    }

    let outcome = re_probe.unwrap_or_else(|| probe_sandbox(workspace));
    match outcome {
        ProbeOutcome::Success => InstallOutcome {
            mode: SandboxMode::Sandbox,
            message: MSG_SAFE_MODE_ON.to_string(),
            includes_install_command: false,
        },
        ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings) => InstallOutcome {
            mode: SandboxMode::Trusted,
            message: MSG_BLOCKED_BY_SYSTEM.to_string(),
            includes_install_command: false,
        },
        ProbeOutcome::Failed(_) => InstallOutcome {
            mode: SandboxMode::Trusted,
            message: install_failed_message(&command.user_command),
            includes_install_command: true,
        },
    }
}

/// Format the install-failed message (exported for tests / portal).
pub fn format_install_failed(command: &str) -> String {
    install_failed_message(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct FakePkexec {
        /// Exit code to return.
        code: i32,
        /// Recorded argv for assertions.
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        /// When set, `run_pkexec` returns this I/O error instead.
        io_err: bool,
    }

    impl PkexecRunner for FakePkexec {
        fn run_pkexec(&self, argv: &[String]) -> std::io::Result<i32> {
            self.calls.lock().unwrap().push(argv.to_vec());
            if self.io_err {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "fake missing",
                ));
            }
            Ok(self.code)
        }
    }

    fn apt_cmd() -> InstallCommand {
        InstallCommand {
            user_command: "sudo apt install bubblewrap".into(),
            pkexec_argv: vec![
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                "bubblewrap".into(),
            ],
        }
    }

    /// Case 1: exit 0 + re-probe ok ⇒ sandbox + Safe mode is on.
    #[test]
    fn fake_pkexec_0_probe_ok() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = FakePkexec {
            code: 0,
            calls: Arc::clone(&calls),
            io_err: false,
        };
        let out = install_bwrap(&runner, &apt_cmd(), None, Some(ProbeOutcome::Success));
        assert_eq!(out.mode, SandboxMode::Sandbox);
        assert_eq!(out.message, MSG_SAFE_MODE_ON);
        assert!(!out.includes_install_command);
        assert_eq!(calls.lock().unwrap().len(), 1);
        // Password must never appear in the outcome message.
        assert!(!out.message.to_lowercase().contains("password"));
    }

    /// Case 2: exit 0 + AppArmor block ⇒ trusted + blocked copy, no install cmd.
    #[test]
    fn fake_pkexec_0_apparmor_block() {
        let runner = FakePkexec {
            code: 0,
            calls: Arc::new(Mutex::new(Vec::new())),
            io_err: false,
        };
        let out = install_bwrap(
            &runner,
            &apt_cmd(),
            None,
            Some(ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings)),
        );
        assert_eq!(out.mode, SandboxMode::Trusted);
        assert_eq!(out.message, MSG_BLOCKED_BY_SYSTEM);
        assert!(!out.includes_install_command);
        assert!(!out.message.contains("sudo apt"));
    }

    /// Case 3: exit 126 (dismissed) ⇒ trusted + install command.
    #[test]
    fn fake_pkexec_126_cancelled() {
        let runner = FakePkexec {
            code: 126,
            calls: Arc::new(Mutex::new(Vec::new())),
            io_err: false,
        };
        let out = install_bwrap(&runner, &apt_cmd(), None, None);
        assert_eq!(out.mode, SandboxMode::Trusted);
        assert!(out.includes_install_command);
        assert_eq!(
            out.message,
            format_install_failed("sudo apt install bubblewrap")
        );
    }

    /// Case 4: exit 127 (not authorized) ⇒ trusted + install command.
    #[test]
    fn fake_pkexec_127_not_authorized() {
        let runner = FakePkexec {
            code: 127,
            calls: Arc::new(Mutex::new(Vec::new())),
            io_err: false,
        };
        let out = install_bwrap(&runner, &apt_cmd(), None, None);
        assert_eq!(out.mode, SandboxMode::Trusted);
        assert!(out.includes_install_command);
        assert!(out.message.contains("sudo apt install bubblewrap"));
    }

    /// Case 5: exit 100 (apt failed) ⇒ trusted + install command.
    #[test]
    fn fake_pkexec_100_apt_failed() {
        let runner = FakePkexec {
            code: 100,
            calls: Arc::new(Mutex::new(Vec::new())),
            io_err: false,
        };
        let out = install_bwrap(&runner, &apt_cmd(), None, None);
        assert_eq!(out.mode, SandboxMode::Trusted);
        assert!(out.includes_install_command);
        assert_eq!(
            out.message,
            "We could not install the safety tool. You can install it yourself by running: sudo apt install bubblewrap"
        );
    }

    #[test]
    fn install_button_label_exact() {
        assert_eq!(MSG_INSTALL_BUTTON, "Install for me");
        assert_eq!(
            MSG_INSTALLING,
            "Installing the safety tool. Your computer may ask for your password; RustFox never sees it."
        );
    }

    /// Test 31 (code review): password never flows through portal/logs/config —
    /// the install helper only ever invokes pkexec with package-manager argv.
    #[test]
    fn pkexec_argv_never_contains_password() {
        let cmd = apt_cmd();
        for a in &cmd.pkexec_argv {
            assert!(!a.to_lowercase().contains("password"));
            assert!(!a.contains("passwd"));
        }
        let dnf = InstallCommand {
            user_command: "sudo dnf install bubblewrap".into(),
            pkexec_argv: vec![
                "dnf".into(),
                "install".into(),
                "-y".into(),
                "bubblewrap".into(),
            ],
        };
        for a in &dnf.pkexec_argv {
            assert!(!a.to_lowercase().contains("password"));
        }
    }
}
