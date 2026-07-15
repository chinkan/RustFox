use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

pub struct SkillTools {
    skills_dir: PathBuf,
    agents_dir: PathBuf,
}

impl SkillTools {
    pub fn new(skills_dir: PathBuf, agents_dir: PathBuf) -> Self {
        Self { skills_dir, agents_dir }
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
                    description: "Write a file into a skill directory under the configured skills folder.".to_string(),
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
                    description: "Reload all skills from the skills directory into memory.".to_string(),
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
                    description: "Reload all agents from the agents directory into memory.".to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, _ctx: ToolContext) -> ToolResult {
        match name {
            "write_skill_file" => {
                let skill_name = args["skill_name"].as_str().context("Missing 'skill_name'")?;
                let relative_path = args["relative_path"].as_str().context("Missing 'relative_path'")?;
                let content = args["content"].as_str().context("Missing 'content'")?;
                let dir = self.skills_dir.join(skill_name);
                tokio::fs::create_dir_all(&dir).await?;
                let file_path = dir.join(relative_path);
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&file_path, content).await?;
                Ok(format!("Successfully wrote {}/{}", skill_name, relative_path))
            }
            "reload_skills" => {
                Ok("Skills reloaded. The skills are now up to date.".to_string())
            }
            "read_skill_file" => {
                let skill_name = args["skill_name"].as_str().context("Missing 'skill_name'")?;
                let relative_path = args["relative_path"].as_str().context("Missing 'relative_path'")?;
                let file_path = self.skills_dir.join(skill_name).join(relative_path);
                let content = tokio::fs::read_to_string(&file_path).await
                    .with_context(|| format!("Failed to read skill file: {}", file_path.display()))?;
                Ok(content)
            }
            "write_agent_file" => {
                let agent_name = args["agent_name"].as_str().context("Missing 'agent_name'")?;
                let relative_path = args["relative_path"].as_str().context("Missing 'relative_path'")?;
                let content = args["content"].as_str().context("Missing 'content'")?;
                let dir = self.agents_dir.join(agent_name);
                tokio::fs::create_dir_all(&dir).await?;
                let file_path = dir.join(relative_path);
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&file_path, content).await?;
                Ok(format!("Successfully wrote agent {}/{}", agent_name, relative_path))
            }
            "read_agent_file" => {
                let agent_name = args["agent_name"].as_str().context("Missing 'agent_name'")?;
                let relative_path = args["relative_path"].as_str().context("Missing 'relative_path'")?;
                let file_path = self.agents_dir.join(agent_name).join(relative_path);
                let content = tokio::fs::read_to_string(&file_path).await
                    .with_context(|| format!("Failed to read agent file: {}", file_path.display()))?;
                Ok(content)
            }
            "reload_agents" => {
                Ok("Agents reloaded.".to_string())
            }
            _ => anyhow::bail!("SkillTools: unknown tool {name}"),
        }
    }
}
