use serde_json::Value as JsonValue;
use std::time::{Duration, Instant};

// Display limits
const MAX_STATUS_TEXT_CHARS: usize = 3800;
const MAX_PLAN_STEPS_RENDERED: usize = 20;
const MAX_DISPLAY_FIELD_CHARS: usize = 60;

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
        /// Full arguments JSON, used only by display-state parsers.
        arguments_json: String,
    },
    /// A tool call completed (successfully or with error).
    Completed { name: String, success: bool },
    /// Sent by the platform when the overall request has finished.
    Finished { success: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanStepStatus {
    Todo,
    InProgress,
    Done,
    Failed,
}

impl PlanStepStatus {
    fn from_tool_status(status: &str) -> Self {
        match status {
            "done" | "completed" | "complete" => Self::Done,
            "failed" | "error" => Self::Failed,
            "in_progress" | "running" | "active" => Self::InProgress,
            _ => Self::Todo,
        }
    }

    fn marker(&self) -> &'static str {
        match self {
            Self::Todo => "[ ]",
            Self::InProgress => "[>]",
            Self::Done => "[x]",
            Self::Failed => "[!]",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanStepDisplay {
    text: String,
    status: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanDisplay {
    title: String,
    steps: Vec<PlanStepDisplay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanStepUpdate {
    step_id: usize,
    status: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolActivityState {
    Running,
    Completed,
    Failed,
}

impl ToolActivityState {
    fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolActivity {
    name: String,
    args_preview: String,
    state: ToolActivityState,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ToolDisplayState {
    plan: Option<PlanDisplay>,
    activities: Vec<ToolActivity>,
    active_plan_update_step: Option<usize>,
}

impl ToolDisplayState {
    fn handle_event(&mut self, event: ToolEvent) {
        match event {
            ToolEvent::Started {
                name,
                args_preview,
                arguments_json,
            } => {
                if name == "plan_create" {
                    if let Some(plan) = parse_plan_create(&arguments_json) {
                        self.plan = Some(plan);
                    }
                } else if name == "plan_update" {
                    self.active_plan_update_step = None;
                    if let Some(update) = parse_plan_update(&arguments_json) {
                        self.apply_plan_update(update);
                    }
                }

                self.activities.push(ToolActivity {
                    name,
                    args_preview,
                    state: ToolActivityState::Running,
                });
                if self.activities.len() > 12 {
                    self.activities.remove(0);
                }
            }
            ToolEvent::Finished { .. } => {
                // Terminal event handled by the platform-level notifier; ignore
                // here so display state focuses on per-tool activity only.
            }
            ToolEvent::Completed { name, success } => {
                if name == "plan_update" {
                    if let Some(step_id) = self.active_plan_update_step.take() {
                        if !success {
                            if let Some(plan) = self.plan.as_mut() {
                                if let Some(step) = plan.steps.get_mut(step_id) {
                                    step.status = PlanStepStatus::Failed;
                                }
                            }
                        }
                    }
                }

                if let Some(activity) = self.activities.iter_mut().rfind(|activity| {
                    activity.name == name && activity.state == ToolActivityState::Running
                }) {
                    activity.state = if success {
                        ToolActivityState::Completed
                    } else {
                        ToolActivityState::Failed
                    };
                }
            }
        }
    }

    fn has_activity(&self) -> bool {
        self.plan.is_some() || !self.activities.is_empty()
    }

    fn format_live(&self) -> String {
        self.format("⏳ Working on your request")
    }

    fn format_completed(&self) -> String {
        // Default successful header and result. Caller may adjust based on overall
        // request success vs failure when rendering the final card.
        self.format("✅ Completed")
    }

    fn apply_plan_update(&mut self, update: PlanStepUpdate) {
        self.active_plan_update_step = Some(update.step_id);
        if let Some(plan) = self.plan.as_mut() {
            if let Some(step) = plan.steps.get_mut(update.step_id) {
                step.status = update.status;
            }
        }
    }

    fn format(&self, header: &str) -> String {
        let mut text = header.to_string();

        if let Some(plan) = &self.plan {
            text.push_str("\n\nPlan\n");
            // Truncate title
            text.push_str(&crate::utils::strings::truncate_chars(
                &plan.title,
                MAX_DISPLAY_FIELD_CHARS,
            ));
            // Render a bounded number of steps
            let total = plan.steps.len();
            let cap = std::cmp::min(total, MAX_PLAN_STEPS_RENDERED);
            for (index, step) in plan.steps.iter().enumerate().take(cap) {
                text.push('\n');
                text.push_str(step.status.marker());
                text.push_str(&format!(
                    " {index}. {}",
                    crate::utils::strings::truncate_chars(&step.text, MAX_DISPLAY_FIELD_CHARS)
                ));
                // Intentionally omit rendering of any plan-update notes here to
                // avoid leaking potentially sensitive content from tool arguments.
            }
            if total > MAX_PLAN_STEPS_RENDERED {
                text.push('\n');
                let more = total - MAX_PLAN_STEPS_RENDERED;
                text.push_str(&format!("[ ] ... {} more steps", more));
            }
        }

        if !self.activities.is_empty() {
            text.push_str("\n\nTool activity");
            for activity in &self.activities {
                text.push('\n');
                text.push_str(&friendly_tool_name(&activity.name));
                if !activity.args_preview.is_empty() {
                    text.push_str(": ");
                    text.push_str(&crate::utils::strings::truncate_chars(
                        &activity.args_preview,
                        MAX_DISPLAY_FIELD_CHARS,
                    ));
                }
                text.push_str(" -- ");
                text.push_str(activity.state.label());
            }
        }

        // Clamp total text size to Telegram-safe limit
        crate::utils::strings::truncate_chars(&text, MAX_STATUS_TEXT_CHARS)
    }
}

fn parse_plan_create(arguments_json: &str) -> Option<PlanDisplay> {
    #[derive(serde::Deserialize)]
    struct PlanCreateArgs {
        title: String,
        steps: Vec<String>,
    }

    let args: PlanCreateArgs = serde_json::from_str(arguments_json).ok()?;
    Some(PlanDisplay {
        title: args.title,
        steps: args
            .steps
            .into_iter()
            .map(|text| PlanStepDisplay {
                text,
                status: PlanStepStatus::Todo,
            })
            .collect(),
    })
}

fn parse_plan_update(arguments_json: &str) -> Option<PlanStepUpdate> {
    #[derive(serde::Deserialize)]
    struct PlanUpdateArgs {
        step_id: usize,
        status: String,
    }

    let args: PlanUpdateArgs = serde_json::from_str(arguments_json).ok()?;
    Some(PlanStepUpdate {
        step_id: args.step_id,
        status: PlanStepStatus::from_tool_status(&args.status),
    })
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
        // Known server names (with both hyphen and underscore variants)
        // Sorted by length descending to match longest first (handles server names with underscores)
        let known_servers = [
            ("google-workspace", "📧"),
            ("google_workspace", "📧"),
            ("brave-search", "🔍"),
            ("brave_search", "🔍"),
            ("puppeteer", "🌐"),
            ("filesystem", "📁"),
            ("github", "🐙"),
            ("sqlite", "🗄️"),
            ("threads", "🧵"),
            ("notion", "📝"),
            ("fetch", "🌐"),
            ("git", "📦"),
        ];

        // Try to match against known server names
        for (server_name, icon) in &known_servers {
            if let Some(func) = rest.strip_prefix(&format!("{}_", server_name)) {
                let human = humanise_function_name(func);
                return format!("{} {}", icon, human);
            }
        }

        // Fallback: split on first underscore (for unknown servers)
        if let Some(sep) = rest.find('_') {
            let func = &rest[sep + 1..];
            let human = humanise_function_name(func);
            return format!("🔧 {}", human);
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

fn key_matches_any(key: &str, allowed: &[&str]) -> bool {
    let lower = key.to_ascii_lowercase();
    allowed.iter().any(|candidate| lower == *candidate)
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "bearer",
        "authorization",
        "api_key",
        "apikey",
        "private_key",
        "cookie",
        "content",
        "command",
        "prompt",
        "message",
        "text",
    ]
    .iter()
    .any(|sensitive| lower.contains(sensitive))
}

/// Formats `args_preview` for display: truncate to 60 chars, strip outer braces for common single-arg calls.
pub fn format_args_preview(args_json: &str) -> String {
    // Privacy-aware preview formatter.
    // Goals:
    // - Never leak sensitive fields (api_key, token, prompt, content, command, etc.)
    // - Only render a compact allowlist of safe keys when present
    // - Never render nested objects/arrays in full

    const SAFE_KEYS: [&str; 13] = [
        "query",
        "path",
        "url",
        "title",
        "description",
        "step_id",
        "status",
        "skill_name",
        "agent",
        "model",
        "language",
        "technology",
        "name",
    ];

    let Ok(val) = serde_json::from_str::<JsonValue>(args_json) else {
        return String::new();
    };
    let Some(obj) = val.as_object() else {
        return String::new();
    };

    if obj.len() == 1 {
        if let Some((key, value)) = obj.iter().next() {
            if is_sensitive_key(key) || !key_matches_any(key, &SAFE_KEYS) {
                return String::new();
            }
            if let Some(s) = value.as_str() {
                return crate::utils::strings::truncate_chars(s, MAX_DISPLAY_FIELD_CHARS);
            }
            if value.is_number() || value.is_boolean() {
                return crate::utils::strings::truncate_chars(
                    &value.to_string(),
                    MAX_DISPLAY_FIELD_CHARS,
                );
            }
        }

        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();
    for safe in SAFE_KEYS.iter() {
        if let Some((_, value)) = obj
            .iter()
            .find(|(key, _)| !is_sensitive_key(key) && key_matches_any(key, &[*safe]))
        {
            if let Some(s) = value.as_str() {
                parts.push(format!(
                    "{}: {}",
                    safe,
                    crate::utils::strings::truncate_chars(s, MAX_DISPLAY_FIELD_CHARS)
                ));
            } else if value.is_number() || value.is_boolean() {
                parts.push(format!("{}: {}", safe, value));
            }
        }
    }

    if parts.is_empty() {
        return String::new();
    }
    let joined = parts.join(", ");
    crate::utils::strings::truncate_chars(&joined, MAX_DISPLAY_FIELD_CHARS)
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
pub struct ToolCallNotifier {
    bot: Bot,
    chat_id: ChatId,
    status_msg: Option<Message>,
    display_state: ToolDisplayState,
    last_edit: Option<Instant>,
}

impl ToolCallNotifier {
    pub fn new(bot: Bot, chat_id: ChatId) -> Self {
        Self {
            bot,
            chat_id,
            status_msg: None,
            display_state: ToolDisplayState::default(),
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
        self.display_state.handle_event(event);
        self.edit_message().await;
    }

    async fn edit_message(&mut self) {
        if self.status_msg.is_none() {
            return;
        }
        let text = self.format_status();
        self.edit_status_text(&text, "edit").await;
    }

    async fn edit_status_text(&mut self, text: &str, _context: &str) {
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

        // Try once, then retry after a short delay on failure
        match self.bot.edit_message_text(self.chat_id, msg.id, text).await {
            Ok(_) => {
                self.last_edit = Some(Instant::now());
                return;
            }
            Err(e) => {
                debug!("Failed to edit tool notifier message (first try): {:#}", e);
            }
        }

        // Wait 1s and retry once
        tokio::time::sleep(Duration::from_millis(1000)).await;
        match self.bot.edit_message_text(self.chat_id, msg.id, text).await {
            Ok(_) => {
                self.last_edit = Some(Instant::now());
            }
            Err(e) => debug!("Failed to edit tool notifier message (retry): {:#}", e),
        }
    }

    fn format_status(&self) -> String {
        self.display_state.format_live()
    }

    fn final_status_text(&self, success: bool) -> Option<String> {
        self.display_state.has_activity().then(|| {
            // Choose header/result text based on overall success
            let mut text = if success {
                self.display_state.format_completed()
            } else {
                self.display_state.format("⛔ Stopped")
            };

            if success {
                text.push_str("\n\nResult\nFinal answer sent below.");
            } else {
                text.push_str("\n\nResult\nRequest ended with an error response below.");
            }
            text
        })
    }

    /// Finalise the status message by persisting a completed summary when useful.
    pub async fn finish(&mut self, success: bool) {
        let Some(ref msg) = self.status_msg else {
            return;
        };

        let Some(text) = self.final_status_text(success) else {
            self.bot.delete_message(self.chat_id, msg.id).await.ok();
            return;
        };

        // Use shared helper to ensure consistent rate-limiting and retry behaviour
        self.edit_status_text(&text, "final").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_args_preview_redacts_sensitive_single_arg() {
        let json = r#"{"api_key":"sk-SECRET-123"}"#;
        let out = format_args_preview(json);
        assert!(out.is_empty(), "sensitive key must be suppressed: {}", out);
    }

    #[test]
    fn test_format_args_preview_suppresses_command_content_and_prompt() {
        let j = r#"{"command":"rm -rf /", "prompt":"write me a secret"}"#;
        // multi-key but all sensitive — should suppress to empty
        let out = format_args_preview(j);
        assert!(
            out.is_empty(),
            "sensitive multi-key should be empty: {}",
            out
        );
    }

    #[test]
    fn test_format_args_preview_suppresses_unknown_single_scalar_key() {
        let preview = format_args_preview(r#"{"message":"private freeform text"}"#);
        assert!(
            preview.is_empty(),
            "unknown single scalar key leaked: {preview}"
        );
    }

    #[test]
    fn test_format_args_preview_suppresses_secret_key_variants() {
        for json in [
            r#"{"private_key":"secret"}"#,
            r#"{"access_token":"secret"}"#,
            r#"{"apiKey":"secret"}"#,
            r#"{"cookie":"secret"}"#,
            r#"{"text":"private text"}"#,
        ] {
            let preview = format_args_preview(json);
            assert!(
                preview.is_empty(),
                "secret/content variant leaked from {json}: {preview}"
            );
        }
    }

    #[test]
    fn test_format_args_preview_suppresses_non_object_and_malformed_args() {
        for raw in [r#""plain string""#, "not-json", r#"["array value"]"#] {
            let preview = format_args_preview(raw);
            assert!(
                preview.is_empty(),
                "non-object or malformed args leaked from {raw}: {preview}"
            );
        }
    }

    #[test]
    fn test_format_args_preview_only_uses_safe_keys_from_multi_arg_object() {
        let j = r#"{"query":"find docs","api_key":"xxx","path":"/tmp/file"}"#;
        let out = format_args_preview(j);
        // should include 'query' and/or 'path' but not api_key
        assert!(out.contains("query") || out.contains("path"));
        assert!(!out.contains("api_key"));
    }

    #[test]
    fn test_tool_display_state_clamps_long_completed_summary() {
        let mut s = ToolDisplayState::default();
        // create a plan with many long steps
        let mut steps = Vec::new();
        for _i in 0..50 {
            steps.push(PlanStepDisplay {
                text: "a very long step description that repeats".repeat(20),
                status: PlanStepStatus::Todo,
            });
        }
        s.plan = Some(PlanDisplay {
            title: "Long Plan Title".to_string(),
            steps,
        });
        let formatted = s.format_completed();
        // Should always be under Telegram's safe limit (we clamp elsewhere to MAX_STATUS_TEXT_CHARS=3800)
        assert!(
            formatted.chars().count() <= 4000,
            "formatted too long: {}",
            formatted.chars().count()
        );
    }

    #[test]
    fn test_notifier_final_status_text_returns_completed_card_when_activity_exists() {
        let mut notifier = ToolCallNotifier::new(Bot::new("TEST_TOKEN"), ChatId(1));
        notifier.display_state.handle_event(ToolEvent::Started {
            name: "read_file".to_string(),
            args_preview: "/tmp/file.txt".to_string(),
            arguments_json: r#"{"path":"/tmp/file.txt"}"#.to_string(),
        });
        notifier.display_state.handle_event(ToolEvent::Completed {
            name: "read_file".to_string(),
            success: true,
        });
        let text = notifier
            .final_status_text(true)
            .expect("activity should produce a final status card");
        assert!(
            text.contains("Completed"),
            "completed header missing: {text}"
        );
        assert!(
            text.contains("Final answer sent below."),
            "result line missing: {text}"
        );
        assert!(
            text.contains("Reading a file"),
            "tool activity missing: {text}"
        );
    }

    #[test]
    fn test_notifier_final_status_text_is_none_without_activity() {
        let notifier = ToolCallNotifier::new(Bot::new("TEST_TOKEN"), ChatId(1));
        assert!(notifier.final_status_text(true).is_none());
    }

    #[test]
    fn test_tool_display_state_renders_plan_create_as_checklist() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Create test plan".to_string(),
            arguments_json:
                r#"{"title":"Create test plan","steps":["Gather context","Implement fix"]}"#
                    .to_string(),
        });

        let text = state.format_live();
        assert!(
            text.contains("Working on your request"),
            "live header missing: {text}"
        );
        assert!(text.contains("Plan"), "plan section missing: {text}");
        assert!(
            text.contains("Create test plan"),
            "plan title missing: {text}"
        );
        assert!(
            text.contains("[ ] 0. Gather context"),
            "first step missing: {text}"
        );
        assert!(
            text.contains("[ ] 1. Implement fix"),
            "second step missing: {text}"
        );
    }

    #[test]
    fn test_tool_display_state_updates_plan_step_status() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["First","Second"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 1".to_string(),
            arguments_json: r#"{"step_id":1,"status":"in_progress","notes":"working"}"#.to_string(),
        });

        let text = state.format_live();
        assert!(
            text.contains("[ ] 0. First"),
            "unchanged step missing: {text}"
        );
        assert!(
            text.contains("[>] 1. Second"),
            "updated step missing: {text}"
        );
    }

    #[test]
    fn test_tool_display_state_does_not_render_plan_update_notes() {
        let mut state = ToolDisplayState::default();
        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["First"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 0".to_string(),
            arguments_json: r#"{"step_id":0,"status":"done","notes":"token=secret"}"#.to_string(),
        });
        let text = state.format_completed();
        assert!(text.contains("[x] 0. First"), "done step missing: {text}");
        assert!(
            !text.contains("token=secret"),
            "plan update note leaked: {text}"
        );
    }

    #[test]
    fn test_notifier_final_status_text_reports_failed_request() {
        let mut notifier = ToolCallNotifier::new(Bot::new("TEST_TOKEN"), ChatId(1));
        notifier.display_state.handle_event(ToolEvent::Started {
            name: "read_file".to_string(),
            args_preview: "/tmp/file.txt".to_string(),
            arguments_json: r#"{"path":"/tmp/file.txt"}"#.to_string(),
        });

        let text = notifier
            .final_status_text(false)
            .expect("activity should produce a final status card");
        assert!(
            text.contains("Stopped") || text.contains("Failed"),
            "failure header missing: {text}"
        );
        assert!(
            text.contains("error response below"),
            "failure result missing: {text}"
        );
        assert!(
            !text.contains("Final answer sent below."),
            "success result shown for failure: {text}"
        );
    }

    #[test]
    fn test_tool_display_state_marks_failed_plan_update_after_completion() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["Only step"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 0".to_string(),
            arguments_json: r#"{"step_id":0,"status":"in_progress"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "plan_update".to_string(),
            success: false,
        });

        let text = state.format_live();
        assert!(
            text.contains("[!] 0. Only step"),
            "failed step missing: {text}"
        );
    }

    #[test]
    fn test_failed_unparsed_plan_update_does_not_mark_previous_step_failed() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["First","Second"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 1".to_string(),
            arguments_json: r#"{"step_id":1,"status":"done"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "plan_update".to_string(),
            success: true,
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "bad update".to_string(),
            arguments_json: r#"{"status":"failed"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "plan_update".to_string(),
            success: false,
        });

        let text = state.format_live();
        assert!(
            text.contains("[x] 1. Second"),
            "valid completed step changed unexpectedly: {text}"
        );
        assert!(
            !text.contains("[!] 1. Second"),
            "stale failed update marked the previous step failed: {text}"
        );
    }

    #[test]
    fn test_tool_display_state_renders_generic_tool_activity_without_plan() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "read_file".to_string(),
            args_preview: "/tmp/file.txt".to_string(),
            arguments_json: r#"{"path":"/tmp/file.txt"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "read_file".to_string(),
            success: true,
        });

        let text = state.format_completed();
        assert!(
            text.contains("Completed"),
            "completed header missing: {text}"
        );
        assert!(
            text.contains("Tool activity"),
            "tool section missing: {text}"
        );
        assert!(
            text.contains("Reading a file"),
            "friendly tool label missing: {text}"
        );
        assert!(
            text.contains("completed"),
            "completion state missing: {text}"
        );
        assert!(
            !text.contains("Plan\n"),
            "plan section should be omitted: {text}"
        );
    }

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
