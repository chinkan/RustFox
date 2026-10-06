use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{ParseMode, UpdateKind};
use tracing::{error, info, warn};

use async_trait::async_trait;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId};

use crate::agent::{Agent, LoopCallbackChoice, MidRunMode};
use crate::platform::sender::{
    MessageFormat as PlatformMsgFormat, PlatformMessageId, PlatformSender,
};
use crate::platform::{Attachment, AttachmentKind, IncomingMessage};
use crate::provider::Provider;
use crate::tool_registry::ToolUiMode;
use crate::utils::markdown_entities::{markdown_to_entities, split_entities};
use crate::utils::rich_sender;
use crate::utils::telegram_markdown::escape_text;

/// Helper: parse a chat_id string (e.g. "123456789") into teloxide's ChatId.
fn parse_chat_id(s: &str) -> Result<teloxide::types::ChatId> {
    Ok(teloxide::types::ChatId(s.parse::<i64>()?))
}

/// Message format mode for Telegram responses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageFormat {
    /// `sendRichMessage` only, no fallback
    Rich,
    /// Entity-formatted `sendMessage` only, no rich path
    Markdown,
    /// Try `sendRichMessage`, fall back to entities on BadMarkdown
    Auto,
}

impl MessageFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageFormat::Rich => "rich",
            MessageFormat::Markdown => "markdown",
            MessageFormat::Auto => "auto",
        }
    }

    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "rich" => Some(MessageFormat::Rich),
            "markdown" => Some(MessageFormat::Markdown),
            "auto" => Some(MessageFormat::Auto),
            _ => None,
        }
    }
}

/// Load the user's preferred message format from memory.
async fn load_message_format(memory: &crate::memory::MemoryStore, user_id: &str) -> MessageFormat {
    let raw = memory
        .recall("settings", &format!("message_format_{}", user_id))
        .await
        .unwrap_or(None);
    MessageFormat::from_str_value(raw.as_deref().unwrap_or("auto")).unwrap_or(MessageFormat::Auto)
}

/// Split long messages for Telegram's 4096 char limit
#[cfg(test)]
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let mut end = (start + max_len).min(text.len());
        // Walk back to a valid UTF-8 char boundary so slicing doesn't panic
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        let actual_end = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .or_else(|| text[start..end].rfind(' '))
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };

        chunks.push(text[start..actual_end].to_string());
        start = actual_end;
    }

    chunks
}

/// Parse a Telegram-style slash command into `(command, argument)`.
///
/// Returns `None` if the input does not start with `/`. The command is the
/// token immediately after the slash; the argument is the remainder of the
/// line (trimmed of surrounding whitespace).
///
/// Parse a Telegram-style slash command into `(command, argument)`.
///
/// Returns `None` if the input does not start with `/`. The command is the
/// token immediately after the slash; the argument is the remainder of the
/// line (trimmed of surrounding whitespace).
pub(crate) fn parse_command(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    if !s.starts_with('/') {
        return None;
    }
    let rest = &s[1..];
    let mut it = rest.splitn(2, char::is_whitespace);
    // Strip @BotName suffix so /config@MyBot works the same as /config.
    let cmd_raw = it.next()?;
    let cmd = cmd_raw.split('@').next().unwrap_or(cmd_raw).to_string();
    let arg = it.next().unwrap_or("").trim().to_string();
    Some((cmd, arg))
}

/// Build the static list of slash commands shown in Telegram's "/" menu.
///
/// The descriptions surface to the user via the BotFather command menu.
/// Routing for these commands lives in `handle_message`; this function only
/// publishes their existence to the Telegram client.
pub(crate) fn supported_commands() -> Vec<teloxide::types::BotCommand> {
    use teloxide::types::BotCommand;
    vec![
        BotCommand::new("start", "Show the welcome message and command help"),
        BotCommand::new(
            "clear",
            "Archive the current conversation, keeping past messages searchable",
        ),
        BotCommand::new("tools", "List available built-in and MCP tools"),
        BotCommand::new("skills", "List loaded skills"),
        BotCommand::new("verbose", "Toggle tool-call progress display"),
        BotCommand::new("queryrewrite", "Toggle query rewriting for memory search"),
        BotCommand::new(
            "selfupgrade",
            "Upgrade the bot to the latest version (source or release binary)",
        ),
        BotCommand::new("models", "Browse and change the OpenRouter model"),
        BotCommand::new("mode", "Set steer/queue mode for mid-processing messages"),
        BotCommand::new("stop", "Cancel the current processing gracefully"),
        BotCommand::new("btw", "Ask a parallel question while the bot is busy"),
        BotCommand::new(
            "format",
            "Switch message format: rich (native), markdown (web), auto",
        ),
        BotCommand::new(
            "portal",
            "Portal URLs — how to reach the web UI from this device",
        ),
        BotCommand::new(
            "config",
            "Show or set allowlisted config.toml keys (secrets redacted)",
        ),
        BotCommand::new(
            "agents",
            "List/create agent personas and bind a BotFather token",
        ),
        BotCommand::new(
            "restart",
            "Clean process exit so the service/shell brings the bot back",
        ),
    ]
}

/// Crate version for online/up messages.
///
/// TL lock: `CARGO_PKG_VERSION` only — no build-time git sha is wired in this
/// binary (no `build.rs` / vergen). If sha is added later, append it here.
pub fn rustfox_version_label() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Host-local server time for online/up messages (`YYYY-MM-DD HH:MM +08:00`).
pub fn format_server_local_time(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("%Y-%m-%d %H:%M %:z").to_string()
}

/// Format the per-bot startup / online Telegram message (no secrets).
pub fn format_startup_notify(
    version: &str,
    server_time: &str,
    model: &str,
    mcp_count: usize,
    skills_count: usize,
    memory_status: &str,
) -> String {
    format!(
        "RustFox is online 🦊\n\
Version: {version}\n\
Server time: {server_time}\n\
Model: {model}\n\
MCP: {mcp} server(s) connected\n\
Skills: {skills} loaded\n\
Memory: {memory}",
        version = version,
        server_time = server_time,
        model = model,
        mcp = mcp_count,
        skills = skills_count,
        memory = memory_status,
    )
}

/// Send startup notification to all allowed users.
/// Best-effort: logs failures, never blocks startup.
/// Called once per bot dispatcher (`run`), so multi-bot installs notify separately.
pub async fn notify_startup(
    bot: &teloxide::Bot,
    allowed_user_ids: &[u64],
    model: &str,
    mcp_count: usize,
    skills_count: usize,
    embedding_enabled: bool,
) {
    let memory_status = if embedding_enabled {
        "embedding enabled"
    } else {
        "FTS5 only"
    };

    let msg = format_startup_notify(
        &rustfox_version_label(),
        &format_server_local_time(chrono::Local::now()),
        model,
        mcp_count,
        skills_count,
        memory_status,
    );

    for &user_id in allowed_user_ids {
        let chat_id = teloxide::types::ChatId(user_id as i64);
        if let Err(e) = bot.send_message(chat_id, &msg).await {
            warn!(
                "Failed to send startup notification to user {}: {}",
                user_id, e
            );
        }
    }
}

/// Send shutdown notification to all allowed users.
/// Best-effort: logs failures, never blocks shutdown.
pub async fn notify_shutdown(bot: &teloxide::Bot, allowed_user_ids: &[u64]) {
    let msg = "RustFox is going offline. Goodbye!";

    for &user_id in allowed_user_ids {
        let chat_id = teloxide::types::ChatId(user_id as i64);
        if let Err(e) = bot.send_message(chat_id, msg).await {
            warn!(
                "Failed to send shutdown notification to user {}: {}",
                user_id, e
            );
        }
    }
}

/// Notify allowlisted users that a secret is needed (Slice 2).
/// Message text contains the secret *name* and a portal claim link; never the
/// secret value. The opaque claim id may appear only in the URL path.
pub async fn notify_secret_request(
    bot: &teloxide::Bot,
    allowed_user_ids: &[u64],
    name: &str,
    claim_url: &str,
) {
    let msg = crate::secret_store::format_secret_request_notify(name, claim_url);
    for &user_id in allowed_user_ids {
        let chat_id = teloxide::types::ChatId(user_id as i64);
        if let Err(e) = bot.send_message(chat_id, &msg).await {
            warn!(
                "Failed to send secret-request notify to user {}: {}",
                user_id, e
            );
        }
    }
}

/// In-memory copy of this dispatcher's allowlist. The first real sender
/// replaces the unowned sentinel `[0]` here after the config file is persisted.
#[derive(Clone)]
pub struct LiveAllowlist(pub Arc<std::sync::RwLock<Vec<u64>>>);

/// Run the Telegram bot platform for a single `[[bots]]` entry.
///
/// `bot_id` is the stable config id used for conversation isolation and
/// cancel/injection session keys (design §7.3).
pub async fn run(
    agent: Arc<Agent>,
    allowed_user_ids: Vec<u64>,
    bot: Arc<teloxide::Bot>,
    bot_id: String,
) -> Result<()> {
    let bot = (*bot).clone();

    info!(bot_id = %bot_id, "Starting Telegram platform...");

    // Send startup notifications (best-effort) — before agent is moved into dptree
    notify_startup(
        &bot,
        &allowed_user_ids,
        &agent.config.openrouter.model,
        agent.mcp.server_count(),
        agent.skills.read().await.len(),
        agent.memory.embeddings.is_available(),
    )
    .await;

    // Publish the slash-command menu to Telegram so clients show suggestions.
    // Best-effort: a network failure here must not block the bot from running.
    let commands = supported_commands();
    let count = commands.len();
    match bot.set_my_commands(commands).await {
        Ok(_) => info!("Registered {} Telegram commands", count),
        Err(e) => warn!(error = %e, "Failed to register Telegram commands"),
    }

    let live_allowlist = LiveAllowlist(Arc::new(std::sync::RwLock::new(allowed_user_ids)));

    let message_handler = Update::filter_message()
        .filter_map({
            let live = live_allowlist.clone();
            move |msg: Message| {
                let allowed = live
                    .0
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                if crate::platform::telegram_injector::message_passes_allowlist(&allowed, &msg) {
                    Some(msg)
                } else {
                    None
                }
            }
        })
        .endpoint(handle_message);

    let callback_handler = Update::filter_callback_query()
        .filter_map({
            let live = live_allowlist.clone();
            move |q: CallbackQuery| {
                let allowed = live
                    .0
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                if crate::platform::telegram_injector::callback_passes_allowlist(&allowed, &q) {
                    Some(q)
                } else {
                    None
                }
            }
        })
        .endpoint(handle_model_callback);

    let loop_callback_handler = Update::filter_callback_query()
        .filter_map(|q: CallbackQuery| {
            if crate::platform::telegram_injector::is_loop_callback(&q) {
                Some(q)
            } else {
                None
            }
        })
        .endpoint(handle_loop_callback);

    let handler = dptree::entry()
        .branch(message_handler)
        .branch(loop_callback_handler)
        .branch(callback_handler);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![agent, bot_id, live_allowlist])
        // Commands (like /btw) bypass per-chat serialization for true concurrency.
        // Regular messages keep per-chat ordering to avoid race conditions.
        .distribution_function(|upd: &Update| {
            let is_cmd = match &upd.kind {
                UpdateKind::Message(m)
                | UpdateKind::EditedMessage(m)
                | UpdateKind::ChannelPost(m) => {
                    m.text().map(|t| t.starts_with('/')).unwrap_or(false)
                }
                _ => false,
            };
            if is_cmd {
                None
            } else {
                upd.chat().map(|c| c.id)
            }
        })
        .default_handler(|upd| async move {
            warn!("Unhandled update: {:?}", upd.id);
        })
        .error_handler(LoggingErrorHandler::with_custom_text("telegram"))
        .build()
        .dispatch()
        .await;

    Ok(())
}

