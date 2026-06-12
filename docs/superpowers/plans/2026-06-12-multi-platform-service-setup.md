# Multi-Platform Service Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate setup wizard into `rustfox --setup`, add `rustfox --service` for background service management across Linux/macOS/Windows, and provide build scripts + CI for release artifacts.

**Architecture:** Extract existing wizard from `src/bin/setup.rs` into new `src/setup/` library module (wizard + service sub-modules). Main binary absorbs `--setup` and `--service` flags via manual arg parsing (no new crate deps). Service templates use `{{MUSTACHE}}` placeholders rendered at install time via `std::env::current_exe()`.

**Tech Stack:** Rust, axum (already dep), serde (already dep), toml (already dep), dirs (already dep), tokio (already dep).

---

### Task 1: Add `pub mod setup` to lib.rs and create module files

**Files:**
- Modify: `src/lib.rs`
- Create: `src/setup/mod.rs`
- Create: `src/setup/service.rs` (minimal stub)
- Create: `src/setup/wizard.rs` (minimal stub)

- [ ] **Step 1: Create `src/setup/` directory**

```bash
mkdir -p src/setup
```

- [ ] **Step 2: Create `src/setup/service.rs` stub**

Create `src/setup/service.rs`:
```rust
pub enum Action { Install, Remove, Status, Start, Stop }
pub fn handle(_action: Action) -> anyhow::Result<()> {
    anyhow::bail!("Service commands not yet implemented")
}
```

- [ ] **Step 3: Create `src/setup/wizard.rs` stub**

Create `src/setup/wizard.rs`:
```rust
use std::path::Path;
pub async fn run(_config_dir: &Path, _cli: bool) -> anyhow::Result<()> {
    anyhow::bail!("Setup wizard not yet implemented")
}
```

- [ ] **Step 4: Create `src/setup/mod.rs` with CLI types and dispatch**

Create `src/setup/mod.rs`:
```rust
//! Setup module — CLI subcommands for `--setup` and `--service`.
pub mod service;
pub mod wizard;

/// Subcommands for `rustfox --setup` and `rustfox --service`.
pub enum Command {
    Setup { cli: bool },
    Service { action: service::Action },
}

/// Parse argv into Command or return None (meaning: normal bot start).
/// Also captures `--config <PATH>` and stores it in `RUSTFOX_CONFIG_PATH` env var.
pub fn parse_args() -> Option<Command> {
    let mut args: Vec<String> = std::env::args().collect();
    args.remove(0);

    let mut i = 0;
    let mut config_path: Option<String> = None;
    let mut command: Option<Command> = None;

    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--setup" => {
                let cli = args.get(i + 1).map(|s| s.as_str()) == Some("--cli");
                command = Some(Command::Setup { cli });
                if cli { i += 1; }
            }
            "--service" => {
                let action_str = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                let action = match action_str {
                    "install" => service::Action::Install,
                    "remove"  => service::Action::Remove,
                    "status"  => service::Action::Status,
                    "start"   => service::Action::Start,
                    "stop"    => service::Action::Stop,
                    _ => {
                        eprintln!("Usage: rustfox --service <install|remove|status|start|stop>");
                        std::process::exit(1);
                    }
                };
                command = Some(Command::Service { action });
                i += 1;
            }
            _ => {
                if config_path.is_none() && !args[i].starts_with('-') {
                    config_path = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    if let Some(path) = config_path {
        std::env::set_var("RUSTFOX_CONFIG_PATH", path);
    }

    command
}
```

- [ ] **Step 5: Add `pub mod setup` to lib.rs**

Edit `src/lib.rs` to add after line 17 (`pub mod tools;`):
```rust
pub mod setup;
```

Place it before `pub mod utils;` so the module order stays somewhat sorted.

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`
Expected: compilation succeeds

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/setup/
git commit -m "feat: add setup module with CLI arg parsing"
```

---

### Task 2: Create service templates

**Files:**
- Create: `scripts/services/rustfox.service.template`
- Create: `scripts/services/com.rustfox.bot.plist.template`
- Create: `scripts/services/install-service.bat.template`
- Create: `scripts/services/uninstall-service.bat.template`

- [ ] **Step 1: Create systemd user service template**

Create `scripts/services/rustfox.service.template`:
```ini
[Unit]
Description=RustFox Telegram AI Assistant
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={{RUSTFOX_BIN}} --config {{RUSTFOX_CONFIG}}
Restart=on-failure
RestartSec=5
Environment=RUSTFOX_HOME={{RUSTFOX_HOME}}

[Install]
WantedBy=default.target
```

- [ ] **Step 2: Create launchd agent template**

Create `scripts/services/com.rustfox.bot.plist.template`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.rustfox.bot</string>
    <key>ProgramArguments</key>
    <array>
        <string>{{RUSTFOX_BIN}}</string>
        <string>--config</string>
        <string>{{RUSTFOX_CONFIG}}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUSTFOX_HOME</key>
        <string>{{RUSTFOX_HOME}}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{{RUSTFOX_HOME}}/Library/Logs/rustfox.log</string>
    <key>StandardErrorPath</key>
    <string>{{RUSTFOX_HOME}}/Library/Logs/rustfox.log</string>
