//! Shared process helpers for sandboxed command execution.

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

/// Cap for Telegram / LLM-facing command output snippets.
pub const OUTPUT_SNIPPET_CHARS: usize = 3500;

/// Max time to wait for pipe drains after the child exits or is killed.
pub const DRAIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Future that sleeps `secs` seconds, or never resolves when `secs == 0` (no timeout).
pub async fn optional_timeout(secs: u64) {
    if secs == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

/// Shared buffer filled by background drain tasks (partials survive abort/timeout).
#[derive(Clone, Default)]
pub struct DrainBuf(Arc<Mutex<String>>);

impl DrainBuf {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(String::new())))
    }

    pub fn spawn_reader<R>(&self, reader: R) -> JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let buf = self.0.clone();
        tokio::spawn(async move {
            drain_into(reader, buf).await;
        })
    }

    pub async fn snapshot(&self) -> String {
        self.0.lock().await.clone()
    }
}

async fn drain_into<R>(mut reader: R, buf: Arc<Mutex<String>>)
where
    R: AsyncRead + Unpin,
{
    let mut tmp = vec![0u8; 8192];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&tmp[..n]);
                buf.lock().await.push_str(&chunk);
            }
            Err(e) => {
                warn!("pipe drain read error: {e}");
                break;
            }
        }
    }
}

/// Drain bytes from an async reader into an mpsc sender until EOF.
pub async fn drain_pipe<R>(mut reader: R, tx: tokio::sync::mpsc::Sender<String>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let _ = tx
                    .send(String::from_utf8_lossy(&buf[..n]).to_string())
                    .await;
            }
            Err(e) => {
                warn!("pipe drain read error: {e}");
                break;
            }
        }
    }
}

/// Wait for drain tasks in parallel; on timeout abort them (captured bytes already in [`DrainBuf`]).
pub async fn finish_drains(mut handles: Vec<JoinHandle<()>>, limit: Duration) {
    if handles.is_empty() {
        return;
    }
    match tokio::time::timeout(limit, futures::future::join_all(handles.iter_mut())).await {
        Ok(_) => {}
        Err(_) => {
            warn!("drain timed out after {limit:?}; aborting pipe readers");
            for h in &handles {
                h.abort();
            }
            let _ = futures::future::join_all(handles).await;
        }
    }
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