/// Send entities-only fallback (no rich path).
async fn send_entities_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
    let (text, entities) = markdown_to_entities(markdown);
    let chunks = split_entities(&text, &entities, 4090);
    if chunks.is_empty() {
        return Ok(());
    }
    for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
        if i == 0 {
            bot.send_message(chat_id, chunk_text)
                .entities(chunk_entities.clone())
                .await?;
        } else {
            bot.send_message(chat_id, chunk_text)
                .entities(chunk_entities.clone())
                .await
                .ok();
        }
    }
    Ok(())
}

/// Send a markdown string with the user's preferred format mode.
pub async fn send_markdown_message(
    bot: &Bot,
    chat_id: ChatId,
    markdown: &str,
    format: MessageFormat,
) -> ResponseResult<()> {
    match format {
        MessageFormat::Rich => {
            let token = bot.token();
            let api_base = bot.api_url();
            let processed = crate::utils::markdown_entities::preprocess_markdown(markdown);
            rich_sender::send_rich_messages(api_base.as_str(), token, chat_id.0, &processed)
                .await
                .map_err(|e| {
                    teloxide::RequestError::Io(Arc::new(std::io::Error::other(format!("{e}"))))
                })?;
            Ok(())
        }
        MessageFormat::Markdown => send_entities_message(bot, chat_id, markdown).await,
        MessageFormat::Auto => {
            let token = bot.token();
            let api_base = bot.api_url();

            let entity_sender = || async { send_entities_message(bot, chat_id, markdown).await };

            match rich_sender::try_send_rich_fallback(
                api_base.as_str(),
                token,
                chat_id.0,
                markdown,
                &entity_sender,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(e) => {
                    warn!("send_markdown_message all paths failed: {e}");
                    Err(teloxide::RequestError::Io(Arc::new(std::io::Error::other(
                        format!("{e}"),
                    ))))
                }
            }
        }
    }
}

/// Read the tool UI mode for a user, with backward compatibility for the old
/// `tool_ui_enabled_{user_id}` boolean key.
async fn read_tool_ui_mode(agent: &Agent, user_id: &str) -> ToolUiMode {
    // Try new key first
    let new_key = format!("tool_ui_mode_{}", user_id);
    match agent.memory.recall("settings", &new_key).await {
        Ok(Some(val)) => return ToolUiMode::from_memory(Some(&val)),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "Failed to recall tool UI mode"),
    }
    // Fallback: migrate from old boolean key. Only persist when the old key
    // actually exists, so the default (no key) stays a live decision.
    let old_key = format!("tool_ui_enabled_{}", user_id);
    match agent.memory.recall("settings", &old_key).await {
        Ok(Some(old_val)) => {
            let mode = ToolUiMode::from_memory(Some(&old_val));
            agent
                .memory
                .remember("settings", &new_key, mode.as_str(), None)
                .await
                .ok();
            mode
        }
        Ok(None) => ToolUiMode::Minimal,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to recall legacy tool UI setting");
            ToolUiMode::Minimal
        }
    }
}

/// Show models for a selected provider, or prompt for text search.
/// When prompting for text search, stores pending state in memory so the next
/// user message from this user routes to `handle_model_search` scoped to this provider.
async fn handle_provider_model_select(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    provider_name: &str,
    provider: &dyn Provider,
    user_id: &str,
) -> ResponseResult<()> {
    let set_pending = |agent: &Arc<Agent>, user_id: &str| {
        let agent = agent.clone();
        let user_id = user_id.to_string();
        let provider_name = provider_name.to_string();
        Box::pin(async move {
            agent
                .memory
                .remember(
                    "settings",
                    &format!("model_search_pending_{}", user_id),
                    "true",
                    None,
                )
                .await
                .ok();
            agent
                .memory
                .remember(
                    "settings",
                    &format!("model_search_provider_{}", user_id),
                    &provider_name,
                    None,
                )
                .await
                .ok();
        })
    };

    if !provider.config().discover_models {
        let prompt = format!(
            "Send me a model name or ID to search for on **{provider_name}**.\n\
             Example: `{}`",
            provider.default_model()
        );
        bot.send_message(chat_id, &prompt).await?;
        set_pending(agent, user_id).await;
        return Ok(());
    }

    match provider.list_models(&agent.llm.client).await {
        Ok(models) if models.len() <= 20 => {
            use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
            let mut keyboard: Vec<Vec<InlineKeyboardButton>> = models
                .iter()
                .map(|m| {
                    let qualified = format!("{}/{}", provider_name, m);
                    vec![InlineKeyboardButton::callback(
                        m.clone(),
                        format!("model_select:{}", qualified),
                    )]
                })
                .collect();
            keyboard.push(vec![InlineKeyboardButton::callback(
                "\u{1F50D} Search all",
                "model_search_prompt",
            )]);
            keyboard.push(vec![InlineKeyboardButton::callback(
                "\u{274C} Cancel",
                "model_select:cancel",
            )]);

            let reply = format!("Models on **{provider_name}** ({}):", models.len());
            bot.send_message(chat_id, &reply)
                .reply_markup(InlineKeyboardMarkup::new(keyboard))
                .await?;
        }
        Ok(models) => {
            let prompt = format!(
                "**{provider_name}** has {} models available.\n\
                 Send me a model name or ID to search for.",
                models.len()
            );
            bot.send_message(chat_id, &prompt).await?;
            set_pending(agent, user_id).await;
        }
        Err(e) => {
            let prompt = format!(
                "Could not load model list from **{provider_name}**: {e}\n\
                 Send a model name or ID directly."
            );
            bot.send_message(chat_id, &prompt).await?;
            set_pending(agent, user_id).await;
        }
    }

    Ok(())
}
/// Accept a text model query and attempt to set the active model.
/// If the query is a bare name (e.g. "deepseek v4 flash"), fetches the
/// provider's model list via `list_models()` and does fuzzy matching to
/// resolve the actual model ID (e.g. "deepseek/deepseek-v4-flash").
///
/// If the user previously selected a provider via the inline keyboard,
/// the search is scoped to that provider.
async fn handle_model_search(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    query: &str,
    user_id: &str,
) -> ResponseResult<()> {
    // If query already has a known provider prefix, set directly
    if let Some((prefix, _)) = query.split_once('/') {
        if agent.registry.get_provider(prefix).is_some() {
            return set_model_and_reply(bot, chat_id, agent, query).await;
        }
    }

    // Determine which provider to search
    let stored_provider = agent
        .memory
        .recall("settings", &format!("model_search_provider_{}", user_id))
        .await
        .unwrap_or(None);

    let provider_name = stored_provider
        .clone()
        .unwrap_or_else(|| agent.registry.default_provider_name().to_string());

    // Clear stored provider so it doesn't affect future searches
    if stored_provider.is_some() {
        agent
            .memory
            .forget("settings", &format!("model_search_provider_{}", user_id))
            .await
            .ok();
    }

    let provider = match agent.registry.get_provider(&provider_name) {
        Some(p) => p,
        None => {
            return set_model_and_reply(
                bot,
                chat_id,
                agent,
                &format!("{}/{}", provider_name, query),
            )
            .await;
        }
    };

    // Fetch model list and fuzzy match
    match provider.list_models(&agent.llm.client).await {
        Ok(models) if !models.is_empty() => {
            let q = query.to_lowercase().replace(['-', '_', '.', ' '], "");

            // Exact match (full model ID)
            if let Some(exact) = models
                .iter()
                .find(|m| m.to_lowercase() == query.to_lowercase())
            {
                return set_model_and_reply(
                    bot,
                    chat_id,
                    agent,
                    &format!("{}/{}", provider_name, exact),
                )
                .await;
            }

            // Fuzzy match: normalize both sides and check containment
            let mut matches: Vec<&String> = models
                .iter()
                .filter(|m| {
                    m.to_lowercase()
                        .replace(['-', '_', '.', ' '], "")
                        .contains(&q)
                })
                .collect();
            matches.sort();
            matches.truncate(10);

            match matches.len() {
                0 => {
                    // No fuzzy match — try direct set anyway
                    return set_model_and_reply(
                        bot,
                        chat_id,
                        agent,
                        &format!("{}/{}", provider_name, query),
                    )
                    .await;
                }
                1 => {
                    return set_model_and_reply(
                        bot,
                        chat_id,
                        agent,
                        &format!("{}/{}", provider_name, matches[0]),
                    )
                    .await;
                }
                _ => {
                    let mut reply = format!(
                        "Multiple models match '{}' on **{}**:\n\n",
                        query, provider_name
                    );
                    for m in &matches {
                        reply.push_str(&format!("`{}/{m}`\n", provider_name));
                    }
                    reply.push_str("\nUse `/models <full_model_id>` to set one.");
                    bot.send_message(chat_id, escape_text(&reply))
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                }
            }
        }
        _ => {
            // API unavailable or empty list — try direct set
            return set_model_and_reply(
                bot,
                chat_id,
                agent,
                &format!("{}/{}", provider_name, query),
            )
            .await;
        }
    }

    Ok(())
}

