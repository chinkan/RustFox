//! Portal MCP Settings API (ADR-0041).
//!
//! `/api/settings/mcp/*` — list presets, upsert/delete servers, enable/disable
//! with per-server hot connect/disconnect. Responses never echo secrets.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::error::PortalError;
use super::PortalState;
use crate::config::McpServerConfig;
use crate::config_edit::write_config_validated;
use crate::mcp_presets::{
    self, is_config_owned, is_credential_ref, is_reserved_preset_name, jina_preset_config,
    mcp_auth_secret_name, mcp_header_secret_name, preset_catalog, server_kind,
    validate_custom_name, EXA_API_KEY_SECRET, EXA_HEADER_API_KEY, EXA_NAME, EXA_URL, JINA_NAME,
    JINA_URL,
};
use crate::secret_store::store_secret;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertMcpBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Bearer plaintext (sealed), `secret:NAME`, or `env:NAME`.
    #[serde(default)]
    pub bearer: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub privacy_accepted: Option<bool>,
    #[serde(default)]
    pub clear_api_key: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnableBody {
    #[serde(default)]
    pub privacy_accepted: Option<bool>,
}

/// Project one row for Settings (no secret values).
pub fn project_server(cfg: &McpServerConfig, connected: bool) -> Value {
    let kind = server_kind(cfg);
    let auth_configured = cfg
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
        || cfg.headers.values().any(|v| !v.trim().is_empty());
    let mut header_names: Vec<String> = cfg.headers.keys().cloned().collect();
    header_names.sort();
    let preset = (kind == "preset").then_some(cfg.name.clone());
    json!({
        "name": cfg.name,
        "kind": kind,
        "preset": preset,
        "enabled": cfg.enabled,
        "url": cfg.url,
        "authConfigured": auth_configured,
        "headerNames": header_names,
        "connected": connected,
        "privacyAccepted": cfg.privacy_accepted_at.is_some(),
        "privacyAcceptedAt": cfg.privacy_accepted_at,
        "configOwned": is_config_owned(cfg),
    })
}

/// Load MCP servers from the process-owned portal config path only.
/// Takes `PortalState` (not a bare `&Path`) so request Path extractors cannot flow in.
fn load_servers(state: &PortalState) -> Result<Vec<McpServerConfig>, PortalError> {
    // Reuse Config::load — sink stays in config.rs (already on main), not a new mcp.rs sink.
    let cfg =
        crate::config::Config::load(state.config_path.as_path()).map_err(PortalError::internal)?;
    Ok(cfg.mcp_servers)
}

fn write_doc(state: &PortalState, doc: &toml::Value) -> Result<(), PortalError> {
    let text = toml::to_string_pretty(doc).map_err(PortalError::internal)?;
    write_config_validated(state.config_path.as_path(), &text).map_err(PortalError::internal)?;
    Ok(())
}

