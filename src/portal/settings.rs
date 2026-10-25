//! Safe settings editing (ADR 0006): whitelisted field PATCH over the live
//! `config.toml`, masked secret projection, `.bak` backup before every write,
//! plus a soul-file editor for non-secret markdown identity files.
//!
//! Raw TOML NEVER crosses the API boundary. Fields are applied through a
//! typed whitelist; each maps to a section/key path in the parsed document.

use std::path::Path;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::error::PortalError;
use super::PortalState;

/// Whitelisted editable scalar fields (docs/portal-api.md → Settings).
/// The wire format is camelCase (the SPA's contract), the Rust fields are
/// snake_case — `rename_all` keeps both honest. Without it, unknown keys
/// silently deserialized to `None` and PATCHes no-oped with a 200.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub general_location: Option<String>,
    #[serde(default)]
    pub default_autonomy_mode: Option<String>,
    #[serde(default)]
    pub portal_port: Option<u16>,
}

fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "••••".to_string();
    }
    let head: String = chars.iter().take(5).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// GET /api/opencode/status — same JSON as wizard (session-gated here).
pub async fn opencode_status() -> Json<Value> {
    let status = crate::opencode::detect_async().await;
    Json(status.to_json())
}

/// GET /api/settings — whitelisted, typed projection with masked secrets.
pub async fn get_settings(State(state): State<PortalState>) -> Result<Json<Value>, PortalError> {
    let cfg = state.agent.config();
    let model = state.agent.current_model().await;

    let mut masked_providers = Vec::new();
    for p in &cfg.provider {
        masked_providers.push(json!({
            "name": p.name,
            "model": p.model,
            "baseUrl": p.base_url,
            "apiKeyMasked": p.api_key.as_deref().map(mask_secret),
        }));
    }

    let live_mode = *state.sandbox_mode.read().unwrap_or_else(|e| e.into_inner());
    let workspace = cfg.sandbox.allowed_directory.clone();
    let sandbox_info = sandbox_status_json(live_mode, workspace).await;

    Ok(Json(json!({
        "editable": {
            "model": model,
            "generalLocation": cfg.general.as_ref().and_then(|g| g.location.clone()).unwrap_or_default(),
            "defaultAutonomyMode": cfg.supervisor.default_autonomy_mode,
            "portalPort": cfg.portal.port,
        },
        // ADR 0011 R7: system-prompt provenance so the SPA can show which
        // layer is live and warn about the divergence trap.
        "systemPrompt": {
            "source": match cfg.system_prompt_source() {
                crate::config::SystemPromptSource::File(_) => "file",
                crate::config::SystemPromptSource::Inline => "inline",
                crate::config::SystemPromptSource::Builtin => "builtin",
            },
            "pointer": cfg.openrouter.system_prompt_file.as_ref().map(|p| p.display().to_string()),
            "divergence": cfg.system_prompt_divergence(),
        },
        "masked": {
            "telegramBotToken": mask_secret(&cfg.telegram.bot_token),
            "openrouterApiKey": mask_secret(&cfg.openrouter.api_key),
            "providers": masked_providers,
            "embeddingApiKey": cfg.embedding.as_ref().map(|e| mask_secret(&e.api_key)),
            "mcpServers": crate::portal::mcp::list_projected(&state).unwrap_or_else(|_| {
                cfg.mcp_servers
                    .iter()
                    .map(|m| crate::portal::mcp::project_server(m, state.agent.mcp_is_connected(&m.name)))
                    .collect()
            }),
        },
        "sandbox": sandbox_info,
        "restartRequired": ["portalPort", "portalEnabled", "portalBind", "portalToken"],
    })))
}

/// Back up `config.toml` → `config.toml.bak`, then atomically write new content.
async fn write_config_with_backup(path: &Path, new_content: &str) -> Result<(), PortalError> {
    if path.exists() {
        let bak = path.with_extension("toml.bak");
        tokio::fs::copy(path, &bak)
            .await
            .map_err(PortalError::internal)?;
    }
    let tmp = path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, new_content)
        .await
        .map_err(PortalError::internal)?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(PortalError::internal)?;
    Ok(())
}

/// Read → parse TOML document as a generic table (lossless-ish for our use).
pub(super) async fn load_doc(path: &Path) -> Result<toml::Value, PortalError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(PortalError::internal)?;
    // toml 1.x Value::from_str is single-value only; from_str parses documents.
    toml::from_str(&content).map_err(|e| PortalError::internal(format!("Config parse error: {e}")))
}

