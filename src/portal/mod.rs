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
pub mod url;

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
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_, ()>;
    fn skill_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>>;
    fn agent_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>>;
    fn remove_scheduler_job(&self, job_id: uuid::Uuid) -> futures::future::BoxFuture<'_, bool>;
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
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move { self.clear_cancel_token(&user_id).await })
    }
    fn skill_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async move {
            self.skills
                .read()
                .await
                .list()
                .into_iter()
                .map(SkillInfo::from_skill)
                .collect()
        })
    }
    fn agent_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async move {
            self.agents
                .read()
                .await
                .list()
                .into_iter()
                .map(SkillInfo::from_skill)
                .collect()
        })
    }
    fn remove_scheduler_job(&self, job_id: uuid::Uuid) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move { self.scheduler.remove_job(job_id).await.is_ok() })
    }
    fn process_message(
        &self,
        incoming: crate::platform::IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move {
            self.process_message(&incoming, tool_event_tx, stream_token_tx, ui_mode)
                .await
        })
    }
    fn set_soul_updated(&self, value: bool) {
        self.soul_updated
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }
    fn provider_names(&self) -> Vec<String> {
        self.registry.provider_names()
    }
    fn config(&self) -> &Config {
        &self.config
    }
}

/// Lightweight view of a loaded skill / agent definition for the API layer
/// (keeps `SkillRegistry` out of handler code so tests can fake it).
#[derive(Clone, Debug)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
}

impl SkillInfo {
    fn from_skill(s: &crate::skills::Skill) -> Self {
        Self {
            name: s.name.clone(),
            description: s.description.clone(),
            model: s.model.clone(),
        }
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
    /// RustFox home dir (resolved by `Config::resolve()`); where
    /// `portal_secret.key` lives. `None` only in odd embedder setups —
    /// cookie auth then fails closed with an internal error.
    pub home_dir: Option<std::path::PathBuf>,
    /// Cached HMAC signing secret (loaded lazily from portal_secret.key).
    pub secret: Arc<std::sync::OnceLock<[u8; 32]>>,
    /// Tokens minted at startup when none is configured (process lifetime).
    pub dev_tokens: Arc<std::sync::Mutex<Vec<String>>>,
    /// Random id for this process boot — the SPA polls `/api/health`,
    /// compares `bootId` and re-authenticates/reconciles after a restart
    /// (ADR 0008B).
    pub boot_id: String,
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
        home_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            agent,
            memory,
            task_store,
            config: Arc::new(config),
            config_path: Arc::new(config_path),
            home_dir,
            secret: Arc::new(std::sync::OnceLock::new()),
            dev_tokens: Arc::new(std::sync::Mutex::new(Vec::new())),
            boot_id: uuid::Uuid::new_v4().simple().to_string(),
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
    // Public: login/logout/me + health. `/health` must stay unauthenticated
    // (ADR 0008B): it is the SPA's restart detector — it polls bootId every
    // 15 s *before* re-auth runs, so gating it would blind the reconnect
    // state machine (docs/portal-api.md marks it Public explicitly).
    let public = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/health", get(data::health));

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
        .route("/stats", get(data::stats))
        .route("/settings", get(settings::get_settings))
        .route("/settings", axum::routing::patch(settings::patch_settings))
        .route("/soul", get(settings::get_soul))
        .route("/soul", axum::routing::put(settings::put_soul));
    let protected = protected.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    // Unmatched /api/* paths must 404 as JSON, not leak the SPA index.html
    // via the static fallback (clients branch on the error envelope).
    let api = public.merge(protected).fallback(|| async {
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": { "code": "not_found", "message": "unknown API endpoint" }
            })),
        )
    });

    Router::new()
        .nest("/api", api)
        .merge(static_serve::router())
        .with_state(state)
}

/// Bind and serve the portal. Returns after binding; the accept loop runs in
/// a spawned task until `shutdown` fires.
pub async fn serve(state: PortalState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let addr = format!("{}:{}", state.config.bind, state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
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
