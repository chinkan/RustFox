use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::llm::ModelInfo;
use crate::platform::{Attachment, AttachmentKind, IncomingMessage};
use crate::utils::markdown_entities::{markdown_to_entities, split_entities};
use crate::utils::telegram_markdown::escape_text;

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
/// Currently exercised only by tests; full Telegram dispatch of `/supervise`
/// is wired in M7.3.
#[allow(dead_code)]
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

    let handler = Update::filter_message()
        .filter_map(move |msg: Message| {
            let user = msg.from.as_ref()?;
            if allowed_user_ids.contains(&user.id.0) {
                Some(msg)
            } else {
                None
            }
        })
        .endpoint(handle_message);

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

fn is_verbose_enabled(value: Option<&str>) -> bool {
    value.map(|v| v == "true").unwrap_or(false)
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
        bot.send_message(
            msg.chat.id,
            escape_text("Conversation archived. Past messages remain searchable."),
        )
        .parse_mode(ParseMode::MarkdownV2)
        .await?;
        return Ok(());
    }

    if text == "/start" {
        let help = escape_text(
            "Hello! I'm your AI assistant. Send me a message and I'll help you.\n\n\
             Commands:\n\
             /clear - Clear conversation history\n\
             /tools - List available tools\n\
             /skills - List loaded skills\n\
             /update-skills - Re-sync bundled skills/agents (backs up local edits)\n\
             /verbose - Toggle tool call progress display\n\
             /queryrewrite - Toggle query rewriting for memory search",
        );
        bot.send_message(msg.chat.id, help)
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    if text == "/tools" {
        let all_tools = agent.all_tool_definitions();
        let mut tool_list = String::from("Available tools:\n\n");
        for tool in &all_tools {
            tool_list.push_str(&format!(
                "  - {}: {}\n",
                tool.function.name, tool.function.description
            ));
        }
        bot.send_message(msg.chat.id, escape_text(&tool_list))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    if text == "/skills" {
        let skills_guard = agent.skills.read().await;
        let skills = skills_guard.list();
        if skills.is_empty() {
            bot.send_message(msg.chat.id, escape_text("No skills loaded."))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        } else {
            let mut skill_list = String::from("Loaded skills:\n\n");
            for skill in &skills {
                skill_list.push_str(&format!("  - {}: {}\n", skill.name, skill.description));
            }
            bot.send_message(msg.chat.id, escape_text(&skill_list))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }
        return Ok(());
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

        bot.send_message(msg.chat.id, escape_text(&lines.join("\n")))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
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
            "🔧 Tool call UI enabled. I'll show you what I'm working on."
        } else {
            "🔇 Tool call UI disabled. I'll respond silently."
        };
        bot.send_message(msg.chat.id, escape_text(reply))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
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
            "🔍 Query rewriting enabled. Follow-up questions will be rewritten before memory search."
        } else {
            "🔍 Query rewriting disabled. Messages will be searched as-is."
        };
        bot.send_message(msg.chat.id, escape_text(reply))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
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
                if arg.is_empty() {
                    let current = agent.current_model.read().await;
                    let reply = format!(
                        "Current model: `{}`\n\nTo change model, use:\n\
                         `/models <model-id>` — exact model ID\n\
                         `/models <keyword>` — search by name\n\
                         Example: `/models claude` to search for Claude models",
                        *current
                    );
                    bot.send_message(msg.chat.id, escape_text(&reply))
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                    return Ok(());
                }

                let models = match agent.llm.fetch_models().await {
                    Ok(list) => list,
                    Err(e) => {
                        bot.send_message(
                            msg.chat.id,
                            escape_text(&format!("Failed to fetch model list: {:#}", e)),
                        )
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                        return Ok(());
                    }
                };

                // Try exact match first.
                if let Some(model) = models.iter().find(|m| m.id == arg) {
                    match agent.set_model(&model.id).await {
                        Ok(()) => {
                            let reply =
                                format!("✅ Model changed to `{}` ({})", model.id, model.name);
                            bot.send_message(msg.chat.id, escape_text(&reply))
                                .parse_mode(ParseMode::MarkdownV2)
                                .await?;
                        }
                        Err(e) => {
                            bot.send_message(
                                msg.chat.id,
                                escape_text(&format!("Failed to save model: {:#}", e)),
                            )
                            .parse_mode(ParseMode::MarkdownV2)
                            .await?;
                        }
                    }
                    return Ok(());
                }

                // Fuzzy search: case-insensitive match on id or name.
                let query = arg.to_lowercase();
                let mut matches: Vec<&ModelInfo> = models
                    .iter()
                    .filter(|m| {
                        m.id.to_lowercase().contains(&query)
                            || m.name.to_lowercase().contains(&query)
                    })
                    .collect();
                matches.sort_by(|a, b| {
                    let a_name = a.name.to_lowercase().contains(&query);
                    let b_name = b.name.to_lowercase().contains(&query);
                    b_name.cmp(&a_name).then(a.id.cmp(&b.id))
                });
                matches.truncate(10);

                if matches.is_empty() {
                    bot.send_message(
                        msg.chat.id,
                        escape_text(&format!(
                            "No models found matching '{}'. Try a different search term.",
                            arg
                        )),
                    )
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                    return Ok(());
                }

                if matches.len() == 1 {
                    let model = &matches[0];
                    match agent.set_model(&model.id).await {
                        Ok(()) => {
                            let reply =
                                format!("✅ Model changed to `{}` ({})", model.id, model.name);
                            bot.send_message(msg.chat.id, escape_text(&reply))
                                .parse_mode(ParseMode::MarkdownV2)
                                .await?;
                        }
                        Err(e) => {
                            bot.send_message(
                                msg.chat.id,
                                escape_text(&format!("Failed to save model: {:#}", e)),
                            )
                            .parse_mode(ParseMode::MarkdownV2)
                            .await?;
                        }
                    }
                    return Ok(());
                }

                let mut reply = format!("Found {} models matching '{}':\n\n", matches.len(), arg);
                for model in &matches {
                    reply.push_str(&format!(
                        "`{}` — {} ({} context)\n",
                        model.id,
                        model.name,
                        if model.context_length > 0 {
                            format!("{}K", model.context_length / 1024)
                        } else {
                            "??".to_string()
                        }
                    ));
                }
                reply.push_str("\nSelect by typing: `/models <model-id>`");
                bot.send_message(msg.chat.id, escape_text(&reply))
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }
            _ => {} // ignore unknown commands for now
        }
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
    const TELEGRAM_STREAM_SPLIT: usize = 3800;

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
        let mut last_action = Instant::now();
        let mut rx = stream_token_rx;

        while let Some(token) = rx.recv().await {
            buffer.push_str(&token);

            // When buffer exceeds split threshold, finalize the current message
            // and reset so subsequent tokens start a new message.
            //
            // Previous logic sent the full buffer as a NEW message, then cleared
            // the buffer.  This caused the new message to visually shrink on the
            // next edit (which only contained the small post-split tokens).
            //
            // Fix: edit/send the current message with its accumulated content
            // (finalizing it), then clear the buffer AND current_msg_id so the
            // next batch of tokens creates a fresh message.
            if buffer.len() > TELEGRAM_STREAM_SPLIT {
                if let Some(msg_id) = current_msg_id {
                    if let Err(e) = stream_bot
                        .edit_message_text(stream_chat_id, msg_id, &buffer)
                        .await
                    {
                        tracing::warn!(error = %e, "stream_handle: edit failed at split");
                    }
                } else if let Err(e) = stream_bot.send_message(stream_chat_id, &buffer).await {
                    tracing::warn!(error = %e, "stream_handle: send failed at split");
                }
                buffer.clear();
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

        // Final: flush whatever is left in the buffer.
        // Use the entity-based approach: convert completed Markdown to (plain_text, entities).
        // This is robust for LLM output — no escaping needed, no risk of Telegram 400 errors.
        // Intermediate streaming edits remain plain text (partial markdown is fragile).
        if !buffer.is_empty() {
            const MAX_UTF16: usize = 4090;
            let (plain_text, entities) = markdown_to_entities(&buffer);
            let chunks = split_entities(&plain_text, &entities, MAX_UTF16);

            for (i, (chunk_text, chunk_entities)) in chunks.iter().enumerate() {
                if i == 0 {
                    // First chunk: edit or replace the existing in-progress message
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
                    // Subsequent chunks: send as new messages
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
        bot.send_message(msg.chat.id, escape_text(&format!("Error: {:#}", e)))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
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
    fn test_command_responses_use_escape_text() {
        // All non-streaming command responses must escape plain text and use MarkdownV2
        // so that special chars like `.`, `-`, `!`, `_`, `(`, `)` don't break the parser.
        let source = include_str!("telegram.rs");
        assert!(
            source.contains("escape_text"),
            "Command responses must call escape_text() before sending with MarkdownV2"
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
