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

/// List a sandbox directory for `list_files`, opening it with no-follow
/// semantics (ADR-0025 §2): `O_NOFOLLOW` on Unix, `FILE_FLAG_OPEN_REPARSE_POINT`
/// + reparse-attribute reject on Windows. Returns sorted `[dir]/[file] name` lines.
pub fn sandbox_list_dir(sandbox_dir: &Path, requested: &str) -> Result<Vec<String>> {
    let path = validate_sandbox_path(sandbox_dir, requested)?;
    let dir = open_nofollow_read(&path).map_err(|e| nofollow_err(e, requested, &path))?;
    if !dir.metadata()?.is_dir() {
        anyhow::bail!("Not a directory: {}", path.display());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        // Symlinks and Windows junctions are reparse points / links, not
        // regular files: label them so the agent does not treat them as files.
        let kind = if ft.is_symlink() {
            "link"
        } else if ft.is_dir() {
            "dir"
        } else {
            "file"
        };
        entries.push(format!("[{kind}] {}", entry.file_name().to_string_lossy()));
    }
    entries.sort();
    Ok(entries)
}

/// Validate a `send_file` path and open it with no-follow semantics; returns
/// the resolved path and its size (taken from the opened handle).
pub fn sandbox_open_for_send(sandbox_dir: &Path, requested: &str) -> Result<(PathBuf, u64)> {
    let path = validate_sandbox_path(sandbox_dir, requested)?;
    let file = open_nofollow_read(&path).map_err(|e| nofollow_err(e, requested, &path))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        anyhow::bail!("Not a regular file: {}", path.display());
    }
    Ok((path, meta.len()))
}