fn doc_table<'a>(doc: &'a mut toml::Value, section: &str) -> &'a mut toml::value::Table {
    if doc.get(section).is_none() {
        if let Some(t) = doc.as_table_mut() {
            t.insert(section.to_string(), toml::Value::Table(Default::default()));
        }
    }
    doc.get_mut(section)
        .and_then(|v| v.as_table_mut())
        .expect("section is a table")
}

/// PATCH /api/settings — apply whitelisted fields, backup first.
pub async fn patch_settings(
    State(state): State<PortalState>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<Value>, PortalError> {
    let mut updated: Vec<String> = Vec::new();
    let mut restart_required: Vec<String> = Vec::new();
    let path = state.config_path.as_path();

    // `model` has a live setter — no TOML edit needed here (and set_model
    // writes the file itself, so ordering matters: model goes first).
    if let Some(model) = patch.model.filter(|m| !m.trim().is_empty()) {
        state
            .agent
            .set_model(model.trim().to_string())
            .await
            .map_err(|e| PortalError::bad_request("model_rejected", e.to_string()))?;
        updated.push("model".to_string());
    }

    let mut doc = match load_doc(path).await {
        Ok(d) => d,
        Err(e) => {
            if updated.is_empty() {
                return Err(e);
            }
            // model already persisted; keep the file edits minimal on failure
            tracing::warn!("Portal settings: reload failed after model change: {:?}", e);
            return Ok(Json(
                json!({ "updated": updated, "restartRequired": restart_required }),
            ));
        }
    };
    let mut dirty = false;

    if let Some(loc) = patch.general_location {
        doc_table(&mut doc, "general").insert("location".to_string(), toml::Value::String(loc));
        dirty = true;
        updated.push("generalLocation".to_string());
        restart_required.push("generalLocation".to_string());
    }
    if let Some(mode) = patch.default_autonomy_mode {
        if !["default", "autopilot", "plan"].contains(&mode.as_str()) {
            return Err(PortalError::bad_request(
                "invalid_autonomy_mode",
                "must be one of: default, autopilot, plan",
            ));
        }
        doc_table(&mut doc, "supervisor").insert(
            "default_autonomy_mode".to_string(),
            toml::Value::String(mode),
        );
        dirty = true;
        updated.push("defaultAutonomyMode".to_string());
        restart_required.push("defaultAutonomyMode".to_string());
    }
    if let Some(port) = patch.portal_port {
        if port == 0 {
            return Err(PortalError::bad_request(
                "invalid_portal_port",
                "port must be 1-65535",
            ));
        }
        doc_table(&mut doc, "portal").insert("port".to_string(), toml::Value::Integer(port as i64));
        dirty = true;
        updated.push("portalPort".to_string());
        restart_required.push("portalPort".to_string());
    }

    if dirty {
        let new_content = toml::to_string_pretty(&doc)
            .map_err(|e| PortalError::internal(format!("Config serialize error: {e}")))?;
        write_config_with_backup(path, &new_content).await?;
        tracing::info!("Portal settings: updated {:?} (backup written)", updated);
    }

    Ok(Json(json!({
        "updated": updated,
        "restartRequired": restart_required,
    })))
}

// ---------------------------------------------------------------------------
// Soul files (markdown identity — no secrets, direct read/write)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SoulWrite {
    pub name: String,
    pub content: String,
}

/// Resolve a whitelisted soul file name to its path under the RustFox home.
fn soul_path(state: &PortalState, name: &str) -> Option<std::path::PathBuf> {
    let cfg = state.agent.config();
    let home = cfg.resolved_home()?;
    // ADR 0011 R7: "system" is the prompt-file editor. It resolves to the
    // configured system_prompt_file pointer; when the pointer is unset it
    // falls back to the conventional prompts/system.md so the file can be
    // prepared — enabling it still requires the one-line config pointer
    // (GET /api/settings reports which source is live).
    if matches!(name, "system" | "SYSTEM.md" | "prompts/system.md") {
        return Some(
            match cfg
                .openrouter
                .system_prompt_file
                .as_deref()
                .filter(|p| !p.as_os_str().is_empty())
            {
                Some(ptr) => cfg.resolve_prompt_path(ptr),
                None => home.join("prompts/system.md"),
            },
        );
    }
    let file = match name {
        "SOUL.md" | "soul" => "SOUL.md",
        "USER.md" | "user" => "USER.md",
        "AGENTS.md" | "agents" => "AGENTS.md",
        "MEMORY.md" | "memory" => "MEMORY.md",
        _ => return None,
    };
    Some(home.join(file))
}

