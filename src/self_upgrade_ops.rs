//! Post-`self_replace` exe path + fox-home stop/kill-all (ADR-0030 delta / ADR-0046).
//!
//! After release binary replace, Linux `current_exe()` often points at a deleted
//! inode. Callers must stash the absolute path **before** replace and reuse it
//! for re-register / foreground restart. Before `rustfox install`, stop the
//! service unit when it would not suicide this process, then kill leftover
//! processes that still share the same fox-home (flock belt, not a replacement).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static STASHED_EXE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Resolve the fox-home used for upgrade stop/kill (env → default `~/.rustfox`).
pub fn fox_home_for_upgrade() -> PathBuf {
    crate::home::default_home(
        std::env::var("RUSTFOX_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
    .unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".rustfox"))
            .unwrap_or_else(|| PathBuf::from(".rustfox"))
    })
}

/// Stash the absolute path of the running executable **before** any replace.
///
/// Prefer canonicalize when the path still exists on disk; otherwise keep the
/// raw `current_exe()` path (still better than a post-replace deleted inode).
pub fn stash_running_exe() -> Result<PathBuf> {
    let raw = std::env::current_exe().context("Failed to get current executable path")?;
    let abs = raw.canonicalize().unwrap_or(raw);
    *STASHED_EXE.lock().unwrap_or_else(|e| e.into_inner()) = Some(abs.clone());
    Ok(abs)
}

/// Absolute exe for post-replace re-register / foreground restart.
///
/// Uses the stashed path when present; do **not** prefer a fresh
/// `current_exe()` after `self_replace` (deleted inode / ENOENT on Linux).
pub fn exe_for_post_replace() -> Result<PathBuf> {
    if let Some(p) = STASHED_EXE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Ok(p);
    }
    let raw = std::env::current_exe().context("Failed to get current executable path")?;
    Ok(raw.canonicalize().unwrap_or(raw))
}

