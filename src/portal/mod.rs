//! Opt-in embedded web portal (ADR 0004).
//!
//! Serves the React SPA (`web/dist`, embedded via `include_dir`) and a
//! REST+SSE API (`docs/portal-api.md`) from inside the main RustFox
//! runtime. Spawned from `main.rs` when `[portal] enabled = true`.
//!
//! ADR-0056: `[device_ws]` shares this bind and port. When only the device
//! socket is on, [`serve_device_api_only`] binds the same address without the SPA.
//! ADR-0066 adds `POST/GET /api/device/v1/audio` on that same bind.

pub mod auth;
pub mod chat;
pub mod control;
pub mod data;
pub mod devices;
pub mod error;
pub mod google;
pub mod install;
pub mod mcp;
pub mod mcp_registry;
pub mod secrets;
pub mod settings;
pub mod static_serve;
pub mod tasks_admin;
pub mod url;

use std::sync::Arc;

use anyhow::Context as _;
use axum::extract::DefaultBodyLimit;
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
    /// ADR-0050 seat←run: true when an in-flight invoke/spawn is tagged to `agent_name`.
    fn is_seat_running(&self, agent_name: &str) -> bool {
        let _ = agent_name;
        false
    }
    /// ADR-0050: whether this seat can receive a live run signal (else definition-only).
    fn seat_has_live_signal(&self, agent_name: &str) -> bool {
        let _ = agent_name;
        true
    }
    fn set_model(&self, model_id: String) -> futures::future::BoxFuture<'_, anyhow::Result<()>>;
    fn reload_skills_and_agents(&self) -> futures::future::BoxFuture<'_, (usize, usize)>;
    fn cancel_processing(&self, user_id: String) -> futures::future::BoxFuture<'_, bool>;
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_, ()>;
    /// Reset mid-run mode for the portal user (`default` + `user_id`).
    /// Default is a no-op so test fakes stay quiet. The real agent deletes
    /// the same key as [`Agent::clear_conversation`] and never takes a
    /// Telegram platform.
    fn reset_web_mid_run_mode(&self, user_id: String) -> futures::future::BoxFuture<'_, ()> {
        let _ = user_id;
        Box::pin(async {})
    }
    fn skill_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>>;
    fn agent_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>>;
    fn remove_scheduler_job(&self, job_id: uuid::Uuid) -> futures::future::BoxFuture<'_, bool>;
    /// Schedule `task` with the live JobScheduler and persist its job id.
    /// Err = arming failed (bad trigger, scheduler down) — the row must NOT
    /// be left active-and-unarmed (portal rolls back; restore logs + retires).
    fn arm_task(
        &self,
        task: crate::scheduler::reminders::ScheduledTask,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<uuid::Uuid>>;
    /// Remove the task's live job if any. Idempotent (no job = success).
    /// Returns whether a live job was actually removed.
    fn disarm_task(
        &self,
        task: crate::scheduler::reminders::ScheduledTask,
    ) -> futures::future::BoxFuture<'_, bool>;
    fn process_message(
        &self,
        incoming: crate::platform::IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>>;
    fn set_soul_updated(&self, value: bool);
    fn provider_names(&self) -> Vec<String>;
    /// Names of every tool available at runtime (builtin registry + MCP),
    /// used by the agents editor to gate `tools:` frontmatter (ADR 0011).
    fn tool_names(&self) -> Vec<String>;
    fn config(&self) -> &Config;
    /// ADR-0056: device `user_text` joins the owner interactive TurnGate.
    fn submit_device_turn(
        &self,
        turn: crate::device_ws::DeviceUserText,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<crate::device_ws::DeviceTurnOutcome>> {
        let _ = turn;
        Box::pin(async { anyhow::bail!("device inbound is not wired on this agent") })
    }
    /// Start the built-in Google HTTP MCP connector. Default is a no-op so
    /// test fakes do not need a live MCP client.
    fn start_google_connector(
        &self,
        config: crate::config::McpServerConfig,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        let _ = config;
        Box::pin(async { Ok(()) })
    }
    /// Connect one MCP server (hot reload). Default no-op for test fakes.
    fn mcp_connect(
        &self,
        config: crate::config::McpServerConfig,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        let _ = config;
        Box::pin(async { Ok(()) })
    }
    /// Disconnect one MCP server by name. Default no-op.
    fn mcp_disconnect(&self, name: String) -> futures::future::BoxFuture<'_, bool> {
        let _ = name;
        Box::pin(async { false })
    }
    fn mcp_is_connected(&self, name: &str) -> bool {
        let _ = name;
        false
    }
    fn mcp_connected_names(&self) -> Vec<String> {
        Vec::new()
    }
}

impl AgentOps for Agent {
    fn current_model(&self) -> futures::future::BoxFuture<'_, String> {
        Box::pin(async move { self.current_model.read().await.clone() })
    }
    fn is_processing(&self, user_id: &str) -> futures::future::BoxFuture<'_, bool> {
        let user_id = user_id.to_string();
        Box::pin(async move {
            self.is_processing(crate::platform::DEFAULT_BOT_ID, &user_id)
                .await
        })
    }
    fn is_seat_running(&self, agent_name: &str) -> bool {
        self.seat_run.is_running(agent_name)
    }
    fn seat_has_live_signal(&self, agent_name: &str) -> bool {
        // Loaded agent definitions are taggable via invoke_agent / spawn_agents.
        let _ = agent_name;
        true
    }
    fn set_model(&self, model_id: String) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move { self.set_model(&model_id).await.map(|_| ()) })
    }
    fn reload_skills_and_agents(&self) -> futures::future::BoxFuture<'_, (usize, usize)> {
        Box::pin(async move { self.reload_skills_and_agents().await })
    }
    fn cancel_processing(&self, user_id: String) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move {
            self.cancel_processing(crate::platform::DEFAULT_BOT_ID, &user_id)
                .await
        })
    }
    fn clear_cancel_token(&self, user_id: String) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            // Portal chat is the interactive lane. Do not drop a scheduled
            // token that shares this bot+user key (ADR-0052).
            self.clear_cancel_token(
                crate::platform::DEFAULT_BOT_ID,
                &user_id,
                crate::agent::CancelLane::Interactive,
            )
            .await
        })
    }
    fn reset_web_mid_run_mode(&self, user_id: String) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.delete_mid_run_mode(crate::platform::DEFAULT_BOT_ID, &user_id)
                .await;
        })
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
    fn arm_task(
        &self,
        task: crate::scheduler::reminders::ScheduledTask,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<uuid::Uuid>> {
        Box::pin(async move { self.arm_task(&task).await })
    }
    fn disarm_task(
        &self,
        task: crate::scheduler::reminders::ScheduledTask,
    ) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move { self.disarm_task(&task).await })
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
    fn tool_names(&self) -> Vec<String> {
        self.all_tool_definitions()
            .into_iter()
            .map(|td| td.function.name)
            .collect()
    }
    fn config(&self) -> &Config {
        &self.config
    }
    fn submit_device_turn(
        &self,
        turn: crate::device_ws::DeviceUserText,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<crate::device_ws::DeviceTurnOutcome>> {
        let agent = self.self_weak.upgrade();
        Box::pin(async move {
            let agent = agent.context("agent dropped")?;
            crate::device_ws::submit_on_agent(&agent, turn).await
        })
    }
    fn start_google_connector(
        &self,
        config: crate::config::McpServerConfig,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move { self.mcp.connect(&config).await })
    }
    fn mcp_connect(
        &self,
        config: crate::config::McpServerConfig,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move { self.mcp.connect(&config).await })
    }
    fn mcp_disconnect(&self, name: String) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move { self.mcp.disconnect(&name).await })
    }
    fn mcp_is_connected(&self, name: &str) -> bool {
        self.mcp.is_connected(name)
    }
    fn mcp_connected_names(&self) -> Vec<String> {
        self.mcp.connected_names()
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
    /// GitHub API client for the skill installer (ADR 0011 B). Swappable for
    /// tests — production default is the reqwest-backed fetcher.
    pub fetcher: Arc<dyn install::GitHubFetcher>,
    pub started_at: std::time::Instant,
    /// Host secret store (Slice 1). Defaults to FakeSecretStore in tests;
    /// production wires keyring/file via [`PortalState::with_secret_store`].
    pub secret_store: Arc<dyn crate::secret_store::SecretStore>,
    /// Pending one-shot secret claims (Slice 2).
    pub pending_secrets: Arc<crate::secret_store::PendingSecretRegistry>,
    /// In-flight Google OAuth `state` values. The callback is public, so a
    /// matching state is the only proof the tap started here.
    pub google_oauth_states:
        Arc<std::sync::Mutex<std::collections::HashMap<String, crate::google_mcp::PendingLogin>>>,
    /// Effective default schedule timezone (ADR-0022).
    pub schedule_tz: crate::scheduler::timezone::ScheduleTimezone,
    /// Live `[sandbox] mode` shared with CommandTool (ADR-0025).
    pub sandbox_mode: std::sync::Arc<std::sync::RwLock<crate::sandbox::SandboxMode>>,
    /// SwitchBot Open API tools (ADR-0057). Same `Arc` as the ToolRegistry
    /// so Portal Settings enable without a restart.
    pub switchbot: crate::switchbot::SwitchBotTools,
    /// Official MCP Registry origin (ADR-0041 §11). Tests point this at a
    /// stand-in. Production uses the official host unless
    /// `RUSTFOX_MCP_REGISTRY_URL` is an http(s) URL.
    pub mcp_registry_base: String,
    /// Live `[voice]` settings (ADR-0063). Production shares the agent's
    /// handle so a Settings save is what the next Telegram note reads.
    pub voice: crate::asr::VoiceRuntime,
    /// Authenticated device sockets that can receive `assistant_audio`.
    pub device_live: crate::device_ws::LiveSockets,
    /// PCM bytes named by `assistant_audio`. Not a replay queue.
    pub device_pcm: crate::device_audio::PcmStore,
}

