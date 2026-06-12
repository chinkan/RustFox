use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Remove,
    Status,
    Start,
    Stop,
}

fn home_dir() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".rustfox")
}

fn render_template(template: &str, bin_path: &Path) -> String {
    let home = home_dir();
    let config_path = home.join("config.toml");
    template
        .replace("{{RUSTFOX_BIN}}", &bin_path.to_string_lossy())
        .replace("{{RUSTFOX_CONFIG}}", &config_path.to_string_lossy())
        .replace("{{RUSTFOX_HOME}}", &home.to_string_lossy())
}

pub fn handle(action: Action) -> Result<()> {
    match action {
        Action::Install => install(),
        Action::Remove => remove(),
        Action::Status => status(),
        Action::Start => start(),
        Action::Stop => stop(),
    }
}

fn install() -> Result<()> {
    let exe = std::env::current_exe().context("Failed to get current executable path")?;
    #[cfg(target_os = "linux")]
    {
        install_systemd(&exe)
    }
    #[cfg(target_os = "macos")]
    {
        install_launchd(&exe)
    }
    #[cfg(target_os = "windows")]
    {
        install_windows_service(&exe)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("Service installation is not supported on this platform")
    }
}

#[cfg(target_os = "linux")]
fn install_systemd(exe: &Path) -> Result<()> {
    let template = include_str!("../../scripts/services/rustfox.service.template");
    let rendered = render_template(template, exe);

    let user_service_dir = dirs::home_dir()
        .context("HOME not set")?
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&user_service_dir)
        .context("Failed to create systemd user services directory")?;

    let service_path = user_service_dir.join("rustfox.service");
    std::fs::write(&service_path, &rendered)
        .with_context(|| format!("Failed to write {}", service_path.display()))?;

    let status = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .context("Failed to run systemctl daemon-reload")?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "rustfox.service"])
        .status()
        .context("Failed to enable/start rustfox service")?;
    if !status.success() {
        anyhow::bail!("systemctl enable --now failed");
    }

    println!("✓ RustFox installed as a systemd user service");
    println!("  Status: systemctl --user status rustfox");
    println!("  Logs:   journalctl --user -u rustfox -f");
    Ok(())
}

