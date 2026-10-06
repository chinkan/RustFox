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
use rustfox::tool_registry::ToolUiMode;

async fn send_schedule_reply(
    bot: &Option<Arc<teloxide::Bot>>,
    chat_id: teloxide::types::ChatId,
    text: &str,
    format: rustfox::platform::telegram::MessageFormat,
) -> anyhow::Result<()> {
    let Some(bot) = bot else {
        tracing::info!("schedule reply skipped; this telegram bot does not own the run");
        return Ok(());
    };
    rustfox::platform::telegram::send_markdown_message(bot, chat_id, text, format)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

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
    let mut config = Config::load(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    // Slice 3: host secret bridge (store + pending) shared by MCP / tools / portal.
    let (secret_bridge, secret_backend) = {
        let home = config.resolved_home().cloned().unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".rustfox"))
                .unwrap_or_else(|| std::path::PathBuf::from(".rustfox"))
        });
        match rustfox::secret_store::open(&home) {
            Ok((store, backend)) => {
                info!("  Secret store: {:?}", backend);
                let pending =
                    std::sync::Arc::new(rustfox::secret_store::PendingSecretRegistry::default());
                let bridge = std::sync::Arc::new(rustfox::secret_store::SecretBridge::new(
                    std::sync::Arc::from(store),
                    pending,
                ));
                let portal_base = format!("http://127.0.0.1:{}/", config.portal.port);
                bridge.set_portal_base(portal_base);
                (bridge, Some(backend))
            }
            Err(e) => {
                warn!("  Secret store unavailable ({e}); using in-memory FakeSecretStore");
                let bridge = std::sync::Arc::new(rustfox::secret_store::SecretBridge::new(
                    std::sync::Arc::new(rustfox::secret_store::FakeSecretStore::new()),
                    std::sync::Arc::new(rustfox::secret_store::PendingSecretRegistry::default()),
                ));
                (bridge, None)
            }
        }
    };
    // P0: one-time migrate plaintext [[bots]].bot_token → SecretStore + scrub config.
    // Only when a durable backend is available — never scrub into an in-memory Fake.
    if secret_backend.is_some() {
        match rustfox::secret_store::migrate_plaintext_bot_tokens(
            &config_path,
            secret_bridge.store().as_ref(),
        ) {
            Ok(n) if n > 0 => {
                info!("  Migrated {n} plaintext credential(s) into SecretStore (config scrubbed)");
                config =
                    Config::load(&config_path).context("reload config after bot-token migrate")?;
            }
            Ok(_) => {}
            Err(e) => {
                warn!("  Bot-token SecretStore migrate skipped: {e:#}");
            }
        }
    } else {
        warn!("  Bot-token migrate skipped (no durable SecretStore backend)");
    }

    // ADR 0016: resolve `secret:openrouter.api_key` before providers are built.
    rustfox::secret_store::resolve_openrouter_api_key(&mut config, &secret_bridge)?;

    // Build provider registry from config
    let (provider_sections, default_provider, fallback_chain) = config.build_providers();
    let registry = Arc::new(
        provider::build_registry(
            &provider_sections,
            &default_provider,
            config.parse_retry_limit(),
            config.rate_limit_retry_limit(),
        )
        .context("Failed to build LLM provider registry")?,
    );
    info!(
        "  Providers: {} (default: {}, fallback: {} model(s))",
        registry.provider_count(),
        registry.default_provider_name(),
        fallback_chain.len()
    );
    // Fail loudly at boot for typos that would otherwise silently no-op
    // at call time (ADR-0012 safety: unknown provider prefix → skipped).
    for entry in &fallback_chain {
        if let Some((prefix, _)) = entry.split_once('/') {
            if registry.get_provider(prefix).is_none() {
                tracing::warn!(
                    "[fallback] chain entry '{entry}' names unknown provider '{prefix}' — it will be skipped at call time"
                );
            }
        }
    }

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
    info!("  Bots: {}", config.bots.len());
    for b in &config.bots {
        info!("    bot id={} allowed_users={:?}", b.id, b.allowed_user_ids);
    }
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

    // MCP manager ready; connect_all runs after Telegram notify callback is wired
    // so missing `secret:NAME` env refs can notify allowlisted users (Slice 3).
    let mcp_manager = McpManager::new().with_secret_bridge(std::sync::Arc::clone(&secret_bridge));

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

    // Dead-letter re-run queue (ADR-0013) shares the same connection.
    let rerun_queue = rustfox::scheduler::reruns::RerunQueue::new(memory.connection());
    match rerun_queue.reset_inflight_on_boot().await {
        Ok(n) if n > 0 => info!("  Rerun queue: reset {n} in-flight row(s) after restart"),
        Ok(_) => {}
        Err(e) => warn!("  Rerun queue boot reset failed: {e:#}"),
    }

    // Create scheduler as Arc so Agent can hold it and closures can reference it
    let scheduler = Arc::new(Scheduler::new().await?);

    // One teloxide::Bot per [[bots]] entry. Schedule runs and approval replies
    // use the bot id stored on the row, never a different bot's token.
    // Conversation history and cancel keys stay isolated per bot_id (§7.3).
    let telegram_api_base =
        rustfox::config::telegram_api_base_url(config.telegram.api_base_url.as_deref());
    let telegram_api_url =
        reqwest::Url::parse(&format!("{telegram_api_base}/")).unwrap_or_else(|_| {
            reqwest::Url::parse(&format!(
                "{}/",
                rustfox::utils::rich_sender::DEFAULT_API_BASE
            ))
            .expect("default Telegram API base is valid")
        });
    if config.telegram.api_base_url.is_some() {
        info!("  Telegram Bot API base: {telegram_api_base} (from [telegram].api_base_url)");
    }

    let store_for_bots = secret_bridge.store();
    let bot_runtimes: Vec<(String, Arc<teloxide::Bot>, Vec<u64>)> = config
        .bots
        .iter()
        .map(|b| {
            let token =
                rustfox::secret_store::resolve_bot_token(store_for_bots.as_ref(), &b.bot_token)
                    .with_context(|| format!("resolve bot token for bots[].id={}", b.id))?;
            // Seed redaction set so accidental echo in tool output is scrubbed.
            if let Some(name) = rustfox::secret_store::parse_secret_ref(&b.bot_token) {
                let _ = secret_bridge.get(name);
            }
            let bot = teloxide::Bot::new(&token).set_api_url(telegram_api_url.clone());
            Ok::<_, anyhow::Error>((b.id.clone(), Arc::new(bot), b.allowed_user_ids.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    let shim = Config::shim_bot(&config.bots);
    let bot = bot_runtimes
        .iter()
        .find(|(id, _, _)| id == &shim.id)
        .map(|(_, b, _)| Arc::clone(b))
        .expect("shim bot must exist in bot_runtimes");
    let telegram_bots = Arc::new(std::sync::RwLock::new(
        bot_runtimes
            .iter()
            .map(|(id, b, _)| (id.clone(), Arc::clone(b)))
            .collect::<std::collections::HashMap<_, _>>(),
    ));

    // Slice 3: missing-secret Telegram notify to allowlisted users (shim bot).
    {
        let notify_bot = (*bot).clone();
        let allow = shim.allowed_user_ids.clone();
        secret_bridge.set_notify(std::sync::Arc::new(move |name, claim_url| {
            let bot = notify_bot.clone();
            let allow = allow.clone();
            let name = name.to_string();
            let claim_url = claim_url.to_string();
            tokio::spawn(async move {
                rustfox::platform::telegram::notify_secret_request(&bot, &allow, &name, &claim_url)
                    .await;
            });
        }));
    }

    // Initialize MCP connections (after notify is live for missing-secret UX).
    mcp_manager.connect_all(&mcp_server_configs).await;

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
    // Late-bound seam so the scheduling tools can arm/disarm through the
    // Agent's `arm_task`/`disarm_task` (persisting the live job id — issue
    // #109). The Agent is constructed below, so the holder is filled then.
    let scheduling_ops = Arc::new(rustfox::scheduler::schedule::OnceScheduling::new());
    tool_registry.register(Box::new(rustfox::scheduling_tools::SchedulingTools::new(
        task_store.clone(),
        Arc::clone(&scheduler),
        scheduling_ops.clone() as Arc<dyn rustfox::scheduler::schedule::SchedulingOps>,
        job_tx.clone(),
        Arc::clone(&bot),
        Arc::clone(&telegram_bots),
        rerun_queue.clone(),
    )));
    tool_registry.register(Box::new(rustfox::skill_tools::SkillTools::new(
        config.skills.directory.clone(),
        config.agents.directory.clone(),
        skills_rw.clone(),
        agents_rw.clone(),
    )));
    tool_registry.register(Box::new(
        rustfox::command_tool::CommandTool::new(
            config.sandbox.allowed_directory.clone(),
            cancel_registry.clone(),
        )
        .with_secrets(
            std::sync::Arc::clone(&secret_bridge),
            config.sandbox.secret_env.clone(),
        ),
    ));

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
            job_tx.clone(),
            Arc::clone(&langsmith),
            config_path.clone(),
            cancel_registry.clone(),
            tool_registry,
            sender.clone(),
            Arc::clone(&bot),
            Arc::clone(&telegram_bots),
            restart_pending.clone(),
            soul_updated.clone(),
        )
    });

    // The agent now exists: give the scheduling tools the real arm/disarm ops.
    scheduling_ops.set(Arc::new(
        rustfox::scheduler::schedule::AgentOpsScheduling::new(Arc::clone(&agent)),
    ));

    // Spawn background runner: receives ScheduledJobRequest, calls process_message, persists result, sends reply
    let agent_for_runner = Arc::clone(&agent);
    let queue_for_runner = rerun_queue.clone();
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

            let processed = agent
                .process_message_outcome(&req.incoming, None, None, ToolUiMode::Minimal)
                .await;

            // Prompt + result, including failure / cancel / max-iterations,
            // with the schedule id. Portal rows also land in the owning bot's
            // Telegram conversation. No approval gate: the run already finished.
            if let Ok(Some(task)) = req.task_store.get_by_id(&req.task_id).await {
                let result_body = match &processed {
                    Ok(outcome) => outcome.text.clone(),
                    Err(err) => format!("Scheduled task failed: {err:#}"),
                };
                if let Err(e) = rustfox::scheduler::history::write_schedule_segment(
                    &agent.memory,
                    &task,
                    &agent.config.bots,
                    &result_body,
                )
                .await
                {
                    tracing::warn!(
                        "Failed to write schedule {} conversation segment: {e:#}",
                        req.task_id
                    );
                }
            }

            let response = match processed {
                Ok(outcome) => {
                    // Read the stop reason before moving the text out.
                    let hit_iteration_cap = outcome.is_max_iterations();
                    let r = outcome.text;
                    // A budget-exhausted run is NOT a success: the loop stopped
                    // mid-task, possibly after side effects. Record it failed
                    // and hand it to the human gate (ADR-0013) instead of
                    // reporting a clean completion.
                    if hit_iteration_cap {
                        let reason = format!(
                            "Reached max iterations ({}) without a final response",
                            agent.config.max_iterations()
                        );
                        tracing::warn!(
                            "Scheduled task {} exhausted its iteration budget: {}",
                            req.task_id,
                            reason
                        );
                        if let Err(e) = req
                            .task_store
                            .update_run(&run_id, Some(&r), Some(&reason), "failed")
                            .await
                        {
                            tracing::warn!("Failed to update run record: {}", e);
                        }
                        match &req.rerun_id {
                            // A re-fire that exhausted its budget → ask, never auto again.
                            Some(rid) => {
                                if let Err(e) =
                                    queue_for_runner.mark_awaiting_user(rid, &reason).await
                                {
                                    tracing::warn!(
                                        "Failed to mark rerun {rid} awaiting_user: {e:#}"
                                    );
                                }
                                if let Ok(cv) = req.incoming.chat_id.parse::<i64>() {
                                    let ask = format!(
                                        "**Scheduled task needs your call** — the re-run ran out of iteration budget.

{}\n\nReply with:\n- retry → try once more automatically\n- cancel → give up\n\n(queue id: `{}`)",
                                        reason,
                                        rid
                                    );
                                    let _ = send_schedule_reply(
                                        &req.owning_telegram_bot,
                                        teloxide::types::ChatId(cv),
                                        &ask,
                                        rustfox::platform::telegram::MessageFormat::Auto,
                                    )
                                    .await;
                                }
                            }
                            // First strike: record a human-gated row (never auto-fired).
                            None => {
                                match queue_for_runner
                                    .enqueue_manual(&req.task_id, &run_id, &reason)
                                    .await
                                {
                                    Ok(qid) => {
                                        tracing::info!(
                                            "Max-iterations on task {} → human-gated queue row {qid}",
                                            req.task_id
                                        );
                                        if let Ok(cv) = req.incoming.chat_id.parse::<i64>() {
                                            let note = format!(
                                                "**Scheduled task stalled:** it hit the iteration cap ({}) without finishing, so I did NOT auto-retry (it may have half-done work).\n\n{}\n\nReply with:\n- retry → try once more automatically\n- cancel → give up\n\n(queue id: `{}`)",
                                                agent.config.max_iterations(),
                                                reason,
                                                qid
                                            );
                                            let _ = send_schedule_reply(
                                                &req.owning_telegram_bot,
                                                teloxide::types::ChatId(cv),
                                                &note,
                                                rustfox::platform::telegram::MessageFormat::Auto,
                                            )
                                            .await;
                                        }
                                    }
                                    Err(qe) => {
                                        tracing::warn!(
                                            "Failed to record stalled run for {}: {qe:#}",
                                            req.task_id
                                        );
                                    }
                                }
                            }
                        }
                        // Deliver whatever partial text the loop produced, then stop.
                        if let Ok(cv) = req.incoming.chat_id.parse::<i64>() {
                            let _ = send_schedule_reply(
                                &req.owning_telegram_bot,
                                teloxide::types::ChatId(cv),
                                &r,
                                rustfox::platform::telegram::MessageFormat::Auto,
                            )
                            .await;
                        }
                        continue;
                    }
                    if let Err(e) = req
                        .task_store
                        .update_run(&run_id, Some(&r), None, "completed")
                        .await
                    {
                        tracing::warn!("Failed to update scheduled task run record: {}", e);
                    }
                    // Issue #109 (related): a one-shot that fired cleanly must
                    // not stay `active` forever — retire it (idempotent, no-op
                    // for recurring).
                    let trig = if req.is_recurring {
                        "recurring"
                    } else {
                        "one_shot"
                    };
                    if let Err(e) = req
                        .task_store
                        .retire_completed_one_shot(&req.task_id, trig)
                        .await
                    {
                        tracing::warn!(
                            "Failed to retire completed one-shot {}: {e:#}",
                            req.task_id
                        );
                    }
                    // ADR-0013 / P0 #2: any successful scheduled run (cron or
                    // re-fire) must clear leftover live queue rows for this
                    // task — otherwise the next watchdog tick can stale-replay
                    // a queued/awaiting_user sibling. Mark the fired row done
                    // first so it shows as `done` rather than `superseded`.
                    if let Some(rid) = &req.rerun_id {
                        if let Err(e) = queue_for_runner.mark_done(rid).await {
                            tracing::warn!("Failed to mark rerun {rid} done: {e:#}");
                        } else {
                            tracing::info!("Rerun {rid} succeeded — queue row closed");
                        }
                    }
                    if let Err(e) = queue_for_runner.supersede_live_for_task(&req.task_id).await {
                        tracing::warn!(
                            "Failed to supersede live reruns for {}: {e:#}",
                            req.task_id
                        );
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
                    // --- ADR-0013 two-strike dead-letter handling ---
                    let chat_id_opt: Option<i64> = req.incoming.chat_id.parse().ok();
                    match &req.rerun_id {
                        // Second strike: the re-fire itself died → ask Kan, never auto again.
                        Some(rid) => {
                            if let Err(e) = queue_for_runner.mark_awaiting_user(rid, &err_str).await
                            {
                                tracing::warn!("Failed to mark rerun {rid} awaiting_user: {e:#}");
                            }
                            if let Some(cv) = chat_id_opt {
                                let ask = format!(
                                    "**Scheduled task failed twice** (re-run {}), holding for your decision.\n\nLast error: {}\n\nReply with:\n- retry → try once more automatically\n- cancel → give up\n\n(queue id: `{}`)",
                                    &rid[..8.min(rid.len())],
                                    err_str,
                                    rid
                                );
                                let _ = send_schedule_reply(
                                    &req.owning_telegram_bot,
                                    teloxide::types::ChatId(cv),
                                    &ask,
                                    rustfox::platform::telegram::MessageFormat::Auto,
                                )
                                .await;
                            }
                            continue;
                        }
                        // First strike: transient LLM death → queue for one auto re-fire.
                        // (is_transient_llm_error downcasts the typed LlmHttpError —
                        // 429/5xx only; 400/401 and non-LLM errors fall through to
                        // the plain failure notice below, unchanged.)
                        None if rustfox::provider::is_transient_llm_error(&e) => {
                            match queue_for_runner
                                .enqueue(&req.task_id, &run_id, &err_str)
                                .await
                            {
                                Ok(qid) => {
                                    tracing::info!(
                                        "Transient failure on task {} → re-fire queued ({qid}), checking hourly",
                                        req.task_id
                                    );
                                    if let Some(cv) = chat_id_opt {
                                        let note = format!(
                                            "**Scheduled task hiccup:** transient provider failure (429/5xx), auto re-fire queued — I'll retry within the hour and only nag you if it fails again.\n\n(queue id: `{qid}`)"
                                        );
                                        let _ = send_schedule_reply(
                                            &req.owning_telegram_bot,
                                            teloxide::types::ChatId(cv),
                                            &note,
                                            rustfox::platform::telegram::MessageFormat::Auto,
                                        )
                                        .await;
                                    }
                                    continue;
                                }
                                Err(qe) => {
                                    tracing::warn!(
                                        "Failed to enqueue rerun for {}: {qe:#}",
                                        req.task_id
                                    );
                                }
                            }
                        }
                        None => {}
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
                    let _ = send_schedule_reply(
                        &req.owning_telegram_bot,
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
            if let Err(e) = send_schedule_reply(
                &req.owning_telegram_bot,
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

    // Spawn dead-letter watchdog (ADR-0013): hourly sweep of pending_reruns.
    // System-level interval job — deliberately NOT a user scheduled_task row
    // (can't be edited/deleted by accident, doesn't pollute the task list).
    {
        let queue = rerun_queue.clone();
        let store = task_store.clone();
        let tx = job_tx.clone();
        let bots_for_watch = Arc::clone(&telegram_bots);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                // 1. Due re-fires.
                let due = match queue.due().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("Rerun watchdog due-scan failed: {e:#}");
                        Vec::new()
                    }
                };
                for row in due {
                    let task = match store.get_by_id(&row.task_id).await {
                        Ok(Some(t)) => t,
                        Ok(None) => {
                            let _ = queue.mark_abandoned(&row.id).await;
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!("Rerun watchdog task lookup failed: {e:#}");
                            continue;
                        }
                    };
                    // Only fire tasks still live & un-soft-deleted (ADR-0013).
                    if task.status != "active" || task.deleted_at.is_some() {
                        tracing::info!(
                            "Rerun {} skipped: task {} is {} — abandoning",
                            row.id,
                            row.task_id,
                            task.status
                        );
                        let _ = queue.mark_abandoned(&row.id).await;
                        continue;
                    }
                    // P0 #1: claim *before* send so a crash mid-dispatch cannot
                    // leave attempts=0 (auto re-fire) and so due()'s attempts=0
                    // filter excludes this row while it is in-flight.
                    if let Err(e) = queue.mark_dispatched(&row.id).await {
                        tracing::warn!("Rerun bump-attempts failed for {}: {e:#}", row.id);
                        continue;
                    }
                    let owning = {
                        let guard = bots_for_watch.read().unwrap_or_else(|err| err.into_inner());
                        rustfox::agent::Agent::build_rerun_request(
                            &tx,
                            &guard,
                            store.clone(),
                            &task,
                            &row.id,
                        )
                    };
                    if let Err(e) = owning {
                        tracing::warn!("Rerun dispatch failed for {}: {e:#}", row.id);
                    }
                }
                // 2. Age out unanswered awaiting_user rows (one-line DM each).
                match queue.expire_awaiting().await {
                    Ok(ids) => {
                        for id in ids {
                            tracing::info!("Rerun {id} expired unanswered → abandoned");
                            let delivery = match queue.get(&id).await {
                                Ok(Some(row)) => match store.get_by_id(&row.task_id).await {
                                    Ok(Some(task)) => {
                                        let guard = bots_for_watch
                                            .read()
                                            .unwrap_or_else(|err| err.into_inner());
                                        let owner = rustfox::scheduler::reminders::owning_bot(
                                            &task.bot_id,
                                            &guard,
                                        )
                                        .cloned();
                                        (task.chat_id.parse::<i64>().ok(), owner)
                                    }
                                    _ => (None, None),
                                },
                                _ => (None, None),
                            };
                            if let (Some(cv), owner) = delivery {
                                let note = format!(
                                    "Scheduled re-run gate expired unanswered — abandoned (queue id: `{id}`)."
                                );
                                let _ = send_schedule_reply(
                                    &owner,
                                    teloxide::types::ChatId(cv),
                                    &note,
                                    rustfox::platform::telegram::MessageFormat::Auto,
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => tracing::warn!("Rerun expiry failed: {e:#}"),
                }
                // 3. Purge old terminal rows.
                if let Err(e) = queue.purge_terminal().await {
                    tracing::warn!("Rerun purge failed: {e:#}");
                }
            }
        });
    }

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
        rustfox::llm::LlmClient::new(registry.clone()).with_fallback_chain(config.fallback_chain()),
        config.memory.summarize_cron.clone(),
        config.memory.summarize_threshold,
        config.learning.user_model_cron.clone(),
        home,
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

    let supervisor = Arc::new(rustfox::supervisor::Supervisor::new(
        config.supervisor.artifacts_dir.clone(),
        memory.connection(),
        sup_registry,
        config.supervisor.risk.clone(),
    ));
    agent.attach_supervisor(Arc::clone(&supervisor)).await;
    match supervisor.resumable_task_ids().await {
        Ok(ids) if !ids.is_empty() => info!(
            "  Supervisor: {} resumable task(s) found at startup",
            ids.len()
        ),
        Ok(_) => info!("  Supervisor: ready (registry has reasoning + shell backends)"),
        Err(e) => warn!("  Supervisor: failed to enumerate resumable tasks: {e}"),
    }

    // Opt-in embedded web portal (ADR 0004): spawned only when [portal] enabled.
    let portal_shutdown = tokio_util::sync::CancellationToken::new();
    if config.portal.enabled {
        let portal_agent: std::sync::Arc<dyn rustfox::portal::AgentOps> = agent.clone();
        let portal_state = rustfox::portal::PortalState::new(
            portal_agent,
            memory.clone(),
            task_store.clone(),
            config.portal.clone(),
            config_path.clone(),
            config.resolved_home().cloned(),
        )
        .with_secret_store(secret_bridge.store())
        .with_pending_secrets(secret_bridge.pending());
        rustfox::portal::auth::ensure_startup_token(&portal_state);
        if let Err(e) = rustfox::portal::serve(portal_state, portal_shutdown.clone()).await {
            // Portal is best-effort: a bind failure must not kill the bot.
            tracing::error!("Portal: failed to start — {e}");
        }
    }

    // Run N Telegram dispatchers (one per [[bots]] entry) with signal-driven
    // graceful shutdown. Each run() notifies its own allowlist on startup.
    // Tool-call UI must leave on the bot that owns the turn, not only the shim.
    {
        let mut senders: std::collections::HashMap<
            String,
            std::sync::Arc<dyn rustfox::platform::sender::PlatformSender>,
        > = std::collections::HashMap::new();
        for (id, dispatch_bot, _) in &bot_runtimes {
            let adapter =
                rustfox::platform::telegram::TelegramAdapter::new(dispatch_bot.as_ref().clone());
            senders.insert(id.clone(), std::sync::Arc::new(adapter));
        }
        agent.set_bot_senders(senders).await;
    }

    info!("Starting {} Telegram dispatcher(s)...", bot_runtimes.len());

    let shutdown_bots = bot_runtimes.clone();
    let mut join_set = tokio::task::JoinSet::new();
    for (bot_id, dispatch_bot, allowlist) in bot_runtimes {
        let dispatch_agent = Arc::clone(&agent);
        let id_for_log = bot_id.clone();
        join_set.spawn(async move {
            info!(bot_id = %id_for_log, "Telegram dispatcher starting");
            let result =
                platform::telegram::run(dispatch_agent, allowlist, dispatch_bot, bot_id).await;
            (id_for_log, result)
        });
    }

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
        Some(joined) = join_set.join_next() => {
            match joined {
                Ok((id, Ok(()))) => {
                    warn!(bot_id = %id, "Telegram dispatcher exited unexpectedly; shutting down");
                }
                Ok((id, Err(e))) => {
                    tracing::error!(bot_id = %id, error = %e, "Telegram dispatcher failed; shutting down");
                }
                Err(e) => {
                    tracing::error!(error = %e, "Telegram dispatcher task panicked; shutting down");
                }
            }
        }
    };

    // Per-bot shutdown notify (each bot's own allowlist)
    for (id, dispatch_bot, allowlist) in &shutdown_bots {
        info!(bot_id = %id, "Sending shutdown notification");
        platform::telegram::notify_shutdown(dispatch_bot, allowlist).await;
    }

    // Abort any remaining dispatchers and wait for them to finish.
    join_set.abort_all();
    while join_set.join_next().await.is_some() {}

    // Brief grace period for message delivery
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    portal_shutdown.cancel();

    info!("Shutdown complete.");

    Ok(())
}
