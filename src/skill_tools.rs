use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::skills::SkillRegistry;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

/// Validate skill/agent name: alphanumeric, hyphens, underscores, 1–64 chars.
pub(crate) fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name cannot be empty".to_string());
    }
    if name.len() > 64 {
        return Err("Skill name too long (max 64 chars)".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Skill name contains invalid characters".to_string());
    }
    Ok(())
}

/// Validate a relative path within a skill/agent directory: no '..' components, non-empty, not absolute.
pub(crate) fn validate_skill_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    if path.contains("..") {
        return Err("Path cannot contain '..'".to_string());
    }
    if path.starts_with('/') {
        return Err("Path cannot be absolute".to_string());
    }
    Ok(())
}

/// Optional 1-based `offset` or positive `limit` from a tool-call object.
/// Missing and JSON null both mean the argument was not set.
fn optional_line_arg(args: &Value, key: &str) -> Result<Option<u64>, anyhow::Error> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let Some(n) = value.as_u64() else {
                anyhow::bail!("{key} must be an integer >= 1, got {value}");
            };
            if n == 0 {
                anyhow::bail!("{key} must be an integer >= 1, got 0");
            }
            Ok(Some(n))
        }
    }
}

/// Lines of a skill file, keeping each line's `\n` terminator.
/// An empty string has zero lines. A trailing newline does not add another line.
fn skill_file_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        Vec::new()
    } else {
        content.split_inclusive('\n').collect()
    }
}

