//! ADR-0046 / #289: single-instance flock + safe `--version`.
//!
//! Covers binary behaviour that unit tests on `instance_lock` cannot:
//! `--version` / `-V` exit 0 with the package version before bot start,
//! and service verbs do not take (or wait on) the bot lock.

use std::process::Command;
use std::time::Duration;

use rustfox::instance_lock;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rustfox")
}

#[test]
fn version_long_flag_exits_zero_with_pkg_version() {
    let out = Command::new(bin())
        .arg("--version")
        .output()
        .expect("spawn rustfox --version");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout={stdout:?}"
    );
    // Must not look like a bot start (no lock / scheduler noise required;
    // empty-ish stderr is the usual happy path).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_lowercase().contains("loading configuration"),
        "stderr unexpectedly started bot: {stderr}"
    );
}

#[test]
fn version_short_flag_exits_zero_with_pkg_version() {
    let out = Command::new(bin())
        .arg("-V")
        .output()
        .expect("spawn rustfox -V");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn status_verb_does_not_block_on_held_bot_lock() {
    let home = TempDir::new().unwrap();
    let _lock = instance_lock::try_acquire(home.path()).expect("hold bot lock");

    // `status` must not acquire the bot flock. Even with the lock held it
    // should return promptly (may exit non-zero if no OS service is
    // installed — that is fine; hanging on the lock is not).
    let mut child = Command::new(bin())
        .arg("status")
        .env("RUSTFOX_HOME", home.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rustfox status");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                panic!("rustfox status blocked >5s while bot lock held (should not take the lock)");
            }
            Err(e) => panic!("wait error: {e}"),
        }
    }
}

#[test]
fn lock_held_library_second_acquire_fails_same_home() {
    let home = TempDir::new().unwrap();
    let _a = instance_lock::try_acquire(home.path()).unwrap();
    let err = instance_lock::try_acquire(home.path()).expect_err("second");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already running"),
        "expected fail-fast message, got: {msg}"
    );
}

#[test]
fn lock_free_library_acquire_ok() {
    let home = TempDir::new().unwrap();
    let lock = instance_lock::try_acquire(home.path()).expect("free acquire");
    assert!(lock.path().ends_with("rustfox.lock"));
}
