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
use serde_json::Value;
use std::collections::HashMap;
use tokio::process::Command;
use tracing::{error, info};

use crate::config::McpServerConfig;
use crate::llm::{FunctionDefinition, ToolDefinition};

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

        let transport_config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
            .auth_header(config.auth_token.clone().unwrap_or_default());

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
