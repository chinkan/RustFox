use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use rustfox::agent::Agent;
use rustfox::config::Config;
use rustfox::mcp::McpManager;
use rustfox::memory::MemoryStore;
use rustfox::platform;
use rustfox::scheduler::tasks::register_builtin_tasks;
use rustfox::scheduler::Scheduler;
use rustfox::setup;
use rustfox::skills::{loader::load_skills_from_dir, SkillSource};

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

    // Check for --setup and --service subcommands before doing anything else
    if let Some(cmd) = setup::parse_args() {
        match cmd {
            setup::Command::Setup { cli } => {
                let cfg_path = if let Ok(path) = std::env::var("RUSTFOX_CONFIG_PATH") {
                    PathBuf::from(path)
                } else {
                    let cwd = PathBuf::from("config.toml");
                    if cwd.exists() {
                        cwd
                    } else {
                        let env_home = std::env::var("RUSTFOX_HOME").ok();
                        if let Some(home) = rustfox::home::default_home(
                            env_home.as_deref(),
                            dirs::home_dir().as_deref(),
                        ) {
                            let candidate = home.join("config.toml");
                            if candidate.exists() {
                                candidate
                            } else {
                                cwd
                            }
                        } else {
                            cwd
                        }
                    }
                };
                let config_dir = cfg_path
                    .parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                return setup::wizard::run(&config_dir, cli).await;
            }
            setup::Command::Service { action } => {
                setup::service::handle(action)?;
                return Ok(());
            }
        }
    }

    // If we reach here, it's a normal bot start — resolve config path
    let config_path = if let Ok(path) = std::env::var("RUSTFOX_CONFIG_PATH") {
        PathBuf::from(path)
    } else {
        let cwd = PathBuf::from("config.toml");
        if cwd.exists() {
            cwd
        } else {
            let env_home = std::env::var("RUSTFOX_HOME").ok();
            if let Some(home) =
                rustfox::home::default_home(env_home.as_deref(), dirs::home_dir().as_deref())
            {
                let candidate = home.join("config.toml");
                if candidate.exists() {
                    candidate
                } else {
                    cwd
                }
            } else {
                cwd
            }
        }
    };

    info!("Loading configuration from: {}", config_path.display());
    let config = Config::load(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    info!("Configuration loaded successfully");
    info!("  Model: {}", config.openrouter.model);
    info!("  Sandbox: {}", config.sandbox.allowed_directory.display());
    if let Some(home) = &config.resolved_home {
        info!("  Home: {}", home.display());
    }
    info!("  Allowed users: {:?}", config.telegram.allowed_user_ids);
    info!("  MCP servers: {}", config.mcp_servers.len());
    let langsmith = std::sync::Arc::new(rustfox::langsmith::LangSmithClient::new(
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
            .map(|cfg| rustfox::memory::embeddings::EmbeddingConfig {
                api_key: cfg.api_key.clone(),
                base_url: cfg.base_url.clone(),
                model: cfg.model.clone(),
                dimensions: cfg.dimensions,
            });

    // Initialize memory store (SQLite + vector embeddings)
    let memory = MemoryStore::open(&config.memory.database_path, embedding_config)
        .context("Failed to initialize memory store")?;
    info!("  Database: {}", config.memory.database_path.display());

    // Refresh any expiring OAuth tokens before connecting to MCP servers
    let http_client = reqwest::Client::new();
    let mut mcp_server_configs = config.mcp_servers.clone();
    let refreshed =
        rustfox::mcp::refresh_expiring_tokens(&mut mcp_server_configs, &config_path, &http_client)
            .await;
    if refreshed > 0 {
        info!("  Refreshed {refreshed} expiring MCP OAuth token(s) at startup");
    }

    // Initialize MCP connections (using possibly-refreshed configs)
    let mut mcp_manager = McpManager::new();
    mcp_manager.connect_all(&mcp_server_configs).await;

    // Seed bundled skills/agents into the instance home on first run.
    // Seed bundled skills/agents into the instance home on first run.
    if let Err(e) = rustfox::skills::seed::seed_dir_if_empty(
        &config.skills.bundled_directory,
        &config.skills.directory,
    )
    .await
    {
        warn!("Skill seeding failed: {e}");
    }
    if let Err(e) = rustfox::skills::seed::seed_dir_if_empty(
        &config.agents.bundled_directory,
        &config.agents.directory,
    )
    .await
    {
        warn!("Agent seeding failed: {e}");
    }
    // Write the home-side lock so /update-skills can diff later (only when seeded
    // into the home and a lock does not already exist).
    if let Some(home) = &config.resolved_home {
        let seed_lock = |lock_name: &str, dir: &std::path::Path| {
            let lock_path = home.join(lock_name);
            if !lock_path.exists() {
                let lock = rustfox::skills::update::SkillLock {
                    version: 1,
                    skills: rustfox::skills::seed::lock_map_for(dir),
                };
                if let Ok(json) = serde_json::to_string_pretty(&lock) {
                    let _ = std::fs::write(&lock_path, json);
                }
            }
        };
        seed_lock("skills-lock.json", &config.skills.directory);
        seed_lock("agents-lock.json", &config.agents.directory);
    }

    // Load skills from instance and bundled directories (instance shadows bundled)
    let mut skills = load_skills_from_dir(
        &config.skills.directory,
        SkillSource::Instance,
        config.skills.directory.clone(),
    )
    .await?;
    let bundled_skills = load_skills_from_dir(
        &config.skills.bundled_directory,
        SkillSource::Bundled,
        config.skills.bundled_directory.clone(),
    )
    .await?;
    for skill in bundled_skills.list() {
        skills.register(
            skill.clone(),
            SkillSource::Bundled,
            config.skills.bundled_directory.clone(),
        );
    }
    info!("  Skills: {}", skills.len());

    // Load agents from instance and bundled directories (instance shadows bundled)
    let mut agents = load_skills_from_dir(
        &config.agents.directory,
        SkillSource::Instance,
        config.agents.directory.clone(),
    )
    .await?;
    let bundled_agents = load_skills_from_dir(
        &config.agents.bundled_directory,
        SkillSource::Bundled,
        config.agents.bundled_directory.clone(),
    )
    .await?;
    for agent in bundled_agents.list() {
        agents.register(
            agent.clone(),
            SkillSource::Bundled,
            config.agents.bundled_directory.clone(),
        );
    }
    info!("  Agents: {}", agents.len());

    // Create ScheduledTaskStore sharing the existing SQLite connection
    let task_store = rustfox::scheduler::reminders::ScheduledTaskStore::new(memory.connection());

    // Create scheduler as Arc so Agent can hold it and closures can reference it
    let scheduler = Arc::new(Scheduler::new().await?);

    // Create Bot early so it can be passed to Agent
    let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));

    // Channel for dispatching scheduled job work from fire closures to background runner
    let (job_tx, mut job_rx) =
        tokio::sync::mpsc::unbounded_channel::<rustfox::agent::ScheduledJobRequest>();

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
            for chunk in rustfox::agent::split_response_chunks(&response, 4000) {
                if chunk.is_empty() {
                    continue;
                }
                if let Err(e) = req.bot.send_message(chat, &chunk).await {
                    tracing::error!("Failed to send scheduled response: {}", e);
                }
            }
        }
    });

    // Spawn background OAuth token refresh task: checks every 30 minutes.
    // `cfgs` is kept across ticks so that updated token_expires_at values
    // are remembered and a freshly-rotated refresh token isn't re-used.
    {
        let mut cfgs = mcp_server_configs.clone();
        let refresh_config_path = config_path.clone();
        let refresh_http_client = http_client.clone();
        tokio::spawn(async move {
            // 30-minute interval — tokens expiring within 5 min are always caught
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30 * 60));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                let refreshed = rustfox::mcp::refresh_expiring_tokens(
                    &mut cfgs,
                    &refresh_config_path,
                    &refresh_http_client,
                )
                .await;
                if refreshed > 0 {
                    tracing::info!(
                        "Background token refresh: updated {refreshed} MCP OAuth token(s)"
                    );
                }
            }
        });
    }

    // Register built-in background tasks and start scheduler
    register_builtin_tasks(
        &scheduler,
        memory.clone(),
        rustfox::llm::LlmClient::new(config.openrouter.clone()),
        config.memory.summarize_cron.clone(),
        config.memory.summarize_threshold,
        config.learning.user_model_cron.clone(),
        config.learning.user_model_path.clone(),
    )
    .await?;
    scheduler.start().await?;
    info!("  Scheduler: active");
    agent.restore_scheduled_tasks().await;
    info!("  Scheduled tasks: restored from DB");

    // Construct Supervisor with a populated backend Registry so resume /
    // future routing paths can resolve backends rather than failing with
    // "backend not found". Held alive in main's scope so the binding isn't
    // dead-code-eliminated.
    let mut sup_registry = rustfox::supervisor::backend::Registry::new();
    sup_registry.register(std::sync::Arc::new(
        rustfox::supervisor::backend::reasoning::ReasoningBackend::from_agent(
            Arc::clone(&agent),
            "supervisor".to_string(),
            "supervisor".to_string(),
        ),
    ));
    sup_registry.register(std::sync::Arc::new(
        rustfox::supervisor::backend::shell::ShellBackend::new(
            config.sandbox.allowed_directory.clone(),
        ),
    ));

    let _supervisor = Arc::new(rustfox::supervisor::Supervisor::new(
        config.supervisor.artifacts_dir.clone(),
        memory.connection(),
        sup_registry,
        config.supervisor.risk.clone(),
    ));
    match _supervisor.resumable_task_ids().await {
        Ok(ids) if !ids.is_empty() => info!(
            "  Supervisor: {} resumable task(s) found at startup",
            ids.len()
        ),
        Ok(_) => info!("  Supervisor: ready (registry has reasoning + shell backends)"),
        Err(e) => warn!("  Supervisor: failed to enumerate resumable tasks: {e}"),
    }

    // Run the Telegram platform with signal-driven graceful shutdown
    info!("Bot is starting...");

    let dispatch_agent = Arc::clone(&agent);
    let dispatch_user_ids = config.telegram.allowed_user_ids.clone();
    let dispatch_bot = Arc::clone(&bot);

    let mut dispatch_handle = tokio::spawn(async move {
        platform::telegram::run(dispatch_agent, dispatch_user_ids, dispatch_bot).await
    });

    // Set up signal handlers (SIGINT via ctrl_c for portability, SIGTERM via unix signal)
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to create SIGTERM handler");

    #[cfg(unix)]
    let terminate = sigterm.recv();
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("SIGINT received, shutting down...");
        }
        _ = terminate => {
            info!("SIGTERM received, shutting down...");
        }
        result = &mut dispatch_handle => {
            result??;
            return Ok(());
        }
    };

    // Send shutdown notification
    platform::telegram::notify_shutdown(&bot, &config.telegram.allowed_user_ids).await;

    // Brief grace period for message delivery
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    info!("Shutdown complete.");

    Ok(())
}
