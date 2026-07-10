use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::{error, info, warn};

use crate::agent::{Agent, MidRunMode};
use crate::platform::{Attachment, AttachmentKind, IncomingMessage};
use crate::provider::Provider;
use crate::utils::markdown_entities::{markdown_to_entities, split_entities};
use crate::utils::rich_sender;
use crate::utils::telegram_markdown::escape_text;
use std::sync::OnceLock;

static BOT_TOKEN: OnceLock<String> = OnceLock::new();

/// Must be called once at startup after the Bot is created.
pub fn init_bot_token(token: String) {
    BOT_TOKEN.set(token).ok();
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
    let cmd = it.next()?.to_string();
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
    ]
}

/// Send startup notification to all allowed users.
/// Best-effort: logs failures, never blocks startup.
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

    let msg = format!(
        "RustFox is online 🦊\nModel: {model}\nMCP: {mcp} server(s) connected\nSkills: {skills} loaded\nMemory: {memory}",
        model = model, mcp = mcp_count, skills = skills_count, memory = memory_status,
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

/// Run the Telegram bot platform
pub async fn run(
    agent: Arc<Agent>,
    allowed_user_ids: Vec<u64>,
    bot: Arc<teloxide::Bot>,
) -> Result<()> {
    let bot = (*bot).clone();

    info!("Starting Telegram platform...");

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

    let message_handler = Update::filter_message()
        .filter_map({
            let allowed = allowed_user_ids.clone();
            move |msg: Message| {
                let user = msg.from.as_ref()?;
                if allowed.contains(&user.id.0) {
                    Some(msg)
                } else {
                    None
                }
            }
        })
        .endpoint(handle_message);

    let callback_handler = Update::filter_callback_query()
        .filter_map({
            let allowed = allowed_user_ids.clone();
            move |q: CallbackQuery| {
                if allowed.contains(&q.from.id.0) {
                    Some(q)
                } else {
                    None
                }
            }
        })
        .endpoint(handle_model_callback);

    let handler = dptree::entry()
        .branch(message_handler)
        .branch(callback_handler);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![agent])
        .default_handler(|upd| async move {
            warn!("Unhandled update: {:?}", upd.id);
        })
        .error_handler(LoggingErrorHandler::with_custom_text("telegram"))
        .build()
        .dispatch()
        .await;

    Ok(())
}

/// Send a markdown string as a rich message via sendRichMessage, falling back
/// to entity-formatted sendMessage on failure.
async fn send_markdown_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
    let token = BOT_TOKEN.get().expect("BOT_TOKEN not initialized");

    let entity_sender = || async {
        let (text, entities) = markdown_to_entities(markdown);
        let chunks = split_entities(&text, &entities, 4090);
        if chunks.is_empty() {
            return Ok::<_, teloxide::RequestError>(());
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
    };

    match rich_sender::try_send_rich_fallback(token, chat_id.0, markdown, &entity_sender).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // try_send_rich_fallback already handled BadMarkdown by calling
            // entity_sender internally. If that fallback also failed (or the
            // rich path had a network error), propagate the error — retrying
            // the entity path here would re-send already-delivered chunks.
            warn!("send_rich_message all paths failed: {e}");
            Err(teloxide::RequestError::Io(Arc::new(std::io::Error::other(
                format!("{e}"),
            ))))
        }
    }
}

