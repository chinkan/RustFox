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
use rustfox::llm::{ChatMessage, MessageContent};
use rustfox::memory::MemoryStore;
use rustfox::platform::tool_notifier::ToolEvent;
use rustfox::platform::IncomingMessage;
use rustfox::portal::{auth, AgentOps, PortalState, SkillInfo};
use rustfox::scheduler::reminders::ScheduledTask;
use rustfox::scheduler::reminders::ScheduledTaskStore;
use rustfox::tool_registry::ToolUiMode;

/// Minimal AgentOps for a UI smoke test. Returns canned data; never calls an
/// LLM. Chat messages ARE persisted to the memory store, so the e2e exercises
/// the full write → reload → history path against real SQLite.
struct PreviewAgent {
    config: Config,
    memory: MemoryStore,
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
        incoming: IncomingMessage,
        tool_event_tx: Option<
            tokio::sync::mpsc::Sender<rustfox::platform::tool_notifier::ToolEvent>,
        >,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        _ui_mode: ToolUiMode,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<String>> {
        // Scripted streaming reply: tool events + token deltas, so the e2e
        // exercises the real SSE bridge (token/tool/done frames) without an
        // LLM. The returned string becomes the `done` frame content.
        Box::pin(async move {
            let text = incoming.text.clone();
            let memory = self.memory.clone();
            let conv = memory
                .get_or_create_conversation(&incoming.platform, &incoming.user_id)
                .await
                .unwrap_or_default();
            if !conv.is_empty() {
                let _ = memory
                    .save_message(
                        &conv,
                        &ChatMessage {
                            role: "user".into(),
                            content: Some(MessageContent::Text(incoming.text.clone())),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
            }
            if let Some(tt) = tool_event_tx {
                let _ = tt
                    .send(ToolEvent::Started {
                        name: "read_file".into(),
                        args_preview: "{\"path\"".into(),
                        arguments_json: "{}".into(),
                    })
                    .await;
                let _ = tt
                    .send(ToolEvent::Completed {
                        name: "read_file".into(),
                        success: true,
                    })
                    .await;
            }
            let reply = format!("Preview reply to: {}", text);
            if let Some(st) = stream_token_tx {
                for word in reply.split_inclusive(' ') {
                    let _ = st.send(word.to_string()).await;
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
            }
            if !conv.is_empty() {
                let _ = memory
                    .save_message(
                        &conv,
                        &ChatMessage {
                            role: "assistant".into(),
                            content: Some(MessageContent::Text(reply.clone())),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
            }
            Ok(reply)
        })
    }
    fn set_soul_updated(&self, _value: bool) {}
    fn provider_names(&self) -> Vec<String> {
        vec!["preview".into()]
    }
    fn tool_names(&self) -> Vec<String> {
        vec![
            "read_file".into(),
            "write_file".into(),
            "list_files".into(),
            "execute_command".into(),
            "plan_create".into(),
            "plan_update".into(),
        ]
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
        Arc::new(PreviewAgent {
            config,
            memory: memory.clone(),
        }),
        memory.clone(),
        task_store.clone(),
        portal_config,
        config_path,
        Some(tmp.clone()),
    );
    // ---------------------------------------------------------------------
    // Deterministic fixtures so the e2e gate can assert per-page rendering:
    // knowledge rows, a web conversation with messages, scheduled tasks+runs.
    // Embeddings are unavailable in-memory → hybrid search falls back to FTS5.
    // ---------------------------------------------------------------------
    memory
        .remember(
            "fact",
            "favourite_author",
            "Favorite author: The Death of Portia",
            None,
        )
        .await?;
    memory
        .remember(
            "project",
            "rustfox",
            "RustFox is a self-hosted Telegram AI assistant",
            None,
        )
        .await?;
    let conv = memory.get_or_create_conversation("web", "web").await?;
    for (role, content) in [
        (
            "user",
            "Which Patrick Rothfuss book comes after The Wise Man's Fear?",
        ),
        (
            "assistant",
            "The Door into Fire… fan sequel aside, official roadmap says The Winds of Winter.",
        ),
        ("user", "Ignore that — what is The Death of Portia?"),
        (
            "assistant",
            "The Death of Portia is a 2021 sci-fi novel by Mur Lafferty.",
        ),
    ] {
        memory
            .save_message(
                &conv,
                &ChatMessage {
                    role: role.into(),
                    content: Some(MessageContent::Text(content.into())),
                    tool_calls: None,
                    tool_call_id: None,
                },
            )
            .await?;
    }
    task_store
        .create(&ScheduledTask {
            id: "preview-task-weather".into(),
            scheduler_job_id: None,
            user_id: "1".into(),
            chat_id: "1".into(),
            platform: "telegram".into(),
            trigger_type: "recurring".into(),
            trigger_value: "0 2 * * 0".into(),
            prompt: "Fetch the HK weather briefing".into(),
            description: "Daily weather briefing".into(),
            status: "active".into(),
            created_at: "2026-09-01T00:00:00Z".into(),
            next_run_at: Some("2026-09-23T00:00:00Z".into()),
        })
        .await?;
    task_store
        .create(&ScheduledTask {
            id: "preview-task-sweep".into(),
            scheduler_job_id: None,
            user_id: "1".into(),
            chat_id: "1".into(),
            platform: "web".into(),
            trigger_type: "one_shot".into(),
            trigger_value: "2026-09-23T12:00:00Z".into(),
            prompt: "Sweep the vault inbox".into(),
            description: "Inbox sweep".into(),
            status: "active".into(),
            created_at: "2026-09-20T00:00:00Z".into(),
            next_run_at: Some("2026-09-23T12:00:00Z".into()),
        })
        .await?;
    task_store
        .insert_run(
            "run-1",
            "preview-task-weather",
            "2026-09-21T23:30:00Z",
            Some("done: 42 messages"),
            None,
            "completed",
        )
        .await?;
    task_store
        .insert_run(
            "run-2",
            "preview-task-weather",
            "2026-09-20T23:30:00Z",
            None,
            Some("timeout after 60s"),
            "failed",
        )
        .await?;

    auth::ensure_startup_token(&state);

    let app = rustfox::portal::router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("PREVIEW_URL=http://127.0.0.1:{port}/");
    println!("PREVIEW_TOKEN=***");
    axum::serve(listener, app).await?;
    Ok(())
}