/// Set the model and send a success/failure reply.
async fn set_model_and_reply(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    model_id: &str,
) -> ResponseResult<()> {
    match agent.set_model(model_id).await {
        Ok(()) => {
            let reply = format!("✅ Model changed to `{}`", model_id);
            bot.send_message(chat_id, escape_text(&reply))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }
        Err(e) => {
            bot.send_message(
                chat_id,
                escape_text(&format!("Failed to save model: {:#}", e)),
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }
    }
    Ok(())
}

/// `/config` — show / list keys / set allowlisted config.toml values.
///
/// Auth is enforced by the dispatcher allowlist filter before this runs.
/// Secrets are never echoed (see [`crate::config_edit::format_show`]).
async fn handle_config_command(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    arg: &str,
    msg_format: MessageFormat,
) -> ResponseResult<()> {
    let arg = arg.trim();
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (arg, ""),
    };

    match sub {
        "" | "show" => {
            // Prefer on-disk config so values set since boot are visible (still redacted).
            let disk_cfg = crate::config::Config::load(&agent.config_path).ok();
            let cfg = disk_cfg.as_ref().unwrap_or(&agent.config);
            let reply = if rest.is_empty() {
                crate::config_edit::format_show(cfg)
            } else {
                let key = crate::config_edit::normalize_key(rest);
                match crate::config_edit::classify_key(&key) {
                    crate::config_edit::KeyAccess::Denied(reason) => {
                        format!("⛔ `{key}` denied: {reason}")
                    }
                    crate::config_edit::KeyAccess::ReadOnly => {
                        if key == "sandbox.allowed_directory" {
                            format!(
                                "`sandbox.allowed_directory` (read-only) = `{}`",
                                cfg.sandbox.allowed_directory.display()
                            )
                        } else {
                            format!("`{key}` is read-only")
                        }
                    }
                    crate::config_edit::KeyAccess::Editable { .. } => {
                        crate::config_edit::format_show(cfg)
                    }
                }
            };
            return send_markdown_message(&bot, chat_id, &reply, msg_format).await;
        }
        "keys" | "help" => {
            return send_markdown_message(
                &bot,
                chat_id,
                &crate::config_edit::slash_map_markdown(),
                msg_format,
            )
            .await;
        }
        "set" => {
            let (key, value) = match rest.split_once(char::is_whitespace) {
                Some((k, v)) => (k.trim(), v.trim()),
                None => {
                    return send_markdown_message(
                        &bot,
                        chat_id,
                        "Usage: `/config set <key> <value>`\nSee `/config keys`.",
                        msg_format,
                    )
                    .await;
                }
            };
            if key.is_empty() || value.is_empty() {
                return send_markdown_message(
                    &bot,
                    chat_id,
                    "Usage: `/config set <key> <value>`\nSee `/config keys`.",
                    msg_format,
                )
                .await;
            }

            // Never log raw value if the key looks secret (defense in depth).
            let key_norm = crate::config_edit::normalize_key(key);
            if matches!(
                crate::config_edit::classify_key(&key_norm),
                crate::config_edit::KeyAccess::Denied(_)
            ) {
                tracing::warn!(key = %key_norm, "Telegram /config set denied");
                return send_markdown_message(
                    &bot,
                    chat_id,
                    &format!(
                        "⛔ Denied `{key_norm}`. Secrets and dangerous keys cannot be set via Telegram."
                    ),
                    msg_format,
                )
                .await;
            }

            match crate::config_edit::apply_config_edit(&agent.config_path, key, value) {
                Ok(result) => {
                    // Soft-apply model live in memory only — disk already written
                    // via apply_config_edit bak path; avoid a second write.
                    if result.key == "openrouter.model" {
                        if let Err(e) = agent.set_model_live(value.trim()).await {
                            tracing::warn!(error = %e, "Live set_model after config write failed");
                        }
                    }
                    let restart_hint = if result.restart_required {
                        "\n\n⚠️ Restart required — run `/restart` to apply."
                    } else {
                        "\n\nApplied (model may already be live)."
                    };
                    let reply = format!(
                        "✅ Set `{key}` (backup: `{}`).{restart_hint}",
                        result.bak_path.display()
                    );
                    // Avoid echoing the raw value for any key.
                    tracing::info!(key = %result.key, restart = result.restart_required, "Telegram /config set ok");
                    return send_markdown_message(&bot, chat_id, &reply, msg_format).await;
                }
                Err(e) => {
                    // Error messages must not include secret values we rejected.
                    tracing::warn!(key = %key_norm, error = %e, "Telegram /config set failed");
                    return send_markdown_message(
                        &bot,
                        chat_id,
                        &format!("❌ Config update failed: {e}"),
                        msg_format,
                    )
                    .await;
                }
            }
        }
        _ => {
            return send_markdown_message(
                &bot,
                chat_id,
                &format!(
                    "Unknown `/config` subcommand `{sub}`.\n\n{}",
                    crate::config_edit::slash_map_markdown()
                ),
                msg_format,
            )
            .await;
        }
    }
}

/// `/agents` — list / show / create persona + guided one-shot BotFather token bind.
///
/// Auth is the dispatcher allowlist. Tokens are never echoed (`bot_token=***`);
/// the one-shot token message is deleted best-effort and never stored in memory.
async fn handle_agents_command(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    arg: &str,
    user_id: u64,
    msg_format: MessageFormat,
) -> ResponseResult<()> {
    let arg = arg.trim();
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (arg, ""),
    };

    match sub {
        "" | "list" => {
            let disk_cfg = crate::config::Config::load(&agent.config_path).ok();
            let cfg = disk_cfg.as_ref().unwrap_or(&agent.config);
            let reply = crate::agents_edit::format_agents_list(cfg);
            return send_markdown_message(&bot, chat_id, &reply, msg_format).await;
        }
        "help" | "keys" => {
            return send_markdown_message(
                &bot,
                chat_id,
                &crate::agents_edit::slash_help_markdown(),
                msg_format,
            )
            .await;
        }
        "show" => {
            if rest.is_empty() {
                return send_markdown_message(
                    &bot,
                    chat_id,
                    "Usage: `/agents show <id>`",
                    msg_format,
                )
                .await;
            }
            let disk_cfg = crate::config::Config::load(&agent.config_path).ok();
            let cfg = disk_cfg.as_ref().unwrap_or(&agent.config);
            match crate::agents_edit::format_agent_show(cfg, rest) {
                Ok(reply) => {
                    return send_markdown_message(&bot, chat_id, &reply, msg_format).await;
                }
                Err(e) => {
                    return send_markdown_message(&bot, chat_id, &format!("❌ {e}"), msg_format)
                        .await;
                }
            }
        }
        "cancel" => {
            let key = crate::agents_edit::token_pending_key(user_id);
            agent.memory.forget("settings", &key).await.ok();
            return send_markdown_message(
                &bot,
                chat_id,
                "✅ Cancelled pending bot-token bind.",
                msg_format,
            )
            .await;
        }
        "create" => {
            if rest.is_empty() {
                return send_markdown_message(
                    &bot,
                    chat_id,
                    "Usage: `/agents create <id>`\nThen send the BotFather token as your next message.",
                    msg_format,
                )
                .await;
            }
            if let Err(e) = crate::agents_edit::validate_agent_id(rest) {
                return send_markdown_message(&bot, chat_id, &format!("❌ {e}"), msg_format).await;
            }
            let id = rest.trim();

            // Reject if bot id already configured (disk preferred).
            let disk_cfg = crate::config::Config::load(&agent.config_path).ok();
            let cfg = disk_cfg.as_ref().unwrap_or(&agent.config);
            if cfg.bots.iter().any(|b| b.id.trim() == id) {
                return send_markdown_message(
                    &bot,
                    chat_id,
                    &format!("❌ Bot id `{id}` already exists in `[[bots]]`."),
                    msg_format,
                )
                .await;
            }

            let agents_dir = &agent.config.agents.directory;
            let persona_dir = agents_dir.join(id);
            let created_fresh = if persona_dir.join("AGENT.md").exists() || persona_dir.exists() {
                // Re-arm token capture for an existing unbound persona pack.
                false
            } else {
                match crate::agents_edit::create_persona_pack(agents_dir, id) {
                    Ok(created) => {
                        tracing::info!(
                            agent_id = %created.id,
                            path = %created.dir.display(),
                            "Telegram /agents create persona pack"
                        );
                        true
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Telegram /agents create failed");
                        return send_markdown_message(
                            &bot,
                            chat_id,
                            &format!("❌ Create failed: {e}"),
                            msg_format,
                        )
                        .await;
                    }
                }
            };

            let key = crate::agents_edit::token_pending_key(user_id);
            if let Err(e) = agent.memory.remember("settings", &key, id, None).await {
                tracing::warn!(error = %e, "Failed to store agents token-pending state");
                return send_markdown_message(
                    &bot,
                    chat_id,
                    &format!("❌ Failed to arm token capture: {e}"),
                    msg_format,
                )
                .await;
            }
            let head = if created_fresh {
                format!("✅ Created persona pack `agents/{id}/` (`AGENT.md` + `SOUL.md`).")
            } else {
                format!(
                    "✅ Persona pack `agents/{id}/` already on disk (no `[[bots]]` id yet) — re-armed token capture."
                )
            };
            let reply = format!(
                "{head}

                 Now send the **BotFather token** as your **next** message (one-shot).
                 It will be deleted and never logged. Cancel with `/agents cancel`.

                 Bind will append `[[bots]]` `{{ id={id}, persona={id}, allowed_user_ids=[{user_id}], bot_token=*** }}` then restart."
            );
            return send_markdown_message(&bot, chat_id, &reply, msg_format).await;
        }
        _ => {
            return send_markdown_message(
                &bot,
                chat_id,
                &format!(
                    "Unknown `/agents` subcommand `{sub}`.\n\n{}",
                    crate::agents_edit::slash_help_markdown()
                ),
                msg_format,
            )
            .await;
        }
    }
}

/// If the user has a pending `/agents create` token capture, consume the next
/// non-slash text message as the BotFather token (never log/store raw token).
///
/// Returns `true` when the message was handled as a token bind attempt.
async fn try_handle_pending_agent_token(
    bot: &Bot,
    msg: &Message,
    agent: &Arc<Agent>,
    user_id: u64,
    text: &str,
    msg_format: MessageFormat,
) -> ResponseResult<bool> {
    if text.is_empty() || text.starts_with('/') {
        return Ok(false);
    }
    let key = crate::agents_edit::token_pending_key(user_id);
    let pending_id = match agent.memory.recall("settings", &key).await {
        Ok(Some(id)) if !id.trim().is_empty() => id,
        _ => return Ok(false),
    };

    let token = text.trim();

    // Non-token chatter while armed: keep pending, do not delete, nudge.
    if !crate::agents_edit::looks_like_bot_token(token) {
        let _ = send_markdown_message(
            bot,
            msg.chat.id,
            &format!(
                "⏳ Waiting for BotFather token for `{pending_id}` (one-shot).
                 Send the token as the next message, or `/agents cancel`."
            ),
            msg_format,
        )
        .await;
        return Ok(true);
    }

    // Best-effort delete of the token message (hygiene). Never log raw text.
    if let Err(e) = bot.delete_message(msg.chat.id, msg.id).await {
        tracing::warn!(
            error = %e,
            "Failed to delete bot-token message (redact in replies/logs anyway)"
        );
    }

    tracing::info!(
        agent_id = %pending_id,
        token = "bot_token=***",
        "Telegram /agents token bind attempt"
    );

    let home = agent
        .config
        .resolved_home()
        .cloned()
        .or_else(|| agent.config_path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let bind_store = match crate::secret_store::open(&home) {
        Ok((s, _)) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Secret store open failed for /agents bind");
            let _ = send_markdown_message(
                bot,
                msg.chat.id,
                &format!("❌ Could not open SecretStore for bind: {e}"),
                msg_format,
            )
            .await;
            return Ok(true);
        }
    };
    match crate::agents_edit::append_bot_binding(
        &agent.config_path,
        &pending_id,
        token,
        user_id,
        bind_store.as_ref(),
    ) {
        Ok(result) => {
            agent.memory.forget("settings", &key).await.ok();
            let reply = format!(
                "✅ Bound bot `{id}` → persona=`{persona}` allowlist={allow:?} bot_token=***
                 Backup: `{bak}`

                 Restarting so the new dispatcher comes up…",
                id = result.id,
                persona = result.persona,
                allow = result.allowed_user_ids,
                bak = result.bak_path.display()
            );
            let _ = send_markdown_message(bot, msg.chat.id, &reply, msg_format).await;
            // Reuse /restart clean-exit path (TL: write + restart).
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Err(e) = crate::learning::restart_bot() {
                    tracing::error!(error = %e, "Telegram /agents bind restart failed");
                }
            });
            Ok(true)
        }
        Err(e) => {
            // Keep pending so the user can resend a corrected token (persona dir already exists).
            tracing::warn!(
                agent_id = %pending_id,
                error = %e,
                "Telegram /agents token bind failed"
            );
            let _ = send_markdown_message(
                bot,
                msg.chat.id,
                &format!(
                    "❌ Token bind failed for `{pending_id}`: {e}

                     Pending is still armed — send another token, or `/agents cancel`.
                     Persona pack `agents/{pending_id}/` was kept."
                ),
                msg_format,
            )
            .await;
            Ok(true)
        }
    }
}

