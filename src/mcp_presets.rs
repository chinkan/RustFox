//! MCP presets + Custom HTTP helpers (ADR-0041).
//!
//! v1 presets: Exa (ADR-0023 pin / consent / stub / soft 429 / x-api-key) and
//! Jina (Bearer via `secret:NAME` / `env:NAME`, tools pin in URL).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::config::McpServerConfig;
use crate::config_edit::write_config_validated;
use crate::secret_store::{is_secret_ref, store_secret, SecretStore};

/// Process-env credential ref (`env:NAME`), resolved at connect time.
pub const ENV_REF_PREFIX: &str = "env:";

pub const EXA_NAME: &str = "exa";
pub const EXA_URL: &str = "https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa";
pub const EXA_API_KEY_SECRET: &str = "exa.mcp.api_key";
pub const EXA_HEADER_API_KEY: &str = "x-api-key";
pub const EXA_TOOLS: &[&str] = &["web_search_exa", "web_fetch_exa"];

pub const JINA_NAME: &str = "jina";
pub const JINA_URL: &str = "https://mcp.jina.ai/v1?include_tools=search_web,read_url";
pub const JINA_TOOLS: &[&str] = &["search_web", "read_url"];

/// Soft tool result when Exa returns 429 / quota (ADR-0023 §8).
pub const EXA_SOFT_429_RESULT: &str =
    "web search was not available this time (rate limited or quota). \
The user can add an Exa API key in portal Settings for more quota. Answer without search results.";

/// Stub `web_search` result while Exa is off (ADR-0023 §7).
pub const WEB_SEARCH_STUB_RESULT: &str =
    "Web search is not enabled. Tell the user they can turn on Exa web search in portal Settings \
(or via the Enable web search control when offered). Answer without live web results.";

/// Reserved preset names — Custom cannot claim these.
pub fn is_reserved_preset_name(name: &str) -> bool {
    matches!(name.trim(), EXA_NAME | JINA_NAME)
}

/// `env:NAME` → `Some(NAME)`; else `None`.
pub fn parse_env_ref(value: &str) -> Option<&str> {
    value
        .trim()
        .strip_prefix(ENV_REF_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn is_env_ref(value: &str) -> bool {
    parse_env_ref(value).is_some()
}

/// True when the config value is already a `secret:` or `env:` ref (no scrub).
pub fn is_credential_ref(value: &str) -> bool {
    is_secret_ref(value) || is_env_ref(value)
}

/// Header names that always carry credentials when set.
pub fn is_credential_header_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    matches!(
        n.as_str(),
        "authorization" | "x-api-key" | "api-key" | "x-auth-token" | "proxy-authorization"
    )
}

/// Static catalog for `GET /api/settings/mcp/presets`.
pub fn preset_catalog() -> Value {
    json!([
        {
            "id": EXA_NAME,
            "label": "Exa",
            "kind": "preset",
            "url": EXA_URL,
            "auth": "x-api-key",
            "authHeader": EXA_HEADER_API_KEY,
            "tools": EXA_TOOLS,
            "keylessDefault": true,
            "requiresPrivacyConsent": true,
            "description": "Web search and fetch via Exa MCP. Optional API key as x-api-key (never Bearer)."
        },
        {
            "id": JINA_NAME,
            "label": "Jina",
            "kind": "preset",
            "url": JINA_URL,
            "auth": "bearer",
            "tools": JINA_TOOLS,
            "keylessDefault": false,
            "requiresPrivacyConsent": false,
            "description": "Jina MCP search_web + read_url. Bearer via secret:NAME or env:NAME."
        }
    ])
}

/// Classify a saved row for the Settings projection.
pub fn server_kind(cfg: &McpServerConfig) -> &'static str {
    if cfg.command.is_some() && cfg.url.is_none() {
        return "stdio";
    }
    if is_reserved_preset_name(&cfg.name) {
        return "preset";
    }
    if cfg.url.is_some() {
        return "custom";
    }
    "stdio"
}

