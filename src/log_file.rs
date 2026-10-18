//! In-app log file sink helpers (ADR-0031).
//!
//! Active log: `{resolved_home}/logs/rustfox.log`
//! Rotate at 32 MiB → `rustfox.log.1` (at most one `.1`).
//! `/logs` send cap: last 16 MiB.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// Default size rotate threshold for the active log file.
pub const LOG_ROTATE_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Max bytes sent by Telegram `/logs` (tail truncate).
pub const LOG_SEND_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Path of the active log file under `home`.
pub fn log_file_path(home: &Path) -> PathBuf {
    home.join("logs").join("rustfox.log")
}

/// Sibling path `rustfox.log.1` next to the active log.
fn rotated_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("rustfox.log");
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}.1"))
}

/// Ensure `logs/` exists under `home`. Returns the active log path.
pub fn ensure_log_dir(home: &Path) -> io::Result<PathBuf> {
    let path = log_file_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(path)
}

/// If `path` exists and its size is ≥ `max_bytes`, rename it to `path.1`
/// (replacing any existing `.1`) so a fresh file can be opened.
pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> io::Result<()> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.len() < max_bytes {
        return Ok(());
    }
    let rotated = rotated_path(path);
    let _ = fs::remove_file(&rotated);
    fs::rename(path, &rotated)?;
    Ok(())
}

/// Rename active log to `.1` if it exists (no size check).
fn force_rotate(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let rotated = rotated_path(path);
    let _ = fs::remove_file(&rotated);
    fs::rename(path, &rotated)?;
    Ok(())
}

/// Read the last `max_bytes` of `path` (or the whole file if smaller).
pub fn tail_bytes(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Path to send for `/logs`: original file if ≤ `max_bytes`, else a temp file
/// containing the last `max_bytes`.
pub fn prepare_log_for_send(path: &Path, max_bytes: u64) -> io::Result<PathBuf> {
    let meta = fs::metadata(path)?;
    if meta.len() <= max_bytes {
        return Ok(path.to_path_buf());
    }
    let data = tail_bytes(path, max_bytes)?;
    // Use a uniquely named dir so the sent filename stays `rustfox.log`.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rustfox-logs-{}-{}", std::process::id(), nanos));
    fs::create_dir_all(&dir)?;
    let tmp_path = dir.join("rustfox.log");
    fs::write(&tmp_path, &data)?;
    Ok(tmp_path)
}

/// Result of parsing Telegram `/logs` args (ADR-0031: bare only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramLogsParse {
    Ok,
    RejectUnknown,
}

/// Parse `/logs` argument: empty/whitespace → Ok; anything else → reject.
pub fn parse_telegram_logs_arg(arg: &str) -> TelegramLogsParse {
    if arg.trim().is_empty() {
        TelegramLogsParse::Ok
    } else {
        TelegramLogsParse::RejectUnknown
    }
}

struct RotatingFileInner {
    path: PathBuf,
    max_bytes: u64,
    file: Option<File>,
}

impl RotatingFileInner {
    fn reopen_append(path: &Path) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }

    fn ensure_open(&mut self) -> io::Result<()> {
        if self.file.is_none() {
            self.file = Some(Self::reopen_append(&self.path)?);
        }
        Ok(())
    }

    fn current_len(&mut self) -> io::Result<u64> {
        self.ensure_open()?;
        match self.file.as_ref().unwrap().metadata() {
            Ok(m) => Ok(m.len()),
            Err(_) => {
                // Stale handle after external rotate — reopen.
                self.file = Some(Self::reopen_append(&self.path)?);
                Ok(self.file.as_ref().unwrap().metadata()?.len())
            }
        }
    }

    fn rotate_now(&mut self) -> io::Result<()> {
        // Drop the open handle so rename works on all platforms.
        self.file = None;
        force_rotate(&self.path)?;
        self.file = Some(Self::reopen_append(&self.path)?);
        Ok(())
    }

    fn write_buf(&mut self, buf: &[u8]) -> io::Result<()> {
        let len = self.current_len()?;
        let next = len.saturating_add(buf.len() as u64);
        // Rotate before a write that would reach/exceed max (keep at most one `.1`).
        // If the file is empty and `buf` alone is huge, write anyway (no loop).
        if len > 0 && next >= self.max_bytes {
            self.rotate_now()?;
        }
        self.ensure_open()?;
        self.file.as_mut().unwrap().write_all(buf)?;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        Ok(())
    }
}