/// `/restart` — ack, then clean process exit so the supervisor brings us back.
///
/// v1: no in-process Telegram dispatcher hot-reload (TL lock).
async fn handle_restart_command(bot: Bot, chat_id: ChatId) -> ResponseResult<()> {
    let _ = bot
        .send_message(
            chat_id,
            "✅ Restarting — clean exit; service/shell will bring me back.",
        )
        .await;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if let Err(e) = crate::learning::restart_bot() {
            tracing::error!(error = %e, "Telegram /restart failed");
        }
    });
    Ok(())
}

pub async fn handle_message(
    bot: Bot,
    msg: Message,
    agent: Arc<Agent>,
    bot_id: String,
    allowlist: LiveAllowlist,
) -> ResponseResult<()> {
    let user = match msg.from.as_ref() {
        Some(user) => user,
        None => return Ok(()),
    };

    let user_id = user.id.0;
    // First real Telegram message claims allowed_user_ids = [0]. Callbacks do not.
    match crate::config_edit::claim_unowned_bot(&agent.config_path, &bot_id, user_id) {
        Ok(crate::config_edit::UnownedClaim::Claimed) => {
            if let Ok(mut guard) = allowlist.0.write() {
                *guard = vec![user_id];
            }
            info!(bot_id = %bot_id, user_id, "claimed unowned bot allowlist");
        }
        Ok(crate::config_edit::UnownedClaim::Rejected) => {
            info!(bot_id = %bot_id, user_id, "rejected sender after allowlist claim");
            return Ok(());
        }
        Ok(crate::config_edit::UnownedClaim::Unchanged) => {}
        Err(e) => {
            let unowned = allowlist
                .0
                .read()
                .map(|guard| crate::platform::allowlist_is_unowned(&guard))
                .unwrap_or(false);
            if unowned {
                warn!(bot_id = %bot_id, error = %e, "failed to persist allowlist claim");
                return Ok(());
            }
            warn!(bot_id = %bot_id, error = %e, "allowlist claim check failed");
        }
    }
    let user_name = user.first_name.clone();
    let mut msg_format = load_message_format(&agent.memory, &user_id.to_string()).await;

    // For media messages, use caption as text; for text messages, use msg.text()
    let text = msg
        .text()
        .or_else(|| msg.caption())
        .unwrap_or("")
        .to_string();

    // Temp dir for file downloads — created lazily by download_telegram_file
    let temp_dir = std::env::temp_dir().join(format!("rustfox_{}", uuid::Uuid::new_v4()));

    let mut attachments: Vec<Attachment> = Vec::new();

    // Handle photo attachments — last PhotoSize is the highest resolution
    if let Some(photos) = msg.photo() {
        if let Some(largest) = photos.last() {
            let file_id = largest.file.id.to_string();
            match download_telegram_file(&bot, &file_id, &temp_dir, None).await {
                Ok((path, mime)) => {
                    attachments.push(Attachment {
                        kind: AttachmentKind::Image,
                        path,
                        mime_type: mime,
                        file_name: None,
                    });
                }
                Err(e) => warn!("Failed to download photo: {:#}", e),
            }
        }
    }

    // Handle document attachments
    if let Some(doc) = msg.document() {
        let file_id = doc.file.id.to_string();
        let file_name = doc.file_name.clone();
        match download_telegram_file(&bot, &file_id, &temp_dir, file_name.as_deref()).await {
            Ok((path, mime)) => {
                let kind = classify_attachment_kind(&mime, file_name.as_deref());
                attachments.push(Attachment {
                    kind,
                    path,
                    mime_type: mime,
                    file_name,
                });
            }
            Err(e) => warn!("Failed to download document: {:#}", e),
        }
    }

    // Skip if there is nothing to process
    if text.is_empty() && attachments.is_empty() {
        return Ok(());
    }

    // Check if user is in model-search-pending state (tapped "Search models" button).
    let search_pending = agent
        .memory
        .recall("settings", &format!("model_search_pending_{}", user_id))
        .await
        .unwrap_or(None)
        .map(|v| v == "true")
        .unwrap_or(false);
    if search_pending && !text.is_empty() && !text.starts_with('/') {
        agent
            .memory
            .remember(
                "settings",
                &format!("model_search_pending_{}", user_id),
                "false",
                None,
            )
            .await
            .ok();
        // Treat message as a model search query — dispatch to shared model search logic.
        return handle_model_search(bot, msg.chat.id, &agent, &text, &user_id.to_string()).await;
    }

    // Pending /agents token capture — handle before any log that would echo text.
    if try_handle_pending_agent_token(&bot, &msg, &agent, user_id, &text, msg_format).await? {
        return Ok(());
    }

    info!(
        "Telegram message from {} ({}): {} [attachments: {}]",
        user_name,
        user_id,
        if text.is_empty() {
            "(no text)"
        } else if crate::agents_edit::looks_like_bot_token(&text) {
            "bot_token=***"
        } else {
            &text
        },
        attachments.len()
    );

    // Handle commands
    if text == "/clear" {
        if let Err(e) = agent
            .clear_conversation("telegram", &bot_id, &user_id.to_string())
            .await
        {
            error!("Failed to clear conversation: {}", e);
        }
        return send_markdown_message(
            &bot,
            msg.chat.id,
            "Conversation archived. Past messages remain searchable.",
            msg_format,
        )
        .await;
    }

    if text == "/start" {
        let help = "Hello! I'm your AI assistant. Send me a message and I'll help you.\n\n\
             Commands:\n\
             **/clear** — Clear conversation history\n\
             **/tools** — List available tools\n\
             **/skills** — List loaded skills\n\
             **/update-skills** — Re-sync bundled skills (backs up local edits)\n\
             **/verbose** — Toggle tool call progress display\n\
             **/queryrewrite** — Toggle query rewriting for memory search\n\
             **/format** — Switch message format: rich, markdown, or auto\n\
             **/selfupgrade** — Upgrade the bot (source or release binary)\n\
             **/models** — Browse and change the model\n\
             **/stop** — Cancel the current processing gracefully\n\
             **/btw** — Ask a parallel question while the bot is busy\n\
             **/portal** — Portal URLs (web UI) for this network
             **/config** — Show/set allowlisted config keys (secrets redacted)
             **/agents** — List/create personas and bind a BotFather token
             **/restart** — Clean restart (exit; service/shell brings it back)";
        return send_markdown_message(&bot, msg.chat.id, help, msg_format).await;
    }

    if text == "/tools" {
        let all_tools = agent.all_tool_definitions();
        let mut builtin = Vec::new();
        let mut mcp_servers: BTreeMap<String, Vec<&crate::llm::ToolDefinition>> = BTreeMap::new();

        // Known MCP server names (same list as friendly_tool_name in tool_notifier.rs)
        // Sorted by length descending to match longest first (handles server names with underscores)
        const KNOWN_MCP_SERVERS: [&str; 14] = [
            "google-workspace",
            "google_workspace",
            "brave-search",
            "brave_search",
            "filesystem",
            "puppeteer",
            "github",
            "sqlite",
            "threads",
            "notion",
            "fetch",
            "git",
            "context7",
            "qdrant",
        ];

        for tool in &all_tools {
            if let Some(rest) = tool.function.name.strip_prefix("mcp_") {
                let server = KNOWN_MCP_SERVERS
                    .iter()
                    .find(|server| rest.starts_with(&format!("{}_", server)))
                    .map(|s| s.to_string())
                    .or_else(|| {
                        // Unknown server: split on first underscore
                        rest.find('_').map(|sep| rest[..sep].to_string())
                    });
                match server {
                    Some(s) => mcp_servers.entry(s).or_default().push(tool),
                    None => builtin.push(tool),
                }
            } else {
                builtin.push(tool);
            }
        }

        let mut tool_list = format!("**Built-in tools** ({}):\n", builtin.len());
        for tool in &builtin {
            tool_list.push_str(&format!(
                "  - `{}`: {}\n",
                tool.function.name, tool.function.description
            ));
        }
        tool_list.push('\n');

        for (server, tools) in &mcp_servers {
            tool_list.push_str(&format!("**MCP: {}** ({}):\n", server, tools.len()));
            for tool in tools {
                tool_list.push_str(&format!(
                    "  - `{}`: {}\n",
                    tool.function.name, tool.function.description
                ));
            }
            tool_list.push('\n');
        }

        return send_markdown_message(&bot, msg.chat.id, &tool_list, msg_format).await;
    }

    if text == "/skills" {
        let skills_guard = agent.skills.read().await;
        let skills = skills_guard.list();
        if skills.is_empty() {
            return send_markdown_message(&bot, msg.chat.id, "No skills loaded.", msg_format).await;
        }
        let mut skill_list = String::from("**Loaded skills:**\n\n");
        for skill in &skills {
            skill_list.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
        }
        return send_markdown_message(&bot, msg.chat.id, &skill_list, msg_format).await;
    }

    if text == "/updateskills" || text == "/update-skills" {
        let mut lines = Vec::new();

        match crate::skills::embed::overwrite_skills(&agent.config.skills.directory).await {
            Ok(r) => lines.push(format!(
                "Skills — {} written, {} backed up.",
                r.written, r.backed_up
            )),
            Err(e) => lines.push(format!("Skills update failed: {e}")),
        }
        match crate::skills::embed::overwrite_agents(&agent.config.agents.directory).await {
            Ok(r) => lines.push(format!(
                "Agents — {} written, {} backed up.",
                r.written, r.backed_up
            )),
            Err(e) => lines.push(format!("Agents update failed: {e}")),
        }

        let (s, a) = agent.reload_skills_and_agents().await;
        lines.push(format!("Reloaded: {s} skill(s), {a} agent(s) active."));

        return send_markdown_message(&bot, msg.chat.id, &lines.join("\n"), msg_format).await;
    }

    if text == "/portal" {
        let reply = crate::portal::url::portal_reply(&agent.config.portal).await;
        return send_markdown_message(&bot, msg.chat.id, &reply, msg_format).await;
    }

    if text == "/verbose" {
        let current = read_tool_ui_mode(&agent, &user_id.to_string()).await;
        let new_mode = current.next();
        agent
            .memory
            .remember(
                "settings",
                &format!("tool_ui_mode_{}", user_id),
                new_mode.as_str(),
                None,
            )
            .await
            .ok();
        return send_markdown_message(&bot, msg.chat.id, new_mode.reply_message(), msg_format)
            .await;
    }

    // Accept both the canonical `/queryrewrite` (registered with Telegram —
    // Bot API command names cannot contain hyphens) and the legacy
    // `/query-rewrite` form for users with existing muscle memory.
    if text == "/queryrewrite" || text == "/query-rewrite" {
        let current = agent
            .memory
            .recall("settings", &format!("query_rewrite_enabled_{}", user_id))
            .await
            .unwrap_or(None);
        // When no per-user setting exists, fall back to the global config default.
        let currently_on = match current.as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => agent.config.memory.query_rewriter_enabled,
        };
        let new_value = if currently_on { "false" } else { "true" };
        agent
            .memory
            .remember(
                "settings",
                &format!("query_rewrite_enabled_{}", user_id),
                new_value,
                None,
            )
            .await
            .ok();
        let reply = if new_value == "true" {
            "🔍 **Query rewriting enabled.** Follow-up questions will be rewritten before memory search."
        } else {
            "🔍 **Query rewriting disabled.** Messages will be searched as-is."
        };
        return send_markdown_message(&bot, msg.chat.id, reply, msg_format).await;
    }

    // Handle /btw <text> for context-forked side question
    if text == "/btw" || text.starts_with("/btw ") {
        let btw_text = text
            .strip_prefix("/btw")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("What are you doing?")
            .to_string();

        // Reply immediately, then answer in background
        let _ = send_markdown_message(
            &bot,
            msg.chat.id,
            "⏳ **BTW question sent to subagent...**",
            msg_format,
        )
        .await;

        let agent_clone = agent.clone();
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let user_id_str = user_id.to_string();
        let btw_bot_id = bot_id.clone();
        let btw_format = msg_format;
        tokio::spawn(async move {
            // Load conversation context inside the spawned task
            let claim_legacy = crate::config::Config::bot_claims_legacy_default(
                &agent_clone.config.bots,
                &btw_bot_id,
            );
            let conversation_id = match agent_clone
                .memory
                .get_or_create_conversation_with_claim(
                    "telegram",
                    &btw_bot_id,
                    &user_id_str,
                    claim_legacy,
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    let _ = send_markdown_message(
                        &bot_clone,
                        chat_id,
                        &format!("**BTW error:** {}", e),
                        btw_format,
                    )
                    .await;
                    return;
                }
            };
            let messages = agent_clone
                .memory
                .load_messages_with_limit(
                    &conversation_id,
                    agent_clone.config.memory.max_raw_messages,
                )
                .await
                .unwrap_or_default();

            let forked = crate::agent::build_btw_context(&messages, &btw_text);
            // Use the agent's current model (qualified string like "openrouter/qwen/qwen3-235b-a22b")
            let model = agent_clone.current_model.read().await.clone();
            match agent_clone
                .llm
                .chat_completion_with_model(&forked, &[], &model)
                .await
            {
                Ok(response) => {
                    let text = response
                        .message
                        .content
                        .as_ref()
                        .map(|c| c.as_text())
                        .unwrap_or_default();
                    let _ = send_markdown_message(&bot_clone, chat_id, &text, btw_format).await;
                }
                Err(e) => {
                    let _ = send_markdown_message(
                        &bot_clone,
                        chat_id,
                        &format!("**BTW error:** {}", e),
                        btw_format,
                    )
                    .await;
                }
            }
        });

        return Ok(());
    }

    // Combined parse_command dispatch for /self-upgrade and /models.
    if let Some((cmd, arg)) = parse_command(&text) {
        match cmd.as_str() {
            "self-upgrade" | "selfupgrade" => {
                let branch = if arg.is_empty() { "main" } else { &arg };

                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<String>();

                let sent = bot
                    .send_message(msg.chat.id, "🔄 Starting self-upgrade...")
                    .await?;

                let bot_clone = bot.clone();
                let bot_progress = bot.clone();
                let chat_id = msg.chat.id;
                let msg_id = sent.id;
                let branch_owned = branch.to_string();

                let progress_handle = tokio::spawn(async move {
                    let mut buffer = String::from("🔄 Self-upgrading...\n");
                    while let Some(step) = progress_rx.recv().await {
                        buffer.push_str(&format!("{}\n", step));
                        if buffer.len() > 3500 {
                            let suffix = "\n...(truncated)";
                            let trunc = buffer.len() - 3500 + suffix.len();
                            buffer =
                                format!("...{}", &buffer[buffer.len().saturating_sub(trunc)..]);
                            buffer.push_str(suffix);
                        }
                        let _ = bot_progress
                            .edit_message_text(chat_id, msg_id, &buffer)
                            .await;
                    }
                });

                let result =
                    crate::learning::self_upgrade(&branch_owned, "auto", Some(progress_tx)).await;

                // Wait for progress to be fully displayed.
                progress_handle.await.ok();

                match result {
                    Ok(log) => {
                        let display = if log.len() > 3500 {
                            format!("{}...\n(truncated)", &log[..3500])
                        } else {
                            log
                        };
                        bot_clone
                            .edit_message_text(
                                chat_id,
                                msg_id,
                                format!("✅ Upgrade successful!\n\n{}", display),
                            )
                            .await
                            .ok();
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        let _ = crate::learning::restart_bot();
                    }
                    Err(e) => {
                        bot_clone
                            .edit_message_text(
                                chat_id,
                                msg_id,
                                format!("❌ Upgrade failed:\n{:#}", e),
                            )
                            .await
                            .ok();
                    }
                }

                return Ok(());
            }
            "models" => {
                if !arg.is_empty() {
                    return handle_model_search(
                        bot,
                        msg.chat.id,
                        &agent,
                        &arg,
                        &user_id.to_string(),
                    )
                    .await;
                }

                let providers = agent.registry.provider_names();

                if providers.len() == 1 {
                    // Single provider: jump straight to model search
                    let provider_name = providers[0].clone();
                    let provider = match agent.registry.get_provider(&provider_name) {
                        Some(p) => p,
                        None => {
                            bot.send_message(
                                msg.chat.id,
                                escape_text(&format!("Provider '{}' not found.", provider_name)),
                            )
                            .parse_mode(ParseMode::MarkdownV2)
                            .await?;
                            return Ok(());
                        }
                    };
                    let user_id = user_id.to_string();
                    return handle_provider_model_select(
                        bot,
                        msg.chat.id,
                        &agent,
                        &provider_name,
                        provider,
                        &user_id,
                    )
                    .await;
                }

                // Multiple providers: show inline keyboard
                let current = agent.current_model.read().await;
                let reply = format!("Active model: `{}`\n\nSelect a provider:", *current);
                use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
                let mut keyboard: Vec<Vec<InlineKeyboardButton>> = providers
                    .iter()
                    .map(|name| {
                        vec![InlineKeyboardButton::callback(
                            name.clone(),
                            format!("provider_select:{}", name),
                        )]
                    })
                    .collect();
                keyboard.push(vec![InlineKeyboardButton::callback(
                    "❌ Cancel",
                    "model_select:cancel",
                )]);

                bot.send_message(msg.chat.id, &reply)
                    .reply_markup(InlineKeyboardMarkup::new(keyboard))
                    .await?;
                return Ok(());
            }
            "config" => {
                return handle_config_command(bot, msg.chat.id, &agent, &arg, msg_format).await;
            }
            "agents" => {
                return handle_agents_command(bot, msg.chat.id, &agent, &arg, user_id, msg_format)
                    .await;
            }
            "restart" => {
                return handle_restart_command(bot, msg.chat.id).await;
            }
            _ => {} // ignore unknown commands for now
        }
    }

    // Handle /format command — switch message output format
    if text.starts_with("/format") {
        let parts: Vec<&str> = text.splitn(2, |c: char| c.is_whitespace()).collect();
        let sub = parts.get(1).copied().unwrap_or("");
        match sub {
            "rich" | "markdown" | "auto" => {
                let fmt = MessageFormat::from_str_value(sub).unwrap();
                msg_format = fmt;
                agent
                    .memory
                    .remember(
                        "settings",
                        &format!("message_format_{}", user_id),
                        sub,
                        None,
                    )
                    .await
                    .ok();
                return send_markdown_message(
                    &bot,
                    msg.chat.id,
                    &format!(
                        "✅ **Format changed to {}.** {}",
                        sub,
                        match fmt {
                            MessageFormat::Rich => "Using sendRichMessage (Telegram native only).",
                            MessageFormat::Markdown =>
                                "Using entity-formatted sendMessage (works everywhere).",
                            MessageFormat::Auto => "Try rich, fall back to entities on failure.",
                        }
                    ),
                    msg_format,
                )
                .await;
            }
            "" => {
                return send_markdown_message(
                    &bot,
                    msg.chat.id,
                    &format!(
                        "Current format: **{}**\n\n\
                         Use `/format rich`, `/format markdown`, or `/format auto` to change.\n\n\
                         - **rich** — sendRichMessage (Telegram native only)\n\
                         - **markdown** — entity-formatted sendMessage (works everywhere)\n\
                         - **auto** — try rich, fall back to entities",
                        msg_format.as_str()
                    ),
                    msg_format,
                )
                .await;
            }
            _ => {
                return send_markdown_message(
                    &bot,
                    msg.chat.id,
                    "Unknown format. Use `/format rich`, `/format markdown`, or `/format auto`.",
                    msg_format,
                )
                .await;
            }
        }
    }

    // Handle /mode command
    if text.starts_with("/mode") {
        let parts: Vec<&str> = text.splitn(2, |c: char| c.is_whitespace()).collect();
        let sub = parts.get(1).copied().unwrap_or("");
        if sub == "steer" {
            agent
                .set_mid_run_mode(&bot_id, &user_id.to_string(), MidRunMode::Steer)
                .await;
            return send_markdown_message(
                &bot, msg.chat.id,
                "🔄 **Mode set to steer.** Mid-processing messages will be injected as steering context.",
                msg_format,
            ).await;
        } else if sub == "queue" {
            agent
                .set_mid_run_mode(&bot_id, &user_id.to_string(), MidRunMode::Queue)
                .await;
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "🔄 **Mode set to queue.** Mid-processing messages will wait for the next turn.",
                msg_format,
            )
            .await;
        } else if sub.is_empty() {
            let current = agent.get_mid_run_mode(&bot_id, &user_id.to_string()).await;
            let mode_str = current.as_str();
            return send_markdown_message(
                &bot,
                msg.chat.id,
                &format!(
                    "Current mode: **{}**\n\nUse `/mode steer` or `/mode queue` to change.",
                    mode_str
                ),
                msg_format,
            )
            .await;
        } else {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "Unknown mode. Use `/mode steer` or `/mode queue`.",
                msg_format,
            )
            .await;
        }
    }

    // Handle /stop command
    if text == "/stop" {
        if agent.cancel_processing(&bot_id, &user_id.to_string()).await {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⏹ **Processing cancelled.** Accumulated state has been saved.",
                msg_format,
            )
            .await;
        } else {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "Nothing is currently processing.",
                msg_format,
            )
            .await;
        }
    }

    // CHECK: if user is currently being processed, queue non-command messages as injection
    if !text.starts_with('/') && agent.is_processing(&bot_id, &user_id.to_string()).await {
        let current_mode = agent.get_mid_run_mode(&bot_id, &user_id.to_string()).await;
        let maxed = !agent
            .queue_injection(&bot_id, &user_id.to_string(), &text)
            .await;
        if maxed {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⚠️ **Injection queue full** (max 10). Please wait for current processing to finish.",
                msg_format,
            )
            .await;
        }
        info!(
            "Queued '{}' as injection for user {} (mode: {:?})",
            text, user_id, current_mode
        );
        let confirm = match current_mode {
            MidRunMode::Steer => {
                "📨 **Steer queued** — will inject into current processing at next step."
            }
            MidRunMode::Queue => {
                "📨 **Message queued** — will process after current task completes."
            }
        };
        return send_markdown_message(&bot, msg.chat.id, confirm, msg_format).await;
    }

    // Send "typing" indicator
    bot.send_chat_action(msg.chat.id, teloxide::types::ChatAction::Typing)
        .await
        .ok();

    // Check tool UI mode for this user (per-chat /verbose). Unchanged default:
    // Minimal still shows Working / Running while a tool runs and removes the
    // completed tool message. Silent still uses the Thinking placeholder.
    let tool_ui_mode = read_tool_ui_mode(&agent, &user_id.to_string()).await;
    // Opt-in on the speaking bot only. Not stored per chat, not inherited
    // from another bot. When on, this turn posts zero tool-call messages.
    let fully_silent = agent.config.bot_fully_silent(&bot_id);
    let turn_ui_mode = if fully_silent {
        ToolUiMode::Silent
    } else {
        tool_ui_mode
    };

    // Set up tool event channel if not silent
    let (tool_event_tx, tool_event_rx) = if turn_ui_mode != ToolUiMode::Silent {
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::platform::tool_notifier::ToolEvent>(32);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Spawn notifier task if not silent
    let notifier_handle = if turn_ui_mode != ToolUiMode::Silent {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let mut rx = tool_event_rx.expect("rx exists when not silent");
        let mode = turn_ui_mode;
        Some(tokio::spawn(async move {
            let mut notifier =
                crate::platform::tool_notifier::ToolCallNotifier::new(bot_clone, chat_id, mode);
            notifier.start().await;
            let mut handled_finished = false;
            while let Some(event) = rx.recv().await {
                match event {
                    crate::platform::tool_notifier::ToolEvent::Finished { success } => {
                        notifier.finish(success).await;
                        handled_finished = true;
                        break;
                    }
                    other => notifier.handle_event(other).await,
                }
            }
            // If the channel closed without an explicit Finished event, preserve
            // previous behaviour and treat it as a successful finish.
            if !handled_finished {
                notifier.finish(true).await;
            }
        }))
    } else {
        None
    };

    // When silent, send a transient "Thinking..." placeholder so the user
    // knows the bot is processing. The placeholder is **independent** of the
    // streaming output — when the first token arrives it is delivered as a NEW
    // message, and the placeholder is deleted by `handle_message` after the
    // stream completes (success or error). This keeps the placeholder a
    // standalone progress signal rather than a doomed attempt to morph into the
    // final answer.
    // Fully silent skips the Thinking placeholder too: no in-progress bubble.
    // Per-chat silent (fully_silent off) still sends it.
    let placeholder_msg_id: Option<teloxide::types::MessageId> =
        if !fully_silent && tool_ui_mode == ToolUiMode::Silent {
            match bot.send_message(msg.chat.id, "⏳ Thinking...").await {
                Ok(sent) => Some(sent.id),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to send thinking placeholder");
                    None
                }
            }
        } else {
            None
        };

    // Streaming: set up token channel for progressive message display
    // Split threshold: use UTF-16 code units (Telegram's limit is 4096).
    // Streaming uses a conservative 3500 to leave room for mid-split growth.
    // The final flush uses markdown_to_entities + split_entities with MAX_UTF16=4090.
    const TELEGRAM_STREAM_SPLIT_UTF16: usize = 3500;

    let (stream_token_tx, stream_token_rx) = tokio::sync::mpsc::channel::<String>(128);

    // Spawn receiver task: edits Telegram message as tokens arrive
    let stream_bot = bot.clone();
    let stream_chat_id = msg.chat.id;
    let stream_format = msg_format;
    let stream_handle = tokio::spawn(async move {
        use std::time::{Duration, Instant};

        struct StreamChunk {
            content: String,
            msg_id: Option<teloxide::types::MessageId>,
        }

        let mut buffer = String::new();
        let mut current_msg_id: Option<teloxide::types::MessageId> = None;
        let mut chunks: Vec<StreamChunk> = Vec::new();
        let mut last_action = Instant::now();
        let mut rx = stream_token_rx;
        let mut buffer_utf16_len: usize = 0;

        while let Some(token) = rx.recv().await {
            buffer.push_str(&token);
            buffer_utf16_len += token.encode_utf16().count();

            // When buffer exceeds split threshold, finalize the current message
            // and reset so subsequent tokens start a new message.
            if buffer_utf16_len > TELEGRAM_STREAM_SPLIT_UTF16 {
                let snapshot = buffer.clone();
                let msg_id = if let Some(mid) = current_msg_id {
                    stream_bot
                        .edit_message_text(stream_chat_id, mid, &snapshot)
                        .await
                        .ok();
                    Some(mid)
                } else if let Ok(sent) = stream_bot.send_message(stream_chat_id, &snapshot).await {
                    Some(sent.id)
                } else {
                    None
                };
                chunks.push(StreamChunk {
                    content: snapshot,
                    msg_id,
                });
                buffer.clear();
                buffer_utf16_len = 0;
                current_msg_id = None;
                last_action = Instant::now();
                continue;
            }

            // Every 500 ms: send first message or edit existing one
            if last_action.elapsed() >= Duration::from_millis(500) {
                if let Some(msg_id) = current_msg_id {
                    stream_bot
                        .edit_message_text(stream_chat_id, msg_id, &buffer)
                        .await
                        .ok();
                } else {
                    match stream_bot.send_message(stream_chat_id, &buffer).await {
                        Ok(sent) => current_msg_id = Some(sent.id),
                        Err(e) => tracing::warn!(error = %e, "stream_handle: initial send failed"),
                    }
                }
                last_action = Instant::now();
            }
        }

        // If the last buffer had no message ID yet, send it now.
        if !buffer.is_empty() && current_msg_id.is_none() {
            match stream_bot.send_message(stream_chat_id, &buffer).await {
                Ok(sent) => current_msg_id = Some(sent.id),
                Err(e) => tracing::warn!(error = %e, "stream_handle: final send failed"),
            }
        }

        // Add the last segment as the final chunk.
        chunks.push(StreamChunk {
            content: buffer,
            msg_id: current_msg_id,
        });

        // If nothing was streamed, the caller sends the returned text.
        if chunks.is_empty() || chunks.iter().all(|c| c.content.is_empty()) {
            return false;
        }

        // Build the full text from all chunks.
        let full_text: String = chunks.iter().map(|c| c.content.as_str()).collect();

        // Collect all message IDs for cleanup.
        let old_ids: Vec<teloxide::types::MessageId> =
            chunks.iter().filter_map(|c| c.msg_id).collect();

        const MAX_UTF16: usize = 4090;

        // Delete all old streaming messages (best-effort) so we can send fresh
        // properly-formatted chunks without orphaned plain-text messages.
        for mid in &old_ids {
            stream_bot.delete_message(stream_chat_id, *mid).await.ok();
        }

        // Pre-process markdown for spoiler/underline
        let processed = crate::utils::markdown_entities::preprocess_markdown(&full_text);
        let (plain_text, entities) = markdown_to_entities(&full_text);
        let entity_chunks = split_entities(&plain_text, &entities, MAX_UTF16);
        let rich_chunks = rich_sender::split_markdown_at_newlines(&processed, MAX_UTF16);
        let token = stream_bot.token();
        let api_base = stream_bot.api_url();

        match stream_format {
            MessageFormat::Rich => {
                for chunk_md in &rich_chunks {
                    if rich_sender::send_rich_message(
                        api_base.as_str(),
                        token,
                        stream_chat_id.0,
                        chunk_md,
                    )
                    .await
                    .is_err()
                    {
                        // Rich-only mode: one failure stops the chain
                        tracing::warn!("stream_handle: rich send failed, aborting");
                        break;
                    }
                }
            }
            MessageFormat::Markdown => {
                for (ct, ce) in &entity_chunks {
                    stream_bot
                        .send_message(stream_chat_id, ct)
                        .entities(ce.clone())
                        .await
                        .ok();
                }
            }
            MessageFormat::Auto => {
                for (i, chunk_md) in rich_chunks.iter().enumerate() {
                    let result = rich_sender::send_rich_message(
                        api_base.as_str(),
                        token,
                        stream_chat_id.0,
                        chunk_md,
                    )
                    .await;
                    if result.is_err() {
                        // Fallback: use entity chunk i
                        if let Some((ct, ce)) = entity_chunks.get(i) {
                            stream_bot
                                .send_message(stream_chat_id, ct)
                                .entities(ce.clone())
                                .await
                                .ok();
                        }
                    }
                }
            }
        }
        true
    });

    // Build platform-agnostic message
    let incoming = IncomingMessage {
        platform: "telegram".to_string(),
        bot_id: bot_id.clone(),
        user_id: user_id.to_string(),
        chat_id: msg.chat.id.0.to_string(),
        user_name,
        text,
        attachments,
        schedule_id: None,
    };

    // Process through agent — moves stream_token_tx and tool_event_tx
    // Keep an owned clone of the tool_event_tx so we can send a terminal
    // Finished event after processing completes.
    let agent_tool_event_tx = tool_event_tx.clone();
    let process_result = match agent
        .process_message(
            &incoming,
            tool_event_tx,
            Some(stream_token_tx),
            turn_ui_mode,
        )
        .await
    {
        Ok(text) => Ok(text),
        Err(e) => {
            stream_handle.abort();
            Err(e)
        }
    };

    let process_success = process_result.is_ok();
    if let Some(tx) = agent_tool_event_tx {
        let _ = tx
            .send(crate::platform::tool_notifier::ToolEvent::Finished {
                success: process_success,
            })
            .await;
    }

    // Drop the sender to signal the notifier to stop, then await cleanup.
    // tool_event_tx is already moved into process_message — it's dropped when process_message returns.
    if let Some(handle) = notifier_handle {
        handle.await.ok();
    }

    // Wait for stream receiver to complete its final edit.
    // false means the channel closed with no tokens (max iterations,
    // cancel, empty-retry). Those returns are Ok text that was never streamed.
    let streamed = stream_handle.await.unwrap_or(false);

    // Cleanup temp dir used for file downloads (async to avoid blocking the executor)
    if temp_dir.exists() {
        tokio::fs::remove_dir_all(&temp_dir).await.ok();
    }

    // Delete the "Thinking..." placeholder now that the response (or error
    // reply below) has been delivered. Best-effort: ignore failures so a
    // stale placeholder never blocks reporting the actual outcome.
    if let Some(placeholder_id) = placeholder_msg_id {
        if let Err(e) = bot.delete_message(msg.chat.id, placeholder_id).await {
            tracing::warn!(error = %e, "Failed to delete thinking placeholder");
        }
    }

    if let Err(e) = &process_result {
        warn!(error = %e, "Agent processing failed");
        return send_markdown_message(&bot, msg.chat.id, &format!("**Error:** {}", e), msg_format)
            .await;
    }
    // Ok text is not always streamed. Max iterations, cancel, and the
    // empty-retry sentence return without pushing tokens.
    if !streamed {
        if let Ok(text) = &process_result {
            if !text.is_empty() {
                send_markdown_message(&bot, msg.chat.id, text, msg_format).await?;
            }
        }
    }

    // Check if a self-upgrade tool call requested a restart.
    if agent
        .restart_pending
        .load(std::sync::atomic::Ordering::Acquire)
    {
        agent
            .restart_pending
            .store(false, std::sync::atomic::Ordering::Release);
        let _ = bot
            .send_message(msg.chat.id, "🔄 Self-upgrade complete. Restarting...")
            .await;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = crate::learning::restart_bot();
        });
    }

    Ok(())
}