/// GET /api/soul?name=SOUL.md
pub async fn get_soul(
    State(state): State<PortalState>,
    axum::extract::Query(q): axum::extract::Query<SoulName>,
) -> Result<Json<Value>, PortalError> {
    let path = soul_path(&state, &q.name)
        .ok_or_else(|| PortalError::bad_request("unknown_soul_file", "not in whitelist"))?;
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(PortalError::internal(e)),
    };
    let mtime = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "name": path.file_name().and_then(|n| n.to_str()).unwrap_or(&q.name),
        "content": content,
        "mtime": mtime,
    })))
}

#[derive(Deserialize)]
pub struct SoulName {
    #[serde(default = "default_soul_name")]
    pub name: String,
}

fn default_soul_name() -> String {
    "SOUL.md".to_string()
}

/// PUT /api/soul — write soul file with backup + soul_updated nudge.
pub async fn put_soul(
    State(state): State<PortalState>,
    Json(body): Json<SoulWrite>,
) -> Result<Json<Value>, PortalError> {
    let path = soul_path(&state, &body.name)
        .ok_or_else(|| PortalError::bad_request("unknown_soul_file", "not in whitelist"))?;
    if body.content.is_empty() {
        return Err(PortalError::bad_request(
            "empty_soul",
            "Refusing to write an empty soul file",
        ));
    }
    // The system-prompt entry may live in a not-yet-created subdir
    // (prompts/); soul siblings of home always exist.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(PortalError::internal)?;
    }
    if path.exists() {
        let bak = path.with_file_name(format!(
            "{}.bak",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("soul")
        ));
        tokio::fs::copy(&path, &bak)
            .await
            .map_err(PortalError::internal)?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("soul")
    ));
    tokio::fs::write(&tmp, &body.content)
        .await
        .map_err(PortalError::internal)?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(PortalError::internal)?;
    // Nudge the agent so it re-reads identity before the next turn.
    state.agent.set_soul_updated(true);
    tracing::info!("Portal: wrote soul file {}", path.display());
    Ok(Json(
        json!({ "ok": true, "name": body.name, "bytes": body.content.len() }),
    ))
}

/// Probe (like the wizard) and project install / blocked-by-system UX for Settings.
async fn sandbox_status_json(
    live_mode: crate::sandbox::SandboxMode,
    workspace: std::path::PathBuf,
) -> Value {
    let outcome = tokio::task::spawn_blocking(move || {
        crate::sandbox::probe_sandbox(Some(workspace.as_path()))
    })
    .await
    .unwrap_or_else(|_| {
        crate::sandbox::ProbeOutcome::Failed(crate::sandbox::ProbeFailure::OtherFailure(
            "probe task failed".into(),
        ))
    });
    let ux = crate::sandbox::install_ux_for_probe(&outcome);
    sandbox_status_json_from_ux(live_mode, &ux)
}

/// Pure projection of a probe's install UX into the Settings JSON (testable
/// without a real probe).
fn sandbox_status_json_from_ux(
    live_mode: crate::sandbox::SandboxMode,
    ux: &crate::sandbox::InstallUx,
) -> Value {
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    };
    let already_sandbox = live_mode == crate::sandbox::SandboxMode::Sandbox;

    let mut can_install = false;
    let mut install_command: Option<String> = None;
    let mut blocked_by_system = false;
    let mut status_message: Option<&'static str> = None;

    match ux {
        crate::sandbox::InstallUx::OfferButton { command } if !already_sandbox => {
            // Linux + bwrap missing + not sandbox mode + can_offer_install_button()
            can_install = true;
            install_command = Some(command.user_command.clone());
        }
        crate::sandbox::InstallUx::CommandOnly { command } if !already_sandbox => {
            install_command = Some(command.clone());
        }
        crate::sandbox::InstallUx::UnsupportedPackageManager if !already_sandbox => {
            status_message = Some(crate::sandbox::MSG_UNSUPPORTED_PACKAGE_MANAGER);
        }
        crate::sandbox::InstallUx::BlockedBySystemSettings => {
            // ADR test 30: system-settings copy only — no Install / no install hint.
            blocked_by_system = true;
            status_message = Some(crate::sandbox::MSG_BLOCKED_BY_SYSTEM);
        }
        _ => {}
    }

    json!({
        "mode": live_mode.as_str(),
        "platform": platform,
        "windowsNote": if cfg!(windows) {
            Some("Safe mode for commands is not yet available on Windows.")
        } else {
            None::<&str>
        },
        "canInstall": can_install,
        "installButton": crate::sandbox::MSG_INSTALL_BUTTON,
        "installingHint": crate::sandbox::MSG_INSTALLING,
        "installCommand": install_command,
        "switchToSafeLabel": "Switch to safe mode",
        "blockedBySystem": blocked_by_system,
        "statusMessage": status_message,
    })
}

