use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use rustfox::agent::Agent;
use rustfox::config::Config;
use rustfox::mcp::McpManager;
use rustfox::memory::MemoryStore;
use rustfox::platform;
use rustfox::provider;
use rustfox::scheduler::tasks::register_builtin_tasks;
use rustfox::scheduler::Scheduler;
use rustfox::setup;
use rustfox::skills::loader::load_skills_from_dir;

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
                let cfg_path = rustfox::home::resolve_config_path(
                    std::env::var("RUSTFOX_CONFIG_PATH").ok().as_deref(),
                    &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    dirs::home_dir().as_deref(),
                );
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
    let config_path = rustfox::home::resolve_config_path(
        std::env::var("RUSTFOX_CONFIG_PATH").ok().as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        dirs::home_dir().as_deref(),
    );

    info!("Loading configuration from: {}", config_path.display());
    let config = Config::load(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    // Build provider registry from config
    let (provider_sections, default_provider, fallback_chain) = config.build_providers();
    let registry = Arc::new(
        provider::build_registry(
            &provider_sections,
            &default_provider,
            config.parse_retry_limit(),
        )
        .context("Failed to build LLM provider registry")?,
    );
    info!(
        "  Providers: {} (default: {}, fallback: {} model(s))",
        registry.provider_count(),
        registry.default_provider_name(),
        fallback_chain.len()
    );

    // Spawn background task to warm context_window_cache for all providers
    {
        let registry_clone = Arc::clone(&registry);
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            for name in registry_clone.provider_names() {
                if let Some(provider) = registry_clone.get_provider(&name) {
                    let model = provider.default_model();
                    if let Some(ctx) = provider.fetch_context_window(&client, model).await {
                        let mut cache = provider.config().context_window_cache.write().await;
                        *cache = Some(ctx);
                        tracing::info!(
                            "Context window cache: {} / {} = {} tokens",
                            name,
                            model,
                            ctx
                        );
                    }
                }
            }
        });
    }

    info!("Configuration loaded successfully");
    let default_provider_obj = registry
        .get_provider(registry.default_provider_name())
        .expect("default provider must exist");
    info!(
        "  Model: {}/{}",
        registry.default_provider_name(),
        default_provider_obj.default_model()
    );
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
    let memory = MemoryStore::open(
        &config.memory.database_path,
        embedding_config,
        config.memory.clone(),
    )
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

    // Seed bundled skills/agents from embedded data into the home directory.
    if let Err(e) = rustfox::skills::embed::seed_skills(&config.skills.directory).await {
        warn!("Skill seeding failed: {e}");
    }
    if let Err(e) = rustfox::skills::embed::seed_agents(&config.agents.directory).await {
        warn!("Agent seeding failed: {e}");
    }
    // Write a home-side lock recording content hashes for future diff/audit.
    if let Some(home) = &config.resolved_home {
        let _ =
            rustfox::skills::seed::write_lock("skills-lock.json", &config.skills.directory, home);
        let _ =
            rustfox::skills::seed::write_lock("agents-lock.json", &config.agents.directory, home);
    }

    // Load skills from the instance directory.
    let skills =
        load_skills_from_dir(&config.skills.directory, config.skills.directory.clone()).await?;
    info!("  Skills: {}", skills.len());

    // Load agents from the instance directory.
    let agents =
        load_skills_from_dir(&config.agents.directory, config.agents.directory.clone()).await?;
    info!("  Agents: {}", agents.len());

    // Create ScheduledTaskStore sharing the existing SQLite connection
    let task_store = rustfox::scheduler::reminders::ScheduledTaskStore::new(memory.connection());

    // Create scheduler as Arc so Agent can hold it and closures can reference it
    let scheduler = Arc::new(Scheduler::new().await?);

    // Create Bot early so it can be passed to Agent
    let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));

    rustfox::platform::telegram::init_bot_token(config.telegram.bot_token.clone());

    // Channel for dispatching scheduled job work from fire closures to background runner
    let (job_tx, mut job_rx) =
        tokio::sync::mpsc::unbounded_channel::<rustfox::agent::ScheduledJobRequest>();

    let cancel_registry = std::sync::Arc::new(rustfox::cancel_registry::CancelRegistry::new());
    let sender: Arc<dyn rustfox::platform::sender::PlatformSender> = Arc::new(
        rustfox::platform::telegram::TelegramAdapter::new((*bot).clone()),
    );
    let skills_rw = Arc::new(tokio::sync::RwLock::new(skills.clone()));
    let agents_rw = Arc::new(tokio::sync::RwLock::new(agents.clone()));
    let restart_pending = Arc::new(AtomicBool::new(false));
    let soul_updated = Arc::new(AtomicBool::new(false));

    let mut tool_registry = rustfox::tool_registry::ToolRegistry::new();
    tool_registry.register(Box::new(rustfox::builtin_tools::BuiltinTools::new(
        config.skills.directory.clone(),
        skills_rw.clone(),
        restart_pending.clone(),
        soul_updated.clone(),
    )));
    tool_registry.register(Box::new(rustfox::memory_tools::MemoryTools::new(
        memory.clone(),
    )));
    tool_registry.register(Box::new(rustfox::scheduling_tools::SchedulingTools::new(
        task_store.clone(),
        Arc::clone(&scheduler),
        job_tx.clone(),
        Arc::clone(&bot),
    )));
    tool_registry.register(Box::new(rustfox::skill_tools::SkillTools::new(
        config.skills.directory.clone(),
        config.agents.directory.clone(),
        skills_rw.clone(),
        agents_rw.clone(),
    )));
    tool_registry.register(Box::new(rustfox::command_tool::CommandTool::new(
        config.sandbox.allowed_directory.clone(),
        cancel_registry.clone(),
        sender.clone(),
    )));

    // Arc::new_cyclic so Agent can store Weak<Self> for job closure captures (breaks Arc cycle)
    let agent = Arc::new_cyclic(|weak| {
        Agent::new(
            config.clone(),
            registry.clone(),
            mcp_manager,
            memory.clone(),
            skills,
            agents,
            task_store.clone(),
            Arc::clone(&scheduler),
            weak.clone(),
            job_tx,
            Arc::clone(&langsmith),
            config_path.clone(),
            cancel_registry.clone(),
            tool_registry,
            sender.clone(),
            restart_pending.clone(),
            soul_updated.clone(),
        )
    });

    // Spawn background runner: receives ScheduledJobRequest, calls process_message, persists result, sends reply
    let agent_for_runner = Arc::clone(&agent);
    tokio::spawn(async move {
        while let Some(req) = job_rx.recv().await {
            let agent = Arc::clone(&agent_for_runner);

            // Persist run record BEFORE processing (capture fire time)
            let run_id = uuid::Uuid::new_v4().to_string();
            let run_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
            if let Err(e) = req
                .task_store
                .insert_run(&run_id, &req.task_id, &run_at, None, None, "running")
                .await
            {
                tracing::warn!("Failed to persist scheduled task run record: {}", e);
            }

            let response = match agent.process_message(&req.incoming, None, None).await {
                Ok(r) => {
                    if let Err(e) = req
                        .task_store
                        .update_run(&run_id, Some(&r), None, "completed")
                        .await
                    {
                        tracing::warn!("Failed to update scheduled task run record: {}", e);
                    }
                    r
                }
                Err(e) => {
                    tracing::error!("Scheduled task {} failed: {}", req.task_id, e);
                    let err_str = format!("{:#}", e);
                    if let Err(e) = req
                        .task_store
                        .update_run(&run_id, None, Some(&err_str), "failed")
                        .await
                    {
                        tracing::warn!("Failed to update failed scheduled task run record: {}", e);
                    }
                    if !req.is_recurring {
                        let _ = req.task_store.set_status(&req.task_id, "failed").await;
                    }
                    // Send error to user via rich message
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
                    let error_msg = format!("**Scheduled task failed:** {}", e);
                    let _ = rustfox::platform::telegram::send_markdown_message(
                        &req.bot,
                        chat,
                        &error_msg,
                        rustfox::platform::telegram::MessageFormat::Auto,
                    )
                    .await;
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
            if let Err(e) = rustfox::platform::telegram::send_markdown_message(
                &req.bot,
                chat,
                &response,
                rustfox::platform::telegram::MessageFormat::Auto,
            )
            .await
            {
                tracing::error!("Failed to send scheduled response: {}", e);
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
    let home = config
        .resolved_home
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    register_builtin_tasks(
        &scheduler,
        memory.clone(),
        rustfox::llm::LlmClient::new(registry.clone()),
        config.memory.summarize_cron.clone(),
        config.memory.summarize_threshold,
        config.learning.user_model_cron.clone(),
        home,
    )
    .await?;
    scheduler.start().await?;
    info!("  Scheduler: active");
    agent.restore_scheduled_tasks(Arc::clone(&bot)).await;
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
