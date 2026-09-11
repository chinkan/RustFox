//! Shared process helpers for sandboxed command execution.

use tracing::warn;

/// Future that sleeps `secs` seconds, or never resolves when `secs == 0` (no timeout).
pub async fn optional_timeout(secs: u64) {
    if secs == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
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
