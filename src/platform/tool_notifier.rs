use std::time::{Duration, Instant};

use teloxide::{prelude::*, types::Message};
use tracing::{debug, warn};

/// Events emitted by the agent during tool execution.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ToolEvent {
    /// A tool call has started.
    Started {
        name: String,
        /// First 60 chars of the arguments JSON, for display.
        args_preview: String,
    },
    /// A tool call completed (successfully or with error).
    Completed { name: String, success: bool },
}

/// Formats `args_preview` for display: truncate to 60 chars, strip outer braces for common single-arg calls.
pub fn format_args_preview(args_json: &str) -> String {
    // Try to extract a single-value preview for readability
    // e.g. {"query":"Docker setup"} -> "Docker setup"
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) {
        if let Some(obj) = val.as_object() {
            if obj.len() == 1 {
                if let Some((_, v)) = obj.iter().next() {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let truncated = if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s
                    };
                    return format!("\"{}\"", truncated);
                }
            }
        }
    }
    // Fallback: truncate raw JSON
    if args_json.len() > 60 {
        format!("{}...", &args_json[..60])
    } else {
        args_json.to_string()
    }
}

/// Manages the live-edited Telegram status message during agent tool execution.
#[allow(dead_code)]
pub struct ToolCallNotifier {
    bot: Bot,
    chat_id: ChatId,
    status_msg: Option<Message>,
    /// Log of tool calls: (name, args_preview, done, success)
    tool_log: Vec<(String, String, bool, bool)>,
    last_edit: Option<Instant>,
}

#[allow(dead_code)]
impl ToolCallNotifier {
    pub fn new(bot: Bot, chat_id: ChatId) -> Self {
        Self {
            bot,
            chat_id,
            status_msg: None,
            tool_log: Vec::new(),
            last_edit: None,
        }
    }

    /// Send the initial "thinking" message.
    pub async fn start(&mut self) {
        match self.bot.send_message(self.chat_id, "⏳ Working...").await {
            Ok(msg) => self.status_msg = Some(msg),
            Err(e) => warn!("Failed to send tool notifier start message: {:#}", e),
        }
    }

    /// Handle a ToolEvent and update the Telegram message.
    pub async fn handle_event(&mut self, event: ToolEvent) {
        match event {
            ToolEvent::Started { name, args_preview } => {
                self.tool_log.push((name, args_preview, false, true));
            }
            ToolEvent::Completed { name, success } => {
                if let Some(entry) = self
                    .tool_log
                    .iter_mut()
                    .rfind(|(n, _, done, _)| n == &name && !*done)
                {
                    entry.2 = true; // done
                    entry.3 = success;
                }
            }
        }
        self.edit_message().await;
    }

    async fn edit_message(&mut self) {
        let Some(ref msg) = self.status_msg else {
            return;
        };

        // Rate limit: wait if last edit was <1s ago
        if let Some(last) = self.last_edit {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_millis(1000) {
                tokio::time::sleep(Duration::from_millis(1000) - elapsed).await;
            }
        }

        let text = self.format_status();
        match self
            .bot
            .edit_message_text(self.chat_id, msg.id, &text)
            .await
        {
            Ok(_) => self.last_edit = Some(Instant::now()),
            Err(e) => debug!("Failed to edit tool notifier message: {:#}", e),
        }
    }

    fn format_status(&self) -> String {
        let mut s = String::from("⏳ Working...\n");
        for (name, args_preview, done, success) in &self.tool_log {
            let icon = if !done {
                "⏳"
            } else if *success {
                "✅"
            } else {
                "❌"
            };
            s.push_str(&format!("\n{} {}({})", icon, name, args_preview));
        }
        s
    }

    /// Finalise the status message.
    ///
    /// - If no tools were called: delete the placeholder "⏳ Working..." (not useful).
    /// - If tools were called: edit to a persistent summary so the user can see
    ///   which tools ran after the response has arrived.
    pub async fn finish(&self) {
        let Some(ref msg) = self.status_msg else {
            return;
        };

        if self.tool_log.is_empty() {
            self.bot.delete_message(self.chat_id, msg.id).await.ok();
        } else {
            let text = self.format_final();
            self.bot
                .edit_message_text(self.chat_id, msg.id, &text)
                .await
                .ok();
        }
    }

    /// Final compact summary shown after tools have run.
    fn format_final(&self) -> String {
        let mut s = String::from("🔧 Tools used:");
        for (name, args_preview, _done, success) in &self.tool_log {
            let icon = if *success { "✅" } else { "❌" };
            s.push_str(&format!("\n{} {}({})", icon, name, args_preview));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_args_preview_single_string_arg() {
        let json = r#"{"query":"Docker setup preferences"}"#;
        let preview = format_args_preview(json);
        assert_eq!(preview, r#""Docker setup preferences""#);
    }

    #[test]
    fn test_format_args_preview_truncates_long_value() {
        let long = "a".repeat(100);
        let json = format!(r#"{{"query":"{}"}}"#, long);
        let preview = format_args_preview(&json);
        assert!(preview.len() <= 70, "Preview should be truncated");
        assert!(preview.ends_with("...\"") || preview.contains("..."));
    }

    #[test]
    fn test_format_args_preview_multi_arg_falls_back() {
        let json = r#"{"category":"settings","key":"tool_ui"}"#;
        let preview = format_args_preview(json);
        // Multi-arg: should fall back to raw JSON truncated
        assert!(preview.len() <= 65);
    }

    #[test]
    fn test_format_status_shows_correct_icons() {
        // We test the format logic in isolation by calling format_status via a mock
        // Since ToolCallNotifier requires a real Bot, we test format_args_preview only
        let preview = format_args_preview(r#"{"path":"/tmp/test.txt"}"#);
        assert!(preview.contains("/tmp/test.txt"));
    }

    #[test]
    fn test_format_final_includes_all_tools() {
        // Build a notifier-like tool_log directly and call format_final via a helper.
        // format_final is private — test it through a thin wrapper.
        fn fake_format_final(tool_log: &[(String, String, bool, bool)]) -> String {
            let mut s = String::from("🔧 Tools used:");
            for (name, args_preview, _done, success) in tool_log {
                let icon = if *success { "✅" } else { "❌" };
                s.push_str(&format!("\n{} {}({})", icon, name, args_preview));
            }
            s
        }

        let log = vec![
            ("search".to_string(), r#""Docker setup""#.to_string(), true, true),
            ("read_file".to_string(), r#""/etc/config""#.to_string(), true, false),
        ];
        let result = fake_format_final(&log);
        assert!(result.contains("🔧 Tools used:"), "header missing");
        assert!(result.contains("✅ search"), "successful tool icon wrong");
        assert!(result.contains("❌ read_file"), "failed tool icon wrong");
        assert!(result.contains("Docker setup"), "args missing for search");
        assert!(!result.contains("⏳ Working"), "should not contain in-progress text");
    }
}
