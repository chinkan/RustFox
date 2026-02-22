use std::sync::Arc;

use anyhow::Result;
use teloxide::prelude::*;
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::platform::IncomingMessage;

/// Split long messages for Telegram's 4096 char limit
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

/// Run the Telegram bot platform
pub async fn run(
    agent: Arc<Agent>,
    allowed_user_ids: Vec<u64>,
    bot: Arc<teloxide::Bot>,
) -> Result<()> {
    let bot = (*bot).clone();

    info!("Starting Telegram platform...");

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

async fn handle_message(bot: Bot, msg: Message, agent: Arc<Agent>) -> ResponseResult<()> {
    let user = match msg.from.as_ref() {
        Some(user) => user,
        None => return Ok(()),
    };

    let user_id = user.id.0;
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => return Ok(()),
    };

    let user_name = user.first_name.clone();

    info!(
        "Telegram message from {} ({}): {}",
        user_name, user_id, text
    );

    // Handle commands
    if text == "/clear" {
        if let Err(e) = agent
            .clear_conversation("telegram", &user_id.to_string())
            .await
        {
            error!("Failed to clear conversation: {}", e);
        }
        bot.send_message(msg.chat.id, "Conversation cleared.")
            .await?;
        return Ok(());
    }

    if text == "/start" {
        bot.send_message(
            msg.chat.id,
            "Hello! I'm your AI assistant. Send me a message and I'll help you.\n\n\
             Commands:\n\
             /clear - Clear conversation history\n\
             /tools - List available tools\n\
             /skills - List loaded skills",
        )
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
        bot.send_message(msg.chat.id, tool_list).await?;
        return Ok(());
    }

    if text == "/skills" {
        let skills_guard = agent.skills.read().await;
        let skills = skills_guard.list();
        if skills.is_empty() {
            bot.send_message(msg.chat.id, "No skills loaded.").await?;
        } else {
            let mut skill_list = String::from("Loaded skills:\n\n");
            for skill in &skills {
                skill_list.push_str(&format!("  - {}: {}\n", skill.name, skill.description));
            }
            bot.send_message(msg.chat.id, skill_list).await?;
        }
        return Ok(());
    }

    // Send "typing" indicator and keep refreshing it every 4 seconds.
    // Telegram's typing status expires after ~5 s; we refresh before it lapses
    // so the user always sees activity feedback during long LLM calls.
    bot.send_chat_action(msg.chat.id, teloxide::types::ChatAction::Typing)
        .await
        .ok();
    let typing_bot = bot.clone();
    let typing_chat_id = msg.chat.id;
    let typing_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(4));
        interval.tick().await; // consume the immediate first tick
        loop {
            interval.tick().await;
            if typing_bot
                .send_chat_action(typing_chat_id, teloxide::types::ChatAction::Typing)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Channel for real-time tool activity hints
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent::AgentEvent>();

    // Spawn status task: receives ToolStarted events, manages one Telegram message
    let status_bot = bot.clone();
    let status_chat_id = msg.chat.id;
    let status_handle: tokio::task::JoinHandle<Option<teloxide::types::MessageId>> =
        tokio::spawn(async move {
            let mut status_msg_id: Option<teloxide::types::MessageId> = None;
            while let Some(event) = event_rx.recv().await {
                match event {
                    crate::agent::AgentEvent::ToolStarted { name } => {
                        let text = format!("⚙️ Calling: {}", name);
                        match status_msg_id {
                            None => {
                                // First tool — send new status message
                                if let Ok(m) =
                                    status_bot.send_message(status_chat_id, &text).await
                                {
                                    status_msg_id = Some(m.id);
                                }
                            }
                            Some(id) => {
                                // Subsequent tools — edit in place
                                let _ = status_bot
                                    .edit_message_text(status_chat_id, id, &text)
                                    .await;
                            }
                        }
                    }
                }
            }
            status_msg_id
        });

    // Build platform-agnostic message
    let incoming = IncomingMessage {
        platform: "telegram".to_string(),
        user_id: user_id.to_string(),
        chat_id: msg.chat.id.0.to_string(),
        user_name,
        text,
    };

    // Process through agent — passes the event sender for live tool hints
    let result = agent.process_message(&incoming, Some(&event_tx)).await;

    // Close the event channel so the status task exits its recv loop
    drop(event_tx);
    typing_handle.abort();

    // Wait for the status task to finish, then delete its message if one was sent
    if let Ok(Some(status_msg_id)) = status_handle.await {
        let _ = bot.delete_message(msg.chat.id, status_msg_id).await;
    }

    match result {
        Ok(response) => {
            for chunk in split_message(&response, 4000) {
                bot.send_message(msg.chat.id, chunk).await.ok();
            }
        }
        Err(e) => {
            error!("Error processing message: {:#}", e);
            bot.send_message(msg.chat.id, format!("Error: {}", e))
                .await?;
        }
    }

    Ok(())
}