/// Hand-written / divergent preset row: portal must not overwrite the pin.
pub fn is_config_owned(cfg: &McpServerConfig) -> bool {
    match cfg.name.as_str() {
        EXA_NAME => {
            cfg.command.is_some() || cfg.url.as_deref().is_some_and(|u| u.trim() != EXA_URL)
        }
        JINA_NAME => {
            cfg.command.is_some() || cfg.url.as_deref().is_some_and(|u| u.trim() != JINA_URL)
        }
        _ => true,
    }
}

/// Default Exa row (keyless). Optional key goes in headers as `secret:exa.mcp.api_key`.
pub fn exa_preset_config(enabled: bool, privacy_accepted_at: Option<i64>) -> McpServerConfig {
    McpServerConfig {
        name: EXA_NAME.to_string(),
        enabled,
        command: None,
        args: Vec::new(),
        env: HashMap::new(),
        url: Some(EXA_URL.to_string()),
        auth_token: None,
        headers: HashMap::new(),
        privacy_accepted_at,
        refresh_token: None,
        token_expires_at: None,
        token_endpoint: None,
        oauth_client_id: None,
        oauth_client_secret: None,
    }
}

/// Default Jina row. Bearer must be a ref (`secret:` / `env:`).
pub fn jina_preset_config(enabled: bool, auth_token: Option<String>) -> McpServerConfig {
    McpServerConfig {
        name: JINA_NAME.to_string(),
        enabled,
        command: None,
        args: Vec::new(),
        env: HashMap::new(),
        url: Some(JINA_URL.to_string()),
        auth_token,
        headers: HashMap::new(),
        privacy_accepted_at: None,
        refresh_token: None,
        token_expires_at: None,
        token_endpoint: None,
        oauth_client_id: None,
        oauth_client_secret: None,
    }
}

/// Soft-check helper: Exa URL is exactly the tools pin.
pub fn exa_url_is_pinned(url: &str) -> bool {
    url.trim() == EXA_URL
}

pub fn jina_url_is_pinned(url: &str) -> bool {
    url.trim() == JINA_URL
}

/// Detect Exa 429 / quota errors in MCP tool failure text.
pub fn is_exa_quota_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("quota")
        || lower.contains("too many requests")
}

/// Whether a URL looks like it embeds a credential (must not be logged).
pub fn url_may_contain_credential(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("apikey=")
        || lower.contains("api_key=")
        || lower.contains("exaapikey=")
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("bearer=")
}

/// Canonical SecretStore name for a scrubbed MCP bearer.
pub fn mcp_auth_secret_name(server: &str) -> String {
    format!("mcp.{}.auth_token", server.trim())
}

/// Canonical SecretStore name for a scrubbed MCP header value.
pub fn mcp_header_secret_name(server: &str, header: &str) -> String {
    let safe: String = header
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("mcp.{}.header.{safe}", server.trim())
}

fn is_plaintext_credential_value(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty() && !is_credential_ref(v)
}

/// Migrate plaintext `auth_token` / credential headers → SecretStore + scrub
/// `config.toml` to `secret:NAME` (ADR-0041 §6). Returns number of fields sealed.
pub fn migrate_plaintext_mcp_credentials(
    config_path: &Path,
    store: &dyn SecretStore,
) -> Result<usize> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let mut doc: ValueToml = parse_toml_doc(&content)?;
    let Some(servers) = doc.get_mut("mcp_servers").and_then(|v| v.as_array_mut()) else {
        return Ok(0);
    };

    let mut sealed = 0usize;
    for server in servers.iter_mut() {
        let Some(table) = server.as_table_mut() else {
            continue;
        };
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }

        if let Some(toml::Value::String(token)) = table.get("auth_token").cloned() {
            if is_plaintext_credential_value(&token) {
                let secret_name = mcp_auth_secret_name(&name);
                let reference = store_secret(store, &secret_name, &token)?;
                table.insert("auth_token".into(), toml::Value::String(reference));
                sealed += 1;
            }
        }

        // headers table
        if let Some(toml::Value::Table(headers)) = table.get_mut("headers") {
            let keys: Vec<String> = headers.keys().cloned().collect();
            for key in keys {
                let Some(toml::Value::String(val)) = headers.get(&key).cloned() else {
                    continue;
                };
                if is_credential_header_name(&key) && is_plaintext_credential_value(&val) {
                    let secret_name = mcp_header_secret_name(&name, &key);
                    let reference = store_secret(store, &secret_name, &val)?;
                    headers.insert(key, toml::Value::String(reference));
                    sealed += 1;
                }
            }
        }
    }

    if sealed == 0 {
        return Ok(0);
    }

    let new_content = toml::to_string_pretty(&doc).context("serialize scrubbed mcp config")?;
    write_config_validated(config_path, &new_content)?;
    Ok(sealed)
}

