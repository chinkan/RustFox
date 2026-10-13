//! Confinement / fail-closed integration tests (ADR-0025 tests 11, 16, 17, 30).

use super::*;
use std::fs;
use std::path::PathBuf;

/// Test 16: prepare / probe fail closed — never a silent trusted wrap.
#[test]
#[cfg(target_os = "linux")]
fn fail_closed_when_bwrap_path_forced_missing() {
    std::env::set_var("RUSTFOX_BWRAP_PATH", "/nonexistent/rustfox-no-bwrap-xyz");
    struct ClearBwrap;
    impl Drop for ClearBwrap {
        fn drop(&mut self) {
            std::env::remove_var("RUSTFOX_BWRAP_PATH");
        }
    }
    let _clear = ClearBwrap;
    let outcome = probe_sandbox(None);
    assert!(
        matches!(
            outcome,
            ProbeOutcome::Failed(ProbeFailure::BwrapMissing)
                | ProbeOutcome::Failed(ProbeFailure::OtherFailure(_))
        ),
        "{outcome:?}"
    );
    let err = wrap::prepare_sandboxed_command(std::path::Path::new("/tmp"), "true");
    assert!(err.is_err(), "must fail closed, got {err:?}");
}

/// Test 30: default runner — blocked / missing ⇒ trusted + matching UX.
#[test]
#[cfg(target_os = "linux")]
fn default_runner_probe_trusted_ux() {
    if std::env::var_os("RUSTFOX_EXPECT_SANDBOX_OK").is_some() {
        return;
    }
    let outcome = probe_sandbox(None);
    match &outcome {
        ProbeOutcome::Success => {
            // Local/dev host where bwrap works without sysctl tweak.
        }
        ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings) => {
            let (mode, msg) = wizard_mode_from_probe(&outcome);
            assert_eq!(mode, SandboxMode::Trusted);
            assert_eq!(msg, Some(MSG_BLOCKED_BY_SYSTEM));
            assert_eq!(
                install_ux_for_probe(&outcome),
                InstallUx::BlockedBySystemSettings
            );
        }
        ProbeOutcome::Failed(_) => {
            let (mode, _) = wizard_mode_from_probe(&outcome);
            assert_eq!(mode, SandboxMode::Trusted);
        }
    }
}

/// Tests 11 + 17: live bwrap confinement (userns-enabled CI job).
#[test]
#[cfg(target_os = "linux")]
fn confinement_when_userns_enabled() {
    if std::env::var_os("RUSTFOX_EXPECT_SANDBOX_OK").is_none() {
        eprintln!("skip: set RUSTFOX_EXPECT_SANDBOX_OK=1 in userns-enabled CI job");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    let outcome = probe_sandbox(Some(ws));
    assert_eq!(outcome, ProbeOutcome::Success, "{outcome:?}");
    let (mode, msg) = wizard_mode_from_probe(&outcome);
    assert_eq!(mode, SandboxMode::Sandbox);
    assert!(msg.is_none());

    let mut cmd = wrap::build_bwrap_std(ws, "cat /etc/hostname > host.txt").unwrap();
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!fs::read_to_string(ws.join("host.txt"))
        .unwrap()
        .trim()
        .is_empty());

    let mut cmd = wrap::build_bwrap_std(ws, "touch /etc/rustfox-should-not-exist").unwrap();
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    assert!(!PathBuf::from("/etc/rustfox-should-not-exist").exists());

    let mut cmd = wrap::build_bwrap_std(ws, "echo ok > inside.txt").unwrap();
    let out = cmd.output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        fs::read_to_string(ws.join("inside.txt")).unwrap().trim(),
        "ok"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_seatbelt_confinement() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    let outcome = probe_sandbox(Some(ws));
    if outcome != ProbeOutcome::Success {
        eprintln!("macOS probe failed: {outcome:?}");
        return;
    }
    let mut cmd = wrap::build_seatbelt_shell_std(ws, "echo ok > inside.txt").unwrap();
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(ws.join("inside.txt")).unwrap().trim(),
        "ok"
    );

    // Home outside workspace should not be readable as the real home listing.
    let mut cmd = wrap::build_seatbelt_shell_std(ws, "test -f \"$HOME/.bashrc\"").unwrap();
    let out = cmd.output().unwrap();
    // Expect failure when HOME points outside the allowed subpath.
    assert!(
        !out.status.success(),
        "home file unexpectedly readable under Seatbelt"
    );
}
