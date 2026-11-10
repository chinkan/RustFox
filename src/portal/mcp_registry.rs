//! Official MCP Registry proxy (ADR-0041 §11).
//!
//! Authenticated reads of public registry metadata. Install still goes
//! through `PUT /api/settings/mcp/{slug}` — these handlers do not write
//! config and do not seal secrets.

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::PortalError;
use super::PortalState;
use crate::mcp_registry::{self, ExistingServer, RegistryError};

#[derive(Debug, Deserialize)]
pub struct RegistrySearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

fn existing_servers(state: &PortalState) -> Vec<ExistingServer> {
    match crate::config::Config::load(state.config_path.as_path()) {
        Ok(cfg) => cfg
            .mcp_servers
            .into_iter()
            .map(|server| ExistingServer {
                name: server.name,
                url: server.url,
                has_command: server
                    .command
                    .as_deref()
                    .is_some_and(|command| !command.trim().is_empty()),
            })
            .collect(),
        Err(err) => {
            tracing::warn!("MCP registry slug check skipped; config unreadable: {err}");
            Vec::new()
        }
    }
}

fn registry_error(err: RegistryError) -> PortalError {
    match err {
        RegistryError::SearchTooShort => PortalError::bad_request(
            "search_too_short",
            "search query must be empty or at least 3 characters",
        ),
        RegistryError::InvalidLimit => {
            PortalError::bad_request("invalid_limit", "limit must be between 1 and 100")
        }
        RegistryError::InvalidCursor => {
            PortalError::bad_request("invalid_cursor", "cursor is invalid")
        }
        RegistryError::InvalidName => {
            PortalError::bad_request("invalid_name", "registry server name is invalid")
        }
        RegistryError::NotFound => PortalError::not_found("registry server"),
        RegistryError::Unavailable => PortalError::new(
            StatusCode::BAD_GATEWAY,
            "registry_unavailable",
            "Official MCP Registry is unreachable",
        ),
    }
}

/// GET /api/mcp/registry/search?q=&cursor=&limit=
pub async fn search(
    State(state): State<PortalState>,
    Query(query): Query<RegistrySearchQuery>,
) -> Result<Json<Value>, PortalError> {
    let body = mcp_registry::proxy_search(
        mcp_registry::registry_http_client(),
        &state.mcp_registry_base,
        &query.q,
        &query.cursor,
        query.limit,
        &existing_servers(&state),
    )
    .await
    .map_err(registry_error)?;
    Ok(Json(body))
}

/// GET /api/mcp/registry/servers/{urlencodedName}
pub async fn server_detail(
    State(state): State<PortalState>,
    AxPath(name): AxPath<String>,
) -> Result<Json<Value>, PortalError> {
    let body = mcp_registry::proxy_detail(
        mcp_registry::registry_http_client(),
        &state.mcp_registry_base,
        &name,
        &existing_servers(&state),
    )
    .await
    .map_err(registry_error)?;
    Ok(Json(body))
}