/// Test/helper: force the stashed path (or clear with `None`).
#[cfg(test)]
pub fn set_stashed_exe_for_test(path: Option<PathBuf>) {
    *STASHED_EXE.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

/// Whether `candidate` should be used for post-replace spawn instead of
/// re-calling `current_exe()` (pure ordering helper for unit tests).
pub fn prefer_stashed_over_current(stashed: Option<&Path>, _current: &Path) -> Option<PathBuf> {
    stashed.map(|p| p.to_path_buf())
}

/// Ordered pre-reregister steps: stop (when safe) + kill-all leftovers, then
/// the caller runs `install` with the **stashed** exe.
///
/// Returns a short human log fragment for the upgrade progress buffer.
pub fn stop_and_kill_before_reregister(fox_home: &Path) -> Result<String> {
    let mut log = String::new();
    match stop_service_if_safe() {
        Ok(Some(msg)) => {
            log.push_str(&format!("  ✓ {msg}\n"));
        }
        Ok(None) => {
            log.push_str("  · service stop skipped (would terminate this process)\n");
        }
        Err(e) => {
            tracing::warn!("pre-reregister service stop: {e:#}");
            log.push_str(&format!("  ⚠ service stop: {e:#}\n"));
        }
    }
    match kill_fox_home_leftovers(fox_home) {
        Ok(n) => {
            log.push_str(&format!(
                "  ✓ kill-all leftovers for {} (signaled {n})\n",
                fox_home.display()
            ));
        }
        Err(e) => {
            tracing::warn!("pre-reregister kill-all: {e:#}");
            log.push_str(&format!("  ⚠ kill-all: {e:#}\n"));
        }
    }
    Ok(log)
}

/// Stop the user service only when this process is **not** its MainPID.
fn stop_service_if_safe() -> Result<Option<&'static str>> {
    if !crate::learning::is_service_installed() {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        let self_pid = std::process::id();
        if let Some(main) = linux_unit_main_pid() {
            if main == self_pid {
                return Ok(None);
            }
        }
        let status = std::process::Command::new("systemctl")
            .args(["--user", "stop", "rustfox.service"])
            .status()
            .context("systemctl --user stop rustfox.service")?;
        if !status.success() {
            anyhow::bail!("systemctl stop failed (exit {:?})", status.code());
        }
        Ok(Some("systemctl --user stop rustfox.service"))
    }
    #[cfg(target_os = "macos")]
    {
        // launchctl stop does not unload KeepAlive agents the way `rustfox stop`
        // does; still best-effort clear of the labelled job before re-register.
        let self_pid = std::process::id();
        if macos_agent_pid() == Some(self_pid) {
            return Ok(None);
        }
        let _ = std::process::Command::new("launchctl")
            .args(["stop", "com.rustfox.bot"])
            .status();
        Ok(Some("launchctl stop com.rustfox.bot"))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("sc")
            .args(["stop", "RustFox"])
            .status();
        Ok(Some("sc stop RustFox"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::id();
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn linux_unit_main_pid() -> Option<u32> {
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "MainPID",
            "--value",
            "rustfox.service",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let pid: u32 = s.trim().parse().ok()?;
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

#[cfg(target_os = "macos")]
fn macos_agent_pid() -> Option<u32> {
    let out = std::process::Command::new("launchctl")
        .args(["list", "com.rustfox.bot"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // launchctl list <label> prints a small plist-like dump including '"PID" = N;'
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"PID\"") {
            let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                if pid > 0 {
                    return Some(pid);
                }
            }
        }
    }
    None
}

/// Signal leftover processes that share `fox_home`, excluding this PID.
///
/// Prefer holders of `{fox_home}/rustfox.lock` (ADR-0046), then cmdline/environ
/// matches. Returns how many PIDs were signaled (TERM then KILL).
pub fn kill_fox_home_leftovers(fox_home: &Path) -> Result<usize> {
    let self_pid = std::process::id();
    let mut pids = leftover_pids_for_fox_home(fox_home, self_pid);
    pids.sort_unstable();
    pids.dedup();
    for pid in &pids {
        signal_pid(*pid);
    }
    Ok(pids.len())
}

/// Collect PIDs that look like leftover rustfox instances for `fox_home`.
pub fn leftover_pids_for_fox_home(fox_home: &Path, self_pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let lock = crate::instance_lock::lock_path(fox_home);
    out.extend(
        pids_holding_path(&lock)
            .into_iter()
            .filter(|p| *p != self_pid),
    );

    #[cfg(target_os = "linux")]
    {
        out.extend(
            linux_rustfox_pids_for_home(fox_home)
                .into_iter()
                .filter(|p| *p != self_pid),
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fox_home;
    }
    out
}

fn pids_holding_path(path: &Path) -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(out) = std::process::Command::new("lsof")
        .args(["-t"])
        .arg(path)
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    pids.push(pid);
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if pids.is_empty() {
            if let Ok(out) = std::process::Command::new("fuser")
                .arg(path.as_os_str())
                .output()
            {
                // fuser prints "path: PID PID" on stdout or stderr depending on build.
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                for tok in combined.split(|c: char| !c.is_ascii_digit()) {
                    if let Ok(pid) = tok.parse::<u32>() {
                        if pid > 1 {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
    }
    pids
}

#[cfg(target_os = "linux")]
fn linux_rustfox_pids_for_home(fox_home: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let home_canon = fox_home
        .canonicalize()
        .unwrap_or_else(|_| fox_home.to_path_buf());
    let default_home = crate::home::default_home(None, dirs::home_dir().as_deref());
    let mut out = Vec::new();
    for ent in entries.flatten() {
        let name = ent.file_name();
        let pid: u32 = match name.to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline.is_empty() {
            continue;
        }
        // cmdline is NUL-separated; first arg is the exe path / name.
        let first = cmdline.split(|b| *b == 0).next().unwrap_or(&[]);
        let first_s = String::from_utf8_lossy(first);
        let base = Path::new(first_s.as_ref())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if base != "rustfox" && !first_s.ends_with("/rustfox") {
            continue;
        }
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        if process_environ_matches_fox_home(&environ, &home_canon, default_home.as_deref()) {
            out.push(pid);
        }
    }
    out
}

/// True when process environ selects `fox_home` (explicit RUSTFOX_HOME or default).
pub fn process_environ_matches_fox_home(
    environ: &[u8],
    fox_home: &Path,
    default_home: Option<&Path>,
) -> bool {
    let mut rustfox_home: Option<PathBuf> = None;
    for entry in environ.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        if let Some(val) = entry.strip_prefix(b"RUSTFOX_HOME=") {
            rustfox_home = Some(PathBuf::from(String::from_utf8_lossy(val).as_ref()));
            break;
        }
    }
    match rustfox_home {
        Some(h) => {
            let canon = h.canonicalize().unwrap_or(h);
            paths_equal_fox_home(&canon, fox_home)
        }
        None => match default_home {
            Some(d) => paths_equal_fox_home(d, fox_home),
            None => false,
        },
    }
}

fn paths_equal_fox_home(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

fn signal_pid(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let p = Pid::from_raw(pid as i32);
        let _ = kill(p, Signal::SIGTERM);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = kill(p, Signal::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

/// Documented step order for pre-reregister (unit-tested).
pub fn pre_reregister_step_names() -> &'static [&'static str] {
    &[
        "stash_exe",
        "stop_and_kill",
        "install_with_stashed_exe",
        "restart_with_stashed_exe",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn prefer_stashed_wins_over_current() {
        let stashed = PathBuf::from("/opt/rustfox/bin/rustfox");
        let current = PathBuf::from("/proc/self/exe");
        assert_eq!(
            prefer_stashed_over_current(Some(&stashed), &current),
            Some(stashed)
        );
        assert_eq!(prefer_stashed_over_current(None, &current), None);
    }

    #[test]
    fn stash_then_exe_for_post_replace_returns_stash() {
        let fake = PathBuf::from("/tmp/rustfox-stash-test-bin");
        set_stashed_exe_for_test(Some(fake.clone()));
        let got = exe_for_post_replace().expect("stashed");
        assert_eq!(got, fake);
        set_stashed_exe_for_test(None);
    }

    #[test]
    fn pre_reregister_order_lists_kill_before_install() {
        let steps = pre_reregister_step_names();
        let kill_idx = steps.iter().position(|s| *s == "stop_and_kill").unwrap();
        let install_idx = steps
            .iter()
            .position(|s| *s == "install_with_stashed_exe")
            .unwrap();
        let stash_idx = steps.iter().position(|s| *s == "stash_exe").unwrap();
        assert!(stash_idx < kill_idx, "stash before kill");
        assert!(
            kill_idx < install_idx,
            "kill-all before install/re-register"
        );
    }

    #[test]
    fn environ_explicit_home_matches() {
        let home = PathBuf::from("/tmp/rf-home-a");
        let env = b"PATH=/usr/bin\0RUSTFOX_HOME=/tmp/rf-home-a\0HOME=/home/u\0";
        assert!(process_environ_matches_fox_home(env, &home, None));
        assert!(!process_environ_matches_fox_home(
            env,
            Path::new("/tmp/other"),
            None
        ));
    }

    #[test]
    fn environ_missing_home_uses_default() {
        let default = PathBuf::from("/home/u/.rustfox");
        let env = b"PATH=/usr/bin\0HOME=/home/u\0";
        assert!(process_environ_matches_fox_home(
            env,
            &default,
            Some(&default)
        ));
        assert!(!process_environ_matches_fox_home(
            env,
            Path::new("/tmp/other"),
            Some(&default)
        ));
    }

    #[test]
    fn leftover_pids_never_include_self() {
        let dir = tempfile::TempDir::new().unwrap();
        let self_pid = std::process::id();
        let pids = leftover_pids_for_fox_home(dir.path(), self_pid);
        assert!(!pids.contains(&self_pid));
    }
}