impl PortalState {
    /// Bot whose schedules the portal manages: the shim bot (#154; no
    /// switcher yet). Telegram delivery looks the owner up by this id.
    pub fn bot_id(&self) -> String {
        let bots = &self.agent.config().bots;
        if bots.is_empty() {
            return crate::platform::DEFAULT_BOT_ID.to_string();
        }
        crate::platform::normalize_bot_id(&Config::shim_bot(bots).id).to_string()
    }

    pub fn new(
        agent: Arc<dyn AgentOps>,
        memory: MemoryStore,
        task_store: ScheduledTaskStore,
        config: PortalConfig,
        config_path: std::path::PathBuf,
        home_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let voice = crate::asr::VoiceRuntime::from_config(agent.config().voice.clone());
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
            fetcher: Arc::new(install::ReqwestFetcher::new()),
            started_at: std::time::Instant::now(),
            secret_store: Arc::new(crate::secret_store::FakeSecretStore::new()),
            pending_secrets: Arc::new(crate::secret_store::PendingSecretRegistry::default()),
            google_oauth_states: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            schedule_tz: crate::scheduler::timezone::ScheduleTimezone {
                tz: chrono_tz::UTC,
                source: crate::scheduler::timezone::TimezoneSource::Fallback,
            },
            sandbox_mode: std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::SandboxMode::Trusted,
            )),
            switchbot: crate::switchbot::SwitchBotTools::disconnected(),
            mcp_registry_base: crate::mcp_registry::registry_base_from_env(),
            voice,
            device_live: crate::device_ws::LiveSockets::new(),
            device_pcm: crate::device_audio::PcmStore::new(),
        }
    }

    /// Point registry reads at another origin (tests, or a pinned mirror).
    pub fn with_mcp_registry_base(mut self, base: impl Into<String>) -> Self {
        self.mcp_registry_base = crate::mcp_registry::normalize_registry_base(Some(&base.into()));
        self
    }

    /// Share the process SwitchBot connector (vault + live enable flag).
    pub fn with_switchbot(mut self, tools: crate::switchbot::SwitchBotTools) -> Self {
        self.switchbot = tools;
        self
    }

    /// Share the process voice handle with Telegram (ADR-0063).
    pub fn with_voice(mut self, voice: crate::asr::VoiceRuntime) -> Self {
        self.voice = voice;
        self
    }

    pub fn with_schedule_tz(mut self, tz: crate::scheduler::timezone::ScheduleTimezone) -> Self {
        self.schedule_tz = tz;
        self
    }

    /// Share the live sandbox mode lock with CommandTool (ADR-0025 portal switch).
    pub fn with_sandbox_mode(
        mut self,
        mode: std::sync::Arc<std::sync::RwLock<crate::sandbox::SandboxMode>>,
    ) -> Self {
        self.sandbox_mode = mode;
        self
    }

    /// Replace the default fake secret store (production / integration tests).
    pub fn with_secret_store(mut self, store: Arc<dyn crate::secret_store::SecretStore>) -> Self {
        self.secret_store = store;
        self
    }

    /// Replace the pending-secrets registry (tests with custom TTL).
    pub fn with_pending_secrets(
        mut self,
        registry: Arc<crate::secret_store::PendingSecretRegistry>,
    ) -> Self {
        self.pending_secrets = registry;
        self
    }

    /// The portal user's identity in memory (ADR 0005).
    pub fn web_user_id(&self) -> String {
        self.config.user_name.clone()
    }

    /// Device token catalog (ADR-0056) on the shared SQLite connection.
    pub async fn device_registry(&self) -> anyhow::Result<crate::device_ws::DeviceRegistry> {
        crate::device_ws::DeviceRegistry::open(
            self.memory.connection(),
            Arc::clone(&self.secret_store),
        )
        .await
    }
}

