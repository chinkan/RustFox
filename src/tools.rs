use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// ADR-0025 canonical copy when a path resolves outside the workspace.
pub fn path_outside_sandbox_msg(requested: &str) -> String {
    format!("The path {requested} is outside your workspace folder, so it cannot be used.")
}

/// Validates that a path is within the allowed sandbox directory.
///
/// ADR-0025 §2:
/// - Canonicalize `allowed_directory` once per check.
/// - Existing path: full canonicalize (follow symlinks); result must be inside.
/// - New path: canonicalize parent, append final file name; parent must be inside.
/// - Dangling symlinks are never followed (`symlink_metadata`, not `exists()`).
///
/// Returns the safe path to use for a subsequent no-follow open.
pub fn validate_sandbox_path(sandbox_dir: &Path, requested: &str) -> Result<PathBuf> {
    let sandbox_canonical = sandbox_dir
        .canonicalize()
        .with_context(|| format!("Sandbox directory not found: {}", sandbox_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        sandbox_dir.join(requested)
    };

    let check_path = match fs::symlink_metadata(&requested_path) {
        Ok(meta) => {
            // Windows: reject symlinks and junctions at the user-facing path
            // (FILE_FLAG_OPEN_REPARSE_POINT + reject; ADR-0025 §2 / test 3).
            #[cfg(windows)]
            if is_reparse_point(&requested_path) {
                anyhow::bail!("{}", path_outside_sandbox_msg(requested));
            }
            // Final component exists (file, dir, or symlink — including dangling).
            if meta.file_type().is_symlink() {
                // Follow only via canonicalize; dangling → reject with outside copy.
                match requested_path.canonicalize() {
                    Ok(canon) => {
                        if !canon.starts_with(&sandbox_canonical) {
                            anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                        }
                        canon
                    }
                    Err(_) => {
                        // Dangling symlink (or unresolvable): never follow.
                        anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                    }
                }
            } else {
                let canon = requested_path
                    .canonicalize()
                    .context("Failed to canonicalize path")?;
                if !canon.starts_with(&sandbox_canonical) {
                    anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                }
                canon
            }
        }
        Err(_) => {
            // New path: canonicalize parent, append final file name.
            let parent = requested_path
                .parent()
                .context("Path has no parent directory")?;
            let file_name = requested_path.file_name().context("Path has no filename")?;
            match parent.canonicalize() {
                Ok(parent_canonical) => {
                    if !parent_canonical.starts_with(&sandbox_canonical) {
                        anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                    }
                    // Symlinked parent that resolves outside is caught above.
                    parent_canonical.join(file_name)
                }
                Err(_) => {
                    // Parent missing: walk up to an existing ancestor inside the sandbox
                    // so write_file can create_dir_all afterward.
                    canonicalize_new_path_via_ancestor(
                        &sandbox_canonical,
                        parent,
                        file_name,
                        requested,
                    )?
                }
            }
        }
    };

    if !check_path.starts_with(&sandbox_canonical) {
        anyhow::bail!("{}", path_outside_sandbox_msg(requested));
    }

    Ok(check_path)
}

/// When the immediate parent of a new file does not exist yet, find the nearest
/// existing ancestor, canonicalize it, and rebuild the path under the sandbox.
fn canonicalize_new_path_via_ancestor(
    sandbox_canonical: &Path,
    parent: &Path,
    file_name: &std::ffi::OsStr,
    requested: &str,
) -> Result<PathBuf> {
    let mut ancestor = parent.to_path_buf();
    let mut suffix = PathBuf::new();
    loop {
        match ancestor.canonicalize() {
            Ok(canon) => {
                if !canon.starts_with(sandbox_canonical) {
                    anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                }
                let rebuilt = canon.join(&suffix).join(file_name);
                // Lexical join must still sit under the sandbox (no .. escape).
                let mut normalized = PathBuf::new();
                for c in rebuilt.components() {
                    match c {
                        std::path::Component::ParentDir => {
                            if !normalized.pop() {
                                anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                            }
                        }
                        std::path::Component::CurDir => {}
                        other => normalized.push(other),
                    }
                }
                if !normalized.starts_with(sandbox_canonical) {
                    anyhow::bail!("{}", path_outside_sandbox_msg(requested));
                }
                return Ok(normalized);
            }
            Err(_) => {
                let name = ancestor
                    .file_name()
                    .context("Cannot resolve parent for new path")?
                    .to_owned();
                suffix = PathBuf::from(name).join(&suffix);
                ancestor = ancestor
                    .parent()
                    .context("Cannot resolve parent for new path")?
                    .to_path_buf();
            }
        }
    }
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

/// Read a file after sandbox validation, opening the final component with
/// no-follow semantics (ADR-0025 §2).
///
/// Residual TOCTOU risk (ADR-0025): where a full `openat`-style walk from an
/// opened sandbox directory handle is unavailable, a symlink can still be
/// swapped between validate and open. Final-component `O_NOFOLLOW` /
/// reparse-point rejection closes the common escape; Phase 1 accepts the rest.
pub fn sandbox_read_to_string(sandbox_dir: &Path, requested: &str) -> Result<String> {
    let path = validate_sandbox_path(sandbox_dir, requested)?;
    let mut file = open_nofollow_read(&path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    Ok(content)
}

/// Write content to a sandbox path with no-follow / create_new semantics.
///
/// - Existing regular file: open write+truncate with `O_NOFOLLOW`.
/// - New file: `create_new` (`O_CREAT|O_EXCL`) so a raced symlink/file is not
///   followed or overwritten (ADR-0025 test 7b).
pub fn sandbox_write(sandbox_dir: &Path, requested: &str, content: &str) -> Result<PathBuf> {
    let path = validate_sandbox_path(sandbox_dir, requested)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory: {}", parent.display()))?;
    }

    // Re-check final component after create_dir_all (parent walk may have raced).
    let is_new = fs::symlink_metadata(&path).is_err();
    let mut file = if is_new {
        open_nofollow_create_new(&path)
            .with_context(|| format!("Failed to create file (create_new): {}", path.display()))?
    } else {
        // Existing: must still be inside after re-validate semantics.
        // Reject if it is now a symlink (dangling or otherwise).
        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat file: {}", path.display()))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("{}", path_outside_sandbox_msg(requested));
        }
        #[cfg(windows)]
        if is_reparse_point(&path) {
            anyhow::bail!("{}", path_outside_sandbox_msg(requested));
        }
        open_nofollow_write_trunc(&path)
            .with_context(|| format!("Failed to open file for write: {}", path.display()))?
    };
    file.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))?;
    Ok(path)
}

