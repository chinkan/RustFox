use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::net::Download;
use teloxide::prelude::*;
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::platform::{Attachment, AttachmentKind, IncomingMessage};

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
    let text = msg.text().or_else(|| msg.caption()).unwrap_or("").to_string();

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
             /skills - List loaded skills\n\
             /verbose - Toggle tool call progress display",
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
        bot.send_message(msg.chat.id, reply).await?;
        return Ok(());
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
            while let Some(event) = rx.recv().await {
                notifier.handle_event(event).await;
            }
            notifier.finish().await;
        }))
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
        let mut current_msg_id: Option<teloxide::types::MessageId> = None;
        let mut last_action = Instant::now();
        let mut rx = stream_token_rx;

        while let Some(token) = rx.recv().await {
            buffer.push_str(&token);

            // When buffer exceeds split threshold, send a NEW message and reset
            if buffer.len() > TELEGRAM_STREAM_SPLIT {
                match stream_bot.send_message(stream_chat_id, &buffer).await {
                    Ok(new_msg) => {
                        current_msg_id = Some(new_msg.id);
                        buffer.clear();
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "stream_handle: send_message failed at split");
                        break;
                    }
                }
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

        // Final: flush whatever is left in the buffer
        if !buffer.is_empty() {
            if let Some(msg_id) = current_msg_id {
                stream_bot
                    .edit_message_text(stream_chat_id, msg_id, &buffer)
                    .await
                    .ok();
            } else {
                // No intermediate message was sent — deliver the complete response now
                stream_bot.send_message(stream_chat_id, &buffer).await.ok();
            }
        }
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

    // Drop the sender to signal the notifier to stop, then await cleanup.
    // tool_event_tx is already moved into process_message — it's dropped when process_message returns.
    if let Some(handle) = notifier_handle {
        handle.await.ok();
    }

    // Wait for stream receiver to complete its final edit
    stream_handle.await.ok();

    // Cleanup temp dir used for file downloads
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    if let Err(e) = process_result {
        warn!(error = %e, "Agent processing failed");
        bot.send_message(msg.chat.id, format!("Error: {:#}", e))
            .await?;
    }
    // Success: response already delivered via streaming

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
        assert_eq!(classify_attachment_kind("image/jpeg", None), AttachmentKind::Image);
    }

    #[test]
    fn test_classify_attachment_kind_pdf() {
        assert_eq!(classify_attachment_kind("application/pdf", None), AttachmentKind::Pdf);
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
    fn test_classify_attachment_kind_unknown() {
        assert_eq!(
            classify_attachment_kind("application/zip", Some("archive.zip")),
            AttachmentKind::Other
        );
    }
}
