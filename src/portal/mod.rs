//! Opt-in embedded web portal (ADR 0004).
//!
//! Serves the React SPA (`web/dist`, embedded via `include_dir`) and a
//! REST+SSE API (`docs/portal-api.md`) from inside the main RustFox
//! runtime. Spawned from `main.rs` only when `[portal] enabled = true`.

pub mod auth;
pub mod chat;
pub mod data;
pub mod error;
pub mod settings;
pub mod static_serve;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::config::{Config, PortalConfig};
use crate::memory::MemoryStore;
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::tool_registry::ToolUiMode;

/// The slice of [`Agent`] the portal needs. Object-safe so integration tests
/// can inject a scripted fake without an LLM (tests/portal_api.rs).
#[allow(async_fn_in_trait, reason = "object-safety via explicit methods below")]
pub trait AgentOps: Send + Sync {
    fn current_model(&self) -> futures::future::BoxFuture<'_, String>;
    fn is_processing(&self, user_id: &str) -> futures::future::BoxFuture<'_, bool>;
    fn set_model(&self, model_id: String) -> futures::future::BoxFuture<'_, anyhow::Result<()>>;
    fn reload_skills_and_agents(&self) -> futures::future::BoxFuture<'_, (usize, usize)>;
    fn cancel_processing(&self, user_id: String) -> futures::future::BoxFuture<'_, bool>;
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_>;
    fn process_message(
        &self,
        incoming: crate::platform::IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>>;
    fn set_soul_updated(&self, value: bool);
    fn provider_names(&self) -> Vec<String>;
    fn config(&self) -> &Config;
}

impl AgentOps for Agent {
    fn current_model(&self) -> futures::future::BoxFuture<'_, String> {
        Box::pin(async move { self.current_model.read().await.clone() })
    }
    fn is_processing(&self, user_id: &str) -> futures::future::BoxFuture<'_, bool> {
        let user_id = user_id.to_string();
        Box::pin(async move { self.is_processing(&user_id).await })
    }
    fn set_model(&self, model_id: String) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move { self.set_model(&model_id).await })
    }
    fn reload_skills_and_agents(&self) -> futures::future::BoxFuture<'_, (usize, usize)> {
        Box::pin(async move { self.reload_skills_and_agents().await })
    }
    fn cancel_processing(&self, user_id: String) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move { self.cancel_processing(&user_id).await })
    }
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_> {
        Box::pin(async move { self.clear_cancel_token(&user_id).await })
    }
    fn process_message(
        &self,
        incoming: crate::platform::IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move { self.process_message(&incoming, tool_event_tx, stream_token_tx, ui_mode).await })
    }
    fn set_soul_updated(&self, value: bool) {
        self.soul_updated.store(value, std::sync::atomic::Ordering::Relaxed);
    }
    fn provider_names(&self) -> Vec<String> {
        self.registry.provider_names()
    }
    fn config(&self) -> &Config {
        &self.config
    }
}

/// Shared state for all portal handlers.
#[derive(Clone)]
pub struct PortalState {
    pub agent: Arc<dyn AgentOps>,
    pub memory: MemoryStore,
    pub task_store: ScheduledTaskStore,
    pub config: Arc<PortalConfig>,
    /// Absolute path to the live `config.toml` (same file the bot loaded).
    pub config_path: Arc<std::path::PathBuf>,
    /// In-memory sessions (ADR 0006): session token → username.
    pub sessions: Arc<tokio::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// Serializes chat generations: one active run per web identity (ADR 0005).
    pub chat_busy: Arc<std::sync::atomic::AtomicBool>,
    pub started_at: std::time::Instant,
}

impl PortalState {
    pub fn new(
        agent: Arc<dyn AgentOps>,
        memory: MemoryStore,
        task_store: ScheduledTaskStore,
        config: PortalConfig,
        config_path: std::path::PathBuf,
    ) -> Self {
        Self {
            agent,
            memory,
            task_store,
            config: Arc::new(config),
            config_path: Arc::new(config_path),
            sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
            chat_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            started_at: std::time::Instant::now(),
        }
    }

    /// The portal user's identity in memory (ADR 0005).
    pub fn web_user_id(&self) -> String {
        self.config.user_name.clone()
    }
}

/// Build the portal router (auth middleware applied to the API group).
pub fn router(state: PortalState) -> Router {
    // Public: login/logout/me (me reports anonymous as unauthenticated).
    let public = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me));

    let protected = Router::new()
        .route("/chat/history", get(chat::history))
        .route("/chat/threads", get(chat::threads))
        .route("/chat", post(chat::send))
        .route("/chat/cancel", post(chat::cancel))
        .route("/agents", get(data::agents))
        .route("/agents/skills", get(data::skills))
        .route("/agents/reload", post(data::reload_skills))
        .route("/memory/search", get(data::memory_search))
        .route("/tasks", get(data::tasks))
        .route("/tasks/{id}/runs", get(data::task_runs))
        .route("/tasks/{id}/enable", post(data::task_enable))
        .route("/tasks/{id}/disable", post(data::task_disable))
        .route("/health", get(data::health))
        .route("/stats", get(data::stats))
        .route("/settings", get(settings::get_settings))
        .route("/settings", axum::routing::patch(settings::patch_settings))
        .route("/soul", get(settings::get_soul))
        .route("/soul", axum::routing::put(settings::put_soul))
        ;
    let protected = protected.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    Router::new()
        .nest("/api", public.merge(protected))
        .merge(static_serve::router())
        .with_state(state)
}

/// Bind and serve the portal. Returns after binding; the accept loop runs in
/// a spawned task until `shutdown` fires.
pub async fn serve(state: PortalState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let addr = format!("{}:{}", state.config.bind, state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| {
            anyhow::anyhow!("Portal failed to bind {addr}: {e} (privileged port? try port > 1024)")
        })?;
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    tracing::info!("Portal: serving on http://{local}/");

    let app = router(state);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
        {
            tracing::error!("Portal: server error: {e}");
        }
    });
    Ok(())
}
