#![allow(dead_code)]

pub mod chat;
pub mod config_page;
pub mod google_auth;

use std::{path::PathBuf, sync::Arc};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Router,
};

use crate::agent::Agent;

/// Shared state for all web handlers.
#[derive(Clone)]
pub struct WebState {
    /// None in setup-only mode; Some in normal mode.
    pub agent: Option<Arc<Agent>>,
    pub config_path: PathBuf,
}

/// Full router for normal mode (chat + config + OAuth).
pub fn build_router(agent: Arc<Agent>, config_path: PathBuf) -> Router {
    let state = WebState {
        agent: Some(agent),
        config_path,
    };
    base_routes()
        .route("/", get(chat::page))
        .route("/chat/send", post(chat::send))
        .route("/chat/stream/{session_id}", get(chat::stream))
        .with_state(state)
}

/// Minimal router for setup-only mode (config + OAuth only; chat returns 503).
pub fn build_setup_router(config_path: PathBuf) -> Router {
    let state = WebState {
        agent: None,
        config_path,
    };
    base_routes()
        .route(
            "/",
            get(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Bot not running — visit /config to complete setup",
                )
            }),
        )
        .route(
            "/chat/send",
            post(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(
                        serde_json::json!({"error":"Bot not running — complete setup first"}),
                    ),
                )
            }),
        )
        .with_state(state)
}

fn base_routes() -> Router<WebState> {
    Router::new()
        .route("/config", get(config_page::page))
        .route("/api/load-config", get(config_page::load_config))
        .route("/api/save-config", post(config_page::save_config))
        .route("/api/google-auth/start", post(google_auth::start))
        .route("/api/google-auth/poll/{device_code}", get(google_auth::poll))
}
