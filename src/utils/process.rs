//! Shared process helpers for sandboxed command execution.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::warn;

/// Future that sleeps `secs` seconds, or never resolves when `secs == 0` (no timeout).
pub async fn optional_timeout(secs: u64) {
    if secs == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

/// Drain bytes from an async reader into an mpsc sender until EOF.
///
/// Used for capturing `ChildStdout` / `ChildStderr` into the shared output
/// buffer during command execution.
pub async fn drain_pipe<R>(mut reader: R, tx: tokio::sync::mpsc::Sender<String>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let _ = tx
                    .send(String::from_utf8_lossy(&buf[..n]).to_string())
                    .await;
            }
        }
    }
}

/// Like [`drain_pipe`] but wraps the entire future in a `timeout` so callers
/// are guaranteed not to hang when a daemon process inherits and holds open
/// the write end of the pipe.
///
/// Returns `true` if the drain completed, `false` if it timed out.
pub async fn drain_pipe_timeout<R>(
    reader: R,
    tx: tokio::sync::mpsc::Sender<String>,
    timeout_secs: u64,
) -> bool
where
    R: AsyncRead + Unpin,
{
    tokio::time::timeout(Duration::from_secs(timeout_secs), drain_pipe(reader, tx))
        .await
        .is_ok()
}

/// SIGKILL the process group (Unix) then the child, and wait for exit.
pub async fn kill_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        if let Err(e) = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        ) {
            warn!("killpg({pid}) failed: {e}");
        }
    }
    if let Err(e) = child.kill().await {
        warn!("child.kill failed: {e}");
    }
    if let Err(e) = child.wait().await {
        warn!("child.wait after kill failed: {e}");
    }
}
