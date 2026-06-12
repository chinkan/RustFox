use anyhow::{Context, Result};
use rmcp::{
    model::{CallToolRequestParams, Tool as McpTool},
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
/// entry, and serialise back.  Comments are lost on round-trip, but the
/// functional data is fully preserved.  This is acceptable for a
/// machine-updated file.
pub async fn update_config_tokens(
    config_path: &Path,
    server_name: &str,
    auth_token: &str,
    new_refresh_token: Option<&str>,
    new_expires_at: Option<i64>,
) -> Result<()> {
    let content = tokio::fs::read_to_string(config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let mut doc: toml::Value = content
        .parse()
        .with_context(|| format!("Failed to parse TOML from {}", config_path.display()))?;

    if let Some(servers) = doc.get_mut("mcp_servers").and_then(|v| v.as_array_mut()) {
        for server in servers.iter_mut() {
            if server
                .get("name")
                .and_then(|v| v.as_str())
                .map(|n| n == server_name)
                .unwrap_or(false)
            {
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
                break;
            }
        }
    }

    let new_content = toml::to_string_pretty(&doc).context("Failed to serialise updated config")?;
    tokio::fs::write(config_path, new_content)
        .await
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    debug!("Persisted refreshed token for MCP server '{server_name}'");
    Ok(())
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
                )
                .await
                {
                    Ok(()) => {
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
        let env = config.env.clone();
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
                    let result = connection
                        .client
                        .call_tool(CallToolRequestParams {
                            meta: None,
                            name: tool_name_owned,
                            arguments: arguments.as_object().cloned(),
                            task: None,
                        })
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
                        .filter_map(|c| c.raw.as_text().map(|t| t.text.clone()))
                        .collect();

                    if text_parts.is_empty() {
                        return Ok(format!("{:?}", result.content));
                    }
                    return Ok(text_parts.join("\n"));
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
}