/// Build the portal router (auth middleware applied to the API group).
pub fn router(state: PortalState) -> Router {
    let device_ws_on = state.agent.config().device_ws.enabled;
    router_with(state, device_ws_on)
}

fn router_with(state: PortalState, device_ws_on: bool) -> Router {
    // Public: login/logout/me (me reports anonymous as unauthenticated).
    // Public: login/logout/me + health. `/health` must stay unauthenticated
    // (ADR 0008B): it is the SPA's restart detector — it polls bootId every
    // 15 s *before* re-auth runs, so gating it would blind the reconnect
    // state machine (docs/portal-api.md marks it Public explicitly).
    let public = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/health", get(data::health))
        .route("/connectors/google/callback", get(google::callback))
        // Slice 2: claim by opaque one-shot token (magic link; no session).
        .route(
            "/secrets/claim/{token}",
            get(secrets::get_claim).post(secrets::submit_claim),
        )
        .route("/secrets/claim/{token}/cancel", post(secrets::cancel_claim));

    let protected = Router::new()
        .route("/chat/history", get(chat::history))
        .route(
            "/chat/threads",
            get(chat::threads).post(chat::create_thread),
        )
        .route("/chat", post(chat::send))
        .route("/chat/cancel", post(chat::cancel))
        .route("/agents", get(data::agents))
        .route("/agents/skills", get(data::skills))
        .route("/agents/reload", post(data::reload_skills))
        // Skills/agents control plane (ADR 0011). `/agents/{name}` never
        // collides with the static `/agents/skills`|`/agents/reload` — axum
        // prefers static segments over path params.
        .route(
            "/skills",
            get(control::list_entries).post(control::create_skill),
        )
        .route("/skills/installed", get(install::installed_list))
        .route("/skills/install", post(install::install_skill))
        .route(
            "/skills/{name}",
            get(control::entry_detail).delete(control::delete_entry),
        )
        .route(
            "/skills/{name}/file",
            get(control::read_file).put(control::write_file),
        )
        .route("/agents", post(control::create_agent))
        .route(
            "/agents/{name}",
            get(control::agent_detail).delete(control::delete_agent),
        )
        .route(
            "/agents/{name}/file",
            get(control::read_agent_file).put(control::write_agent_file),
        )
        .route("/memory/search", get(data::memory_search))
        .route("/tasks", get(data::tasks).post(tasks_admin::task_create))
        .route("/schedule/timezone", get(data::schedule_timezone))
        .route(
            "/tasks/{id}",
            axum::routing::put(tasks_admin::task_update).delete(tasks_admin::task_delete),
        )
        .route("/tasks/{id}/runs", get(data::task_runs))
        .route("/tasks/{id}/enable", post(tasks_admin::task_enable))
        .route("/tasks/{id}/disable", post(tasks_admin::task_disable))
        .route("/stats", get(data::stats))
        .route("/opencode/status", get(settings::opencode_status))
        .route("/settings", get(settings::get_settings))
        .route(
            "/connectors/google",
            get(google::status).post(google::start),
        )
        .route("/connectors/google/client-id", post(google::set_client_id))
        .route("/settings", axum::routing::patch(settings::patch_settings))
        .route(
            "/settings/sandbox/switch-safe",
            axum::routing::post(settings::switch_to_safe_mode),
        )
        .route(
            "/settings/sandbox/trusted",
            axum::routing::post(settings::switch_to_trusted_mode),
        )
        .route(
            "/settings/sandbox/install",
            axum::routing::post(settings::install_sandbox_tool),
        )
        .route(
            "/settings/self-upgrade",
            axum::routing::post(settings::self_upgrade),
        )
        .route("/settings/mcp/presets", get(mcp::list_presets))
        .route(
            "/settings/mcp/{name}",
            axum::routing::put(mcp::upsert_server).delete(mcp::delete_server),
        )
        .route(
            "/settings/mcp/{name}/enable",
            axum::routing::post(mcp::enable_server),
        )
        .route(
            "/settings/mcp/{name}/disable",
            axum::routing::post(mcp::disable_server),
        )
        .route("/mcp/registry/search", get(mcp_registry::search))
        .route(
            "/mcp/registry/servers/{name}",
            get(mcp_registry::server_detail),
        )
        .route("/soul", get(settings::get_soul))
        .route("/soul", axum::routing::put(settings::put_soul))
        // Slice 2: authenticated pending-secret admin.
        .route(
            "/secrets/pending",
            get(secrets::list_pending).post(secrets::create_pending),
        )
        .route(
            "/secrets/pending/{id}",
            axum::routing::delete(secrets::cancel_pending_by_id),
        )
        .route(
            "/settings/devices",
            get(devices::list_devices).post(devices::create_device),
        )
        .route(
            "/settings/devices/{id}/rotate",
            post(devices::rotate_device),
        )
        .route(
            "/settings/devices/{id}",
            axum::routing::delete(devices::revoke_device),
        );
    let protected = protected.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    // Unmatched /api/* paths must 404 as JSON, not leak the SPA index.html
    // via the static fallback (clients branch on the error envelope).
    let mut public = public;
    if device_ws_on {
        public = public
            .route("/device/v1/ws", get(devices::ws))
            .route(
                "/device/v1/audio",
                post(devices::post_audio)
                    .layer(DefaultBodyLimit::max(crate::device_audio::MAX_WAV_BYTES)),
            )
            .route("/device/v1/audio/{id}", get(devices::get_audio));
    }

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
    let device_on = state.agent.config().device_ws.enabled;
    let bind = state.config.bind.clone();
    let addr = format!("{}:{}", state.config.bind, state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        anyhow::anyhow!("Portal failed to bind {addr}: {e} (privileged port? try port > 1024)")
    })?;
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    tracing::info!("Portal: serving on http://{local}/");
    if device_on {
        crate::device_ws::warn_if_public_bind(&bind);
        crate::device_ws::log_listening(&local);
    }

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

