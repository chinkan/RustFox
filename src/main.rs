mod agent;
mod config;
mod langsmith;
mod llm;
mod mcp;
mod memory;
mod platform;
mod scheduler;
mod skills;
mod tools;
mod utils;

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
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rustfox=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    info!("Loading configuration from: {}", config_path.display());
    let config = Config::load(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    info!("Configuration loaded successfully");
    info!("  Model: {}", config.openrouter.model);
    info!("  Sandbox: {}", config.sandbox.allowed_directory.display());
    info!("  Allowed users: {:?}", config.telegram.allowed_user_ids);
    info!("  MCP servers: {}", config.mcp_servers.len());
    let langsmith = std::sync::Arc::new(crate::langsmith::LangSmithClient::new(
        config.langsmith.as_ref(),
    ));
    if langsmith.is_enabled() {
        info!(
            "  LangSmith: enabled (project: {})",
            config.langsmith.as_ref().unwrap().project
        );
    } else {
        info!("  LangSmith: disabled (no [langsmith] config)");
    }

    // Build embedding config if configured
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

    // Initialize memory store (SQLite + vector embeddings)
    let memory = MemoryStore::open(&config.memory.database_path, embedding_config)
        .context("Failed to initialize memory store")?;
    info!("  Database: {}", config.memory.database_path.display());

    // Initialize MCP connections
    let mut mcp_manager = McpManager::new();
    mcp_manager.connect_all(&config.mcp_servers).await;

    // Load skills from markdown files
    let skills = load_skills_from_dir(&config.skills.directory).await?;
    info!("  Skills: {}", skills.len());

    // Load agents from the agents directory
    let agents = load_skills_from_dir(&config.agents.directory).await?;
    info!("  Agents: {}", agents.len());

    // Create ScheduledTaskStore sharing the existing SQLite connection
    let task_store = crate::scheduler::reminders::ScheduledTaskStore::new(memory.connection());

    // Create scheduler as Arc so Agent can hold it and closures can reference it
    let scheduler = Arc::new(Scheduler::new().await?);

    // Create Bot early so it can be passed to Agent
    let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));

    // Channel for dispatching scheduled job work from fire closures to background runner
    let (job_tx, mut job_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent::ScheduledJobRequest>();

    // Arc::new_cyclic so Agent can store Weak<Self> for job closure captures (breaks Arc cycle)
    let agent = Arc::new_cyclic(|weak| {
        Agent::new(
            config.clone(),
            mcp_manager,
            memory.clone(),
            skills,
            agents,
            task_store.clone(),
            Arc::clone(&scheduler),
            Arc::clone(&bot),
            weak.clone(),
            job_tx,
            Arc::clone(&langsmith),
        )
    });

    // Spawn background runner: receives ScheduledJobRequest, calls process_message, sends reply
    let agent_for_runner = Arc::clone(&agent);
    tokio::spawn(async move {
        use teloxide::prelude::*;
        while let Some(req) = job_rx.recv().await {
            let agent = Arc::clone(&agent_for_runner);
            // Mark one-shot as completed (before running, so failure can override)
            if !req.is_recurring {
                let _ = req.task_store.set_status(&req.task_id, "completed").await;
            }
            let response = match agent.process_message(&req.incoming, None, None).await {
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

    // Register built-in background tasks and start scheduler
    register_builtin_tasks(
        &scheduler,
        memory.clone(),
        crate::llm::LlmClient::new(config.openrouter.clone()),
        config.memory.summarize_cron.clone(),
        config.memory.summarize_threshold,
    )
    .await?;
    scheduler.start().await?;
    info!("  Scheduler: active");
    agent.restore_scheduled_tasks().await;
    info!("  Scheduled tasks: restored from DB");

    // Run the Telegram platform
    info!("Bot is starting...");
    platform::telegram::run(
        agent,
        config.telegram.allowed_user_ids.clone(),
        Arc::clone(&bot),
    )
    .await?;

    Ok(())
}