/// Shared rotating file handle for the tracing file layer.
#[derive(Clone)]
pub struct RotatingFileWriter {
    inner: Arc<Mutex<RotatingFileInner>>,
}

impl RotatingFileWriter {
    /// Open (or create) the log file under `home`, creating `logs/` as needed.
    pub fn open(home: &Path, max_bytes: u64) -> io::Result<Self> {
        let path = ensure_log_dir(home)?;
        rotate_if_needed(&path, max_bytes)?;
        let file = RotatingFileInner::reopen_append(&path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(RotatingFileInner {
                path,
                max_bytes,
                file: Some(file),
            })),
        })
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| io::Error::other(format!("log file lock poisoned: {e}")))?;
        guard.write_buf(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| io::Error::other(format!("log file lock poisoned: {e}")))?;
        guard.flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingFileWriter {
    type Writer = RotatingFileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_path_joins_logs_rustfox() {
        let home = Path::new("/tmp/fake-rustfox-home");
        assert_eq!(
            log_file_path(home),
            PathBuf::from("/tmp/fake-rustfox-home/logs/rustfox.log")
        );
    }

    #[test]
    fn rotate_when_past_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, vec![b'x'; 100]).unwrap();
        rotate_if_needed(&path, 50).unwrap();
        assert!(!path.exists(), "active should be renamed away");
        let rotated = dir.path().join("rustfox.log.1");
        assert!(rotated.exists());
        assert_eq!(fs::metadata(&rotated).unwrap().len(), 100);
        File::create(&path).unwrap();
        rotate_if_needed(&path, 50).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn rotate_replaces_existing_dot1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        let rotated = dir.path().join("rustfox.log.1");
        fs::write(&rotated, b"old").unwrap();
        fs::write(&path, vec![b'n'; 80]).unwrap();
        rotate_if_needed(&path, 40).unwrap();
        assert_eq!(fs::read(&rotated).unwrap(), vec![b'n'; 80]);
        assert!(!path.exists());
    }

    #[test]
    fn rotate_noop_when_under_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, b"small").unwrap();
        rotate_if_needed(&path, 1000).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("rustfox.log.1").exists());
    }

    #[test]
    fn rotate_noop_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        rotate_if_needed(&path, 10).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn tail_bytes_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, b"abcdefghijklmnopqrstuvwxyz").unwrap();
        let got = tail_bytes(&path, 5).unwrap();
        assert_eq!(got, b"vwxyz");
    }

    #[test]
    fn tail_bytes_whole_file_when_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, b"abc").unwrap();
        let got = tail_bytes(&path, 100).unwrap();
        assert_eq!(got, b"abc");
    }

    #[test]
    fn prepare_log_for_send_returns_original_when_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, b"hello").unwrap();
        let out = prepare_log_for_send(&path, 100).unwrap();
        assert_eq!(out, path);
    }

    #[test]
    fn prepare_log_for_send_temp_when_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustfox.log");
        fs::write(&path, b"0123456789abcdef").unwrap();
        let out = prepare_log_for_send(&path, 4).unwrap();
        assert_ne!(out, path);
        assert_eq!(fs::read(&out).unwrap(), b"cdef");
        let _ = fs::remove_file(&out);
    }

    #[test]
    fn parse_telegram_logs_bare_ok() {
        assert_eq!(parse_telegram_logs_arg(""), TelegramLogsParse::Ok);
        assert_eq!(parse_telegram_logs_arg("  "), TelegramLogsParse::Ok);
    }

    #[test]
    fn parse_telegram_logs_unknown_rejected() {
        for arg in ["tail", "100", "all", "main"] {
            assert_eq!(
                parse_telegram_logs_arg(arg),
                TelegramLogsParse::RejectUnknown,
                "arg={arg}"
            );
        }
    }

    #[test]
    fn writer_rotates_on_write_past_max() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("logs")).unwrap();
        let path = log_file_path(home);
        fs::write(&path, vec![b'a'; 40]).unwrap();
        let writer = RotatingFileWriter::open(home, 50).unwrap();
        {
            let mut w = writer.clone();
            w.write_all(b"0123456789").unwrap();
            w.flush().unwrap();
        }
        assert!(path.exists());
        let rotated = home.join("logs").join("rustfox.log.1");
        assert!(rotated.exists());
        assert_eq!(fs::metadata(&rotated).unwrap().len(), 40);
        assert_eq!(fs::read(&path).unwrap(), b"0123456789");
    }
}