#[cfg(target_os = "linux")]
fn remove_systemd() -> Result<()> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "rustfox.service"])
        .status();
    let status = std::process::Command::new("systemctl")
        .args(["--user", "disable", "rustfox.service"])
        .status()
        .context("Failed to disable service")?;
    if !status.success() {
        anyhow::bail!("systemctl disable failed");
    }

    let service_path = dirs::home_dir()
        .context("HOME not set")?
        .join(".config")
        .join("systemd")
        .join("user")
        .join("rustfox.service");
    let _ = std::fs::remove_file(&service_path);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    println!("✓ RustFox systemd service removed");
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchd(exe: &Path) -> Result<()> {
    let template = include_str!("../../scripts/services/com.rustfox.bot.plist.template");
    let rendered = render_template(template, exe);

    let agent_dir = dirs::home_dir()
        .context("HOME not set")?
        .join("Library")
        .join("LaunchAgents");
    std::fs::create_dir_all(&agent_dir).context("Failed to create LaunchAgents directory")?;

    let plist_path = agent_dir.join("com.rustfox.bot.plist");
    std::fs::write(&plist_path, &rendered)
        .with_context(|| format!("Failed to write {}", plist_path.display()))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()
        .context("Failed to run launchctl load")?;
    if !status.success() {
        anyhow::bail!("launchctl load failed");
    }

    println!("✓ RustFox installed as a launchd agent");
    println!("  Status: launchctl list com.rustfox.bot");
    println!(
        "  Logs:   {}/Library/Logs/rustfox.log",
        dirs::home_dir().unwrap_or_default().display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_launchd() -> Result<()> {
    let agent_dir = dirs::home_dir()
        .context("HOME not set")?
        .join("Library")
        .join("LaunchAgents");
    let plist_path = agent_dir.join("com.rustfox.bot.plist");

    let _ = std::process::Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .status();
    let _ = std::fs::remove_file(&plist_path);

    println!("✓ RustFox launchd agent removed");
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_windows_service(exe: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let template = include_str!("../../scripts/services/install-service.bat.template");
    let rendered = render_template(template, exe);

    let tmp = std::env::temp_dir().join("rustfox-install-service.bat");
    std::fs::write(&tmp, &rendered).context("Failed to write install batch script")?;

    let status = std::process::Command::new("cmd")
        .arg("/c")
        .arg(&tmp)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status()
        .context("Failed to run install-service.bat")?;
    if !status.success() {
        anyhow::bail!("Service installation failed");
    }

    let _ = std::fs::remove_file(&tmp);
    println!("✓ RustFox installed as a Windows service");
    println!("  Manage: sc query RustFox");
    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_windows_service() -> Result<()> {
    use std::os::windows::process::CommandExt;

    let template = include_str!("../../scripts/services/uninstall-service.bat.template");
    let rendered = render_template(template, &std::env::current_exe().unwrap_or_default());

    let tmp = std::env::temp_dir().join("rustfox-uninstall-service.bat");
    std::fs::write(&tmp, &rendered).context("Failed to write uninstall batch script")?;

    let status = std::process::Command::new("cmd")
        .arg("/c")
        .arg(&tmp)
        .creation_flags(0x08000000)
        .status()
        .context("Failed to run uninstall-service.bat")?;
    if !status.success() {
        anyhow::bail!("Service removal failed");
    }

    let _ = std::fs::remove_file(&tmp);
    println!("✓ RustFox Windows service removed");
    Ok(())
}

fn remove() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        remove_systemd()
    }
    #[cfg(target_os = "macos")]
    {
        remove_launchd()
    }
    #[cfg(target_os = "windows")]
    {
        remove_windows_service()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("Service removal is not supported on this platform")
    }
}

fn status() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("systemctl")
            .args(["--user", "--no-pager", "status", "rustfox.service"])
            .output()
            .context("Failed to run systemctl status")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("launchctl")
            .args(["list", "com.rustfox.bot"])
            .output()
            .context("Failed to run launchctl list")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("sc")
            .args(["query", "RustFox"])
            .output()
            .context("Failed to run sc query")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("Service status is not supported on this platform")
    }
}

fn start() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "rustfox.service"])
            .status()
            .context("Failed to start service")?;
        if !status.success() {
            anyhow::bail!("systemctl start failed");
        }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("launchctl")
            .args(["start", "com.rustfox.bot"])
            .status()
            .context("Failed to start service")?;
        if !status.success() {
            anyhow::bail!("launchctl start failed");
        }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("sc")
            .args(["start", "RustFox"])
            .status()
            .context("Failed to start service")?;
        if !status.success() {
            anyhow::bail!("sc start failed");
        }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("Starting services is not supported on this platform")
    }
}

fn stop() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "stop", "rustfox.service"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() {
            anyhow::bail!("systemctl stop failed");
        }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("launchctl")
            .args(["stop", "com.rustfox.bot"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() {
            anyhow::bail!("launchctl stop failed");
        }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("sc")
            .args(["stop", "RustFox"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() {
            anyhow::bail!("sc stop failed");
        }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("Stopping services is not supported on this platform")
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_template_replaces_placeholders() {
        let template = "bin={{RUSTFOX_BIN}}\nconfig={{RUSTFOX_CONFIG}}\nhome={{RUSTFOX_HOME}}\n";
        let bin_path = Path::new("/usr/local/bin/rustfox");
        let result = render_template(template, bin_path);
        assert!(result.contains("/usr/local/bin/rustfox"));
        assert!(result.contains(".rustfox/config.toml"));
        assert!(result.contains(".rustfox\n"));
        assert!(!result.contains("{{RUSTFOX_BIN}}"));
        assert!(!result.contains("{{RUSTFOX_CONFIG}}"));
        assert!(!result.contains("{{RUSTFOX_HOME}}"));
    }

    #[test]
    fn test_render_template_empty_home_does_not_panic() {
        let template = "{{RUSTFOX_HOME}}";
        let bin_path = Path::new("/usr/local/bin/rustfox");
        let result = render_template(template, bin_path);
        assert!(!result.contains("{{"));
    }
}
