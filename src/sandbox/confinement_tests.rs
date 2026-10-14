//! Confinement / fail-closed integration tests (ADR-0025 tests 11, 16, 17, 30).

use super::*;
use std::fs;
use std::path::PathBuf;

use super::test_env::{bwrap_env_lock as env_lock, ForcedBwrapPath};

fn expect_sandbox_ok() -> bool {
    std::env::var_os("RUSTFOX_EXPECT_SANDBOX_OK").is_some()
}

/// Regression (#191): a workspace under /tmp must stay visible in sandbox mode.
#[test]
#[cfg(target_os = "linux")]
fn confinement_workspace_under_tmp() {
    let _env = env_lock();
    if !expect_sandbox_ok() {
        eprintln!("skip: set RUSTFOX_EXPECT_SANDBOX_OK=1 in userns-enabled CI job");
        return;
    }
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let ws = dir.path();
    assert_eq!(probe_sandbox(Some(ws)), ProbeOutcome::Success);
    let mut cmd = wrap::build_bwrap_std(ws, "echo ok > inside.txt").unwrap();
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
}

/// Test 16: prepare / probe fail closed — never a silent trusted wrap.
#[test]
#[cfg(target_os = "linux")]
fn fail_closed_when_bwrap_path_forced_missing() {
    let _bwrap = ForcedBwrapPath::set("/nonexistent/rustfox-no-bwrap-xyz");
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
///
/// With `RUSTFOX_EXPECT_BWRAP_INSTALLED=1` (default-AppArmor CI job, bwrap
/// installed, userns restricted) the probe must actually find bwrap and
/// classify the outcome as a real start or "blocked by system settings";
/// a blocked probe must also make sandboxed commands fail closed.
#[test]
#[cfg(target_os = "linux")]
fn default_runner_probe_trusted_ux() {
    let _env = env_lock();
    if expect_sandbox_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_sandbox(Some(dir.path()));
    eprintln!("default runner probe outcome: {outcome:?}");
    if std::env::var_os("RUSTFOX_EXPECT_BWRAP_INSTALLED").is_some() {
        assert!(
            matches!(
                outcome,
                ProbeOutcome::Success | ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings)
            ),
            "bwrap is installed: expected Success or BlockedBySystemSettings, got {outcome:?}"
        );
    }
    match &outcome {
        ProbeOutcome::Success => {
            // Local/dev host where bwrap works without sysctl tweak.
            assert_eq!(wizard_mode_from_probe(&outcome).0, SandboxMode::Sandbox);
        }
        ProbeOutcome::Failed(ProbeFailure::BlockedBySystemSettings) => {
            let (mode, msg) = wizard_mode_from_probe(&outcome);
            assert_eq!(mode, SandboxMode::Trusted);
            assert_eq!(msg, Some(MSG_BLOCKED_BY_SYSTEM));
            assert_eq!(
                install_ux_for_probe(&outcome),
                InstallUx::BlockedBySystemSettings
            );
            // Fail closed: the sandboxed command must not run on the host.
            let marker = dir.path().join("ran-unconfined");
            if let Ok(mut cmd) = wrap::build_bwrap_std(dir.path(), "touch ran-unconfined") {
                let out = cmd.output().unwrap();
                assert!(!out.status.success(), "blocked probe but bwrap ran");
            }
            assert!(!marker.exists(), "command escaped a blocked sandbox");
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
    let _env = env_lock();
    if !expect_sandbox_ok() {
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

/// Test 17: the host home directory (outside the workspace) is not visible
/// or writable from inside the sandbox.
#[test]
#[cfg(target_os = "linux")]
fn confinement_home_not_visible() {
    let _env = env_lock();
    if !expect_sandbox_ok() {
        eprintln!("skip: set RUSTFOX_EXPECT_SANDBOX_OK=1 in userns-enabled CI job");
        return;
    }
    let home = PathBuf::from(std::env::var("HOME").expect("HOME set in CI"));
    assert!(home.is_dir(), "host HOME must exist: {}", home.display());
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    assert!(
        !ws.starts_with(&home),
        "workspace must be outside HOME for this test"
    );
    let marker = home.join(format!("rustfox-t17-{}", std::process::id()));

    // `ls ~` fails: HOME is not mounted in the sandbox.
    let mut cmd = wrap::build_bwrap_std(ws, "ls \"$HOME\"").unwrap();
    let out = cmd.output().unwrap();
    assert!(
        !out.status.success(),
        "home listing visible in sandbox: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // `touch ~/outside` fails and nothing appears on the host.
    let script = format!("touch \"{}\"", marker.display());
    let mut cmd = wrap::build_bwrap_std(ws, &script).unwrap();
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "write to HOME succeeded in sandbox");
    assert!(!marker.exists(), "sandbox created {}", marker.display());
}

/// macOS Seatbelt confinement (tests 11 + 17). With
/// `RUSTFOX_EXPECT_SANDBOX_OK=1` (macOS CI job) a failed probe fails the test.
#[test]
#[cfg(target_os = "macos")]
fn macos_seatbelt_confinement() {
    let _env = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    let outcome = probe_sandbox(Some(ws));
    if outcome != ProbeOutcome::Success {
        assert!(
            !expect_sandbox_ok(),
            "macOS Seatbelt probe failed: {outcome:?}"
        );
        eprintln!("skip: macOS probe failed: {outcome:?}");
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

    // A file outside the workspace (not under an allowed system path) is not readable.
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "SECRET").unwrap();
    let script = format!("cat \"{}\"", secret.display());
    let mut cmd = wrap::build_seatbelt_shell_std(ws, &script).unwrap();
    let out = cmd.output().unwrap();
    assert!(
        !out.status.success() && !String::from_utf8_lossy(&out.stdout).contains("SECRET"),
        "outside file readable under Seatbelt"
    );

    // Test 17: home listing is denied and home is not writable.
    let mut cmd = wrap::build_seatbelt_shell_std(ws, "ls \"$HOME\"").unwrap();
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "home listing visible under Seatbelt");
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let marker = home.join(format!("rustfox-t17-{}", std::process::id()));
    let script = format!("touch \"{}\"", marker.display());
    let mut cmd = wrap::build_seatbelt_shell_std(ws, &script).unwrap();
    let out = cmd.output().unwrap();
    assert!(
        !out.status.success(),
        "write to HOME succeeded under Seatbelt"
    );
    assert!(!marker.exists());
}
