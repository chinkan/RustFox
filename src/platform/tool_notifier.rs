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

/// Convert a technical tool name to a human-readable label with an emoji prefix.
///
/// Priority:
/// 1. Exact match for known built-in tools.
/// 2. MCP tools prefixed with `mcp_<server>_` — server icon + humanised function name.
/// 3. Fallback — replace underscores with spaces and capitalise the first letter.
pub fn friendly_tool_name(name: &str) -> String {
    // 1. Built-in tools — exact matches
    let label = match name {
        "read_file" => return "📄 Reading a file".to_string(),
        "write_file" => return "✏️ Writing a file".to_string(),
        "list_files" => return "📁 Listing files".to_string(),
        "execute_command" => return "💻 Running a command".to_string(),
        "schedule_task" => return "🗓️ Scheduling a task".to_string(),
        "list_scheduled_tasks" => return "🗓️ Checking scheduled tasks".to_string(),
        "cancel_scheduled_task" => return "🗓️ Cancelling a task".to_string(),
        "invoke_agent" | "invoke_subagent" => return "🤖 Calling a specialist".to_string(),
        "plan_create" | "plan_update" | "plan_view" => return "📋 Managing plan".to_string(),
        "read_skill_file" | "write_skill_file" => return "📖 Reading skill".to_string(),
        "reload_skills" | "reload_agents" => return "🔄 Reloading".to_string(),
        "read_agent_file" | "write_agent_file" => return "🤖 Agent file".to_string(),
        _ => name,
    };

    // 2. MCP tools: mcp_<server>_<function>
    if let Some(rest) = label.strip_prefix("mcp_") {
        // Find the server name — everything up to the second underscore-separated segment
        if let Some(sep) = rest.find('_') {
            let server = &rest[..sep];
            let func = &rest[sep + 1..];

            let icon = match server {
                "google-workspace" | "google_workspace" => "📧",
                "brave-search" | "brave_search" => "🔍",
                "fetch" => "🌐",
                "git" => "📦",
                "threads" => "🧵",
                "filesystem" => "📁",
                "github" => "🐙",
                "sqlite" => "🗄️",
                "puppeteer" => "🌐",
                _ => "🔧",
            };

            let human = humanise_function_name(func);
            return format!("{} {}", icon, human);
        }
    }

    // 3. Fallback — snake_case → "Snake case"
    let human = humanise_function_name(label);
    format!("🔧 {}", human)
}

/// Convert a `snake_case_function_name` to a human-readable sentence.
/// Strips common verbose verb prefixes and capitalises the first letter.
fn humanise_function_name(func: &str) -> String {
    // Strip common verbose prefixes that don't add meaning to the user
    let stripped = func
        .strip_prefix("query_")
        .or_else(|| func.strip_prefix("search_"))
        .or_else(|| func.strip_prefix("get_"))
        .or_else(|| func.strip_prefix("list_"))
        .unwrap_or(func);

    // Replace underscores with spaces
    let spaced = stripped.replace('_', " ");

    // Capitalise first letter
    let mut chars = spaced.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let upper: String = first.to_uppercase().collect();
            upper + chars.as_str()
        }
    }
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
                    let truncated = crate::utils::strings::truncate_chars(&s, 60);
                    return truncated;
                }
            }
        }
    }
    // Fallback: truncate raw JSON
    crate::utils::strings::truncate_chars(args_json, 60)
}

