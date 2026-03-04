mod agent;
mod config;
mod llm;
mod mcp;
mod memory;
mod platform;
mod scheduler;
mod skills;
mod tools;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::agent::Agent;
use crate::config::Config;
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::scheduler::tasks::register_builtin_tasks;
use crate::scheduler::Scheduler;
use crate::skills::loader::load_skills_from_dir;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rustfox=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    match Config::load(&config_path) {
        Ok(config) => run_normal_mode(config, config_path).await,
        Err(e) => {
            tracing::warn!("Config load failed ({e:#}). Starting setup-only mode.");
            run_setup_mode(config_path).await
        }
    }
}

async fn run_setup_mode(config_path: PathBuf) -> Result<()> {
    let port: u16 = std::env::var("RUSTFOX_SETUP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);

    info!("No valid config found — starting setup server on :{port}");
    info!("Open http://localhost:{port}/config to configure the bot.");

    let router = crate::web::build_setup_router(config_path);
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind setup server to {addr}"))?;

    axum::serve(listener, router)
        .await
        .context("Setup server error")?;

    Ok(())
}

async fn run_normal_mode(config: Config, config_path: PathBuf) -> Result<()> {
    info!("Configuration loaded successfully");
    info!("  Model: {}", config.openrouter.model);
    info!("  Sandbox: {}", config.sandbox.allowed_directory.display());
    info!("  Allowed users: {:?}", config.telegram.allowed_user_ids);
    info!("  MCP servers: {}", config.mcp_servers.len());

    let embedding_config =
        config
            .embedding
            .as_ref()
            .map(|cfg| crate::memory::embeddings::EmbeddingConfig {
                api_key: cfg.api_key.clone(),
                base_url: cfg.base_url.clone(),
                model: cfg.model.clone(),
                dimensions: cfg.dimensions,
            });

    let memory = MemoryStore::open(&config.memory.database_path, embedding_config)
        .context("Failed to initialize memory store")?;
    info!("  Database: {}", config.memory.database_path.display());

    let mut mcp_manager = McpManager::new();
    mcp_manager.connect_all(&config.mcp_servers).await;

    let skills = load_skills_from_dir(&config.skills.directory).await?;
    info!("  Skills: {}", skills.len());

    let task_store = crate::scheduler::reminders::ScheduledTaskStore::new(memory.connection());
    let scheduler = Arc::new(Scheduler::new().await?);
    let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));

    let (job_tx, mut job_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent::ScheduledJobRequest>();

    let agent = Arc::new_cyclic(|weak| {
        Agent::new(
            config.clone(),
            mcp_manager,
            memory.clone(),
            skills,
            task_store.clone(),
            Arc::clone(&scheduler),
            Arc::clone(&bot),
            weak.clone(),
            job_tx,
        )
    });

    // Background scheduled job runner
    let agent_for_runner = Arc::clone(&agent);
    tokio::spawn(async move {
        use teloxide::prelude::*;
        while let Some(req) = job_rx.recv().await {
            let agent = Arc::clone(&agent_for_runner);
            if !req.is_recurring {
                let _ = req.task_store.set_status(&req.task_id, "completed").await;
            }
            let response = match agent.process_message(&req.incoming).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Scheduled task {} failed: {}", req.task_id, e);
                    if !req.is_recurring {
                        let _ = req.task_store.set_status(&req.task_id, "failed").await;
                    }
                    continue;
                }
            };
            let chat_id_val: i64 = match req.incoming.chat_id.parse() {
                Ok(v) => v,
                Err(_) => {
                    tracing::error!(
                        "Unparseable chat_id '{}' for task {}",
                        req.incoming.chat_id,
                        req.task_id
                    );
                    continue;
                }
            };
            let chat = teloxide::types::ChatId(chat_id_val);
            for chunk in crate::agent::split_response_chunks(&response, 4000) {
                if chunk.is_empty() {
                    continue;
                }
                if let Err(e) = req.bot.send_message(chat, &chunk).await {
                    tracing::error!("Failed to send scheduled response: {}", e);
                }
            }
        }
    });

    register_builtin_tasks(&scheduler, memory).await?;
    scheduler.start().await?;
    info!("  Scheduler: active");
    agent.restore_scheduled_tasks().await;
    info!("  Scheduled tasks: restored from DB");

    // Spawn web server if enabled
    if config.web.enabled {
        let web_agent = Arc::clone(&agent);
        let web_port = config.web.port;
        let web_config_path = config_path.clone();
        tokio::spawn(async move {
            let router = crate::web::build_router(web_agent, web_config_path);
            let addr = format!("127.0.0.1:{web_port}");
            match tokio::net::TcpListener::bind(&addr).await {
                Ok(listener) => {
                    info!("Web UI listening on http://127.0.0.1:{web_port}");
                    if let Err(e) = axum::serve(listener, router).await {
                        tracing::error!("Web server error: {e}");
                    }
                }
                Err(e) => tracing::error!("Web server failed to bind {addr}: {e}"),
            }
        });
    }

    info!("Bot is starting...");
    platform::telegram::run(
        agent,
        config.telegram.allowed_user_ids.clone(),
        Arc::clone(&bot),
    )
    .await?;

    Ok(())
}