/// Validate + return path for list_files / send_file (metadata checked by caller).
pub fn sandbox_list_dir(sandbox_dir: &Path, requested: &str) -> Result<PathBuf> {
    let path = validate_sandbox_path(sandbox_dir, requested)?;
    // Refuse to list through a symlink/reparse at the resolved user path when
    // the original requested path is itself a reparse point.
    let user_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        sandbox_dir.join(requested)
    };
    if let Ok(meta) = fs::symlink_metadata(&user_path) {
        if meta.file_type().is_symlink() {
            // Symlink-to-inside was allowed by validate via canonicalize; listing
            // the canonical dir is fine. Symlink-to-outside already rejected.
            // Still reject if canonicalize somehow left us with a symlink path.
        }
        #[cfg(windows)]
        if is_reparse_point(&user_path) {
            anyhow::bail!("{}", path_outside_sandbox_msg(requested));
        }
    }
    Ok(path)
}

fn open_nofollow_read(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        open_windows_nofollow(path, true, false, false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().read(true).open(path)
    }
}

fn open_nofollow_write_trunc(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        open_windows_nofollow(path, false, true, false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().write(true).truncate(true).open(path)
    }
}

fn open_nofollow_create_new(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_CREAT|O_EXCL from create_new; O_NOFOLLOW rejects a raced symlink.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        open_windows_nofollow(path, false, true, true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
}

#[cfg(windows)]
fn open_windows_nofollow(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OPEN_REPARSE_POINT: open the reparse point itself, then reject.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut opts = OpenOptions::new();
    opts.read(read)
        .write(write)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if create_new {
        opts.create_new(true);
    } else if write {
        opts.truncate(true);
    }
    let file = opts.open(path)?;
    if is_reparse_point(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "reparse point (symlink/junction) rejected",
        ));
    }
    Ok(file)
}