async fn persist_sandbox_mode(
    path: &Path,
    mode: crate::sandbox::SandboxMode,
) -> Result<(), PortalError> {
    let mut doc = load_doc(path).await?;
    doc_table(&mut doc, "sandbox").insert(
        "mode".to_string(),
        toml::Value::String(mode.as_str().to_string()),
    );
    let new_content = toml::to_string_pretty(&doc)
        .map_err(|e| PortalError::internal(format!("Config serialize error: {e}")))?;
    write_config_with_backup(path, &new_content).await?;
    Ok(())
}

/// POST /api/settings/sandbox/switch-safe — re-probe, then write `sandbox` on success.
pub async fn switch_to_safe_mode(
    State(state): State<PortalState>,
) -> Result<Json<Value>, PortalError> {
    let workspace = state.agent.config().sandbox.allowed_directory.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::sandbox::probe_sandbox(Some(workspace.as_path()))
    })
    .await
    .map_err(PortalError::internal)?;

    match outcome {
        crate::sandbox::ProbeOutcome::Success => {
            persist_sandbox_mode(
                state.config_path.as_path(),
                crate::sandbox::SandboxMode::Sandbox,
            )
            .await?;
            *state
                .sandbox_mode
                .write()
                .unwrap_or_else(|e| e.into_inner()) = crate::sandbox::SandboxMode::Sandbox;
            Ok(Json(json!({
                "ok": true,
                "mode": "sandbox",
                "message": crate::sandbox::MSG_SAFE_MODE_ON,
            })))
        }
        crate::sandbox::ProbeOutcome::Failed(
            crate::sandbox::ProbeFailure::BlockedBySystemSettings,
        ) => Ok(Json(json!({
            "ok": false,
            "mode": "trusted",
            "message": crate::sandbox::MSG_BLOCKED_BY_SYSTEM,
        }))),
        crate::sandbox::ProbeOutcome::Failed(crate::sandbox::ProbeFailure::WindowsUnavailable) => {
            Ok(Json(json!({
                "ok": false,
                "mode": "trusted",
                "message": crate::sandbox::MSG_WINDOWS_SANDBOX,
            })))
        }
        crate::sandbox::ProbeOutcome::Failed(crate::sandbox::ProbeFailure::BwrapMissing) => {
            match crate::sandbox::detect_install_command() {
                Some(cmd) => Ok(Json(json!({
                    "ok": false,
                    "mode": "trusted",
                    "message": crate::sandbox::format_install_failed(&cmd.user_command),
                    "installCommand": cmd.user_command,
                    "canInstall": crate::sandbox::can_offer_install_button(),
                }))),
                None => Ok(Json(json!({
                    "ok": false,
                    "mode": "trusted",
                    "message": crate::sandbox::MSG_UNSUPPORTED_PACKAGE_MANAGER,
                    "canInstall": false,
                }))),
            }
        }
        crate::sandbox::ProbeOutcome::Failed(crate::sandbox::ProbeFailure::OtherFailure(msg)) => {
            Ok(Json(json!({
                "ok": false,
                "mode": "trusted",
                "message": crate::sandbox::MSG_SANDBOX_FAILED,
                "detail": msg,
            })))
        }
    }
}