fn servers_array_mut(doc: &mut toml::Value) -> Result<&mut Vec<toml::Value>, PortalError> {
    let root = doc
        .as_table_mut()
        .ok_or_else(|| PortalError::internal("config root not a table"))?;
    let entry = root
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    entry
        .as_array_mut()
        .ok_or_else(|| PortalError::internal("mcp_servers is not an array"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn table_from_config(cfg: &McpServerConfig) -> toml::value::Table {
    let mut t = toml::map::Map::new();
    t.insert("name".into(), toml::Value::String(cfg.name.clone()));
    t.insert("enabled".into(), toml::Value::Boolean(cfg.enabled));
    if let Some(url) = &cfg.url {
        t.insert("url".into(), toml::Value::String(url.clone()));
    }
    if let Some(token) = &cfg.auth_token {
        t.insert("auth_token".into(), toml::Value::String(token.clone()));
    }
    if let Some(ts) = cfg.privacy_accepted_at {
        t.insert("privacy_accepted_at".into(), toml::Value::Integer(ts));
    }
    if !cfg.headers.is_empty() {
        let mut h = toml::map::Map::new();
        for (k, v) in &cfg.headers {
            h.insert(k.clone(), toml::Value::String(v.clone()));
        }
        t.insert("headers".into(), toml::Value::Table(h));
    }
    t
}

fn parse_row(table: &toml::value::Table) -> Result<McpServerConfig, PortalError> {
    toml::Value::Table(table.clone())
        .try_into()
        .map_err(|e| PortalError::internal(format!("mcp row: {e}")))
}

fn row_index(servers: &[toml::Value], name: &str) -> Option<usize> {
    servers
        .iter()
        .position(|s| s.get("name").and_then(|v| v.as_str()) == Some(name))
}

fn is_config_owned_table(table: &toml::value::Table, name: &str) -> bool {
    let url = table.get("url").and_then(|v| v.as_str());
    let has_command = table.get("command").and_then(|v| v.as_str()).is_some();
    match name {
        EXA_NAME => has_command || url.is_some_and(|u| u.trim() != EXA_URL),
        JINA_NAME => has_command || url.is_some_and(|u| u.trim() != JINA_URL),
        _ => true,
    }
}

fn upsert_table(servers: &mut Vec<toml::Value>, table: toml::value::Table) {
    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let replacement = toml::Value::Table(table);
    if let Some(i) = row_index(servers, &name) {
        servers[i] = replacement;
    } else {
        servers.push(replacement);
    }
}

fn seal_bearer(
    store: &dyn crate::secret_store::SecretStore,
    server: &str,
    bearer: &str,
) -> Result<Option<String>, PortalError> {
    let bearer = bearer.trim();
    if bearer.is_empty() {
        return Ok(None);
    }
    if is_credential_ref(bearer) {
        return Ok(Some(bearer.to_string()));
    }
    let name = mcp_auth_secret_name(server);
    Ok(Some(
        store_secret(store, &name, bearer).map_err(PortalError::internal)?,
    ))
}

fn seal_header_map(
    store: &dyn crate::secret_store::SecretStore,
    server: &str,
    headers: &HashMap<String, String>,
) -> Result<HashMap<String, String>, PortalError> {
    let mut out = HashMap::new();
    for (k, v) in headers {
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() || v.is_empty() {
            continue;
        }
        if is_credential_ref(v) {
            out.insert(k.to_string(), v.to_string());
            continue;
        }
        if mcp_presets::is_credential_header_name(k) || v.len() >= 8 {
            let secret_name = mcp_header_secret_name(server, k);
            let reference = store_secret(store, &secret_name, v).map_err(PortalError::internal)?;
            out.insert(k.to_string(), reference);
        } else {
            out.insert(k.to_string(), v.to_string());
        }
    }
    Ok(out)
}

async fn apply_hot_reload(state: &PortalState, cfg: &McpServerConfig) -> Result<bool, PortalError> {
    if cfg.enabled {
        if cfg.name == EXA_NAME && cfg.privacy_accepted_at.is_none() {
            let _ = state.agent.mcp_disconnect(cfg.name.clone()).await;
            return Ok(false);
        }
        // Reconnect: drop existing first so header/token changes apply.
        let _ = state.agent.mcp_disconnect(cfg.name.clone()).await;
        state
            .agent
            .mcp_connect(cfg.clone())
            .await
            .map_err(|e| PortalError::bad_request("mcp_connect_failed", e.to_string()))?;
        Ok(true)
    } else {
        state.agent.mcp_disconnect(cfg.name.clone()).await;
        Ok(false)
    }
}

/// GET /api/settings/mcp/presets
pub async fn list_presets() -> Json<Value> {
    Json(json!({ "presets": preset_catalog() }))
}

/// Build masked.mcpServers from disk + live connections.
pub fn list_projected(state: &PortalState) -> Result<Vec<Value>, PortalError> {
    let servers = load_servers(state)?;
    let connected = state.agent.mcp_connected_names();
    Ok(servers
        .iter()
        .map(|s| project_server(s, connected.iter().any(|c| c == &s.name)))
        .collect())
}

/// PUT /api/settings/mcp/{name}
pub async fn upsert_server(
    State(state): State<PortalState>,
    AxPath(path_name): AxPath<String>,
    Json(body): Json<UpsertMcpBody>,
) -> Result<Json<Value>, PortalError> {
    let name = body
        .name
        .as_deref()
        .unwrap_or(&path_name)
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(PortalError::bad_request("invalid_name", "name is empty"));
    }

    let existing_servers = load_servers(&state).unwrap_or_default();
    let prev = existing_servers.iter().find(|s| s.name == name).cloned();
    let store = state.secret_store.as_ref();

    let cfg = if is_reserved_preset_name(&name) {
        if let Some(ref p) = prev {
            if is_config_owned(p) {
                return Err(PortalError::bad_request(
                    "config_owned",
                    format!("{name} is configured in config.toml and is not portal-managed"),
                ));
            }
        }

        if name == EXA_NAME {
            if body.bearer.as_deref().is_some_and(|b| !b.trim().is_empty()) {
                return Err(PortalError::bad_request(
                    "exa_no_bearer",
                    "Exa uses x-api-key header, not Bearer",
                ));
            }
            let privacy = prev
                .as_ref()
                .and_then(|p| p.privacy_accepted_at)
                .or_else(|| (body.privacy_accepted == Some(true)).then_some(now_unix()));
            let enabled = body
                .enabled
                .unwrap_or_else(|| prev.as_ref().map(|p| p.enabled).unwrap_or(true));
            let mut c = mcp_presets::exa_preset_config(enabled, privacy);
            if let Some(p) = &prev {
                c.headers = p.headers.clone();
            }
            if body.clear_api_key == Some(true) {
                c.headers.remove(EXA_HEADER_API_KEY);
                let _ = store.delete(EXA_API_KEY_SECRET);
            } else if let Some(headers) = &body.headers {
                if let Some(key) = headers
                    .get(EXA_HEADER_API_KEY)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                {
                    let reference = if is_credential_ref(key) {
                        key.to_string()
                    } else {
                        store_secret(store, EXA_API_KEY_SECRET, key)
                            .map_err(PortalError::internal)?
                    };
                    c.headers.insert(EXA_HEADER_API_KEY.to_string(), reference);
                }
            }
            if body.privacy_accepted == Some(true) && c.privacy_accepted_at.is_none() {
                c.privacy_accepted_at = Some(now_unix());
            }
            c
        } else {
            // jina
            let enabled = body
                .enabled
                .unwrap_or_else(|| prev.as_ref().map(|p| p.enabled).unwrap_or(true));
            let auth = match body.bearer.as_deref() {
                Some(b) => seal_bearer(store, &name, b)?,
                None => prev.as_ref().and_then(|p| p.auth_token.clone()),
            };
            let mut c = jina_preset_config(enabled, auth);
            c.headers = match &body.headers {
                Some(h) => seal_header_map(store, &name, h)?,
                None => prev.map(|p| p.headers).unwrap_or_default(),
            };
            c
        }
    } else {
        validate_custom_name(&name)
            .map_err(|e| PortalError::bad_request("invalid_name", e.to_string()))?;
        let url = body
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| prev.as_ref().and_then(|p| p.url.as_deref()))
            .ok_or_else(|| PortalError::bad_request("missing_url", "Custom MCP requires url"))?
            .to_string();
        let enabled = body
            .enabled
            .unwrap_or_else(|| prev.as_ref().map(|p| p.enabled).unwrap_or(true));
        let auth_token = match body.bearer.as_deref() {
            Some(b) => seal_bearer(store, &name, b)?,
            None => prev.as_ref().and_then(|p| p.auth_token.clone()),
        };
        let headers = match &body.headers {
            Some(h) => seal_header_map(store, &name, h)?,
            None => prev.map(|p| p.headers).unwrap_or_default(),
        };
        McpServerConfig {
            name: name.clone(),
            enabled,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url),
            auth_token,
            headers,
            privacy_accepted_at: None,
            refresh_token: None,
            token_expires_at: None,
            token_endpoint: None,
            oauth_client_id: None,
            oauth_client_secret: None,
        }
    };

    let mut doc = super::settings::load_doc(state.config_path.as_path()).await?;
    {
        let servers = servers_array_mut(&mut doc)?;
        upsert_table(servers, table_from_config(&cfg));
    }
    write_doc(&state, &doc)?;
    let connected = apply_hot_reload(&state, &cfg).await?;
    Ok(Json(json!({ "server": project_server(&cfg, connected) })))
}

