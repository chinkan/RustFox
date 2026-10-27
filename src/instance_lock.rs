//! Exclusive single-instance flock on the fox-home (ADR-0046 / #289).
//!
//! Held for the life of a normal bot start (bare / `run` / systemd ExecStart).
//! Service verbs and `--setup` must not acquire this lock.

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::path::{Path, PathBuf};

/// Lock file name under the resolved fox-home.
pub const LOCK_FILE_NAME: &str = "rustfox.lock";

/// RAII guard: the OS advisory lock is held until this value is dropped
/// (file closed). Not a pidfile — no stale-pid footgun.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Path of the lock file for a fox-home.
pub fn lock_path(home: &Path) -> PathBuf {
    home.join(LOCK_FILE_NAME)
}

/// True when an I/O error means the exclusive flock is already held.
///
/// Unix `fs2` typically surfaces [`std::io::ErrorKind::WouldBlock`]. On Windows,
/// a held lock often returns raw OS error 33 (`ERROR_LOCK_VIOLATION`) instead.
/// Some platforms may also use [`std::io::ErrorKind::ResourceBusy`].
pub(crate) fn is_lock_contended(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ResourceBusy
    ) || (cfg!(windows) && e.raw_os_error() == Some(33)) // ERROR_LOCK_VIOLATION
}

/// Try to acquire an exclusive, non-blocking flock on `{home}/rustfox.lock`.
///
/// Creates `home` if needed. Returns a clear error when another process
/// already holds the lock for the same home.
pub fn try_acquire(home: &Path) -> Result<InstanceLock> {
    std::fs::create_dir_all(home).with_context(|| format!("create fox-home {}", home.display()))?;
    let path = lock_path(home);
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open lock file {}", path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(InstanceLock { _file: file, path }),
        Err(e) if is_lock_contended(&e) => {
            bail!(
                "another rustfox instance is already running for fox-home {} \
                 (lock held: {}). Stop it first (`rustfox stop` / systemctl --user stop rustfox, \
                 then kill any stray processes), or use a different RUSTFOX_HOME.",
                home.display(),
                path.display()
            );
        }
        Err(e) => Err(e).with_context(|| format!("acquire exclusive flock on {}", path.display())),
    }
}

/// True when `arg` is a version probe flag (`--version` or `-V`).
pub fn is_version_flag(arg: &str) -> bool {
    matches!(arg, "--version" | "-V")
}

/// Print `CARGO_PKG_VERSION` to stdout (no trailing label beyond the version).
pub fn print_version() {
    println!("{}", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};
    use tempfile::TempDir;

    #[test]
    fn lock_free_acquire_ok() {
        let dir = TempDir::new().unwrap();
        let lock = try_acquire(dir.path()).expect("first acquire");
        assert_eq!(lock.path(), lock_path(dir.path()));
        assert!(lock_path(dir.path()).is_file());
    }

    #[test]
    fn lock_held_second_fails() {
        let dir = TempDir::new().unwrap();
        let _first = try_acquire(dir.path()).expect("first acquire");
        let err = try_acquire(dir.path()).expect_err("second must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already running") || msg.contains("lock held"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn lock_released_on_drop_allows_reacquire() {
        let dir = TempDir::new().unwrap();
        {
            let _first = try_acquire(dir.path()).unwrap();
        }
        let _second = try_acquire(dir.path()).expect("reacquire after drop");
    }

    #[test]
    fn different_homes_both_ok() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let _la = try_acquire(a.path()).unwrap();
        let _lb = try_acquire(b.path()).unwrap();
    }

    #[test]
    fn version_flags_recognized() {
        assert!(is_version_flag("--version"));
        assert!(is_version_flag("-V"));
        assert!(!is_version_flag("--help"));
        assert!(!is_version_flag("version"));
        assert!(!is_version_flag("-v"));
    }

    #[test]
    fn is_lock_contended_would_block() {
        let e = Error::from(ErrorKind::WouldBlock);
        assert!(is_lock_contended(&e));
    }

    #[test]
    fn is_lock_contended_resource_busy() {
        let e = Error::from(ErrorKind::ResourceBusy);
        assert!(is_lock_contended(&e));
    }

    #[test]
    fn is_lock_contended_unrelated_kind_false() {
        let e = Error::from(ErrorKind::PermissionDenied);
        assert!(!is_lock_contended(&e));
    }

    #[test]
    #[cfg(windows)]
    fn is_lock_contended_windows_error_lock_violation() {
        // ERROR_LOCK_VIOLATION = 33
        let e = Error::from_raw_os_error(33);
        assert!(
            is_lock_contended(&e),
            "Windows ERROR_LOCK_VIOLATION must map to contended"
        );
    }

    #[test]
    #[cfg(windows)]
    fn is_lock_contended_windows_unrelated_os_error_false() {
        let e = Error::from_raw_os_error(5); // ERROR_ACCESS_DENIED
        assert!(!is_lock_contended(&e));
    }
}