</dict>
</plist>
```

- [ ] **Step 3: Create Windows install-service.bat template**

Create `scripts/services/install-service.bat.template`:
```batch
@echo off
sc create RustFox binPath= "{{RUSTFOX_BIN}} --config {{RUSTFOX_CONFIG}}" start= auto
sc description RustFox "RustFox Telegram AI Assistant"
sc failure RustFox reset=86400 actions=restart/5000/restart/10000
sc start RustFox
```

- [ ] **Step 4: Create Windows uninstall-service.bat template**

Create `scripts/services/uninstall-service.bat.template`:
```batch
@echo off
sc stop RustFox
sc delete RustFox
```

- [ ] **Step 5: Commit**

```bash
git add scripts/services/
git commit -m "feat(setup): add service templates for systemd, launchd, windows"
```

---

### Task 3: Create `src/setup/service.rs` with platform service management

**Files:**
- Create: `src/setup/service.rs`

- [ ] **Step 1: Write the service module**

Create `src/setup/service.rs`:
```rust
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
    dirs::home_dir().expect("Could not determine home directory").join(".rustfox")
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
    { install_systemd(&exe) }
    #[cfg(target_os = "macos")]
    { install_launchd(&exe) }
    #[cfg(target_os = "windows")]
    { install_windows_service(&exe) }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { anyhow::bail!("Service installation is not supported on this platform") }
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
    std::fs::create_dir_all(&agent_dir)
        .context("Failed to create LaunchAgents directory")?;

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
    println!("  Logs:   {}/Library/Logs/rustfox.log", dirs::home_dir().unwrap_or_default().display());
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

    // Write the rendered batch to a temp file and execute it
    let tmp = std::env::temp_dir().join("rustfox-install-service.bat");
    std::fs::write(&tmp, &rendered)
        .context("Failed to write install batch script")?;

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
    std::fs::write(&tmp, &rendered)
        .context("Failed to write uninstall batch script")?;

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

// ── Linux helpers ──

fn remove() -> Result<()> {
    #[cfg(target_os = "linux")]
    { remove_systemd() }
    #[cfg(target_os = "macos")]
    { remove_launchd() }
    #[cfg(target_os = "windows")]
    { remove_windows_service() }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { anyhow::bail!("Service removal is not supported on this platform") }
}

fn status() -> Result<()> {
    #[cfg(target_os = "linux")] {
        let output = std::process::Command::new("systemctl")
            .args(["--user", "--no-pager", "status", "rustfox.service"])
            .output()
            .context("Failed to run systemctl status")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(target_os = "macos")] {
        let output = std::process::Command::new("launchctl")
            .args(["list", "com.rustfox.bot"])
            .output()
            .context("Failed to run launchctl list")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(target_os = "windows")] {
        let output = std::process::Command::new("sc")
            .args(["query", "RustFox"])
            .output()
            .context("Failed to run sc query")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))] {
        anyhow::bail!("Service status is not supported on this platform")
    }
}

fn start() -> Result<()> {
    #[cfg(target_os = "linux")] {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "rustfox.service"])
            .status()
            .context("Failed to start service")?;
        if !status.success() { anyhow::bail!("systemctl start failed"); }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(target_os = "macos")] {
        let status = std::process::Command::new("launchctl")
            .args(["start", "com.rustfox.bot"])
            .status()
            .context("Failed to start service")?;
        if !status.success() { anyhow::bail!("launchctl start failed"); }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(target_os = "windows")] {
        let status = std::process::Command::new("sc")
            .args(["start", "RustFox"])
            .status()
            .context("Failed to start service")?;
        if !status.success() { anyhow::bail!("sc start failed"); }
        println!("✓ Service started");
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))] {
        anyhow::bail!("Starting services is not supported on this platform")
    }
}