/// API-only Axum on the portal bind/port when `[device_ws]` is on and the
/// portal UI is off. No SPA. Device tokens still come from Settings, so
/// issue them with the portal enabled first (or with both flags on).
pub async fn serve_device_api_only(
    state: PortalState,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let bind = state.config.bind.clone();
    crate::device_ws::warn_if_public_bind(&bind);
    let addr = format!("{}:{}", state.config.bind, state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        anyhow::anyhow!(
            "Device WebSocket failed to bind {addr}: {e} (privileged port? try port > 1024)"
        )
    })?;
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    crate::device_ws::log_listening(&local);
    tracing::info!("Device WebSocket: portal UI is off; serving the device API only");

    let app = device_api_router(state);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
        {
            tracing::error!("Device WebSocket: server error: {e}");
        }
    });
    Ok(())
}

/// Router used when the portal SPA is off: the device socket, device audio, and nothing else.
pub fn device_api_router(state: PortalState) -> Router {
    Router::new()
        .route(crate::device_ws::WS_PATH, get(devices::ws))
        .route(
            crate::device_ws::AUDIO_PATH,
            post(devices::post_audio)
                .layer(DefaultBodyLimit::max(crate::device_audio::MAX_WAV_BYTES)),
        )
        .route(crate::device_ws::AUDIO_ID_PATH, get(devices::get_audio))
        .fallback(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({
                    "error": { "code": "not_found", "message": "unknown API endpoint" }
                })),
            )
        })
        .with_state(state)
}