/// Map a no-follow open failure: a symlink/reparse point raced in after
/// validation gets the ADR-0025 outside-workspace copy.
fn nofollow_err(e: std::io::Error, requested: &str, path: &Path) -> anyhow::Error {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return anyhow::anyhow!("{}", path_outside_sandbox_msg(requested));
    }
    #[cfg(windows)]
    if e.kind() == std::io::ErrorKind::PermissionDenied && is_reparse_point(path) {
        return anyhow::anyhow!("{}", path_outside_sandbox_msg(requested));
    }
    anyhow::Error::new(e).context(format!("Failed to open: {}", path.display()))
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
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    // Windows has no O_NOFOLLOW. FILE_FLAG_OPEN_REPARSE_POINT opens the reparse
    // point itself (symlink/junction) instead of its target; we then reject it
    // based on the opened handle's attributes. FILE_FLAG_BACKUP_SEMANTICS lets
    // the same helper open directories (list_files).
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let mut opts = OpenOptions::new();
    opts.read(read)
        .write(write)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    if create_new {
        opts.create_new(true);
    } else if write {
        opts.truncate(true);
    }
    let file = opts.open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
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
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn outside_msg(path: &str) -> String {
        path_outside_sandbox_msg(path)
    }

    /// Unwrap a symlink creation result. On Windows, creating symlinks needs
    /// admin or Developer Mode; `ERROR_PRIVILEGE_NOT_HELD` (1314) skips the
    /// test with a printed reason (junction variants still run unprivileged).
    /// Any other error panics.
    fn symlink_created(res: std::io::Result<()>, test: &str) -> bool {
        match res {
            Ok(()) => true,
            Err(e) if cfg!(windows) && e.raw_os_error() == Some(1314) => {
                eprintln!("skip {test}: symlink creation needs admin or Developer Mode ({e})");
                false
            }
            Err(e) => panic!("{test}: symlink creation failed: {e}"),
        }
    }

    #[cfg(unix)]
    fn file_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    fn file_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(unix)]
    fn dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    fn dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    /// Create a directory junction with `mklink /J` (no privilege needed).
    /// Returns false (test skips) if junctions are unavailable.
    #[cfg(windows)]
    fn junction_created(link: &Path, target: &Path, test: &str) -> bool {
        let status = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                link.to_str().unwrap(),
                target.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("skip {test}: mklink /J failed");
        }
        status.success()
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
        if !symlink_created(file_link(&target, &ws.join("a")), "adr0025_1") {
            return;
        }

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
        if !symlink_created(file_link(&outside_file, &ws.join("b")), "adr0025_2") {
            return;
        }

        let err = sandbox_read_to_string(&ws, "b").unwrap_err().to_string();
        assert_eq!(err, outside_msg("b"));
    }

    #[test]
    #[cfg(windows)]
    fn adr0025_3_windows_junction_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let outside = dir.path().join("outside_j");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("win.ini"), "x").unwrap();
        let junction = ws.join("j");
        if !junction_created(&junction, &outside, "adr0025_3") {
            return;
        }
        let err = validate_sandbox_path(&ws, "j").unwrap_err().to_string();
        assert_eq!(err, outside_msg("j"));
        // list_files on the junction itself.
        let err = sandbox_list_dir(&ws, "j").unwrap_err().to_string();
        assert_eq!(err, outside_msg("j"));
        // Listing the workspace shows the junction as a link, not a file.
        let listed = sandbox_list_dir(&ws, ".").unwrap();
        assert!(listed.contains(&"[link] j".to_string()), "{listed:?}");
        // read_file / send_file of a file through the junction.
        let err = sandbox_read_to_string(&ws, "j\\win.ini")
            .unwrap_err()
            .to_string();
        assert_eq!(err, outside_msg("j\\win.ini"));
        let err = sandbox_open_for_send(&ws, "j\\win.ini")
            .unwrap_err()
            .to_string();
        assert_eq!(err, outside_msg("j\\win.ini"));
        // The reparse point itself is rejected by the nofollow open helper.
        let err = open_nofollow_read(&junction).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    #[cfg(windows)]
    fn adr0025_5_windows_absolute_outside_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let path = r"C:\Windows\win.ini";
        assert_eq!(
            validate_sandbox_path(&ws, path).unwrap_err().to_string(),
            outside_msg(path)
        );
        assert_eq!(
            sandbox_read_to_string(&ws, path).unwrap_err().to_string(),
            outside_msg(path)
        );
        assert_eq!(
            sandbox_open_for_send(&ws, path).unwrap_err().to_string(),
            outside_msg(path)
        );
        assert_eq!(
            sandbox_list_dir(&ws, r"C:\Windows")
                .unwrap_err()
                .to_string(),
            outside_msg(r"C:\Windows")
        );
    }

    /// Junction variant of test 1: a dangling junction (target removed) must
    /// not let a write create anything at the old target.
    #[test]
    #[cfg(windows)]
    fn adr0025_1_windows_dangling_junction_write_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let gone = dir.path().join("gone");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&gone).unwrap();
        if !junction_created(&ws.join("a"), &gone, "adr0025_1_junction") {
            return;
        }
        fs::remove_dir(&gone).unwrap();
        for path in ["a", "a\\new.txt"] {
            assert!(sandbox_write(&ws, path, "pwned").is_err(), "path={path}");
        }
        assert!(
            !gone.exists(),
            "nothing created at dangling junction target"
        );
    }

    /// Junction variants of tests 6 and 7b: writing through a junction parent,
    /// or onto a junction at the final name, is rejected and leaves the
    /// outside directory untouched.
    #[test]
    #[cfg(windows)]
    fn adr0025_6_7b_windows_junction_write_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let outside = dir.path().join("tmp_outside");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&outside).unwrap();
        if !junction_created(&ws.join("d"), &outside, "adr0025_6_junction") {
            return;
        }
        let err = sandbox_write(&ws, "d\\new.txt", "nope")
            .unwrap_err()
            .to_string();
        assert_eq!(err, outside_msg("d\\new.txt"));
        assert!(sandbox_write(&ws, "d", "nope").is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
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
        if !symlink_created(dir_link(&outside, &ws.join("d")), "adr0025_6") {
            return;
        }

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
        assert_eq!(listed, vec!["[file] ok.txt".to_string()]);
        let (sent, len) = sandbox_open_for_send(&ws, "nested/deep/ok.txt").unwrap();
        assert!(sent.starts_with(ws.canonicalize().unwrap()));
        assert_eq!(len, 5);
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
        if !symlink_created(file_link(&outside_file, &ws.join("e")), "adr0025_7a") {
            return;
        }

        for path in ["../out/passwd", "/etc/passwd", "e"] {
            let err = validate_sandbox_path(&ws, path).unwrap_err().to_string();
            assert_eq!(err, outside_msg(path), "path={path}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn adr0025_list_and_send_symlink_escape_rejected() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let outside = dir.path().join("out");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "SECRET").unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("ld")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), ws.join("sf")).unwrap();

        for path in ["ld", "ld/", "../out"] {
            let err = sandbox_list_dir(&ws, path).unwrap_err().to_string();
            assert_eq!(err, outside_msg(path), "path={path}");
        }
        for path in ["sf", "ld/secret.txt", "../out/secret.txt"] {
            let err = sandbox_open_for_send(&ws, path).unwrap_err().to_string();
            assert_eq!(err, outside_msg(path), "path={path}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn list_files_labels_symlinks_as_link() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("sub")).unwrap();
        fs::write(ws.join("a.txt"), "x").unwrap();
        std::os::unix::fs::symlink(ws.join("sub"), ws.join("to_sub")).unwrap();
        std::os::unix::fs::symlink(ws.join("a.txt"), ws.join("to_a")).unwrap();
        let listed = sandbox_list_dir(&ws, ".").unwrap();
        assert_eq!(
            listed,
            vec!["[dir] sub", "[file] a.txt", "[link] to_a", "[link] to_sub"]
        );
    }

    #[test]
    #[cfg(unix)]
    fn adr0025_nofollow_open_rejects_symlink_final_component() {
        // Simulates a symlink swapped in after validation: the helper used by
        // list_files / send_file must refuse to follow it.
        let dir = tempdir().unwrap();
        let outside = dir.path().join("out");
        fs::create_dir_all(&outside).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let err = open_nofollow_read(&link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        let mapped = nofollow_err(err, "link", &link).to_string();
        assert_eq!(mapped, outside_msg("link"));
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
        if !symlink_created(file_link(&outside, &target), "adr0025_7b") {
            return;
        }

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
