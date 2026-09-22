//! Standalone portal preview server — serves the REAL embedded web/dist
//! (via `include_dir!`, same code path as production) with a scripted fake
//! `AgentOps` and in-memory SQLite. No LLM, no Telegram, no network deps.
//!
//! Purpose: end-to-end smoke of the compiled-in frontend (Playwright, curl,
//! or a browser). This is exactly the binary content produced by
//! `scripts/build-all.sh` — if it white-screens here, it white-screens in prod.
//!
//! Usage:
//!   cargo run --example portal_preview            # binds 127.0.0.1:8123
//!   PORTAL_PREVIEW_TOKEN=mytoken cargo run --example portal_preview

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rustfox::config::{Config, PortalConfig};
use rustfox::memory::MemoryStore;
use rustfox::platform::IncomingMessage;
use rustfox::portal::{auth, AgentOps, PortalState, SkillInfo};
use rustfox::scheduler::reminders::ScheduledTaskStore;
use rustfox::tool_registry::ToolUiMode;

/// Minimal AgentOps for a UI smoke test. Returns canned data; never calls an LLM.
struct PreviewAgent {
    config: Config,
}

impl AgentOps for PreviewAgent {
    fn current_model(&self) -> futures::future::BoxFuture<'_, String> {
        Box::pin(async { "preview-model".to_string() })
    }
    fn is_processing(&self, _user_id: &str) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn set_model(&self, _model_id: String) -> futures::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn reload_skills_and_agents(&self) -> futures::future::BoxFuture<'_, (usize, usize)> {
        Box::pin(async { (1, 1) })
    }
    fn cancel_processing(&self, _user_id: String) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn clear_cancel_token(&self, _user_id: String) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn skill_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async {
            vec![SkillInfo {
                name: "preview-skill".into(),
                description: "skill for the UI preview".into(),
                model: None,
            }]
        })
    }
    fn agent_entries(&self) -> futures::future::BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async {
            vec![SkillInfo {
                name: "preview-agent".into(),
                description: "agent for the UI preview".into(),
                model: Some("preview-model".into()),
            }]
        })
    }
    fn remove_scheduler_job(&self, _job_id: uuid::Uuid) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    fn process_message(
        &self,
        _incoming: IncomingMessage,
        _tool_event_tx: Option<
            tokio::sync::mpsc::Sender<rustfox::platform::tool_notifier::ToolEvent>,
        >,
        _stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        _ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async {
            Ok("preview: no LLM attached — this is a UI smoke server.".into())
        })
    }
    fn set_soul_updated(&self, _value: bool) {}
    fn provider_names(&self) -> Vec<String> {
        vec!["preview".into()]
    }
    fn config(&self) -> &Config {
        &self.config
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let port: u16 = std::env::var("PORTAL_PREVIEW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8123);
    let token =
        std::env::var("PORTAL_PREVIEW_TOKEN").unwrap_or_else(|_| "preview-token".to_string());

    // Minimal on-disk config (Config::load wants a real file; settings reads it).
    let tmp = std::env::temp_dir().join(format!("rustfox-preview-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let config_path: PathBuf = tmp.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"[telegram]
bot_token = "preview"
allowed_user_ids = [1]

[openrouter]
api_key = "preview"

[sandbox]
allowed_directory = "."

[portal]
enabled = true
port = {port}
bind = "127.0.0.1"
token = "{token}"
user_name = "web"
"#
        ),
    )?;

    let mut config = Config::load(&config_path)?;
    config.resolved_home = Some(tmp.clone());

    let portal_config = PortalConfig {
        enabled: true,
        port,
        bind: "127.0.0.1".into(),
        token: Some(token.clone()),
        token_sha256: None,
        user_name: "web".into(),
    };

    let memory = MemoryStore::open_in_memory()?;
    let task_store = ScheduledTaskStore::new(memory.connection());
    let state = PortalState::new(
        Arc::new(PreviewAgent { config }),
        memory,
        task_store,
        portal_config,
        config_path,
        Some(tmp.clone()),
    );
    auth::ensure_startup_token(&state);

    let app = rustfox::portal::router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("PREVIEW_URL=http://127.0.0.1:{port}/");
    println!("PREVIEW_TOKEN=***");
    axum::serve(listener, app).await?;
    Ok(())
}
