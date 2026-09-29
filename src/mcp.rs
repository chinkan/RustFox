use anyhow::{Context, Result};
use rmcp::{
    model::{CallToolRequestParams, ContentBlock, Tool as McpTool},
    service::RunningService,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, ConfigureCommandExt,
        StreamableHttpClientTransport, TokioChildProcess,
    },
    ServiceExt,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

use crate::config::McpServerConfig;
use crate::llm::{FunctionDefinition, ToolDefinition};

// ── OAuth token refresh ────────────────────────────────────────────────────────

/// Response from the token endpoint when refreshing an access token.
#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    /// Authorization servers rotate the refresh token on each use.
    #[serde(default)]
    refresh_token: Option<String>,
    /// Lifetime of the new access token in seconds.
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Returns true when the access token for an HTTP MCP server has expired or
/// will expire within the next 5 minutes.
pub fn token_needs_refresh(config: &McpServerConfig) -> bool {
    if config.refresh_token.is_none() || config.token_endpoint.is_none() {
        return false;
    }
    match config.token_expires_at {
        None => false,
        Some(expires_at) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            // Refresh if expiry is within the next 5 minutes (300 seconds)
            expires_at - now <= 300
        }
    }
}

/// Exchange a refresh token for a new access token.
///
/// Uses HTTP Basic Auth with `oauth_client_id` : `oauth_client_secret` when a
/// client ID is present.  Returns the updated token fields.
pub async fn refresh_oauth_token(
    config: &McpServerConfig,
    http_client: &reqwest::Client,
) -> Result<(String, Option<String>, Option<i64>)> {
    let refresh_token = config
        .refresh_token
        .as_deref()
        .context("No refresh_token in config")?;
    let token_endpoint = config
        .token_endpoint
        .as_deref()
        .context("No token_endpoint in config")?;

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];

    let mut request = http_client.post(token_endpoint).form(&params);

    // Add HTTP Basic Auth when a client_id is available.
    if let Some(client_id) = &config.oauth_client_id {
        request = request.basic_auth(client_id, config.oauth_client_secret.as_deref());
    }

    let resp = request
        .send()
        .await
        .context("Token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Token refresh failed ({status}) for '{}': {body}",
            config.name
        );
    }

    let tok: TokenRefreshResponse = resp
        .json()
        .await
        .context("Failed to parse token refresh response")?;

    let new_expires_at = tok.expires_in.map(|secs| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        now + secs as i64
    });

    Ok((tok.access_token, tok.refresh_token, new_expires_at))
}

/// Rewrite the `auth_token`, `refresh_token`, and `token_expires_at` fields for
/// a named `[[mcp_servers]]` entry inside `config.toml`, preserving all other
/// content.
///
/// Strategy: parse the TOML into a `toml::Value`, update the matching server
/// entry, and serialise back via [`crate::config_edit::write_config_validated`]
/// (validate → `.bak` → atomic write → restore-on-post-write-fail). Comments
/// are lost on round-trip, but functional sections are fully preserved. This is
/// acceptable for a machine-updated file.
///
/// Returns the backup path (`config.toml.bak`). Secrets are never logged.
///
/// # Errors
///
/// Returns an error (without touching the config file or creating `.bak`) when
/// no `[[mcp_servers]]` entry matches `server_name`. The error message includes
/// the server name only — never token values.
pub fn update_config_tokens(
    config_path: &Path,
    server_name: &str,
    auth_token: &str,
    new_refresh_token: Option<&str>,
    new_expires_at: Option<i64>,
) -> Result<std::path::PathBuf> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let mut doc: toml::Value = content
        .parse()
        .with_context(|| format!("Failed to parse TOML from {}", config_path.display()))?;

    let servers = doc
        .get_mut("mcp_servers")
        .and_then(|v| v.as_array_mut())
        .with_context(|| format!("no [[mcp_servers]] entry named `{server_name}`"))?;

    let mut found = false;
    for server in servers.iter_mut() {
        if server.get("name").and_then(|v| v.as_str()) != Some(server_name) {
            continue;
        }
        if let toml::Value::Table(table) = server {
            table.insert(
                "auth_token".to_string(),
                toml::Value::String(auth_token.to_string()),
            );
            if let Some(rt) = new_refresh_token {
                table.insert(
                    "refresh_token".to_string(),
                    toml::Value::String(rt.to_string()),
                );
            }
            if let Some(ea) = new_expires_at {
                table.insert("token_expires_at".to_string(), toml::Value::Integer(ea));
            }
        }
        found = true;
        break;
    }
    if !found {
        anyhow::bail!("no [[mcp_servers]] entry named `{server_name}`");
    }

    let new_content = toml::to_string_pretty(&doc).context("Failed to serialise updated config")?;
    let bak = crate::config_edit::write_config_validated(config_path, &new_content)?;

    debug!("Persisted refreshed token for MCP server '{server_name}'");
    Ok(bak)
}

