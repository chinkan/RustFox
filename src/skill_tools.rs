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
fn validate_skill_name(name: &str) -> Result<(), String> {
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
fn validate_skill_path(path: &str) -> Result<(), String> {
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
                    description: "Read a file from a skill directory.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string" },
                            "relative_path": { "type": "string" }
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
                let file_path = self.skills_dir.join(skill_name).join(relative_path);
                let content = tokio::fs::read_to_string(&file_path)
                    .await
                    .with_context(|| {
                        format!("Failed to read skill file: {}", file_path.display())
                    })?;
                Ok(content)
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
            _ => anyhow::bail!("SkillTools: unknown tool {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