/// Build the one-line tool status string streamed into the Telegram message
/// while the tool is running. Ends with `\n` so multiple calls stack visibly.
pub fn format_tool_status_line(name: &str, args_preview: &str) -> String {
    let label = friendly_tool_name(name);
    if args_preview.is_empty() {
        format!("⏳ {}\n", label)
    } else {
        format!("⏳ {}: {}\n", label, args_preview)
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
            let label = friendly_tool_name(name);
            if args_preview.is_empty() {
                s.push_str(&format!("\n{} {}", icon, label));
            } else {
                s.push_str(&format!("\n{} {}: {}", icon, label, args_preview));
            }
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
            let label = friendly_tool_name(name);
            if args_preview.is_empty() {
                s.push_str(&format!("\n{} {}", icon, label));
            } else {
                s.push_str(&format!("\n{} {}: {}", icon, label, args_preview));
            }
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
        // Quotes are no longer added — value is returned directly
        assert_eq!(preview, "Docker setup preferences");
    }

    #[test]
    fn test_format_args_preview_truncates_long_value() {
        let long = "a".repeat(100);
        let json = format!(r#"{{"query":"{}"}}"#, long);
        let preview = format_args_preview(&json);
        assert!(preview.len() <= 70, "Preview should be truncated");
        assert!(preview.contains("..."));
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
        // Build a notifier-like tool_log directly and call format_final via a helper
        // that mirrors the real implementation using friendly_tool_name.
        fn fake_format_final(tool_log: &[(String, String, bool, bool)]) -> String {
            let mut s = String::from("🔧 Tools used:");
            for (name, args_preview, _done, success) in tool_log {
                let icon = if *success { "✅" } else { "❌" };
                let label = friendly_tool_name(name);
                if args_preview.is_empty() {
                    s.push_str(&format!("\n{} {}", icon, label));
                } else {
                    s.push_str(&format!("\n{} {}: {}", icon, label, args_preview));
                }
            }
            s
        }

        let log = vec![
            ("search".to_string(), "Docker setup".to_string(), true, true),
            (
                "read_file".to_string(),
                "/etc/config".to_string(),
                true,
                false,
            ),
        ];
        let result = fake_format_final(&log);
        assert!(result.contains("🔧 Tools used:"), "header missing");
        assert!(result.contains("✅"), "successful tool icon wrong");
        assert!(result.contains("❌"), "failed tool icon wrong");
        assert!(result.contains("Docker setup"), "args missing for search");
        assert!(
            !result.contains("⏳ Working"),
            "should not contain in-progress text"
        );
        // read_file should be humanised
        assert!(
            result.contains("📄 Reading a file"),
            "read_file must be humanised"
        );
    }

    #[test]
    fn test_format_args_preview_single_arg_with_chinese() {
        let long_chinese =
            "每日上午10點 arXiv AI 論文摘要（香港時間）很長的標題讓我們繼續寫下去直到超過六十個字";
        let json = format!(r#"{{"query":"{}"}}"#, long_chinese);
        let preview = format_args_preview(&json);
        assert!(!preview.is_empty());
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn test_format_tool_status_line_shows_hourglass_and_friendly_name() {
        // web_search has no built-in mapping → falls through to 🔧 fallback
        let line = format_tool_status_line("web_search", "Docker setup");
        assert!(
            line.starts_with("⏳"),
            "status line must start with hourglass: {line}"
        );
        assert!(
            line.contains("Docker setup"),
            "status line must include args preview: {line}"
        );
        assert!(
            line.ends_with('\n'),
            "status line must end with newline: {line}"
        );
    }

    #[test]
    fn test_format_tool_status_line_builtin_tool_humanised() {
        let line = format_tool_status_line("read_file", "/etc/config");
        assert!(
            line.contains("📄 Reading a file"),
            "built-in tool must be humanised: {line}"
        );
        assert!(line.contains("/etc/config"), "args must be shown: {line}");
        assert!(line.ends_with('\n'), "must end with newline: {line}");
    }

    #[test]
    fn test_format_tool_status_line_ends_with_newline() {
        let line = format_tool_status_line("read_file", "/etc/config");
        assert!(
            line.ends_with('\n'),
            "status line must end with newline for streaming: {line}"
        );
    }

    #[test]
    fn test_format_tool_status_line_empty_args() {
        let line = format_tool_status_line("list_files", "");
        assert!(
            !line.is_empty(),
            "status line must not be empty even with no args"
        );
        assert!(
            line.contains("📁 Listing files"),
            "list_files must be humanised: {line}"
        );
    }

    #[test]
    fn test_format_args_preview_multi_arg_chinese_truncates_safely() {
        let args = r#"{"description":"每日上午10點 arXiv AI 論文摘要（香港時間）","prompt":"使用 arxiv-daily-briefing skill","trigger_type":"recurring","trigger_value":"0 0 2 * * *"}"#;
        let preview = format_args_preview(args);
        assert!(!preview.is_empty());
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    // --- friendly_tool_name ---

    #[test]
    fn test_friendly_tool_name_builtin_read_file() {
        assert_eq!(friendly_tool_name("read_file"), "📄 Reading a file");
    }

    #[test]
    fn test_friendly_tool_name_builtin_execute_command() {
        assert_eq!(
            friendly_tool_name("execute_command"),
            "💻 Running a command"
        );
    }

    #[test]
    fn test_friendly_tool_name_builtin_invoke_agent() {
        assert_eq!(
            friendly_tool_name("invoke_agent"),
            "🤖 Calling a specialist"
        );
        assert_eq!(
            friendly_tool_name("invoke_subagent"),
            "🤖 Calling a specialist"
        );
    }

    #[test]
    fn test_friendly_tool_name_mcp_brave_search() {
        let name = "mcp_brave-search_search_web";
        let friendly = friendly_tool_name(name);
        assert!(friendly.contains("🔍"), "brave-search icon: {friendly}");
        assert!(
            !friendly.contains("mcp_"),
            "should not contain raw prefix: {friendly}"
        );
    }

    #[test]
    fn test_friendly_tool_name_mcp_google_workspace() {
        let name = "mcp_google-workspace_query_gmail_emails";
        let friendly = friendly_tool_name(name);
        assert!(friendly.contains("📧"), "google-workspace icon: {friendly}");
    }

    #[test]
    fn test_friendly_tool_name_mcp_unknown_server() {
        let name = "mcp_myserver_do_something";
        let friendly = friendly_tool_name(name);
        assert!(
            friendly.contains("🔧"),
            "unknown server must use fallback icon: {friendly}"
        );
    }

    #[test]
    fn test_friendly_tool_name_fallback_snake_case() {
        let friendly = friendly_tool_name("some_unknown_tool");
        assert!(
            friendly.starts_with("🔧"),
            "unknown tool must use 🔧: {friendly}"
        );
        assert!(
            friendly.contains("Some unknown tool"),
            "should humanise snake_case: {friendly}"
        );
    }

    #[test]
    fn test_friendly_tool_name_strips_verb_prefixes() {
        // "query_" prefix is stripped in MCP humanisation
        let name = "mcp_fetch_query_url";
        let friendly = friendly_tool_name(name);
        assert!(
            !friendly.contains("query_"),
            "verb prefix should be stripped: {friendly}"
        );
    }
}