/// POST /api/settings/sandbox/install — Linux pkexec install + re-probe (ADR-0025 §3a).
pub async fn install_sandbox_tool(
    State(state): State<PortalState>,
) -> Result<Json<Value>, PortalError> {
    if cfg!(not(target_os = "linux")) {
        return Err(PortalError::bad_request(
            "install_not_applicable",
            "Install helper is only available on Linux",
        ));
    }
    let Some(command) = crate::sandbox::detect_install_command() else {
        return Err(PortalError::bad_request(
            "unsupported_package_manager",
            crate::sandbox::MSG_UNSUPPORTED_PACKAGE_MANAGER,
        ));
    };
    if !crate::sandbox::can_offer_install_button() {
        return Err(PortalError::bad_request(
            "install_unavailable",
            command.user_command.clone(),
        ));
    }
    let workspace = state.agent.config().sandbox.allowed_directory.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::sandbox::install_bwrap(
            &crate::sandbox::install::RealPkexec,
            &command,
            Some(workspace.as_path()),
            None,
        )
    })
    .await
    .map_err(PortalError::internal)?;

    persist_sandbox_mode(state.config_path.as_path(), outcome.mode).await?;
    *state
        .sandbox_mode
        .write()
        .unwrap_or_else(|e| e.into_inner()) = outcome.mode;

    Ok(Json(json!({
        "ok": outcome.mode == crate::sandbox::SandboxMode::Sandbox,
        "mode": outcome.mode.as_str(),
        "message": outcome.message,
        "includesInstallCommand": outcome.includes_install_command,
        "installingHint": crate::sandbox::MSG_INSTALLING,
    })))
}

/// POST /api/settings/sandbox/trusted — switch back to trusted mode.
pub async fn switch_to_trusted_mode(
    State(state): State<PortalState>,
) -> Result<Json<Value>, PortalError> {
    persist_sandbox_mode(
        state.config_path.as_path(),
        crate::sandbox::SandboxMode::Trusted,
    )
    .await?;
    *state
        .sandbox_mode
        .write()
        .unwrap_or_else(|e| e.into_inner()) = crate::sandbox::SandboxMode::Trusted;
    Ok(Json(json!({
        "ok": true,
        "mode": "trusted",
        "message": "Trusted mode is on. Commands are not OS-confined.",
    })))
}

/// Map portal Settings choice → Telegram `/selfupgrade` arg (ADR-0030).
///
/// Only `auto` | `main` | `release`. Free-text branches are not accepted.
pub fn portal_selfupgrade_choice_to_arg(choice: &str) -> Option<&'static str> {
    match choice.trim() {
        "auto" => Some(""),
        "main" => Some("main"),
        "release" => Some("release"),
        _ => None,
    }
}

/// POST body for `/api/settings/self-upgrade` (camelCase).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfUpgradeBody {
    pub choice: String,
}

/// POST /api/settings/self-upgrade — sync upgrade then restart (ADR-0030).
///
/// Waits on [`crate::learning::self_upgrade`] (can take many minutes for a
/// cargo build or release download). There is no axum/tower request-timeout
/// middleware on the portal; the handler blocks until upgrade finishes, then
/// returns 200 and schedules [`crate::learning::restart_bot`] after a short
/// delay so the response can flush (Telegram parity).
pub async fn self_upgrade(Json(body): Json<SelfUpgradeBody>) -> Result<Json<Value>, PortalError> {
    let Some(arg) = portal_selfupgrade_choice_to_arg(&body.choice) else {
        return Err(PortalError::bad_request(
            "invalid_selfupgrade_choice",
            "Only Auto, Main, or Release are allowed.",
        ));
    };

    let (mode, branch) = match crate::learning::parse_telegram_selfupgrade_arg(arg) {
        crate::learning::TelegramSelfUpgradeParse::RejectUnknown => {
            return Err(PortalError::bad_request(
                "invalid_selfupgrade_choice",
                "Only Auto, Main, or Release are allowed.",
            ));
        }
        crate::learning::TelegramSelfUpgradeParse::Ok { mode, branch } => (mode, branch),
    };

    let log = crate::learning::self_upgrade(branch, mode, None)
        .await
        .map_err(|e| PortalError::bad_request("self_upgrade_failed", format!("{e:#}")))?;

    // Mirror Telegram: deliver success, then restart after a short delay.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Err(e) = crate::learning::restart_bot() {
            tracing::error!("Portal self-upgrade: restart_bot failed: {e:#}");
        }
    });

    Ok(Json(json!({
        "ok": true,
        "mode": mode,
        "log": log,
    })))
}

#[cfg(test)]
mod sandbox_settings_tests {
    use super::*;