/// Issue #337 Opt A. Both `offset` and `limit` absent → original text, no header.
/// Either present → `[relative_path lines X-Y of N]` plus that slice.
/// `X`/`Y` are 1-based inclusive and match the lines actually returned
/// (`limit` past EOF clamps). Out of range and empty files return an error.
fn slice_skill_content(
    content: &str,
    relative_path: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<String, anyhow::Error> {
    if offset.is_none() && limit.is_none() {
        return Ok(content.to_string());
    }
    if let Some(0) = limit {
        anyhow::bail!("limit must be an integer >= 1, got 0");
    }
    let start_line = offset.unwrap_or(1);
    if start_line == 0 {
        anyhow::bail!("offset must be an integer >= 1, got 0");
    }

    let lines = skill_file_lines(content);
    let total = u64::try_from(lines.len()).unwrap_or(u64::MAX);
    if total == 0 {
        anyhow::bail!("{relative_path} is empty (0 lines); offset/limit cannot select a line");
    }
    if start_line > total {
        anyhow::bail!("offset {start_line} is past the end of {relative_path} ({total} lines)");
    }

    let start = usize::try_from(start_line - 1)
        .map_err(|_| anyhow::anyhow!("offset {start_line} is too large"))?;
    let end = match limit {
        Some(limit) => start
            .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
            .min(lines.len()),
        None => lines.len(),
    };
    let end_line = u64::try_from(end).unwrap_or(u64::MAX);
    let body: String = lines[start..end].concat();
    Ok(format!(
        "[{relative_path} lines {start_line}-{end_line} of {total}]\n{body}"
    ))
}

pub struct SkillTools {
    skills_dir: PathBuf,
    agents_dir: PathBuf,
    skills: Arc<RwLock<SkillRegistry>>,
    agents: Arc<RwLock<SkillRegistry>>,
}

impl SkillTools {
    pub fn new(
        skills_dir: PathBuf,
        agents_dir: PathBuf,
        skills: Arc<RwLock<SkillRegistry>>,
        agents: Arc<RwLock<SkillRegistry>>,
    ) -> Self {
        Self {
            skills_dir,
            agents_dir,
            skills,
            agents,
        }
    }
}

#[async_trait]
impl ToolHandler for SkillTools {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "write_skill_file".to_string(),
                    description:
                        "Write a file into a skill directory under the configured skills folder."
                            .to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string", "description": "Skill directory name" },
                            "relative_path": { "type": "string", "description": "Path within the skill directory" },
                            "content": { "type": "string", "description": "Full file content to write" }
                        },
                        "required": ["skill_name", "relative_path", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "reload_skills".to_string(),
                    description: "Reload all skills from the skills directory into memory."
                        .to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_skill_file".to_string(),
                    description: concat!(
                        "Read a file from a skill directory. ",
                        "Omit offset and limit to return the full file unchanged. ",
                        "Set offset (1-based line) and/or limit (line count) to return a slice ",
                        "prefixed with [path lines X-Y of N]."
                    )
                    .to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string" },
                            "relative_path": { "type": "string" },
                            "offset": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "1-based line number to start at. Omit together with limit to return the full file."
                            },
                            "limit": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Number of lines to return from offset (or from line 1 if offset is omitted). A limit past EOF is clamped; the header shows the actual last line."
                            }
                        },
                        "required": ["skill_name", "relative_path"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "write_agent_file".to_string(),
                    description: "Write a file into an agent directory.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent_name": { "type": "string" },
                            "relative_path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["agent_name", "relative_path", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_agent_file".to_string(),
                    description: "Read a file from an agent directory.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent_name": { "type": "string" },
                            "relative_path": { "type": "string" }
                        },
                        "required": ["agent_name", "relative_path"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "reload_agents".to_string(),
                    description: "Reload all agents from the agents directory into memory."
                        .to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
            // invoke_agent / spawn_agents: definitions live here so the LLM sees
            // them; execution is intercepted by Agent special_tool_handler
            // (circular dependency with run_subagent — §7.5 peer invoke).
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "invoke_agent".to_string(),
                    description: concat!(
                        "Delegate a task to a named agent running as an isolated agentic loop. ",
                        "Resolves agents/<name>/AGENT.md, then subagent skills, then [[bots]] ",
                        "persona id (optional synonym: bot=\"...\"). ",
                        "Reply stays in the caller's chat (via <persona>: attribution). ",
                        "Nested peer depth max 2; cycles are rejected."
                    )
                    .to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "description": "Name of the agent / bot persona to invoke (e.g. 'verifier', 'researcher')"
                            },
                            "bot": {
                                "type": "string",
                                "description": "Optional synonym for agent — targets a [[bots]] persona id"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "The task content to pass to the agent"
                            },
                            "model": {
                                "type": "string",
                                "description": "Optional: override the agent's declared model for this invocation"
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional: override the agent's declared tool whitelist"
                            }
                        },
                        "required": ["prompt"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "spawn_agents".to_string(),
                    description: concat!(
                        "Spawn one or more isolated ad-hoc subagents. ",
                        "Each gets its own agentic loop with system context auto-injected. ",
                        "When multiple tasks are provided via the 'tasks' array, they run concurrently. ",
                        "For a single subagent, use shorthand fields (system_prompt+prompt)."
                    )
                    .to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "tasks": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "system_prompt": {
                                            "type": "string",
                                            "description": "Instructions for this subagent"
                                        },
                                        "prompt": {
                                            "type": "string",
                                            "description": "The task to execute"
                                        },
                                        "model": {
                                            "type": "string",
                                            "description": "Optional model override"
                                        },
                                        "tools": {
                                            "type": "array",
                                            "items": { "type": "string" },
                                            "description": "Optional tool whitelist"
                                        }
                                    },
                                    "required": ["system_prompt", "prompt"]
                                }
                            },
                            "system_prompt": {
                                "type": "string",
                                "description": "Shorthand: system prompt for a single subagent"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "Shorthand: task for a single subagent"
                            },
                            "model": {
                                "type": "string",
                                "description": "Shorthand: optional model override"
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Shorthand: optional tool whitelist"
                            }
                        }
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, _ctx: ToolContext) -> ToolResult {
        match name {
            "write_skill_file" => {
                let skill_name = args["skill_name"]
                    .as_str()
                    .context("Missing 'skill_name'")?;
                let relative_path = args["relative_path"]
                    .as_str()
                    .context("Missing 'relative_path'")?;
                let content = args["content"].as_str().context("Missing 'content'")?;
                validate_skill_name(skill_name).map_err(|e| anyhow::anyhow!(e))?;
                validate_skill_path(relative_path).map_err(|e| anyhow::anyhow!(e))?;
                let dir = self.skills_dir.join(skill_name);
                tokio::fs::create_dir_all(&dir).await?;
                let file_path = dir.join(relative_path);
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&file_path, content).await?;
                Ok(format!(
                    "Successfully wrote {}/{}",
                    skill_name, relative_path
                ))
            }
            "reload_skills" => {
                let skills_dir = self.skills_dir.clone();
                match crate::skills::loader::load_skills_from_dir(&skills_dir, skills_dir.clone())
                    .await
                {
                    Ok(new_reg) => {
                        let count = new_reg.len();
                        let mut skills = self.skills.write().await;
                        *skills = new_reg;
                        Ok(format!("Skills reloaded. {} skill(s) now active.", count))
                    }
                    Err(e) => Ok(format!("Failed to reload skills: {}", e)),
                }
            }
            "read_skill_file" => {
                let skill_name = args["skill_name"]
                    .as_str()
                    .context("Missing 'skill_name'")?;
                let relative_path = args["relative_path"]
                    .as_str()
                    .context("Missing 'relative_path'")?;
                validate_skill_name(skill_name).map_err(|e| anyhow::anyhow!(e))?;
                validate_skill_path(relative_path).map_err(|e| anyhow::anyhow!(e))?;
                let offset = optional_line_arg(&args, "offset")?;
                let limit = optional_line_arg(&args, "limit")?;
                let file_path = self.skills_dir.join(skill_name).join(relative_path);
                let content = tokio::fs::read_to_string(&file_path)
                    .await
                    .with_context(|| {
                        format!("Failed to read skill file: {}", file_path.display())
                    })?;
                slice_skill_content(&content, relative_path, offset, limit)
            }
            "write_agent_file" => {
                let agent_name = args["agent_name"]
                    .as_str()
                    .context("Missing 'agent_name'")?;
                let relative_path = args["relative_path"]
                    .as_str()
                    .context("Missing 'relative_path'")?;
                let content = args["content"].as_str().context("Missing 'content'")?;
                validate_skill_name(agent_name).map_err(|e| anyhow::anyhow!(e))?;
                validate_skill_path(relative_path).map_err(|e| anyhow::anyhow!(e))?;
                let dir = self.agents_dir.join(agent_name);
                tokio::fs::create_dir_all(&dir).await?;
                let file_path = dir.join(relative_path);
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&file_path, content).await?;
                Ok(format!(
                    "Successfully wrote agent {}/{}",
                    agent_name, relative_path
                ))
            }
            "read_agent_file" => {
                let agent_name = args["agent_name"]
                    .as_str()
                    .context("Missing 'agent_name'")?;
                let relative_path = args["relative_path"]
                    .as_str()
                    .context("Missing 'relative_path'")?;
                validate_skill_name(agent_name).map_err(|e| anyhow::anyhow!(e))?;
                validate_skill_path(relative_path).map_err(|e| anyhow::anyhow!(e))?;
                let file_path = self.agents_dir.join(agent_name).join(relative_path);
                let content = tokio::fs::read_to_string(&file_path)
                    .await
                    .with_context(|| {
                        format!("Failed to read agent file: {}", file_path.display())
                    })?;
                Ok(content)
            }
            "reload_agents" => {
                let agents_dir = self.agents_dir.clone();
                match crate::skills::loader::load_skills_from_dir(&agents_dir, agents_dir.clone())
                    .await
                {
                    Ok(new_reg) => {
                        let count = new_reg.len();
                        let mut agents = self.agents.write().await;
                        *agents = new_reg;
                        Ok(format!("Agents reloaded. {} agent(s) active.", count))
                    }
                    Err(e) => Ok(format!("Failed to reload agents: {}", e)),
                }
            }
            "invoke_agent" | "spawn_agents" => {
                // Handled by Agent::special_tool_handler (peer invoke / nested loops).
                anyhow::bail!(
                    "{name} must be handled by the agent special_tool_handler —                      tool registry fallback should not run"
                )
            }
            _ => anyhow::bail!("SkillTools: unknown tool {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel_registry::CancelRegistry;
    use crate::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
    use crate::skills::SkillRegistry;
    use crate::tool_registry::ToolUiMode;
    use std::path::Path;

    #[test]
    fn test_validate_skill_name_valid() {
        assert!(validate_skill_name("creating-skills").is_ok());
        assert!(validate_skill_name("my-skill-123").is_ok());
        assert!(validate_skill_name("a").is_ok());
    }

    #[test]
    fn test_validate_skill_name_empty() {
        assert!(validate_skill_name("").is_err());
    }

    #[test]
    fn test_validate_skill_name_too_long() {
        let long = "a".repeat(65);
        assert!(validate_skill_name(&long).is_err());
    }

    #[test]
    fn test_validate_skill_name_invalid_chars() {
        // The new validation (is_ascii_alphanumeric) allows uppercase and underscores
        assert!(validate_skill_name("my skill").is_err()); // space
        assert!(validate_skill_name("my/skill").is_err()); // slash
    }

    #[test]
    fn test_validate_skill_path_valid() {
        assert!(validate_skill_path("SKILL.md").is_ok());
        assert!(validate_skill_path("reference.md").is_ok());
        assert!(validate_skill_path("scripts/helper.py").is_ok());
        assert!(validate_skill_path("scripts/sub/tool.sh").is_ok());
    }

    #[test]
    fn test_validate_skill_path_traversal() {
        assert!(validate_skill_path("../other-skill/SKILL.md").is_err());
        assert!(validate_skill_path("scripts/../../../etc/passwd").is_err());
        assert!(validate_skill_path("..").is_err());
    }

    #[test]
    fn test_validate_skill_path_empty() {
        assert!(validate_skill_path("").is_err());
    }

    #[test]
    fn test_validate_skill_path_absolute() {
        assert!(validate_skill_path("/etc/passwd").is_err());
        assert!(validate_skill_path("/SKILL.md").is_err());
    }

    #[test]
    fn test_read_skill_file_validates_skill_name() {
        // validate_skill_name is reused — just verify the boundary
        assert!(validate_skill_name("valid-skill").is_ok());
        assert!(validate_skill_name("../evil").is_err());
        assert!(validate_skill_name("").is_err());
    }

    #[test]
    fn test_read_skill_file_validates_relative_path() {
        assert!(validate_skill_path("SKILL.md").is_ok());
        assert!(validate_skill_path("style-guide.md").is_ok());
        assert!(validate_skill_path("../other-skill/SKILL.md").is_err());
        assert!(validate_skill_path("/etc/passwd").is_err());
        assert!(validate_skill_path("").is_err());
    }

    struct NopSender;

    #[async_trait]
    impl PlatformSender for NopSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn skill_tools(dir: &Path) -> SkillTools {
        SkillTools::new(
            dir.to_path_buf(),
            dir.join("agents"),
            Arc::new(RwLock::new(SkillRegistry::new())),
            Arc::new(RwLock::new(SkillRegistry::new())),
        )
    }

    fn ctx(dir: &Path) -> ToolContext {
        ToolContext {
            sandbox_dir: dir.to_path_buf(),
            home_dir: None,
            sender: Arc::new(NopSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "u".into(),
            chat_id: "1".into(),
            bot_id: crate::platform::DEFAULT_BOT_ID.into(),
            tool_ui_mode: ToolUiMode::Silent,
        }
    }

    fn write_skill(dir: &Path, rel: &str, content: &str) {
        let path = dir.join("demo").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    async fn read_skill(dir: &Path, extra: Value) -> ToolResult {
        let tools = skill_tools(dir);
        let mut args = json!({
            "skill_name": "demo",
            "relative_path": "SKILL.md",
        });
        if let (Some(obj), Some(extra_obj)) = (args.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        tools.execute("read_skill_file", args, ctx(dir)).await
    }

    fn read_skill_def(dir: &Path) -> crate::llm::ToolDefinition {
        skill_tools(dir)
            .define()
            .into_iter()
            .find(|d| d.function.name == "read_skill_file")
            .expect("read_skill_file definition")
    }

    #[test]
    fn read_skill_file_schema_offset_and_limit_are_optional_integers() {
        let dir = tempfile::tempdir().unwrap();
        let def = read_skill_def(dir.path());
        let params = &def.function.parameters;
        let props = &params["properties"];
        assert_eq!(props["offset"]["type"], "integer");
        assert_eq!(props["limit"]["type"], "integer");
        assert!(
            props["offset"]["description"]
                .as_str()
                .unwrap_or("")
                .contains("1-based"),
            "offset description should state 1-based lines"
        );
        let required: Vec<&str> = params["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(required, vec!["skill_name", "relative_path"]);
    }

    #[tokio::test]
    async fn read_skill_file_without_offset_or_limit_returns_full_text() {
        let dir = tempfile::tempdir().unwrap();
        let content = "alpha\nbeta\ngamma";
        write_skill(dir.path(), "SKILL.md", content);
        let got = read_skill(dir.path(), json!({})).await.unwrap();
        assert_eq!(got, content);
    }

    #[tokio::test]
    async fn read_skill_file_explicit_null_offset_and_limit_returns_full_text() {
        let dir = tempfile::tempdir().unwrap();
        let content = "alpha\nbeta\n";
        write_skill(dir.path(), "SKILL.md", content);
        let got = read_skill(dir.path(), json!({"offset": null, "limit": null}))
            .await
            .unwrap();
        assert_eq!(got, content);
    }

    #[tokio::test]
    async fn read_skill_file_empty_file_without_window_returns_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "");
        let got = read_skill(dir.path(), json!({})).await.unwrap();
        assert_eq!(got, "");
    }

    #[tokio::test]
    async fn read_skill_file_slice_prefixes_header_with_inclusive_range() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\ngamma\ndelta\n");
        let got = read_skill(dir.path(), json!({"offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(got, "[SKILL.md lines 2-3 of 4]\nbeta\ngamma\n");
    }

    #[tokio::test]
    async fn read_skill_file_offset_only_reads_through_eof() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\ngamma\ndelta\n");
        let got = read_skill(dir.path(), json!({"offset": 3})).await.unwrap();
        assert_eq!(got, "[SKILL.md lines 3-4 of 4]\ngamma\ndelta\n");
    }

    #[tokio::test]
    async fn read_skill_file_limit_only_starts_at_line_one() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\ngamma\ndelta\n");
        let got = read_skill(dir.path(), json!({"limit": 2})).await.unwrap();
        assert_eq!(got, "[SKILL.md lines 1-2 of 4]\nalpha\nbeta\n");
    }

    #[tokio::test]
    async fn read_skill_file_limit_past_eof_clamps_header_to_actual_end() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\ngamma\n");
        let got = read_skill(dir.path(), json!({"offset": 2, "limit": 50}))
            .await
            .unwrap();
        assert_eq!(got, "[SKILL.md lines 2-3 of 3]\nbeta\ngamma\n");
    }

    #[tokio::test]
    async fn read_skill_file_slice_keeps_relative_path_and_last_line_without_newline() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "notes/guide.md", "one\r\ntwo");
        let got = read_skill(
            dir.path(),
            json!({"relative_path": "notes/guide.md", "offset": 2, "limit": 1}),
        )
        .await
        .unwrap();
        assert_eq!(got, "[notes/guide.md lines 2-2 of 2]\ntwo");
    }

    #[tokio::test]
    async fn read_skill_file_offset_past_end_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\ngamma\ndelta\n");
        let result = read_skill(dir.path(), json!({"offset": 9, "limit": 1})).await;
        assert!(
            result.is_err(),
            "expected an error for offset past EOF, got {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("9"), "{msg}");
        assert!(msg.contains("4"), "{msg}");
    }

    #[tokio::test]
    async fn read_skill_file_slice_of_empty_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "");
        let result = read_skill(dir.path(), json!({"offset": 1, "limit": 1})).await;
        assert!(
            result.is_err(),
            "expected an error when slicing an empty file, got {result:?}"
        );
        let msg = result.unwrap_err().to_string().to_lowercase();
        assert!(msg.contains("empty") || msg.contains("0 lines"), "{msg}");
    }

    #[tokio::test]
    async fn read_skill_file_rejects_non_positive_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "alpha\nbeta\n");

        let zero = read_skill(dir.path(), json!({"offset": 0, "limit": 1})).await;
        assert!(zero.is_err(), "offset 0 should error, got {zero:?}");
        assert!(zero.unwrap_err().to_string().contains("offset"));

        let negative = read_skill(dir.path(), json!({"offset": -3})).await;
        assert!(
            negative.is_err(),
            "negative offset should error, got {negative:?}"
        );
        let neg_msg = negative.unwrap_err().to_string();
        assert!(neg_msg.contains("offset"), "{neg_msg}");
        assert!(neg_msg.contains("-3"), "{neg_msg}");

        let limit_zero = read_skill(dir.path(), json!({"limit": 0})).await;
        assert!(
            limit_zero.is_err(),
            "limit 0 should error, got {limit_zero:?}"
        );
        assert!(limit_zero.unwrap_err().to_string().contains("limit"));

        let fractional = read_skill(dir.path(), json!({"offset": 1.5})).await;
        assert!(
            fractional.is_err(),
            "fractional offset should error, got {fractional:?}"
        );

        let stringy = read_skill(dir.path(), json!({"limit": "2"})).await;
        assert!(
            stringy.is_err(),
            "string limit should error, got {stringy:?}"
        );
    }

    #[tokio::test]
    async fn read_skill_file_blank_line_is_a_real_line() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "SKILL.md", "\n");
        let got = read_skill(dir.path(), json!({"offset": 1, "limit": 1}))
            .await
            .unwrap();
        assert_eq!(got, "[SKILL.md lines 1-1 of 1]\n\n");
    }
}