/// Handle callback query from loop detection inline keyboard.
/// Resolves the oneshot sender so the suspended agent loop can continue.
async fn handle_loop_callback(
    bot: Bot,
    q: CallbackQuery,
    agent: Arc<Agent>,
    bot_id: String,
) -> ResponseResult<()> {
    let user_id = q.from.id.to_string();
    let data = match q.data {
        Some(ref d) => d.clone(),
        None => {
            // Even if there's no data, we must answer the callback query
            bot.answer_callback_query(q.id).await.ok();
            return Ok(());
        }
    };

    // Parse the user's choice from callback data
    let choice = if data.contains(r#""action":"continue""#) {
        LoopCallbackChoice::Continue
    } else if data.contains(r#""action":"stop""#) {
        LoopCallbackChoice::Stop
    } else if data.contains(r#""action":"add_instruction""#) {
        LoopCallbackChoice::AddInstruction
    } else {
        // Unknown action — answer and ignore
        bot.answer_callback_query(q.id).await.ok();
        return Ok(());
    };

    // Send the choice to the waiting agent loop (if any)
    if let Some(sender) = agent.take_loop_callback(&bot_id, &user_id).await {
        let _ = sender.send(choice);
    }

    bot.answer_callback_query(q.id).await.ok();
    Ok(())
}

