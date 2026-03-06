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

    // Send "typing" indicator
    bot.send_chat_action(msg.chat.id, teloxide::types::ChatAction::Typing)
        .await
        .ok();

    // Build platform-agnostic message
    let incoming = IncomingMessage {
        platform: "telegram".to_string(),
        user_id: user_id.to_string(),
        chat_id: msg.chat.id.0.to_string(),
        user_name,
        text,
    };

    // Process through agent
    match agent.process_message(&incoming).await {
        Ok(response) => {
            if response.is_empty() {
                warn!(
                    user_id = user_id,
                    "Agent returned empty response — nothing will be sent to Telegram"
                );
            }
            let chunks = split_message(&response, 4000);
            let total = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if chunk.is_empty() {
                    continue;
                }
                match bot.send_message(msg.chat.id, &chunk).await {
                    Ok(_) => {
                        if total > 1 {
                            info!(
                                "Sent Telegram chunk {}/{} ({} chars)",
                                i + 1,
                                total,
                                chunk.len()
                            );
                        }
                    }
                    Err(e) => {
                        error!(
                            user_id = user_id,
                            chunk = i + 1,
                            total_chunks = total,
                            "Failed to send Telegram message: {:#}",
                            e
                        );
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