fn stop() -> Result<()> {
    #[cfg(target_os = "linux")] {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "stop", "rustfox.service"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() { anyhow::bail!("systemctl stop failed"); }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(target_os = "macos")] {
        let status = std::process::Command::new("launchctl")
            .args(["stop", "com.rustfox.bot"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() { anyhow::bail!("launchctl stop failed"); }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(target_os = "windows")] {
        let status = std::process::Command::new("sc")
            .args(["stop", "RustFox"])
            .status()
            .context("Failed to stop service")?;
        if !status.success() { anyhow::bail!("sc stop failed"); }
        println!("✓ Service stopped");
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))] {
        anyhow::bail!("Stopping services is not supported on this platform")
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_template_replaces_placeholders() {
        let template = "bin={{RUSTFOX_BIN}}\nconfig={{RUSTFOX_CONFIG}}\nhome={{RUSTFOX_HOME}}";
        let bin_path = Path::new("/usr/local/bin/rustfox");
        let result = render_template(template, bin_path);
        assert!(result.contains("/usr/local/bin/rustfox"));
        assert!(result.contains(".rustfox/config.toml"));
        assert!(result.contains(".rustfox\n"));
        // Should NOT contain raw placeholders
        assert!(!result.contains("{{RUSTFOX_BIN}}"));
        assert!(!result.contains("{{RUSTFOX_CONFIG}}"));
        assert!(!result.contains("{{RUSTFOX_HOME}}"));
    }

    #[test]
    fn test_render_template_empty_home_does_not_panic() {
        // Should handle gracefully even if home dir is weird
        let template = "{{RUSTFOX_HOME}}";
        let bin_path = Path::new("/usr/local/bin/rustfox");
        let result = render_template(template, bin_path);
        assert!(!result.contains("{{"));
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compilation succeeds

- [ ] **Step 3: Run tests**

Run: `cargo test --lib setup::service::tests -v`
Expected: both tests pass

- [ ] **Step 4: Commit**

```bash
git add src/setup/service.rs
git commit -m "feat(setup): add service management module (systemd, launchd, windows)"
```

---

### Task 4: Create `src/setup/wizard.rs` — extract wizard from bin/setup.rs

**Files:**
- Create: `src/setup/wizard.rs`
- Modify: `src/bin/setup.rs` (thin wrapper — done in Task 6)

- [ ] **Step 1: Write wizard.rs with shared functions**

Create `src/setup/wizard.rs`:
```rust
//! Setup wizard — web (Axum server + browser) and CLI modes.
//!
//! Extracted from `src/bin/setup.rs` so the main binary can reuse it
//! via `rustfox --setup`.

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{oneshot, Mutex};

const INDEX_HTML: &str = include_str!("../../setup/index.html");
const SETUP_PORT: u16 = 8719;

fn redirect_uri() -> String {
    format!("http://localhost:{SETUP_PORT}/oauth/callback")
}

/// Run the setup wizard.
/// If `cli` is true, runs in terminal mode. Otherwise starts an Axum web server.
pub async fn run(config_dir: &Path, cli: bool) -> Result<()> {
    if cli {
        return run_cli(config_dir);
    }
    run_web(config_dir).await
}

// ── OAuth session types ────────────────────────────────────────────────

#[derive(Clone)]
struct OAuthSession {
    server_name: String,
    code_verifier: String,
    client_id: String,
    client_secret: Option<String>,
    token_endpoint: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Clone)]
struct WizardState {
    config_path: PathBuf,
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    oauth_sessions: Arc<Mutex<HashMap<String, OAuthSession>>>,
    http_client: reqwest::Client,
}

// ── Request/response types ─────────────────────────────────────────────

#[derive(Deserialize)]
struct SaveRequest { config: String }

#[derive(Serialize)]
struct SaveResponse { ok: bool, path: String }

#[derive(Serialize, Default)]
pub struct ExistingConfig {
    pub exists: bool,
    pub telegram_token: String,
    pub allowed_user_ids: String,
    pub openrouter_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub location: String,
    pub db_path: String,
    pub supports_vision: bool,
    pub base_url: String,
    pub home_dir: String,
    pub skills_dir: String,
    pub agents_dir: String,
    pub ocr_model_dir: String,
    pub agent_max_iterations: u32,
    pub agent_empty_response_retry_limit: u32,
    pub langsmith_key: String,
    pub langsmith_project: String,
    pub embedding_key: String,
    pub embedding_base_url: String,
    pub embedding_model: String,
    pub embedding_dimensions: u32,
    pub query_rewriter_enabled: bool,
    pub learning_skill_extraction_enabled: bool,
    pub learning_skill_extraction_threshold: u32,
    pub learning_user_model_update_interval: u32,
    pub learning_user_model_cron: String,
    pub mcp_servers: Vec<ExistingMcpServer>,
}

#[derive(Serialize, Default, Clone)]
pub struct ExistingMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawConfig {
    pub telegram: Option<RawTelegram>,
    pub openrouter: Option<RawOpenRouter>,
    pub memory: Option<RawMemory>,
    pub general: Option<RawGeneral>,
    pub agent: Option<RawAgent>,
    pub langsmith: Option<RawLangSmith>,
    pub embedding: Option<RawEmbedding>,
    pub ocr: Option<RawOcr>,
    pub learning: Option<RawLearning>,
    pub supervisor: Option<RawSupervisor>,
    pub subagents: Option<RawSubagents>,
    pub skills: Option<RawSkills>,
    pub agents_config: Option<RawAgentsConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<RawMcpServer>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawTelegram {
    pub bot_token: Option<String>,
    pub allowed_user_ids: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawOpenRouter {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub supports_vision: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawMemory {
    pub database_path: Option<String>,
    pub query_rewriter_enabled: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawGeneral {
    pub location: Option<String>,
    pub home: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawAgent {
    pub max_iterations: Option<u32>,
    pub empty_response_retry_limit: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawLangSmith {
    pub api_key: Option<String>,
    pub project: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawEmbedding {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawOcr {
    pub model_dir: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawLearning {
    pub user_model_path: Option<String>,
    pub skill_extraction_enabled: Option<bool>,
    pub skill_extraction_threshold: Option<u32>,
    pub user_model_update_interval: Option<u32>,
    pub user_model_cron: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSupervisor {
    pub default_autonomy_mode: Option<String>,
    pub artifacts_dir: Option<String>,
    pub risk: Option<RawSupervisorRisk>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSupervisorRisk {
    pub require_approval_for_low: Option<bool>,
    pub require_approval_for_medium: Option<bool>,
    pub auto_execute_only_low: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSubagents {
    pub default_tools: Option<Vec<String>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSkills {
    pub directory: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawAgentsConfig {
    pub directory: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawMcpServer {
    pub name: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
}

// ── OAuth API types ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OAuthStartQuery { server: String, url: String }

#[derive(Serialize)]
struct OAuthStartResponse { state: String, auth_url: String }

#[derive(Deserialize)]
struct OAuthCallbackQuery { code: String, state: String }

#[derive(Deserialize)]
struct OAuthTokenQuery { state: String }

#[derive(Serialize)]
struct OAuthTokenPollResponse {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth_client_secret: Option<String>,
}

#[derive(Deserialize)]
struct OAuthDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

#[derive(Serialize)]
struct ClientRegistrationRequest {
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    token_endpoint_auth_method: String,
}

#[derive(Deserialize)]
struct ClientRegistrationResponse { client_id: String, client_secret: Option<String> }

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

// ── Web mode ───────────────────────────────────────────────────────────

async fn run_web(config_dir: &Path) -> Result<()> {
    let config_path = config_dir.join("config.toml");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let state = WizardState {
        config_path,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        oauth_sessions: Arc::new(Mutex::new(HashMap::new())),
        http_client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/load-config", get(load_config))
        .route("/api/save-config", post(save_config))
        .route("/api/install-service", post(install_service))
        .route("/api/oauth/start", get(oauth_start))
        .route("/oauth/callback", get(oauth_callback))
        .route("/api/oauth/token", get(oauth_token_poll))
        .with_state(state);

    let addr = format!("127.0.0.1:{SETUP_PORT}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    println!("\n============================================");
    println!("  RustFox Setup Wizard");
    println!("  http://localhost:{SETUP_PORT}");
    println!("============================================");
    println!("Press Ctrl-C to exit without saving.\n");

    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        let url = format!("http://localhost:{SETUP_PORT}");
        let _ = std::process::Command::new("xdg-open").arg(&url).status();
        let _ = std::process::Command::new("open").arg(&url).status();
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async { let _ = shutdown_rx.await; })
        .await
        .context("Server error")?;

    Ok(())
}

// ── Web handlers ───────────────────────────────────────────────────────

async fn serve_index() -> Html<&'static str> { Html(INDEX_HTML) }

async fn save_config(
    State(st): State<WizardState>,
    Json(body): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, StatusCode> {
    tokio::fs::write(&st.config_path, &body.config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let path = st.config_path.to_string_lossy().to_string();
    println!("\n✓ config.toml saved to {path}");

    let tx = st.shutdown_tx.lock().await.take();
    if let Some(tx) = tx {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            let _ = tx.send(());
        });
    }

    Ok(Json(SaveResponse { ok: true, path }))
}

/// POST /api/install-service
///
/// Installs the bot as a background service. Returns JSON with success/error.
/// Called by the frontend after config is saved (user clicks "Install as service").
/// Uses spawn_blocking because service::handle() performs synchronous I/O
/// (std::fs::write, std::process::Command) that would block the async runtime.
async fn install_service(
    State(_st): State<WizardState>,
) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(|| {
        crate::setup::service::handle(crate::setup::service::Action::Install)
    })
    .await
    .unwrap_or(Err(anyhow::anyhow!("Task join failed")));
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn load_config(State(st): State<WizardState>) -> Json<ExistingConfig> {
    match tokio::fs::read_to_string(&st.config_path).await {
        Ok(content) => Json(parse_existing_config(&content)),
        Err(_) => Json(ExistingConfig::default()),
    }
}

// ── OAuth handlers ─────────────────────────────────────────────────────

async fn oauth_start(
    State(st): State<WizardState>,
    Query(params): Query<OAuthStartQuery>,
) -> Result<Json<OAuthStartResponse>, (StatusCode, String)> {
    let err = |status: StatusCode, msg: String| (status, msg);

    let parsed = reqwest::Url::parse(&params.url)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("Invalid MCP URL: {e}")))?;
    let mut origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or_default());
    if let Some(port) = parsed.port() { origin = format!("{origin}:{port}"); }

    let discovery = discover_oauth_endpoints(&st.http_client, &origin)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;

    let reg_endpoint = discovery.registration_endpoint.ok_or_else(|| {
        err(StatusCode::NOT_IMPLEMENTED,
            "MCP server does not advertise a Dynamic Client Registration endpoint".into())
    })?;

    let redir = redirect_uri();
    let reg_body = ClientRegistrationRequest {
        client_name: "RustFox Setup".into(),
        redirect_uris: vec![redir.clone()],
        grant_types: vec!["authorization_code".into()],
        response_types: vec!["code".into()],
        token_endpoint_auth_method: "none".into(),
    };

    let reg_resp: ClientRegistrationResponse = st.http_client
        .post(&reg_endpoint).json(&reg_body).send().await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("Registration request failed: {e}")))?
        .json().await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("Registration response parse failed: {e}")))?;

    let code_verifier = pkce_verifier();
    let code_challenge = pkce_challenge(&code_verifier);
    let oauth_state = random_state();

    let mut auth_url = reqwest::Url::parse(&discovery.authorization_endpoint)
        .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("Invalid authorization_endpoint: {e}")))?;
    auth_url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg_resp.client_id)
        .append_pair("redirect_uri", &redir)
        .append_pair("state", &oauth_state)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256");

    st.oauth_sessions.lock().await.insert(
        oauth_state.clone(),
        OAuthSession {
            server_name: params.server.clone(),
            code_verifier,
            client_id: reg_resp.client_id,
            client_secret: reg_resp.client_secret,
            token_endpoint: discovery.token_endpoint,
            access_token: None,
            refresh_token: None,
            expires_in: None,
        },
    );

    Ok(Json(OAuthStartResponse { state: oauth_state, auth_url: auth_url.to_string() }))
}

async fn oauth_callback(
    State(st): State<WizardState>,
    Query(params): Query<OAuthCallbackQuery>,
) -> Html<String> {
    let (server_name, code_verifier, client_id, client_secret, token_endpoint) = {
        let sessions = st.oauth_sessions.lock().await;
        match sessions.get(&params.state) {
            Some(s) => (
                s.server_name.clone(), s.code_verifier.clone(),
                s.client_id.clone(), s.client_secret.clone(),
                s.token_endpoint.clone(),
            ),
            None => return Html(
                "<html><body><p>Unknown OAuth state. Please close this window and try again.</p>\
                 <script>setTimeout(()=>window.close(),3000)</script></body></html>".into(),
            ),
        }
    };

    let redir = redirect_uri();
    let mut token_params = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", params.code.clone()),
        ("redirect_uri", redir),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    if let Some(secret) = client_secret {
        token_params.push(("client_secret", secret));
    }

    match st.http_client.post(&token_endpoint).form(&token_params).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<OAuthTokenResponse>().await {
                Ok(tok) => {
                    if let Some(session) = st.oauth_sessions.lock().await.get_mut(&params.state) {
                        session.access_token = Some(tok.access_token);
                        session.refresh_token = tok.refresh_token;
                        session.expires_in = tok.expires_in;
                    }
                    Html(format!(
                        "<html><head><title>Authorized</title></head><body>\
                         <p style=\"font-family:sans-serif;text-align:center;margin-top:4rem\">\
                         ✅ {server_name} authorization successful! You can close this window.</p>\
                         <script>window.close();</script></body></html>"
                    ))
                }
                Err(e) => Html(format!(
                    "<html><body><p>Failed to parse token response: {e}</p>\
                     <script>setTimeout(()=>window.close(),5000)</script></body></html>"
                )),
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Html(format!(
                "<html><body><p>Token exchange failed ({status}): {body}</p>\
                 <script>setTimeout(()=>window.close(),5000)</script></body></html>"
            ))
        }
        Err(e) => Html(format!(
            "<html><body><p>Token request error: {e}</p>\
             <script>setTimeout(()=>window.close(),5000)</script></body></html>"
        )),
    }
}

async fn oauth_token_poll(
    State(st): State<WizardState>,
    Query(params): Query<OAuthTokenQuery>,
) -> Result<Json<OAuthTokenPollResponse>, StatusCode> {
    let sessions = st.oauth_sessions.lock().await;
    let session = sessions.get(&params.state).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(OAuthTokenPollResponse {
        ready: session.access_token.is_some(),
        token: session.access_token.clone(),
        refresh_token: session.refresh_token.clone(),
        expires_in: session.expires_in,
        token_endpoint: Some(session.token_endpoint.clone()),
        oauth_client_id: Some(session.client_id.clone()),
        oauth_client_secret: session.client_secret.clone(),
    }))
}

// ── OAuth helpers ──────────────────────────────────────────────────────

async fn discover_oauth_endpoints(
    client: &reqwest::Client, origin: &str,
) -> anyhow::Result<OAuthDiscovery> {
    let urls = [
        format!("{origin}/.well-known/oauth-authorization-server"),
        format!("{origin}/.well-known/openid-configuration"),
    ];
    for url in &urls {
        let resp = client.get(url).send().await?;
        if resp.status().is_success() {
            return resp.json::<OAuthDiscovery>()
                .await
                .with_context(|| format!("Failed to parse OAuth discovery from {url}"));
        }
    }
    anyhow::bail!("No OAuth discovery document found at {origin}")
}

fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── CLI mode ───────────────────────────────────────────────────────────

fn run_cli(config_dir: &Path) -> Result<()> {
    use std::io::{self, Write};

    println!("============================================");
    println!("  RustFox CLI Setup");
    println!("============================================");
    println!("Press Enter to accept [defaults].\n");

    let read_line = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim().to_owned())
    };

    let or_default = |s: String, default: &str| {
        if s.is_empty() { default.to_owned() } else { s }
    };

    let tg_token = read_line("Telegram bot token: ")?;
    let user_ids = read_line("Allowed user IDs (comma-separated): ")?;
    let or_key = read_line("OpenRouter API key: ")?;
    let model = or_default(
        read_line("Model [moonshotai/kimi-k2.5]: ")?, "moonshotai/kimi-k2.5",
    );
    let db_path = or_default(read_line("Memory DB path [rustfox.db]: ")?, "rustfox.db");
    let location = read_line("Your location (optional, e.g. Tokyo, Japan): ")?;

    let config = format_config(&ConfigParams {
        tg_token: &tg_token,
        user_ids: &user_ids,
        or_key: &or_key,
        model: &model,
        max_tokens: 4096,
        db_path: &db_path,
        location: &location,
    });

    let config_path = config_dir.join("config.toml");
    std::fs::write(&config_path, &config)
        .with_context(|| format!("Could not write {}", config_path.display()))?;

    println!("\n✓ config.toml saved to {}", config_path.display());

    // Offer service installation
    print!("\nInstall as a background service? [Y/n]: ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    if buf.trim().is_empty() || buf.trim().eq_ignore_ascii_case("y") {
        if let Err(e) = crate::setup::service::handle(crate::setup::service::Action::Install) {
            eprintln!("Warning: Service installation failed: {e}");
            eprintln!("You can retry later with: rustfox --service install");
        }
    }

    Ok(())
}

// ── Config formatting ──────────────────────────────────────────────────

pub struct ConfigParams<'a> {
    pub tg_token: &'a str,
    pub user_ids: &'a str,
    pub or_key: &'a str,
    pub model: &'a str,
    pub max_tokens: u32,
    pub db_path: &'a str,
    pub location: &'a str,
}

pub fn format_config(p: &ConfigParams<'_>) -> String {
    let ids: Vec<&str> = p.user_ids
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let ids_str = ids.join(", ");
    let loc_line = if p.location.is_empty() {
        "# location = \"Your City, Country\"".to_owned()
    } else {
        format!("location = \"{}\"", p.location)
    };
    let tg_token = p.tg_token;
    let or_key = p.or_key;
    let model = p.model;
    let max_tokens = p.max_tokens;
    let db_path = p.db_path;

    format!(
        r#"[telegram]
bot_token = "{tg_token}"
allowed_user_ids = [{ids_str}]

[openrouter]
api_key = "{or_key}"
model = "{model}"
base_url = "https://openrouter.ai/api/v1"
max_tokens = {max_tokens}
system_prompt = """You are a helpful AI assistant with access to tools. \
Use the available tools to help the user with their tasks. \
When using file or terminal tools, operate only within the allowed sandbox directory. \
Be concise and helpful."""

[memory]
database_path = "{db_path}"

[skills]
directory = "skills"

[general]
{loc_line}
"#
    )
}

// ── Config parsing ─────────────────────────────────────────────────────

pub fn parse_existing_config(content: &str) -> ExistingConfig {
    let raw: RawConfig = match toml::from_str(content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Could not parse existing config.toml: {e}");
            return ExistingConfig::default();
        }
    };

    let tg = raw.telegram.clone().unwrap_or_default();
    let openrouter = raw.openrouter.clone().unwrap_or_default();
    let mem = raw.memory.clone().unwrap_or_default();

    let allowed_user_ids = tg.allowed_user_ids.unwrap_or_default().iter()
        .map(|v| match v {
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let mcp_servers = raw.mcp_servers.clone().into_iter()
        .filter_map(|s| {
            let name = s.name.filter(|n| !n.is_empty())?;
            Some(ExistingMcpServer {
                name,
                command: s.command.unwrap_or_default(),
                args: s.args,
                env: s.env,
            })
        })
        .collect();

    let mut cfg = ExistingConfig {
        exists: true,
        telegram_token: tg.bot_token.unwrap_or_default(),
        allowed_user_ids,
        openrouter_key: openrouter.api_key.clone().unwrap_or_default(),
        model: openrouter.model.clone().unwrap_or_default(),
        max_tokens: openrouter.max_tokens.unwrap_or(0),
        system_prompt: openrouter.system_prompt.clone().unwrap_or_default(),
        location: raw.general.as_ref().and_then(|g| g.location.clone()).unwrap_or_default(),
        db_path: mem.database_path.clone().unwrap_or_default(),
        mcp_servers,
        ..ExistingConfig::default()
    };

    if let Some(ref or_cfg) = raw.openrouter {
        cfg.supports_vision = or_cfg.supports_vision.unwrap_or(false);
        cfg.base_url = or_cfg.base_url.clone().unwrap_or_default();
    }
    if let Some(ref general) = raw.general {
        cfg.home_dir = general.home.clone().unwrap_or_default();
    }
    if let Some(ref agent) = raw.agent {
        cfg.agent_max_iterations = agent.max_iterations.unwrap_or(25);
        cfg.agent_empty_response_retry_limit = agent.empty_response_retry_limit.unwrap_or(3);
    }
    if let Some(ref langsmith) = raw.langsmith {
        cfg.langsmith_key = langsmith.api_key.clone().unwrap_or_default();
        cfg.langsmith_project = langsmith.project.clone().unwrap_or_default();
    }
    if let Some(ref embedding) = raw.embedding {
        cfg.embedding_key = embedding.api_key.clone().unwrap_or_default();
        cfg.embedding_base_url = embedding.base_url.clone().unwrap_or_default();
        cfg.embedding_model = embedding.model.clone().unwrap_or_default();
        cfg.embedding_dimensions = embedding.dimensions.unwrap_or(0);
    }
    if let Some(ref ocr) = raw.ocr {
        cfg.ocr_model_dir = ocr.model_dir.clone().unwrap_or_default();
    }
    if let Some(ref learning) = raw.learning {
        cfg.learning_skill_extraction_enabled = learning.skill_extraction_enabled.unwrap_or(false);
        cfg.learning_skill_extraction_threshold = learning.skill_extraction_threshold.unwrap_or(0);
        cfg.learning_user_model_update_interval = learning.user_model_update_interval.unwrap_or(0);
        cfg.learning_user_model_cron = learning.user_model_cron.clone().unwrap_or_default();
    }
    if let Some(ref skills) = raw.skills {
        cfg.skills_dir = skills.directory.clone().unwrap_or_default();
    }
    if let Some(ref agents) = raw.agents_config {
        cfg.agents_dir = agents.directory.clone().unwrap_or_default();
    }

    cfg
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_toml_returns_not_exists() {
        let cfg = parse_existing_config("this is not valid toml !!!");
        assert!(!cfg.exists);
    }

    #[test]
    fn test_pkce_verifier_length() {
        let v = pkce_verifier();
        assert_eq!(v.len(), 43);
    }

    #[test]
    fn test_pkce_challenge_is_base64url() {
        let verifier = pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        assert_eq!(challenge.len(), 43);
    }

    #[test]
    fn test_random_state_is_32_hex_chars() {
        let s = random_state();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    fn cfg(tg: &str, ids: &str, key: &str, model: &str,
           _sandbox: &str, db: &str, loc: &str) -> String {
        format_config(&ConfigParams {
            tg_token: tg, user_ids: ids, or_key: key, model,
            max_tokens: 4096, db_path: db, location: loc,
        })
    }

    #[test]
    fn test_telegram_section_present() {
        let out = cfg("mytoken", "123456", "key", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[telegram]"));
        assert!(out.contains(r#"bot_token = "mytoken""#));
    }

    #[test]
    fn test_openrouter_section_present() {
        let out = cfg("t", "1", "sk-or-abc", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[openrouter]"));
        assert!(out.contains(r#"api_key = "sk-or-abc""#));
    }

    #[test]
    fn test_location_included_when_set() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "Tokyo, Japan");
        assert!(out.contains(r#"location = "Tokyo, Japan""#));
    }

    #[test]
    fn test_location_commented_when_empty() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("# location ="));
        assert!(!out.contains("\nlocation = "));
    }

    #[test]
    fn test_multiple_user_ids_comma_separated() {
        let out = cfg("t", "111, 222, 333", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("allowed_user_ids = [111, 222, 333]"));
    }

    // ── Tests migrated from src/bin/setup.rs ──

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[telegram]
bot_token = "mytoken123"
allowed_user_ids = [111, 222]

[openrouter]
api_key = "sk-or-test"
model = "gpt-4o"
max_tokens = 2048
system_prompt = "Be helpful."

[sandbox]
allowed_directory = "/tmp/test"

[memory]
database_path = "test.db"

[general]
location = "Tokyo, Japan"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "mytoken123");
        assert_eq!(cfg.allowed_user_ids, "111, 222");
        assert_eq!(cfg.openrouter_key, "sk-or-test");
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.max_tokens, 2048);
        assert_eq!(cfg.system_prompt, "Be helpful.");
        assert_eq!(cfg.location, "Tokyo, Japan");
        assert_eq!(cfg.db_path, "test.db");
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn test_parse_config_with_mcp_servers() {
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = [1]

[openrouter]
api_key = "k"

[sandbox]
allowed_directory = "/tmp"

[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]

[[mcp_servers]]
name = "brave-search"
command = "npx"
args = ["-y", "@brave/brave-search-mcp-server"]
[mcp_servers.env]
BRAVE_API_KEY = "brave123"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.mcp_servers.len(), 2);
        assert_eq!(cfg.mcp_servers[0].name, "git");
        assert_eq!(cfg.mcp_servers[0].command, "uvx");
        assert_eq!(cfg.mcp_servers[0].args, vec!["mcp-server-git"]);
        assert!(cfg.mcp_servers[0].env.is_empty());
        assert_eq!(cfg.mcp_servers[1].name, "brave-search");
        assert_eq!(cfg.mcp_servers[1].env.get("BRAVE_API_KEY").unwrap(), "brave123");
    }

    #[test]
    fn test_parse_partial_config_missing_sections_default_to_empty() {
        let toml = r#"
[telegram]
bot_token = "partial"
allowed_user_ids = [42]
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "partial");
        assert_eq!(cfg.model, "");
    }

    #[test]
    fn test_parse_string_user_ids() {
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = ["111", "222"]

[openrouter]
api_key = "k"

[sandbox]
allowed_directory = "/tmp"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.allowed_user_ids, "111, 222");
    }

    #[test]
    fn test_skills_section_present() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("[skills]"));
        assert!(out.contains(r#"directory = "skills""#));
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compilation succeeds

- [ ] **Step 3: Run tests**

Run: `cargo test --lib setup::wizard::tests -v`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/setup/wizard.rs
git commit -m "feat(setup): extract wizard into library module (web + CLI + OAuth)"
```

---

### Task 5: Update `src/bin/setup.rs` as thin wrapper

**Files:**
- Modify: `src/bin/setup.rs`

- [ ] **Step 1: Rewrite setup.rs as thin wrapper**

Replace the entire content of `src/bin/setup.rs` with:
```rust
//! Thin wrapper — delegates to `rustfox::setup::wizard`.
//!
//! Kept for backwards compat with `./setup.sh` and `cargo run --bin setup`.
//! New users should use `rustfox --setup` instead.

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = std::env::args().any(|a| a == "--cli");
    let config_dir = std::env::var("RUSTFOX_CONFIG_PATH")
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    rustfox::setup::wizard::run(&config_dir, cli).await
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compilation succeeds

- [ ] **Step 3: Run tests**

Run: `cargo test -v`
Expected: all existing tests still pass

- [ ] **Step 4: Commit**

```bash
git add src/bin/setup.rs
git commit -m "refactor(setup): bin/setup.rs becomes thin wrapper around rustfox::setup::wizard"
```

---

### Task 6: Update `src/main.rs` with `--setup` and `--service` dispatch

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add import for setup module**

Add after line 15:
```rust
use rustfox::setup;
```

- [ ] **Step 2: Add setup/service dispatch before config loading**

Replace the config-path detection block (lines 28-47) with:

```rust
    // Check for --setup and --service subcommands before doing anything else
    if let Some(cmd) = setup::parse_args() {
        match cmd {
            setup::Command::Setup { cli } => {
                let config_dir = std::env::var("RUSTFOX_CONFIG_PATH")
                    .map(PathBuf::from)
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                return setup::wizard::run(&config_dir, cli).await;
            }
            setup::Command::Service { action } => {
                setup::service::handle(action)?;
                return Ok(());
            }
        }
    }

    // If we reach here, it's a normal bot start — resolve config path
    let config_path = if let Ok(path) = std::env::var("RUSTFOX_CONFIG_PATH") {
        PathBuf::from(path)
    } else {
        let cwd = PathBuf::from("config.toml");
        if cwd.exists() {
            cwd
        } else {
            let env_home = std::env::var("RUSTFOX_HOME").ok();
            if let Some(home) =
                rustfox::home::default_home(env_home.as_deref(), dirs::home_dir().as_deref())
            {
                let candidate = home.join("config.toml");
                if candidate.exists() { candidate } else { cwd }
            } else {
                cwd
            }
        }
    };
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compilation succeeds

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add --setup and --service dispatch to main binary"
```

---

### Task 7: Update `release.yml` for single-binary release

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Create release workflow**

Create `.github/workflows/release.yml` (replaces the previous workflow — setup binary is now part of the main binary, so only `rustfox` is shipped):

Key changes from the previous version:
1. Single binary (setup is now part of `rustfox` via `--setup`)
2. Service templates bundled in archives
3. Native target only (no cross-compilation)

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: Build ${{ matrix.target }}
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            rustfox_bin: rustfox
            archive_name: rustfox-${{ github.ref_name }}-x86_64-unknown-linux-gnu.tar.gz
          - os: macos-latest
            target: aarch64-apple-darwin
            rustfox_bin: rustfox
            archive_name: rustfox-${{ github.ref_name }}-aarch64-apple-darwin.tar.gz
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            rustfox_bin: rustfox.exe
            archive_name: rustfox-${{ github.ref_name }}-x86_64-pc-windows-msvc.zip

    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}

      - name: Build
        run: cargo build --release

      - name: Stage release files
        shell: bash
        run: |
          mkdir staging
          cp target/release/${{ matrix.rustfox_bin }} staging/
          cp config.example.toml staging/
          cp scripts/install.sh staging/
          cp -r scripts/services staging/services
          # Only for Windows
          if [ "${{ matrix.target }}" = "x86_64-pc-windows-msvc" ]; then
            cp scripts/services/install-service.bat.template staging/install-service.bat
            cp scripts/services/uninstall-service.bat.template staging/uninstall-service.bat
          fi

      - name: Create archive (Unix)
        if: runner.os != 'Windows'
        run: tar -czf ${{ matrix.archive_name }} -C staging .

      - name: Create archive (Windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: Compress-Archive -Path staging\* -DestinationPath ${{ matrix.archive_name }}

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.archive_name }}
          path: ${{ matrix.archive_name }}
          if-no-files-found: error

  release:
    name: Create Release
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write

    steps:
      - uses: actions/checkout@v4

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts

      - name: Create release
        uses: softprops/action-gh-release@v2
        with:
          files: artifacts/**/*
          generate_release_notes: true
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: update release workflow for single-binary + service templates"
```

---

### Task 8: Create universal install script

**Files:**
- Create: `scripts/install.sh`

- [ ] **Step 1: Write install.sh**

Create `scripts/install.sh`:
```bash
#!/usr/bin/env bash
# RustFox universal installer.
# Detects platform, installs via cargo, runs setup, offers service install.
set -euo pipefail

RUSTFOX_VERSION="${1:-latest}"

echo "============================================"
echo "  RustFox Installer"
echo "============================================"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# When run from the repo, SCRIPT_DIR = <repo>/scripts/, so project root is SCRIPT_DIR/..
# When run from a release archive, install.sh is at archive root (no Cargo.toml there;
# the archive is for binary-only installs — use `tar xzf` and run `rustfox --setup`).
PROJECT_ROOT="$SCRIPT_DIR"
if [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
    PROJECT_ROOT="$SCRIPT_DIR"
elif [ -f "$SCRIPT_DIR/../Cargo.toml" ]; then
    PROJECT_ROOT="$SCRIPT_DIR/.."
else
    echo "Error: Cannot find Cargo.toml. Run install.sh from the rustfox repository root."
    exit 1
fi

# Check prerequisites
if ! command -v cargo &>/dev/null; then
    echo "Error: Rust/Cargo not found."
    echo "Install Rust from https://rustup.rs and try again."
    exit 1
fi

# Install from source
echo ""
echo "Installing rustfox from ${PROJECT_ROOT}..."
cargo install --path "$PROJECT_ROOT" --locked

echo ""
echo "✓ rustfox installed to $(which rustfox)"

# Offer setup
echo ""
echo "Run the setup wizard to configure your bot:"
echo "  rustfox --setup"
echo ""
echo "Or use the CLI wizard:"
echo "  rustfox --setup --cli"
echo ""
echo "After setup, install as a background service:"
echo "  rustfox --service install"
```

- [ ] **Step 2: Make it executable and commit**

```bash
chmod +x scripts/install.sh
git add scripts/install.sh
git commit -m "feat: add universal install script for rustfox"
```

---

### Task 9: Create Debian/RedHat package build scripts (deferred — minimal stubs)

**Files:**
- Create: `scripts/build-deb.sh`
- Create: `scripts/build-rpm.sh`
- Create: `scripts/build-macos.sh`
- Create: `scripts/build-windows.ps1`

Note: These are entry-point stubs that document the intended packaging approach. Full
implementation (control files, postinst/prerm scripts, signing) is deferred.

- [ ] **Step 1: Create Debian build script stub**

Create `scripts/build-deb.sh`:
```bash
#!/usr/bin/env bash
# Build .deb package from a pre-built binary in dist/
# Usage: TARGET=x86_64-unknown-linux-gnu scripts/build-deb.sh
set -euo pipefail
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
case "$TARGET" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
  *) echo "Unknown arch for $TARGET"; exit 1 ;;
esac
echo "TODO: build .deb for $ARCH from dist/rustfox"
echo "See docs/superpowers/specs/2026-06-12-multi-platform-service-setup-design.md"
```

- [ ] **Step 2: Create RPM build script stub**

Create `scripts/build-rpm.sh`:
```bash
#!/usr/bin/env bash
# Build .rpm package from a pre-built binary in dist/
set -euo pipefail
echo "TODO: build .rpm from dist/rustfox"
echo "See docs/superpowers/specs/2026-06-12-multi-platform-service-setup-design.md"
```

- [ ] **Step 3: Create macOS build script stub**

Create `scripts/build-macos.sh`:
```bash
#!/usr/bin/env bash
# Build .tar.gz from a pre-built binary in dist/
set -euo pipefail
echo "TODO: build macOS .tar.gz from dist/rustfox"
echo "See docs/superpowers/specs/2026-06-12-multi-platform-service-setup-design.md"
```

- [ ] **Step 4: Create Windows build script stub**

Create `scripts/build-windows.ps1`:
```powershell
# Build .zip from a pre-built binary in dist/
Write-Output "TODO: build Windows .zip from dist/rustfox.exe"
Write-Output "See docs/superpowers/specs/2026-06-12-multi-platform-service-setup-design.md"
```

- [ ] **Step 5: Make scripts executable and commit**

```bash
chmod +x scripts/build-deb.sh scripts/build-rpm.sh scripts/build-macos.sh
git add scripts/build-deb.sh scripts/build-rpm.sh scripts/build-macos.sh scripts/build-windows.ps1
git commit -m "chore: add platform build script stubs"
```

---

### Task 10: Final verification

- [ ] **Step 1: Run cargo check**

Run: `cargo check`
Expected: success

- [ ] **Step 2: Run cargo clippy**

Run: `cargo clippy -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Run cargo fmt**

Run: `cargo fmt --all -- --check`
Expected: no formatting errors

- [ ] **Step 4: Run cargo test**

Run: `cargo test -v`
Expected: all tests pass (existing + new setup tests)
