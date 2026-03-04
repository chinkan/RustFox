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
use google_auth::OAuthSession;

/// Shared state for all web handlers.
#[derive(Clone)]
pub struct WebState {
    /// None in setup-only mode; Some in normal mode.
    pub agent: Option<Arc<Agent>>,
    pub config_path: PathBuf,
    pub oauth_session: Arc<tokio::sync::Mutex<OAuthSession>>,
}

/// Full router for normal mode (chat + config + OAuth).
pub fn build_router(agent: Arc<Agent>, config_path: PathBuf) -> Router {
    let state = WebState {
        agent: Some(agent),
        config_path,
        oauth_session: Arc::new(tokio::sync::Mutex::new(OAuthSession::default())),
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
        oauth_session: Arc::new(tokio::sync::Mutex::new(OAuthSession::default())),
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
        .route("/api/google-auth/callback", get(google_auth::callback))
        .route("/api/google-auth/status", get(google_auth::status))
}