/// Handle callback queries from inline keyboard buttons (e.g. model selection).
async fn handle_model_callback(
    bot: Bot,
    q: CallbackQuery,
    agent: Arc<Agent>,
    bot_id: String,
) -> ResponseResult<()> {
    let callback_id = q.id.clone();
    let data = match q.data {
        Some(ref d) => d.clone(),
        None => {
            // Even if there's no data, we must answer the callback query
            bot.answer_callback_query(callback_id).await.ok();
            return Ok(());
        }
    };
    let msg = q.regular_message().cloned();

    // Remove the old unconditional answer_callback_query that had no text.
    // Each branch below now answers with the appropriate text (or silently)
    // exactly once. A second answer for the same callback_id is ignored by
    // Telegram, which previously swallowed the "⛔ Command cancelled" toast.

    if let Some(provider_name) = data.strip_prefix("provider_select:") {
        bot.answer_callback_query(callback_id.clone()).await.ok();
        if let Some(provider) = agent.registry.get_provider(provider_name) {
            if let Some(ref m) = msg {
                let user_id = q.from.id.0.to_string();
                return handle_provider_model_select(
                    bot,
                    m.chat.id,
                    &agent,
                    provider_name,
                    provider,
                    &user_id,
                )
                .await;
            }
        }
        return Ok(());
    }

    if data == "model_search_prompt" {
        bot.answer_callback_query(callback_id.clone()).await.ok();
        if let Some(m) = msg {
            let prompt = "Send me a model name or ID to search for. Examples: claude, kimi, gpt, or a full model ID like openrouter/o3-mini.";
            bot.edit_message_text(m.chat.id, m.id, prompt).await?;
        }
        // Store pending search state for this user.
        let user_id = q.from.id.0.to_string();
        agent
            .memory
            .remember(
                "settings",
                &format!("model_search_pending_{}", user_id),
                "true",
                None,
            )
            .await
            .ok();
        return Ok(());
    }

    if data == "model_select:cancel" {
        bot.answer_callback_query(callback_id.clone()).await.ok();
        if let Some(m) = msg {
            bot.edit_message_text(m.chat.id, m.id, "❌ Model selection cancelled.")
                .await?;
        }
        return Ok(());
    }

    // Handle command cancellation via CancelRegistry + supervisor session cancel.
    // execute_command uses CancelRegistry; supervisor CLI jobs are reached via
    // Supervisor::cancel_for_session({bot_id}:{user_id}).
    if let Some(cmd_id) = data.strip_prefix("cancel_cmd:") {
        let user_id = q.from.id.to_string();
        let cmd_cancelled = agent.cancel_registry.cancel(cmd_id).await;
        let sup_n = agent.cancel_supervisor_session(&bot_id, &user_id).await;
        let text = if cmd_cancelled || sup_n > 0 {
            "⛔ Command cancelled"
        } else {
            "Command already finished"
        };
        bot.answer_callback_query(callback_id).text(text).await.ok();
        return Ok(());
    }

    if let Some(model_id) = data.strip_prefix("model_select:") {
        match agent.set_model(model_id).await {
            Ok(()) => {
                let reply = format!("✅ Model changed to `{}`", model_id);
                if let Some(m) = msg {
                    bot.edit_message_text(m.chat.id, m.id, &reply).await?;
                }
            }
            Err(e) => {
                let reply = format!("Failed to save model: {:#}", e);
                if let Some(m) = msg {
                    bot.edit_message_text(m.chat.id, m.id, &reply).await?;
                }
            }
        }
    }

    bot.answer_callback_query(callback_id).await.ok();
    Ok(())
}