    #[tokio::test]
    // Std lock on purpose: it is shared with sync tests; held for the whole
    // test so no concurrent test changes RUSTFOX_BWRAP_PATH mid-probe.
    #[allow(clippy::await_holding_lock)]
    async fn sandbox_status_json_has_mode() {
        let _bwrap = crate::sandbox::test_env::bwrap_env_lock();
        let ws = std::env::temp_dir().join("rustfox-portal-sandbox-status-test");
        let _ = std::fs::create_dir_all(&ws);
        let v = sandbox_status_json(crate::sandbox::SandboxMode::Trusted, ws).await;
        assert_eq!(v["mode"], "trusted");
        assert!(v.get("switchToSafeLabel").is_some());
        assert!(v.get("blockedBySystem").is_some());
        // When already in sandbox mode, never offer install.
        let ws2 = std::env::temp_dir().join("rustfox-portal-sandbox-status-test2");
        let _ = std::fs::create_dir_all(&ws2);
        let v2 = sandbox_status_json(crate::sandbox::SandboxMode::Sandbox, ws2).await;
        assert_eq!(v2["canInstall"], false);
        assert!(
            v2["installCommand"].is_null(),
            "sandbox mode must not expose installCommand"
        );
    }

    /// ADR-0025 test 30: blocked by AppArmor / userns — system-settings copy
    /// only, no Install button and no install command.
    #[test]
    fn sandbox_status_json_blocked_by_system() {
        let v = sandbox_status_json_from_ux(
            crate::sandbox::SandboxMode::Trusted,
            &crate::sandbox::InstallUx::BlockedBySystemSettings,
        );
        assert_eq!(v["mode"], "trusted");
        assert_eq!(v["blockedBySystem"], true);
        assert_eq!(v["canInstall"], false);
        assert!(v["installCommand"].is_null());
        assert_eq!(v["statusMessage"], crate::sandbox::MSG_BLOCKED_BY_SYSTEM);
    }

    /// ADR-0025 test 28: unsupported package manager — canonical copy, no
    /// button and no invented install command.
    #[test]
    fn sandbox_status_json_unsupported_package_manager() {
        let v = sandbox_status_json_from_ux(
            crate::sandbox::SandboxMode::Trusted,
            &crate::sandbox::InstallUx::UnsupportedPackageManager,
        );
        assert_eq!(v["blockedBySystem"], false);
        assert_eq!(v["canInstall"], false);
        assert!(v["installCommand"].is_null());
        assert_eq!(
            v["statusMessage"],
            crate::sandbox::MSG_UNSUPPORTED_PACKAGE_MANAGER
        );
        // Already in sandbox mode: no install advice at all.
        let v2 = sandbox_status_json_from_ux(
            crate::sandbox::SandboxMode::Sandbox,
            &crate::sandbox::InstallUx::UnsupportedPackageManager,
        );
        assert!(v2["statusMessage"].is_null());
    }
}

#[cfg(test)]
mod self_upgrade_settings_tests {
    use super::*;
    use crate::learning::{parse_telegram_selfupgrade_arg, TelegramSelfUpgradeParse};

    #[test]
    fn choice_auto_maps_to_bare_arg_and_auto_mode() {
        let arg = portal_selfupgrade_choice_to_arg("auto").expect("auto");
        assert_eq!(arg, "");
        assert_eq!(
            parse_telegram_selfupgrade_arg(arg),
            TelegramSelfUpgradeParse::Ok {
                mode: "auto",
                branch: "main",
            }
        );
    }

    #[test]
    fn choice_main_maps_to_source() {
        let arg = portal_selfupgrade_choice_to_arg("main").expect("main");
        assert_eq!(arg, "main");
        assert_eq!(
            parse_telegram_selfupgrade_arg(arg),
            TelegramSelfUpgradeParse::Ok {
                mode: "source",
                branch: "main",
            }
        );
    }

    #[test]
    fn choice_release_maps_to_release() {
        let arg = portal_selfupgrade_choice_to_arg("release").expect("release");
        assert_eq!(arg, "release");
        assert_eq!(
            parse_telegram_selfupgrade_arg(arg),
            TelegramSelfUpgradeParse::Ok {
                mode: "release",
                branch: "main",
            }
        );
    }

    #[test]
    fn unknown_choice_rejected() {
        for bad in [
            "develop",
            "feature/x",
            "",
            "AUTO",
            "Main",
            "Release",
            "source",
        ] {
            assert!(
                portal_selfupgrade_choice_to_arg(bad).is_none(),
                "expected reject for {bad:?}"
            );
        }
    }
}