/// Validates that a path is within the RustFox home directory.
/// Returns the canonicalized path if valid.
pub fn validate_home_path(home_dir: &Path, requested: &str) -> Result<PathBuf> {
    let home_canonical = home_dir
        .canonicalize()
        .with_context(|| format!("Home directory not found: {}", home_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        home_dir.join(requested)
    };

    let check_path = match fs::symlink_metadata(&requested_path) {
        Ok(_) => requested_path
            .canonicalize()
            .context("Failed to canonicalize path")?,
        Err(_) => {
            let parent = requested_path
                .parent()
                .context("Path has no parent directory")?;
            let parent_canonical = parent
                .canonicalize()
                .with_context(|| format!("Parent directory not found: {}", parent.display()))?;
            parent_canonical.join(requested_path.file_name().context("Path has no filename")?)
        }
    };

    if !check_path.starts_with(&home_canonical) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the home directory '{}'",
            requested,
            home_dir.display()
        );
    }

    Ok(check_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn outside_msg(path: &str) -> String {
        path_outside_sandbox_msg(path)
    }

    #[test]
    fn test_validate_home_path_allows_home_files() {
        let dir = tempdir().unwrap();
        let home = dir.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        let soul = home.join("SOUL.md");
        std::fs::write(&soul, "# Soul").unwrap();

        let result = validate_home_path(&home, "SOUL.md").unwrap();
        assert_eq!(result, soul.canonicalize().unwrap());
    }

    #[test]
    fn test_validate_home_path_denies_outside() {
        let dir = tempdir().unwrap();
        let home = dir.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "data").unwrap();

        let result = validate_home_path(&home, "../outside.txt");
        assert!(result.is_err());
    }

    // --- ADR-0025 file-tool tests 1–7, 7a, 7b ---

    #[test]
    fn adr0025_1_dangling_symlink_write_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let target = PathBuf::from("/nonexistent/x");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, ws.join("a")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, ws.join("a")).unwrap();

        let err = sandbox_write(&ws, "a", "pwned").unwrap_err().to_string();
        assert_eq!(err, outside_msg("a"));
        assert!(!target.exists(), "nothing created at dangling target");
    }

    #[test]
    fn adr0025_2_symlink_to_outside_read_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let outside_dir = dir.path().join("outside");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "SECRET").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, ws.join("b")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside_file, ws.join("b")).unwrap();

        let err = sandbox_read_to_string(&ws, "b").unwrap_err().to_string();
        assert_eq!(err, outside_msg("b"));
    }

    #[test]
    #[cfg(windows)]
    fn adr0025_3_windows_junction_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        // Create a junction to an outside directory via std if available;
        // otherwise skip when junction creation is unsupported.
        let outside = dir.path().join("outside_j");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("win.ini"), "x").unwrap();
        let junction = ws.join("j");
        // Use mklink /J via cmd
        let status = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                junction.to_str().unwrap(),
                outside.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skip: mklink /J failed");
            return;
        }
        let err = validate_sandbox_path(&ws, "j").unwrap_err().to_string();
        assert_eq!(err, outside_msg("j"));
        let err2 = validate_sandbox_path(&ws, "j/win.ini")
            .unwrap_err()
            .to_string();
        assert!(
            err2 == outside_msg("j/win.ini") || err2.contains("outside"),
            "{err2}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn adr0025_3_windows_junction_skipped_on_unix() {
        // Placeholder so the suite documents test 3 as cfg(windows)-only.
    }

    #[test]
    #[cfg(unix)]
    fn adr0025_4_toc_tou_race_zero_outside_access() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let outside = dir.path().join("outside_race.txt");
        fs::write(&outside, "OUTSIDE_CONTENT").unwrap();
        let race_path = ws.join("c");
        fs::write(&race_path, "INSIDE").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let swaps = Arc::new(AtomicUsize::new(0));
        let stop_w = Arc::clone(&stop);
        let swaps_w = Arc::clone(&swaps);
        let race_path_w = race_path.clone();
        let outside_w = outside.clone();

        let worker = std::thread::spawn(move || {
            let mut as_symlink = false;
            while !stop_w.load(Ordering::Relaxed) && swaps_w.load(Ordering::Relaxed) < 10_000 {
                let _ = fs::remove_file(&race_path_w);
                if as_symlink {
                    let _ = std::os::unix::fs::symlink(&outside_w, &race_path_w);
                } else {
                    let _ = fs::write(&race_path_w, "INSIDE");
                }
                as_symlink = !as_symlink;
                swaps_w.fetch_add(1, Ordering::Relaxed);
            }
        });

        let start = Instant::now();
        let mut attempts = 0u64;
        let mut outside_reads = 0u64;
        let mut outside_writes = 0u64;
        while attempts < 10_000 && start.elapsed() < Duration::from_secs(10) {
            attempts += 1;
            if let Ok(content) = sandbox_read_to_string(&ws, "c") {
                if content.contains("OUTSIDE_CONTENT") {
                    outside_reads += 1;
                }
            }
            let marker = format!("WRITE-{attempts}");
            if sandbox_write(&ws, "c", &marker).is_ok() {
                if let Ok(oc) = fs::read_to_string(&outside) {
                    if oc.contains(&marker) {
                        outside_writes += 1;
                    }
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = worker.join();

        assert_eq!(outside_reads, 0, "read outside content during race");
        assert_eq!(outside_writes, 0, "wrote outside file during race");
        assert!(
            swaps.load(Ordering::Relaxed) > 0 || attempts > 0,
            "race loop did not run"
        );
    }

    #[test]
    fn adr0025_5_dotdot_and_absolute_outside_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        for path in ["../outside.txt", "../../etc/passwd", "/etc/passwd"] {
            let err = validate_sandbox_path(&ws, path).unwrap_err().to_string();
            assert_eq!(err, outside_msg(path), "path={path}");
        }
    }

    #[test]
    fn adr0025_6_write_via_symlinked_parent_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let outside = dir.path().join("tmp_outside");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, ws.join("d")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, ws.join("d")).unwrap();

        let err = sandbox_write(&ws, "d/new.txt", "nope")
            .unwrap_err()
            .to_string();
        assert_eq!(err, outside_msg("d/new.txt"));
        assert!(!outside.join("new.txt").exists());
    }

    #[test]
    fn adr0025_7_regular_files_and_nested_dirs_work() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("nested/deep")).unwrap();

        let path = sandbox_write(&ws, "nested/deep/ok.txt", "hello").unwrap();
        assert!(path.starts_with(ws.canonicalize().unwrap()));
        let content = sandbox_read_to_string(&ws, "nested/deep/ok.txt").unwrap();
        assert_eq!(content, "hello");
        // overwrite existing
        sandbox_write(&ws, "nested/deep/ok.txt", "world").unwrap();
        assert_eq!(
            sandbox_read_to_string(&ws, "nested/deep/ok.txt").unwrap(),
            "world"
        );
        let listed = sandbox_list_dir(&ws, "nested/deep").unwrap();
        assert!(listed.is_dir());
    }

    #[test]
    fn adr0025_7a_send_file_paths_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let outside_dir = dir.path().join("out");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        let outside_file = outside_dir.join("passwd");
        fs::write(&outside_file, "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, ws.join("e")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside_file, ws.join("e")).unwrap();

        for path in ["../out/passwd", "/etc/passwd", "e"] {
            let err = validate_sandbox_path(&ws, path).unwrap_err().to_string();
            assert_eq!(err, outside_msg(path), "path={path}");
        }
    }

    #[test]
    fn adr0025_7b_create_new_does_not_overwrite_raced_file() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let target = ws.join("raced.txt");
        fs::write(&target, "ORIGINAL").unwrap();

        // Path exists as regular file: write truncates (allowed). Separate case:
        // pre-created symlink at the name → create_new / nofollow must not follow.
        let _ = fs::remove_file(&target);
        let outside = dir.path().join("outside_raced.txt");
        fs::write(&outside, "OUT").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &target).unwrap();

        let err = sandbox_write(&ws, "raced.txt", "NEW")
            .unwrap_err()
            .to_string();
        assert_eq!(err, outside_msg("raced.txt"));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "OUT");
    }

    #[test]
    fn adr0025_7b_create_new_fails_if_file_appears() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        // Simulate: validate sees absence, then file appears before create_new.
        // Directly call open_nofollow_create_new after pre-creating.
        let path = ws.join("appears.txt");
        fs::write(&path, "PRE").unwrap();
        let err = open_nofollow_create_new(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "PRE");
    }
}