/// DELETE /api/settings/mcp/{name} — Custom only.
pub async fn delete_server(
    State(state): State<PortalState>,
    AxPath(name): AxPath<String>,
) -> Result<Json<Value>, PortalError> {
    let name = name.trim();
    if is_reserved_preset_name(name) || name == "google" {
        return Err(PortalError::bad_request(
            "preset_delete_forbidden",
            "Presets cannot be deleted; disable them instead",
        ));
    }
    let mut doc = super::settings::load_doc(state.config_path.as_path()).await?;
    let servers = servers_array_mut(&mut doc)?;
    let before = servers.len();
    servers.retain(|s| s.get("name").and_then(|v| v.as_str()) != Some(name));
    if servers.len() == before {
        return Err(PortalError::bad_request(
            "not_found",
            format!("no MCP server named `{name}`"),
        ));
    }
    write_doc(&state, &doc)?;
    state.agent.mcp_disconnect(name.to_string()).await;
    Ok(Json(json!({ "deleted": name })))
}

/// POST /api/settings/mcp/{name}/enable
pub async fn enable_server(
    State(state): State<PortalState>,
    AxPath(name): AxPath<String>,
    body: Result<Json<EnableBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, PortalError> {
    let privacy = body.ok().and_then(|b| b.0.privacy_accepted);
    set_enabled(&state, &name, true, privacy).await
}

/// POST /api/settings/mcp/{name}/disable
pub async fn disable_server(
    State(state): State<PortalState>,
    AxPath(name): AxPath<String>,
) -> Result<Json<Value>, PortalError> {
    set_enabled(&state, &name, false, None).await
}

async fn set_enabled(
    state: &PortalState,
    name: &str,
    enabled: bool,
    privacy_accepted: Option<bool>,
) -> Result<Json<Value>, PortalError> {
    let name = name.trim();
    let mut doc = super::settings::load_doc(state.config_path.as_path()).await?;

    {
        let servers = servers_array_mut(&mut doc)?;
        if row_index(servers, name).is_none() {
            if name == EXA_NAME {
                let privacy = (privacy_accepted == Some(true)).then_some(now_unix());
                let cfg = mcp_presets::exa_preset_config(enabled, privacy);
                servers.push(toml::Value::Table(table_from_config(&cfg)));
            } else if name == JINA_NAME {
                let cfg = jina_preset_config(enabled, None);
                servers.push(toml::Value::Table(table_from_config(&cfg)));
            } else {
                return Err(PortalError::bad_request(
                    "not_found",
                    format!("no MCP server named `{name}`"),
                ));
            }
        }

        let idx = row_index(servers, name).unwrap();
        let table = servers[idx]
            .as_table_mut()
            .ok_or_else(|| PortalError::internal("mcp row not a table"))?;

        if name == EXA_NAME && is_config_owned_table(table, name) {
            return Err(PortalError::bad_request(
                "config_owned",
                "Exa is configured in config.toml; portal toggle is disabled",
            ));
        }

        if enabled && name == EXA_NAME {
            let has = table
                .get("privacy_accepted_at")
                .and_then(|v| v.as_integer())
                .is_some();
            if !has {
                if privacy_accepted == Some(true) {
                    table.insert(
                        "privacy_accepted_at".into(),
                        toml::Value::Integer(now_unix()),
                    );
                } else {
                    return Err(PortalError::bad_request(
                        "privacy_required",
                        "Exa enable requires privacyAccepted: true on first enable",
                    ));
                }
            }
        }

        table.insert("enabled".into(), toml::Value::Boolean(enabled));
        if name == EXA_NAME && !is_config_owned_table(table, name) {
            table.insert("url".into(), toml::Value::String(EXA_URL.into()));
        }
        if name == JINA_NAME && !is_config_owned_table(table, name) {
            table.insert("url".into(), toml::Value::String(JINA_URL.into()));
        }
    }

    // Re-borrow for parse after mutations
    let cfg = {
        let servers = servers_array_mut(&mut doc)?;
        let idx = row_index(servers, name).unwrap();
        let table = servers[idx]
            .as_table()
            .ok_or_else(|| PortalError::internal("mcp row not a table"))?;
        parse_row(table)?
    };
    write_doc(state, &doc)?;
    let connected = apply_hot_reload(state, &cfg).await?;
    Ok(Json(json!({ "server": project_server(&cfg, connected) })))
}