type ValueToml = toml::Value;

fn parse_toml_doc(content: &str) -> Result<toml::Value> {
    toml::from_str(content).context("parse config.toml for MCP credential migrate")
}

/// Build OpenAI tool definition for the Exa-off `web_search` stub.
pub fn web_search_stub_definition() -> crate::llm::ToolDefinition {
    use crate::llm::{FunctionDefinition, ToolDefinition};
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "web_search".to_string(),
            description: "Search the public web. Currently a stub until Exa web search is enabled \
in portal Settings."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" }
                },
                "required": ["query"]
            }),
        },
    }
}

/// Validate Custom name (non-empty, not reserved, safe identifier).
pub fn validate_custom_name(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("MCP server name is empty");
    }
    if is_reserved_preset_name(name) {
        bail!("name `{name}` is reserved for a preset; use enable on that preset instead");
    }
    if name == "google" {
        bail!("name `google` is reserved for the Google connector");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        bail!("MCP server name contains invalid characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::FakeSecretStore;

    #[test]
    fn env_ref_parse() {
        assert_eq!(parse_env_ref("env:JINA_API_KEY"), Some("JINA_API_KEY"));
        assert_eq!(parse_env_ref(" secret:x "), None);
        assert!(is_credential_ref("env:FOO"));
        assert!(is_credential_ref("secret:FOO"));
        assert!(!is_credential_ref("plaintext"));
    }

    #[test]
    fn exa_pin_and_quota() {
        assert!(exa_url_is_pinned(EXA_URL));
        assert!(!exa_url_is_pinned("https://mcp.exa.ai/mcp"));
        assert!(is_exa_quota_error("HTTP 429 Too Many Requests"));
        assert!(is_exa_quota_error("quota exceeded"));
        assert!(!is_exa_quota_error("connection reset"));
    }

    #[test]
    fn url_credential_guard() {
        assert!(url_may_contain_credential(
            "https://mcp.exa.ai/mcp?exaApiKey=secret"
        ));
        assert!(!url_may_contain_credential(EXA_URL));
    }

    #[test]
    fn migrate_scrubs_auth_and_x_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[telegram]
bot_token = "123:ABC"
allowed_user_ids = [1]
[openrouter]
api_key = "sk-test"
[sandbox]
allowed_directory = "/tmp"

[[mcp_servers]]
name = "jina"
url = "https://mcp.jina.ai/v1?include_tools=search_web,read_url"
auth_token = "jina-plain-key"

[[mcp_servers]]
name = "exa"
url = "https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa"
enabled = true
headers = { x-api-key = "exa-plain-key" }
"#,
        )
        .unwrap();
        let store = FakeSecretStore::new();
        let n = migrate_plaintext_mcp_credentials(&path, &store).unwrap();
        assert_eq!(n, 2);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("jina-plain-key"));
        assert!(!after.contains("exa-plain-key"));
        assert!(after.contains("secret:mcp.jina.auth_token"));
        assert!(after.contains("secret:mcp.exa.header.x-api-key"));
        assert_eq!(
            store.get("mcp.jina.auth_token").unwrap().unwrap().expose(),
            "jina-plain-key"
        );
    }

    #[test]
    fn config_owned_when_url_diverges() {
        let mut cfg = exa_preset_config(true, Some(1));
        assert!(!is_config_owned(&cfg));
        cfg.url = Some("https://mcp.exa.ai/mcp".into());
        assert!(is_config_owned(&cfg));
    }

    #[test]
    fn validate_rejects_reserved() {
        assert!(validate_custom_name("exa").is_err());
        assert!(validate_custom_name("my-server").is_ok());
    }
}