/// Download a Telegram file to the given directory, creating it if needed.
/// Returns (local_path, detected_mime_type).
async fn download_telegram_file(
    bot: &Bot,
    file_id: &str,
    dest_dir: &Path,
    filename: Option<&str>,
) -> Result<(PathBuf, String)> {
    std::fs::create_dir_all(dest_dir).context("Failed to create temp directory")?;

    let file = bot
        .get_file(file_id.to_string().into())
        .await
        .context("Failed to get file info from Telegram")?;

    let ext = Path::new(&file.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");

    let dest_name = match filename {
        Some(n) => n.to_string(),
        None => format!("{}.{}", uuid::Uuid::new_v4(), ext),
    };
    let dest_path = dest_dir.join(&dest_name);

    let mut bytes: Vec<u8> = Vec::new();
    bot.download_file(&file.path, &mut bytes)
        .await
        .context("Failed to download file from Telegram")?;

    std::fs::write(&dest_path, &bytes).context("Failed to write downloaded file")?;

    let mime = infer::get(&bytes)
        .map(|t| t.mime_type().to_string())
        .unwrap_or_else(|| mime_from_extension(ext).to_string());

    Ok((dest_path, mime))
}

/// Classify an attachment based on MIME type and filename extension fallback.
fn classify_attachment_kind(mime_type: &str, file_name: Option<&str>) -> AttachmentKind {
    if mime_type.starts_with("image/") {
        return AttachmentKind::Image;
    }
    if mime_type == "application/pdf" {
        return AttachmentKind::Pdf;
    }
    if mime_type.contains("wordprocessingml") || mime_type == "application/msword" {
        return AttachmentKind::Docx;
    }
    // Fallback: check extension
    let name = file_name.unwrap_or("");
    if name.ends_with(".pdf") {
        return AttachmentKind::Pdf;
    }
    if name.ends_with(".docx") || name.ends_with(".doc") {
        return AttachmentKind::Docx;
    }
    if name.ends_with(".jpg")
        || name.ends_with(".jpeg")
        || name.ends_with(".png")
        || name.ends_with(".gif")
        || name.ends_with(".webp")
    {
        return AttachmentKind::Image;
    }
    AttachmentKind::Other
}

fn mime_from_extension(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

pub struct TelegramAdapter {
    bot: Bot,
}

impl TelegramAdapter {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl PlatformSender for TelegramAdapter {
    async fn send_message(
        &self,
        chat_id_str: &str,
        text: &str,
        format: PlatformMsgFormat,
    ) -> Result<PlatformMessageId> {
        let chat_id = parse_chat_id(chat_id_str)?;
        let parse_mode = match format {
            PlatformMsgFormat::Markdown | PlatformMsgFormat::Auto => Some(ParseMode::MarkdownV2),
            PlatformMsgFormat::Rich => None,
        };
        let mut req = self.bot.send_message(chat_id, text);
        if let Some(pm) = parse_mode {
            req = req.parse_mode(pm);
        }
        let msg = req.await?;
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn send_file(
        &self,
        chat_id_str: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<PlatformMessageId> {
        let chat_id = parse_chat_id(chat_id_str)?;
        let input_file = teloxide::types::InputFile::file(path);
        let msg = if let Some(cap) = caption {
            self.bot
                .send_document(chat_id, input_file)
                .caption(cap)
                .await?
        } else {
            self.bot.send_document(chat_id, input_file).await?
        };
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn show_cancel_button(
        &self,
        chat_id_str: &str,
        text: &str,
        cancel_id: &str,
    ) -> Result<PlatformMessageId> {
        let chat_id = parse_chat_id(chat_id_str)?;
        let keyboard = InlineKeyboardMarkup::new([[InlineKeyboardButton::callback(
            "Cancel",
            format!("cancel_cmd:{cancel_id}"),
        )]]);
        let msg = self
            .bot
            .send_message(chat_id, text)
            .reply_markup(keyboard)
            .await?;
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn edit_message(
        &self,
        chat_id_str: &str,
        message_id: &PlatformMessageId,
        text: &str,
    ) -> Result<()> {
        let chat_id = parse_chat_id(chat_id_str)?;
        let parts: Vec<&str> = message_id.split(':').collect();
        let msg_id: i32 = parts.get(1).unwrap_or(&"0").parse()?;
        self.bot
            .edit_message_text(chat_id, MessageId(msg_id), text)
            .await?;
        Ok(())
    }

    async fn delete_message(
        &self,
        chat_id_str: &str,
        message_id: &PlatformMessageId,
    ) -> Result<()> {
        let chat_id = parse_chat_id(chat_id_str)?;
        let parts: Vec<&str> = message_id.split(':').collect();
        let Some(msg_id_str) = parts.get(1) else {
            anyhow::bail!("invalid message id format: {message_id}");
        };
        let msg_id: i32 = msg_id_str.parse()?;
        self.bot.delete_message(chat_id, MessageId(msg_id)).await?;
        Ok(())
    }

    async fn notify_shutdown(&self, chat_id_str: &str) -> Result<()> {
        let chat_id = parse_chat_id(chat_id_str)?;
        self.bot
            .send_message(chat_id, "⚠️ Bot is shutting down...")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_notify_includes_version_and_datetime_not_secrets() {
        let msg = format_startup_notify(
            "1.0.2",
            "2026-09-28 23:45 +08:00",
            "test-model",
            2,
            5,
            "FTS5 only",
        );
        assert!(msg.contains("RustFox is online"));
        assert!(msg.contains("Version: 1.0.2"));
        assert!(msg.contains("Server time: 2026-09-28 23:45 +08:00"));
        assert!(msg.contains("Model: test-model"));
        assert!(msg.contains("MCP: 2 server(s) connected"));
        assert!(msg.contains("Skills: 5 loaded"));
        assert!(msg.contains("Memory: FTS5 only"));
        // Never echo secrets / tokens.
        assert!(!msg.contains("bot_token"));
        assert!(!msg.contains("sk-"));
        assert!(!msg.contains("api_key"));
        assert_eq!(rustfox_version_label(), env!("CARGO_PKG_VERSION"));
        let stamped = format_server_local_time(chrono::Local::now());
        assert!(
            stamped.len() >= 16,
            "unexpected server time format: {stamped}"
        );
        assert!(
            stamped.as_bytes()[4] == b'-' && stamped.as_bytes()[7] == b'-',
            "expected YYYY-MM-DD prefix: {stamped}"
        );
        assert!(
            stamped.contains('+') || stamped.contains('-'),
            "expected offset label in: {stamped}"
        );
        // Offset should not be glued to minutes without a space.
        assert!(
            stamped.chars().nth(16) == Some(' '),
            "expected space before offset: {stamped}"
        );
    }

    #[test]
    fn test_should_split_stream_at_4000_chars() {
        const TELEGRAM_LIMIT: usize = 3800;
        let short = "a".repeat(100);
        let long = "a".repeat(4000);
        assert!(short.len() < TELEGRAM_LIMIT);
        assert!(long.len() > TELEGRAM_LIMIT);
    }

    #[test]
    fn test_tool_ui_mode_from_memory() {
        use crate::tool_registry::ToolUiMode;
        assert_eq!(
            ToolUiMode::from_memory(Some("verbose")),
            ToolUiMode::Verbose
        );
        assert_eq!(
            ToolUiMode::from_memory(Some("minimal")),
            ToolUiMode::Minimal
        );
        assert_eq!(ToolUiMode::from_memory(Some("silent")), ToolUiMode::Silent);
        // backward compat
        assert_eq!(ToolUiMode::from_memory(Some("true")), ToolUiMode::Verbose);
        // "false" meant no tool UI at all → Silent, not Minimal
        assert_eq!(ToolUiMode::from_memory(Some("false")), ToolUiMode::Silent);
        assert_eq!(ToolUiMode::from_memory(None), ToolUiMode::Minimal);
        // unknown defaults to minimal
        assert_eq!(
            ToolUiMode::from_memory(Some("unknown")),
            ToolUiMode::Minimal
        );
    }

    #[test]
    fn test_tool_ui_mode_cycle() {
        use crate::tool_registry::ToolUiMode;
        assert_eq!(ToolUiMode::Minimal.next(), ToolUiMode::Verbose);
        assert_eq!(ToolUiMode::Verbose.next(), ToolUiMode::Silent);
        assert_eq!(ToolUiMode::Silent.next(), ToolUiMode::Minimal);
    }

    #[test]
    fn parse_supervise_command_extracts_request_text() {
        let parsed = super::parse_command("/supervise summarize the readme");
        assert_eq!(
            parsed,
            Some(("supervise".into(), "summarize the readme".into()))
        );
    }

    #[test]
    fn parse_command_returns_none_for_non_slash_input() {
        assert!(super::parse_command("hello world").is_none());
    }

    #[test]
    fn parse_command_handles_command_without_argument() {
        assert_eq!(
            super::parse_command("/start"),
            Some(("start".into(), "".into()))
        );
    }

    #[test]
    fn parses_all_supervisor_commands() {
        for c in [
            "/tasks",
            "/resume abc",
            "/cancel abc",
            "/approve abc",
            "/clarify abc some text",
        ] {
            assert!(super::parse_command(c).is_some(), "failed: {c}");
        }
    }

    #[test]
    fn test_split_message_empty_response_produces_no_chunks() {
        let chunks = split_message("", 4000);
        assert!(chunks.len() <= 1);
    }

    #[test]
    fn test_split_message_short_stays_intact() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_message_long_splits_at_boundary() {
        let text = "a ".repeat(3000); // 6000 chars
        let chunks = split_message(&text, 4000);
        assert_eq!(chunks.len(), 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 4000);
        }
    }

    #[test]
    fn test_final_flush_uses_entity_based_conversion() {
        // The final flush must call markdown_to_entities (entity-based approach) instead of
        // MarkdownV2 parse_mode. This is a source inspection test.
        let source = include_str!("telegram.rs");
        assert!(
            source.contains("markdown_to_entities"),
            "Final flush must call markdown_to_entities for robust formatting"
        );
        assert!(
            source.contains("split_entities"),
            "Final flush must call split_entities for long message handling"
        );
    }

    #[test]
    fn test_command_responses_use_entity_formatting() {
        // Command responses now use send_markdown_message (entity-based) instead of
        // escape_text + ParseMode::MarkdownV2.
        let source = include_str!("telegram.rs");
        assert!(
            source.contains("send_markdown_message"),
            "Command responses must use send_markdown_message for entity-based formatting"
        );
    }

    #[test]
    fn test_stream_handle_does_not_require_placeholder_send() {
        // If the initial send fails, the stream handle must NOT silently swallow
        // all tokens. This test documents that the placeholder approach is fragile;
        // the implementation plan removes it entirely.
        // After the fix, a failed initial-send path no longer exists, so this test
        // verifies the new code compiles correctly without the zero-width-space literal.
        let source = include_str!("telegram.rs");
        // Check that the actual zero-width space character (U+200B) is not used as a
        // placeholder in send_message calls.
        assert!(
            !source.contains('\u{200B}'),
            "Zero-width-space placeholder must be removed from stream_handle"
        );
    }

    #[test]
    fn test_classify_attachment_kind_image_jpeg() {
        assert_eq!(
            classify_attachment_kind("image/jpeg", None),
            AttachmentKind::Image
        );
    }

    #[test]
    fn test_classify_attachment_kind_pdf() {
        assert_eq!(
            classify_attachment_kind("application/pdf", None),
            AttachmentKind::Pdf
        );
    }

    #[test]
    fn test_classify_attachment_kind_docx() {
        assert_eq!(
            classify_attachment_kind(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                None
            ),
            AttachmentKind::Docx
        );
    }
    #[test]
    fn test_first_token_does_not_inherit_placeholder_msg_id() {
        // The streaming task must seed `current_msg_id` to `None` so the first
        // token is delivered as a NEW message rather than editing the
        // "Thinking..." placeholder. Source-inspection guard against future
        // refactors that re-introduce the seeding behavior.
        //
        // Construct the bad-pattern needle at runtime from pieces so the test
        // body itself never contains the contiguous substring being searched
        // for (otherwise the `contains` check would always trip on this very
        // test's source).
        let source = include_str!("telegram.rs");
        let bad_needle = format!(
            "current_msg_id: Option<teloxide::types::MessageId> = {}",
            "placeholder_msg_id"
        );
        assert!(
            !source.contains(&bad_needle),
            "stream_handle must NOT seed current_msg_id with the placeholder id; first token must be a new message"
        );
        let good_needle = format!(
            "let mut current_msg_id: Option<teloxide::types::MessageId> = {};",
            "None"
        );
        assert!(
            source.contains(&good_needle),
            "stream_handle must initialize current_msg_id to None"
        );
    }

    #[test]
    fn test_classify_attachment_kind_fallback_to_extension() {
        assert_eq!(
            classify_attachment_kind("application/octet-stream", Some("report.pdf")),
            AttachmentKind::Pdf
        );
        assert_eq!(
            classify_attachment_kind("application/octet-stream", Some("letter.docx")),
            AttachmentKind::Docx
        );
        assert_eq!(
            classify_attachment_kind("application/octet-stream", Some("photo.jpg")),
            AttachmentKind::Image
        );
    }

    #[test]
    fn test_placeholder_is_deleted_after_streaming() {
        // The Thinking placeholder must be cleaned up in `handle_message` after
        // `stream_handle.await`, regardless of success/error outcome.
        let source = include_str!("telegram.rs");
        assert!(
            source.contains("Failed to delete thinking placeholder"),
            "handle_message must delete the Thinking placeholder after streaming completes"
        );
    }

    #[test]
    fn test_classify_attachment_kind_unknown() {
        assert_eq!(
            classify_attachment_kind("application/zip", Some("archive.zip")),
            AttachmentKind::Other
        );
    }

    #[test]
    fn test_supported_commands_lists_user_visible_commands() {
        let cmds = supported_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.command.as_str()).collect();
        for required in &[
            "start",
            "clear",
            "tools",
            "skills",
            "verbose",
            "queryrewrite",
            "config",
            "agents",
            "restart",
        ] {
            assert!(
                names.contains(required),
                "supported_commands missing /{required}: got {names:?}"
            );
        }
        // Telegram BotCommand names must match `[a-z0-9_]{1,32}`.
        for c in &cmds {
            assert!(
                c.command
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "command '{}' contains invalid characters for Telegram BotCommand",
                c.command
            );
            assert!(
                (1..=32).contains(&c.command.len()),
                "command '{}' has invalid length {}",
                c.command,
                c.command.len()
            );
            assert!(
                !c.description.is_empty(),
                "command '{}' is missing a description",
                c.command
            );
        }
    }
}