/// Refresh tokens for every HTTP MCP server that is near expiry, writing the
/// new credentials back to `config_path`.  Returns the number of servers that
/// were refreshed.
pub async fn refresh_expiring_tokens(
    configs: &mut [McpServerConfig],
    config_path: &Path,
    http_client: &reqwest::Client,
) -> usize {
    let mut refreshed = 0usize;
    for cfg in configs.iter_mut() {
        if !token_needs_refresh(cfg) {
            continue;
        }
        info!(
            "Access token for MCP server '{}' is expiring; refreshing...",
            cfg.name
        );
        match refresh_oauth_token(cfg, http_client).await {
            Ok((new_token, new_rt, new_exp)) => {
                // Persist to disk first; only update in-memory state on success to
                // avoid a situation where the runtime uses a token that was never saved.
                match update_config_tokens(
                    config_path,
                    &cfg.name,
                    &new_token,
                    new_rt.as_deref(),
                    new_exp,
                ) {
                    Ok(_bak) => {
                        cfg.auth_token = Some(new_token);
                        if new_rt.is_some() {
                            cfg.refresh_token = new_rt;
                        }
                        cfg.token_expires_at = new_exp;
                        refreshed += 1;
                        info!("Token refreshed successfully for MCP server '{}'", cfg.name);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to persist refreshed token for '{}': {e:#}",
                            cfg.name
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Token refresh failed for MCP server '{}': {e:#}", cfg.name);
            }
        }
    }
    refreshed
}

/// Represents a connected MCP server with its tools
pub struct McpConnection {
    pub name: String,
    pub client: RunningService<rmcp::service::RoleClient, ()>,
    pub tools: Vec<McpTool>,
}

/// Manages multiple MCP server connections
pub struct McpManager {
    connections: HashMap<String, McpConnection>,
    /// Slice 3: resolve `secret:NAME` env refs + redact tool results.
    secret_bridge: Option<std::sync::Arc<crate::secret_store::SecretBridge>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            secret_bridge: None,
        }
    }

    /// Attach SecretBridge before `connect_all` (Slice 3).
    pub fn with_secret_bridge(
        mut self,
        bridge: std::sync::Arc<crate::secret_store::SecretBridge>,
    ) -> Self {
        self.secret_bridge = Some(bridge);
        self
    }

    pub fn set_secret_bridge(&mut self, bridge: std::sync::Arc<crate::secret_store::SecretBridge>) {
        self.secret_bridge = Some(bridge);
    }

    fn redact_tool_text(&self, text: &str) -> String {
        match &self.secret_bridge {
            Some(b) => b.redact(text),
            None => text.to_string(),
        }
    }

    /// Connect to an MCP server — dispatches to HTTP or stdio based on config.
    pub async fn connect(&mut self, config: &McpServerConfig) -> Result<()> {
        if config.url.is_some() {
            self.connect_http(config).await
        } else {
            self.connect_stdio(config).await
        }
    }

    /// Connect to an HTTP-based MCP server using the Streamable HTTP transport.
    async fn connect_http(&mut self, config: &McpServerConfig) -> Result<()> {
        let url = config
            .url
            .as_deref()
            .context("HTTP MCP server config missing 'url'")?;

        info!("Connecting to HTTP MCP server '{}': {}", config.name, url);

        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.to_string());

        // Only set the auth header when a non-empty token is provided.
        // Using unwrap_or_default() would pass an empty string, causing
        // reqwest to send "Authorization: Bearer " (empty token) which
        // remote servers (e.g. Notion) reject with 401 invalid_token.
        match &config.auth_token {
            Some(token) if !token.is_empty() => {
                transport_config = transport_config.auth_header(token.clone());
            }
            None => {
                tracing::debug!(
                    "HTTP MCP server '{}' has no auth_token configured; \
                     requests will be sent without an Authorization header",
                    config.name
                );
            }
            _ => {}
        }

        let transport = StreamableHttpClientTransport::from_config(transport_config);

        // `()` implements rmcp's `ServiceExt` as the default no-op client handler;
        // calling `.serve(transport)` on it returns a `RunningService` connected
        // to the given transport without any application-level request handling.
        let client = ().serve(transport).await.with_context(|| {
            format!("Failed to initialize HTTP MCP connection: {}", config.name)
        })?;

        self.register_client(config, client).await
    }

    /// Connect to a stdio-based MCP server via a child process.
    async fn connect_stdio(&mut self, config: &McpServerConfig) -> Result<()> {
        let command_str = config
            .command
            .as_deref()
            .context("Stdio MCP server config missing 'command'")?;

        info!(
            "Connecting to stdio MCP server '{}': {} {:?}",
            config.name, command_str, config.args
        );

        let args = config.args.clone();
        let env = if let Some(ref bridge) = self.secret_bridge {
            bridge.resolve_env_map(&config.env).map_err(|e| {
                anyhow::anyhow!(
                    "MCP '{}': required secret missing or invalid ({e})",
                    config.name
                )
            })?
        } else {
            config.env.clone()
        };
        let cmd = command_str.to_string();

        let transport = TokioChildProcess::new(Command::new(&cmd).configure(move |c| {
            for arg in &args {
                c.arg(arg);
            }
            for (key, value) in &env {
                c.env(key, value);
            }
        }))
        .with_context(|| format!("Failed to start MCP server process: {}", config.name))?;

        // `()` is rmcp's default no-op client handler; see `connect_http` for details.
        let client = ()
            .serve(transport)
            .await
            .with_context(|| format!("Failed to initialize MCP connection: {}", config.name))?;

        self.register_client(config, client).await
    }

    /// Register a connected client, listing its tools and storing it.
    async fn register_client(
        &mut self,
        config: &McpServerConfig,
        client: RunningService<rmcp::service::RoleClient, ()>,
    ) -> Result<()> {
        let server_info = client.peer_info();
        info!(
            "Connected to MCP server '{}': {:?}",
            config.name, server_info
        );

        let tools = client
            .list_all_tools()
            .await
            .with_context(|| format!("Failed to list tools from MCP server: {}", config.name))?;

        info!(
            "MCP server '{}' provides {} tools",
            config.name,
            tools.len()
        );
        for tool in &tools {
            info!("  - {}: {:?}", tool.name, tool.description);
        }

        self.connections.insert(
            config.name.clone(),
            McpConnection {
                name: config.name.clone(),
                client,
                tools,
            },
        );

        Ok(())
    }

    /// Connect to all configured MCP servers, logging errors but not failing
    pub async fn connect_all(&mut self, configs: &[McpServerConfig]) {
        for config in configs {
            if !config.enabled {
                tracing::info!(
                    "Skipping disabled MCP server '{}' (mcp.{}.enabled = false)",
                    config.name,
                    config.name
                );
                continue;
            }
            if let Err(e) = self.connect(config).await {
                error!("Failed to connect to MCP server '{}': {:#}", config.name, e);
            }
        }
    }

    /// Number of connected MCP servers
    pub fn server_count(&self) -> usize {
        self.connections.len()
    }

    /// Get all MCP tools as OpenRouter-compatible tool definitions
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = Vec::new();

        for connection in self.connections.values() {
            for tool in &connection.tools {
                let parameters = tool.schema_as_json_value();
                definitions.push(ToolDefinition {
                    tool_type: "function".to_string(),
                    function: FunctionDefinition {
                        name: format!("mcp_{}_{}", connection.name, tool.name),
                        description: tool
                            .description
                            .as_deref()
                            .unwrap_or("MCP tool")
                            .to_string(),
                        parameters,
                    },
                });
            }
        }

        definitions
    }

    /// Find which MCP server owns a tool and call it
    pub async fn call_tool(&self, prefixed_name: &str, arguments: &Value) -> Result<String> {
        // Tool names are prefixed with "mcp_{server_name}_{tool_name}"
        let without_mcp = prefixed_name
            .strip_prefix("mcp_")
            .context("MCP tool name must start with 'mcp_'")?;

        // Find the matching connection
        for connection in self.connections.values() {
            let prefix = format!("{}_", connection.name);
            if let Some(tool_name) = without_mcp.strip_prefix(&prefix) {
                // Verify this tool exists on this server
                if connection
                    .tools
                    .iter()
                    .any(|t| t.name.as_ref() == tool_name)
                {
                    info!(
                        "Calling MCP tool '{}' on server '{}'",
                        tool_name, connection.name
                    );

                    let tool_name_owned: std::borrow::Cow<'static, str> =
                        std::borrow::Cow::Owned(tool_name.to_string());
                    let mut call_params = CallToolRequestParams::new(tool_name_owned);
                    if let Some(args) = arguments.as_object().cloned() {
                        call_params = call_params.with_arguments(args);
                    }
                    let result = connection
                        .client
                        .call_tool(call_params)
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to call MCP tool '{}' on server '{}'",
                                tool_name, connection.name
                            )
                        })?;

                    // Extract text content from the result
                    let text_parts: Vec<String> = result
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect();

                    if text_parts.is_empty() {
                        return Ok(self.redact_tool_text(&format!("{:?}", result.content)));
                    }
                    return Ok(self.redact_tool_text(&text_parts.join("\n")));
                }
            }
        }

        anyhow::bail!("MCP tool not found: {}", prefixed_name)
    }

    /// Check if a tool name belongs to an MCP server
    pub fn is_mcp_tool(&self, name: &str) -> bool {
        name.starts_with("mcp_")
    }

    /// Shutdown all MCP connections
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) {
        for (name, connection) in self.connections.drain() {
            info!("Shutting down MCP server: {}", name);
            if let Err(e) = connection.client.cancel().await {
                error!("Error shutting down MCP server '{}': {}", name, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpServerConfig;
    use std::collections::HashMap;

    fn base_config() -> McpServerConfig {
        McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some("https://example.com/mcp".to_string()),
            auth_token: Some("tok".to_string()),
            refresh_token: Some("rt".to_string()),
            token_expires_at: None,
            token_endpoint: Some("https://example.com/token".to_string()),
            oauth_client_id: None,
            oauth_client_secret: None,
        }
    }

    #[test]
    fn test_no_refresh_without_refresh_token() {
        let mut cfg = base_config();
        cfg.refresh_token = None;
        assert!(!token_needs_refresh(&cfg));
    }

    #[test]
    fn test_no_refresh_without_token_endpoint() {
        let mut cfg = base_config();
        cfg.token_endpoint = None;
        assert!(!token_needs_refresh(&cfg));
    }

    #[test]
    fn test_no_refresh_when_no_expiry() {
        let cfg = base_config(); // token_expires_at == None
        assert!(!token_needs_refresh(&cfg));
    }

    #[test]
    fn test_needs_refresh_when_expired() {
        let mut cfg = base_config();
        // Set expiry 1 hour in the past
        let past = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 3600;
        cfg.token_expires_at = Some(past);
        assert!(token_needs_refresh(&cfg));
    }

    #[test]
    fn test_needs_refresh_when_expiring_within_5_min() {
        let mut cfg = base_config();
        // Set expiry 60 seconds from now (within 5-minute window)
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 60;
        cfg.token_expires_at = Some(soon);
        assert!(token_needs_refresh(&cfg));
    }

    #[test]
    fn test_no_refresh_when_expiry_far_future() {
        let mut cfg = base_config();
        // Set expiry 1 hour from now
        let far = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        cfg.token_expires_at = Some(far);
        assert!(!token_needs_refresh(&cfg));
    }

    fn oauth_config_toml() -> String {
        r#"
[telegram]
bot_token = "123456:ABC-SECRETTOKEN"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-or-v1-SECRETKEY000"
model = "moonshotai/kimi-k2.6"

[sandbox]
allowed_directory = "/tmp/sandbox"

[learning]
skill_extraction_enabled = true

[[mcp_servers]]
name = "exa"
url = "https://mcp.example.com/exa"
auth_token = "OLD_AUTH_TOKEN_AAA"
refresh_token = "OLD_REFRESH_TOKEN_BBB"
token_expires_at = 1000
token_endpoint = "https://auth.example.com/token"
oauth_client_id = "client-id"

[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]
"#
        .to_string()
    }

    fn write_cfg(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn update_config_tokens_writes_bak_and_updates_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains("OLD_AUTH_TOKEN_AAA"));

        let bak = update_config_tokens(
            &path,
            "exa",
            "NEW_AUTH_TOKEN_CCC",
            Some("NEW_REFRESH_TOKEN_DDD"),
            Some(9999),
        )
        .unwrap();

        assert!(bak.exists(), "shared write path must create .bak");
        let bak_content = std::fs::read_to_string(&bak).unwrap();
        assert!(
            bak_content.contains("OLD_AUTH_TOKEN_AAA"),
            "bak must be pre-edit snapshot"
        );
        assert!(
            bak_content.contains("OLD_REFRESH_TOKEN_BBB"),
            "bak must retain old refresh token"
        );

        let new_content = std::fs::read_to_string(&path).unwrap();
        assert!(
            new_content.contains("NEW_AUTH_TOKEN_CCC"),
            "auth_token not updated: {new_content}"
        );
        assert!(
            new_content.contains("NEW_REFRESH_TOKEN_DDD"),
            "refresh_token not updated: {new_content}"
        );
        assert!(
            new_content.contains("9999"),
            "token_expires_at not updated: {new_content}"
        );
        assert!(
            !new_content.contains("OLD_AUTH_TOKEN_AAA"),
            "old auth_token should be replaced"
        );
        crate::config_edit::validate_config_str(&new_content).unwrap();
    }

    #[test]
    fn update_config_tokens_preserves_unrelated_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());

        update_config_tokens(
            &path,
            "exa",
            "NEW_AUTH_TOKEN_CCC",
            Some("NEW_REFRESH_TOKEN_DDD"),
            Some(9999),
        )
        .unwrap();

        let new_content = std::fs::read_to_string(&path).unwrap();
        // Unrelated sections / sibling MCP server must survive
        assert!(
            new_content.contains("skill_extraction_enabled"),
            "learning section wiped: {new_content}"
        );
        assert!(
            new_content.contains("mcp-server-git"),
            "git mcp_servers entry wiped: {new_content}"
        );
        assert!(
            new_content.contains(r#"name = "git""#),
            "git server name missing: {new_content}"
        );
        assert!(
            new_content.contains("123456:ABC-SECRETTOKEN"),
            "telegram section wiped"
        );
        assert!(
            new_content.contains("sk-or-v1-SECRETKEY000"),
            "openrouter section wiped"
        );
        // Sibling fields on the same server preserved
        assert!(
            new_content.contains("https://mcp.example.com/exa"),
            "exa url wiped"
        );
        assert!(new_content.contains("client-id"), "oauth_client_id wiped");
    }

    #[test]
    fn update_config_tokens_restore_from_bak() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());

        let bak = update_config_tokens(
            &path,
            "exa",
            "NEW_AUTH_TOKEN_CCC",
            Some("NEW_REFRESH_TOKEN_DDD"),
            Some(9999),
        )
        .unwrap();
        assert!(bak.exists());

        // Corrupt live file then restore via shared helper (same path as
        // write_config_validated post-write failure recovery).
        std::fs::write(&path, "[[[broken").unwrap();
        crate::config_edit::restore_from_bak(&path).unwrap();
        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(
            restored.contains("OLD_AUTH_TOKEN_AAA"),
            "restore must recover pre-edit tokens: {restored}"
        );
        crate::config_edit::validate_config_str(&restored).unwrap();
    }

    #[test]
    fn update_config_tokens_rejects_invalid_without_touching_bak_path_errors_no_secret() {
        // Missing file: error must not echo the secret we tried to write.
        let secret = "SUPER_SECRET_TOKEN_SHOULD_NOT_LEAK_XYZ";
        let err = update_config_tokens(
            Path::new("/tmp/rustfox-nonexistent-config-dir/config.toml"),
            "exa",
            secret,
            Some(secret),
            Some(1),
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains(secret), "error must not echo secrets: {err}");

        // Pre-validate abort: invalid serialised content must not modify file.
        // Covered indirectly via write_config_validated; here ensure a valid
        // update still leaves secrets only on disk (not in Result Display).
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());
        let bak =
            update_config_tokens(&path, "exa", secret, Some("NEW_REFRESH_OK"), Some(42)).unwrap();
        // Ok path returns PathBuf — Display of PathBuf is the bak path, not token.
        let bak_display = format!("{bak:?}");
        assert!(!bak_display.contains(secret));
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains(secret), "token must be persisted to disk");
    }

    #[test]
    fn update_config_tokens_prevalidate_abort_leaves_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());
        let before = std::fs::read_to_string(&path).unwrap();

        // Direct shared-helper check: invalid content never touches config.
        let err = crate::config_edit::write_config_validated(&path, "not = valid = toml [[[")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Validation failed") || err.contains("parse"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(
            !path.with_extension("toml.bak").exists(),
            "pre-validate abort must not create .bak"
        );
    }

    #[test]
    fn update_config_tokens_missing_server_errors_without_touching_file_or_bak() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_cfg(&dir, &oauth_config_toml());
        let before = std::fs::read_to_string(&path).unwrap();
        let secret = "SUPER_SECRET_MISSING_SERVER_TOKEN_XYZ";

        let err = update_config_tokens(&path, "does-not-exist", secret, Some(secret), Some(42))
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("no [[mcp_servers]] entry named `does-not-exist`"),
            "expected hard-error for missing server: {err}"
        );
        assert!(!err.contains(secret), "error must not echo secrets: {err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "missing server must leave config file untouched"
        );
        assert!(
            !path.with_extension("toml.bak").exists(),
            "missing server must not create .bak"
        );
    }
}
