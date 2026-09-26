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
            "mcpServers": cfg.mcp_servers.iter().map(|m| json!({ "name": m.name })).collect::<Vec<_>>(),
        },
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
async fn load_doc(path: &Path) -> Result<toml::Value, PortalError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(PortalError::internal)?;
    content
        .parse::<toml::Value>()
        .map_err(|e| PortalError::internal(format!("Config parse error: {e}")))
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