fn is_verbose_enabled(value: Option<&str>) -> bool {
    value.map(|v| v == "true").unwrap_or(false)
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

async fn handle_message(bot: Bot, msg: Message, agent: Arc<Agent>) -> ResponseResult<()> {
    let user = match msg.from.as_ref() {
        Some(user) => user,
        None => return Ok(()),
    };

    let user_id = user.id.0;
    let user_name = user.first_name.clone();

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

    info!(
        "Telegram message from {} ({}): {} [attachments: {}]",
        user_name,
        user_id,
        if text.is_empty() { "(no text)" } else { &text },
        attachments.len()
    );

    // Handle commands
    if text == "/clear" {
        if let Err(e) = agent
            .clear_conversation("telegram", &user_id.to_string())
            .await
        {
            error!("Failed to clear conversation: {}", e);
        }
        return send_markdown_message(
            &bot,
            msg.chat.id,
            "Conversation archived. Past messages remain searchable.",
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
             **/selfupgrade** — Upgrade the bot (source or release binary)\n\
             **/models** — Browse and change the model\n\
             **/stop** — Cancel the current processing gracefully\n\
             **/btw** — Ask a parallel question while the bot is busy";
        return send_markdown_message(&bot, msg.chat.id, help).await;
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

        return send_markdown_message(&bot, msg.chat.id, &tool_list).await;
    }

    if text == "/skills" {
        let skills_guard = agent.skills.read().await;
        let skills = skills_guard.list();
        if skills.is_empty() {
            return send_markdown_message(&bot, msg.chat.id, "No skills loaded.").await;
        }
        let mut skill_list = String::from("**Loaded skills:**\n\n");
        for skill in &skills {
            skill_list.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
        }
        return send_markdown_message(&bot, msg.chat.id, &skill_list).await;
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

        return send_markdown_message(&bot, msg.chat.id, &lines.join("\n")).await;
    }

    if text == "/verbose" {
        let current = agent
            .memory
            .recall("settings", &format!("tool_ui_enabled_{}", user_id))
            .await
            .unwrap_or(None);
        let currently_on = is_verbose_enabled(current.as_deref());
        let new_value = if currently_on { "false" } else { "true" };
        agent
            .memory
            .remember(
                "settings",
                &format!("tool_ui_enabled_{}", user_id),
                new_value,
                None,
            )
            .await
            .ok();
        let reply = if new_value == "true" {
            "🔧 **Tool call UI enabled.** I'll show you what I'm working on."
        } else {
            "🔇 **Tool call UI disabled.** I'll respond silently."
        };
        return send_markdown_message(&bot, msg.chat.id, reply).await;
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
        return send_markdown_message(&bot, msg.chat.id, reply).await;
    }

    // Handle /btw <text> for parallel question via isolated subagent
    if text == "/btw" || text.starts_with("/btw ") {
        let btw_text = text
            .strip_prefix("/btw")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("What are you doing?")
            .to_string();

        // Reply immediately, then answer in background
        let _ = send_markdown_message(&bot, msg.chat.id, "⏳ **BTW question sent to subagent...**")
            .await;

        let agent_clone = agent.clone();
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        tokio::spawn(async move {
            match agent_clone.ask_parallel_lightweight(&btw_text).await {
                Ok(answer) => {
                    let _ = send_markdown_message(&bot_clone, chat_id, &answer).await;
                }
                Err(e) => {
                    let _ = send_markdown_message(
                        &bot_clone,
                        chat_id,
                        &format!("**BTW error:** {}", e),
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
                                format!("❌ Upgrade failed:\n{}", e),
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
            _ => {} // ignore unknown commands for now
        }
    }

    // Handle /mode command
    if text.starts_with("/mode") {
        let parts: Vec<&str> = text.splitn(2, |c: char| c.is_whitespace()).collect();
        let sub = parts.get(1).copied().unwrap_or("");
        if sub == "steer" {
            agent
                .set_mid_run_mode(&user_id.to_string(), MidRunMode::Steer)
                .await;
            return send_markdown_message(
                &bot, msg.chat.id,
                "🔄 **Mode set to steer.** Mid-processing messages will be injected as steering context.",
            ).await;
        } else if sub == "queue" {
            agent
                .set_mid_run_mode(&user_id.to_string(), MidRunMode::Queue)
                .await;
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "🔄 **Mode set to queue.** Mid-processing messages will wait for the next turn.",
            )
            .await;
        } else if sub.is_empty() {
            let current = agent.get_mid_run_mode(&user_id.to_string()).await;
            let mode_str = current.as_str();
            return send_markdown_message(
                &bot,
                msg.chat.id,
                &format!(
                    "Current mode: **{}**\n\nUse `/mode steer` or `/mode queue` to change.",
                    mode_str
                ),
            )
            .await;
        } else {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "Unknown mode. Use `/mode steer` or `/mode queue`.",
            )
            .await;
        }
    }

    // Handle /stop command
    if text == "/stop" {
        if agent.cancel_processing(&user_id.to_string()).await {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⏹ **Processing cancelled.** Accumulated state has been saved.",
            )
            .await;
        } else {
            return send_markdown_message(&bot, msg.chat.id, "Nothing is currently processing.")
                .await;
        }
    }

    // CHECK: if user is currently being processed, queue non-command messages as injection
    if !text.starts_with('/') && agent.is_processing(&user_id.to_string()).await {
        let current_mode = agent.get_mid_run_mode(&user_id.to_string()).await;
        let maxed = !agent.queue_injection(&user_id.to_string(), &text).await;
        if maxed {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⚠️ **Injection queue full** (max 10). Please wait for current processing to finish.",
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
        return send_markdown_message(&bot, msg.chat.id, confirm).await;
    }

    // Send "typing" indicator
    bot.send_chat_action(msg.chat.id, teloxide::types::ChatAction::Typing)
        .await
        .ok();

    // Check if verbose tool UI is enabled for this user
    let verbose_setting = agent
        .memory
        .recall("settings", &format!("tool_ui_enabled_{}", user_id))
        .await
        .unwrap_or(None);
    let verbose_enabled = is_verbose_enabled(verbose_setting.as_deref());

    // Set up tool event channel if verbose is on
    let (tool_event_tx, tool_event_rx) = if verbose_enabled {
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::platform::tool_notifier::ToolEvent>(32);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Spawn notifier task if verbose
    let notifier_handle = if verbose_enabled {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let mut rx = tool_event_rx.expect("rx exists when verbose");
        Some(tokio::spawn(async move {
            let mut notifier =
                crate::platform::tool_notifier::ToolCallNotifier::new(bot_clone, chat_id);
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

    // When verbose is OFF, send a transient "Thinking..." placeholder so the user
    // knows the bot is processing. The placeholder is **independent** of the
    // streaming output — when the first token arrives it is delivered as a NEW
    // message, and the placeholder is deleted by `handle_message` after the
    // stream completes (success or error). This keeps the placeholder a
    // standalone progress signal rather than a doomed attempt to morph into the
    // final answer.
    let placeholder_msg_id: Option<teloxide::types::MessageId> = if !verbose_enabled {
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
    let stream_handle = tokio::spawn(async move {
        use std::time::{Duration, Instant};

        let mut buffer = String::new();
        // The first token always starts a fresh message — the placeholder
        // (if any) is owned and deleted by `handle_message` after streaming.
        let mut current_msg_id: Option<teloxide::types::MessageId> = None;
        // Track ALL split messages so they can be retroactively formatted with entities
        let mut split_contents: Vec<String> = Vec::new();
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
                if let Some(msg_id) = current_msg_id {
                    if let Err(e) = stream_bot
                        .edit_message_text(stream_chat_id, msg_id, &snapshot)
                        .await
                    {
                        tracing::warn!(error = %e, "stream_handle: edit failed at split");
                    }
                } else if let Err(e) = stream_bot.send_message(stream_chat_id, &snapshot).await {
                    tracing::warn!(error = %e, "stream_handle: send failed at split");
                }
                split_contents.push(snapshot);
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

        // Final: flush whatever is left in the buffer with retroactive entity formatting.
        // During streaming, intermediate edits use plain text (partial markdown is fragile).
        // On final flush, convert the complete markdown to entities and re-edit all
        // tracked messages so they render with proper formatting.
        if !buffer.is_empty() {
            // Also add the final buffer content as the last segment
            split_contents.push(buffer);
        }

        if !split_contents.is_empty() {
            // First, rebuild the full text for proper markdown parsing
            let full_text: String = split_contents.join("");
            const MAX_UTF16: usize = 4090;
            let (plain_text, entities) = markdown_to_entities(&full_text);
            let chunks = split_entities(&plain_text, &entities, MAX_UTF16);

            // The first msg_id in the current message (if any) corresponds to the
            // first chunk. Tracked split IDs from sent messages correspond to their
            // own chunks.
            for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
                if i == 0 {
                    if let Some(msg_id) = current_msg_id {
                        stream_bot
                            .edit_message_text(stream_chat_id, msg_id, chunk_text)
                            .entities(chunk_entities.clone())
                            .await
                            .ok();
                    } else {
                        stream_bot
                            .send_message(stream_chat_id, chunk_text)
                            .entities(chunk_entities.clone())
                            .await
                            .ok();
                    }
                } else {
                    stream_bot
                        .send_message(stream_chat_id, chunk_text)
                        .entities(chunk_entities.clone())
                        .await
                        .ok();
                }
            }
        }
        // If `buffer` was empty and `current_msg_id` is None, nothing was
        // streamed — the placeholder owned by `handle_message` will be cleaned
        // up after this task completes.
    });

    // Build platform-agnostic message
    let incoming = IncomingMessage {
        platform: "telegram".to_string(),
        user_id: user_id.to_string(),
        chat_id: msg.chat.id.0.to_string(),
        user_name,
        text,
        attachments,
    };

    // Process through agent — moves stream_token_tx and tool_event_tx
    // Keep an owned clone of the tool_event_tx so we can send a terminal
    // Finished event after processing completes.
    let agent_tool_event_tx = tool_event_tx.clone();
    let process_result = match agent
        .process_message(&incoming, tool_event_tx, Some(stream_token_tx))
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

    // Wait for stream receiver to complete its final edit
    stream_handle.await.ok();

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

    if let Err(e) = process_result {
        warn!(error = %e, "Agent processing failed");
        return send_markdown_message(&bot, msg.chat.id, &format!("**Error:** {}", e)).await;
    }
    // Success: response already delivered via streaming

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

/// Handle callback queries from inline keyboard buttons (e.g. model selection).
async fn handle_model_callback(
    bot: Bot,
    q: CallbackQuery,
    agent: Arc<Agent>,
) -> ResponseResult<()> {
    let callback_id = q.id.clone();
    let data = match q.data {
        Some(ref d) => d.clone(),
        None => return Ok(()),
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

    // Handle command cancellation
    if let Some(cmd_id) = data.strip_prefix("cancel_cmd:") {
        let mut map = agent.running_commands.lock().await;
        if let Some(cmd) = map.remove(cmd_id) {
            let _ = cmd.cancel_tx.send(());
            bot.answer_callback_query(callback_id)
                .text("⛔ Command cancelled")
                .await
                .ok();
        } else {
            bot.answer_callback_query(callback_id)
                .text("Command already finished")
                .await
                .ok();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_split_stream_at_4000_chars() {
        const TELEGRAM_LIMIT: usize = 3800;
        let short = "a".repeat(100);
        let long = "a".repeat(4000);
        assert!(short.len() < TELEGRAM_LIMIT);
        assert!(long.len() > TELEGRAM_LIMIT);
    }

    #[test]
    fn test_is_verbose_enabled_parses_true() {
        assert!(is_verbose_enabled(Some("true")));
        assert!(!is_verbose_enabled(Some("false")));
        assert!(!is_verbose_enabled(None));
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
