//! Build OS-sandboxed process commands (bwrap / Seatbelt).

use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

/// Why preparing a sandboxed command failed (fail-closed — never fall back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStartError {
    BwrapMissing,
    WindowsUnavailable,
    Other(String),
}

/// Whether `bwrap` is findable on PATH (Linux).
#[cfg(not(windows))]
pub fn bwrap_on_path() -> bool {
    #[cfg(target_os = "linux")]
    {
        which_bwrap().is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn which_bwrap() -> Option<PathBuf> {
    // Test/CI override: when set, only that path counts (missing ⇒ fail closed).
    if let Ok(p) = std::env::var("RUSTFOX_BWRAP_PATH") {
        let pb = PathBuf::from(&p);
        return if pb.is_file() { Some(pb) } else { None };
    }
    // Prefer `which`-style PATH walk without spawning.
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = Path::new(dir).join("bwrap");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // Common absolute locations.
    for p in ["/usr/bin/bwrap", "/bin/bwrap"] {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    None
}

/// Sync probe command (used by [`super::probe`]).
#[cfg(not(windows))]
pub fn build_probe_command(workspace: &Path) -> Result<StdCommand, SandboxStartError> {
    #[cfg(target_os = "linux")]
    {
        build_bwrap_std(workspace, "true")
    }
    #[cfg(target_os = "macos")]
    {
        build_seatbelt_std(workspace, "/usr/bin/true")
    }
    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
    {
        let _ = workspace;
        Err(SandboxStartError::Other(
            "OS sandbox not supported on this platform".into(),
        ))
    }
}

/// Prepare a Tokio command that runs `user_command` under the OS sandbox.
///
/// On success the returned command is ready for `.spawn()`; callers still set
/// stdout/stderr/env/process_group. Fail-closed: never returns a trusted wrap.
pub fn prepare_sandboxed_command(
    workspace: &Path,
    user_command: &str,
) -> Result<TokioCommand, SandboxStartError> {
    #[cfg(windows)]
    {
        let _ = (workspace, user_command);
        return Err(SandboxStartError::WindowsUnavailable);
    }
    #[cfg(target_os = "linux")]
    {
        build_bwrap_tokio(workspace, user_command)
    }
    #[cfg(target_os = "macos")]
    {
        build_seatbelt_tokio(workspace, user_command)
    }
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    {
        let _ = (workspace, user_command);
        Err(SandboxStartError::Other(
            "OS sandbox not supported on this platform".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn ro_bind_args(args: &mut Vec<String>) {
    // ADR: RO bind needed system dirs as present.
    const CANDIDATES: &[&str] = &[
        "/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32", "/etc", "/opt",
    ];
    for p in CANDIDATES {
        if Path::new(p).exists() {
            args.push("--ro-bind".into());
            args.push((*p).into());
            args.push((*p).into());
        }
    }
}

#[cfg(target_os = "linux")]
fn build_bwrap_args(
    workspace: &Path,
    inner_argv: &[&str],
) -> Result<Vec<String>, SandboxStartError> {
    let ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let ws_str = ws.to_string_lossy().into_owned();

    let mut args: Vec<String> = Vec::new();
    args.push("--die-with-parent".into());
    // Do NOT --unshare-net: Ubuntu AppArmor often blocks RTM_NEWADDR.
    args.push("--unshare-pid".into());
    ro_bind_args(&mut args);
    // bwrap mounts in argument order: private /tmp, /dev, /proc must come
    // before the workspace bind, or a workspace under /tmp is hidden.
    args.push("--tmpfs".into());
    args.push("/tmp".into());
    args.push("--dev".into());
    args.push("/dev".into());
    args.push("--proc".into());
    args.push("/proc".into());
    args.push("--bind".into());
    args.push(ws_str.clone());
    args.push(ws_str.clone());
    args.push("--chdir".into());
    args.push(ws_str);
    for a in inner_argv {
        args.push((*a).into());
    }
    Ok(args)
}

#[cfg(target_os = "linux")]
pub(crate) fn build_bwrap_std(
    workspace: &Path,
    shell_cmd: &str,
) -> Result<StdCommand, SandboxStartError> {
    let bwrap = which_bwrap().ok_or(SandboxStartError::BwrapMissing)?;
    let args = build_bwrap_args(workspace, &["sh", "-c", shell_cmd])?;
    let mut cmd = StdCommand::new(bwrap);
    cmd.args(&args);
    Ok(cmd)
}

#[cfg(target_os = "linux")]
fn build_bwrap_tokio(
    workspace: &Path,
    user_command: &str,
) -> Result<TokioCommand, SandboxStartError> {
    let bwrap = which_bwrap().ok_or(SandboxStartError::BwrapMissing)?;
    let args = build_bwrap_args(workspace, &["sh", "-c", user_command])?;
    let mut cmd = TokioCommand::new(bwrap);
    cmd.args(&args);
    // cwd is set inside the sandbox via --chdir; keep host cwd irrelevant.
    Ok(cmd)
}

#[cfg(target_os = "macos")]
fn seatbelt_profile(workspace: &Path) -> Result<String, SandboxStartError> {
    let ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let ws_str = ws
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    // Equivalent confinement to Linux bwrap: RO system, RW workspace,
    // private-ish tmp; home outside workspace not readable.
    Ok(format!(
        r#"(version 1)
(deny default)
(allow process*)
(allow signal)
(allow sysctl-read)
(allow mach-lookup)
(allow mach-register)
(allow file-read-metadata)
(allow file-read* (literal "/"))
; dyld must map system libraries executable; without this every binary is
; killed at launch with no stderr under (deny default).
(allow file-map-executable
  (subpath "/usr/lib")
  (subpath "/System/Library/Frameworks")
  (subpath "/System/Library/PrivateFrameworks")
  (subpath "/System/Library/SubFrameworks")
  (subpath "/Library/Apple/usr/lib")
)
(allow file-read*
  (subpath "/usr")
  (subpath "/bin")
  (subpath "/sbin")
  (subpath "/System")
  (subpath "/Library")
  (subpath "/private/etc")
  (subpath "/private/var/db")
  (subpath "/private/var/select")
  (subpath "/private/tmp")
  (subpath "/tmp")
  (subpath "/dev")
  (subpath "/opt")
  (literal "/etc")
  (literal "/tmp")
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/tty")
)
(allow file-read* file-write*
  (subpath "{ws}")
  (subpath "/tmp")
  (subpath "/private/tmp")
  (subpath "/var/tmp")
  (subpath "/private/var/tmp")
)
(allow file-write* (literal "/dev/null"))
(allow file-ioctl (literal "/dev/null") (literal "/dev/tty"))
(allow network*)
"#,
        ws = ws_str
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn build_seatbelt_std(
    workspace: &Path,
    prog: &str,
) -> Result<StdCommand, SandboxStartError> {
    let profile = seatbelt_profile(workspace)?;
    let mut cmd = StdCommand::new("sandbox-exec");
    cmd.arg("-p").arg(profile).arg(prog);
    cmd.current_dir(workspace);
    Ok(cmd)
}

// Only the Seatbelt confinement tests use this; non-test builds deny dead code.
#[cfg(all(target_os = "macos", test))]
pub(crate) fn build_seatbelt_shell_std(
    workspace: &Path,
    shell_cmd: &str,
) -> Result<StdCommand, SandboxStartError> {
    let profile = seatbelt_profile(workspace)?;
    let mut cmd = StdCommand::new("sandbox-exec");
    cmd.arg("-p")
        .arg(profile)
        .arg("sh")
        .arg("-c")
        .arg(shell_cmd);
    cmd.current_dir(workspace);
    Ok(cmd)
}

#[cfg(target_os = "macos")]
fn build_seatbelt_tokio(
    workspace: &Path,
    user_command: &str,
) -> Result<TokioCommand, SandboxStartError> {
    let profile = seatbelt_profile(workspace)?;
    let mut cmd = TokioCommand::new("sandbox-exec");
    cmd.arg("-p")
        .arg(profile)
        .arg("sh")
        .arg("-c")
        .arg(user_command);
    cmd.current_dir(workspace);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_args_mount_private_dirs_before_workspace_bind() {
        let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        let args = build_bwrap_args(dir.path(), &["true"]).unwrap();
        let pos = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        let bind = pos("--bind");
        assert!(pos("--tmpfs") < bind, "{args:?}");
        assert!(pos("--dev") < bind, "{args:?}");
        assert!(pos("--proc") < bind, "{args:?}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_args_include_workspace_bind() {
        let dir = tempfile::tempdir().unwrap();
        let args = build_bwrap_args(dir.path(), &["true"]).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("--bind"));
        assert!(joined.contains("--ro-bind"));
        assert!(joined.contains("--tmpfs"));
        assert!(!joined.contains("--unshare-net"));
    }

    #[test]
    #[cfg(windows)]
    fn windows_prepare_refuses() {
        let err = prepare_sandboxed_command(Path::new("C:\\"), "echo hi");
        assert!(
            matches!(err, Err(SandboxStartError::WindowsUnavailable)),
            "{:?}",
            err.err()
        );
    }
}
