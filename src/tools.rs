use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::llm::{FunctionDefinition, ToolDefinition};

/// Validates that a path is within the allowed sandbox directory.
/// Returns the canonicalized path if valid.
fn validate_sandbox_path(sandbox_dir: &Path, requested: &str) -> Result<PathBuf> {
    let sandbox_canonical = sandbox_dir
        .canonicalize()
        .with_context(|| format!("Sandbox directory not found: {}", sandbox_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        sandbox_dir.join(requested)
    };

    // For paths that don't exist yet (write_file), check the parent
    let check_path = if requested_path.exists() {
        requested_path
            .canonicalize()
            .context("Failed to canonicalize path")?
    } else {
        let parent = requested_path
            .parent()
            .context("Path has no parent directory")?;
        let parent_canonical = parent
            .canonicalize()
            .with_context(|| format!("Parent directory not found: {}", parent.display()))?;
        parent_canonical.join(requested_path.file_name().context("Path has no filename")?)
    };

    if !check_path.starts_with(&sandbox_canonical) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the sandbox directory '{}'",
            requested,
            sandbox_dir.display()
        );
    }

    Ok(check_path)
}

pub fn builtin_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "read_file".to_string(),
                description: "Read the contents of a file within the sandbox directory"
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path (relative to sandbox or absolute within sandbox)"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "write_file".to_string(),
                description: "Write content to a file within the sandbox directory. Creates parent directories if needed.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path (relative to sandbox or absolute within sandbox)"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "list_files".to_string(),
                description: "List files and directories within a path in the sandbox directory"
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The directory path (relative to sandbox or absolute within sandbox). Defaults to sandbox root."
                        }
                    },
                    "required": []
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "execute_command".to_string(),
                description:
                    "Execute a shell command within the sandbox directory. The working directory is set to the sandbox."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "plan_create".to_string(),
                description: "Create a new execution plan with ordered steps. Call this BEFORE starting any multi-step task. Stores the plan in the sandbox for tracking.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Short title describing the overall goal"
                        },
                        "steps": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Ordered list of step descriptions"
                        }
                    },
                    "required": ["title", "steps"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "plan_update".to_string(),
                description: "Update a step's status in the active plan. Call before starting a step (in_progress) and after finishing (done or failed).".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "step_id": {
                            "type": "integer",
                            "description": "Zero-based index of the step to update"
                        },
                        "status": {
                            "type": "string",
                            "enum": ["todo", "in_progress", "done", "failed"],
                            "description": "New status for the step"
                        },
                        "notes": {
                            "type": "string",
                            "description": "Optional notes — result summary, error message, etc."
                        }
                    },
                    "required": ["step_id", "status"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "plan_view".to_string(),
                description: "View the current plan as a checklist. Call at the end of execution to review progress before synthesising the final answer.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        },
    ]
}

pub async fn execute_builtin_tool(
    tool_name: &str,
    arguments: &Value,
    sandbox_dir: &Path,
) -> Result<String> {
    match tool_name {
        "read_file" => {
            let path = arguments["path"]
                .as_str()
                .context("Missing 'path' argument")?;
            let full_path = validate_sandbox_path(sandbox_dir, path)?;
            info!("Reading file: {}", full_path.display());
            let content = tokio::fs::read_to_string(&full_path)
                .await
                .with_context(|| format!("Failed to read file: {}", full_path.display()))?;
            Ok(content)
        }
        "write_file" => {
            let path = arguments["path"]
                .as_str()
                .context("Missing 'path' argument")?;
            let content = arguments["content"]
                .as_str()
                .context("Missing 'content' argument")?;
            let full_path = validate_sandbox_path(sandbox_dir, path)?;

            // Create parent directories if they don't exist
            if let Some(parent) = full_path.parent() {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("Failed to create directories: {}", parent.display())
                })?;
            }

            info!("Writing file: {}", full_path.display());
            tokio::fs::write(&full_path, content)
                .await
                .with_context(|| format!("Failed to write file: {}", full_path.display()))?;
            Ok(format!(
                "File written successfully: {}",
                full_path.display()
            ))
        }
        "list_files" => {
            let path = arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let full_path = validate_sandbox_path(sandbox_dir, path)?;
            info!("Listing files: {}", full_path.display());

            let mut entries = Vec::new();
            let mut read_dir = tokio::fs::read_dir(&full_path)
                .await
                .with_context(|| format!("Failed to read directory: {}", full_path.display()))?;

            while let Some(entry) = read_dir.next_entry().await? {
                let file_type = entry.file_type().await?;
                let prefix = if file_type.is_dir() {
                    "[DIR]"
                } else {
                    "[FILE]"
                };
                entries.push(format!(
                    "{} {}",
                    prefix,
                    entry.file_name().to_string_lossy()
                ));
            }

            entries.sort();
            if entries.is_empty() {
                Ok("Directory is empty".to_string())
            } else {
                Ok(entries.join("\n"))
            }
        }
        "execute_command" => {
            let command = arguments["command"]
                .as_str()
                .context("Missing 'command' argument")?;

            info!("Executing command in sandbox: {}", command);

            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(sandbox_dir)
                .output()
                .await
                .with_context(|| format!("Failed to execute command: {}", command))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let mut result = String::new();
            if !stdout.is_empty() {
                result.push_str(&format!("STDOUT:\n{}\n", stdout));
            }
            if !stderr.is_empty() {
                result.push_str(&format!("STDERR:\n{}\n", stderr));
            }
            result.push_str(&format!(
                "Exit code: {}",
                output.status.code().unwrap_or(-1)
            ));
            Ok(result)
        }
        "plan_create" => {
            let title = arguments["title"]
                .as_str()
                .context("Missing 'title' argument")?;
            let steps = arguments["steps"]
                .as_array()
                .context("Missing 'steps' argument")?;

            let plan_steps: Vec<serde_json::Value> = steps
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    json!({
                        "id": i,
                        "description": s.as_str().unwrap_or(""),
                        "status": "todo",
                        "notes": ""
                    })
                })
                .collect();

            let plan = json!({
                "title": title,
                "steps": plan_steps
            });

            let plan_path = sandbox_dir.join(".rustfox_plan.json");
            tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)
                .await
                .context("Failed to write plan file")?;

            info!("Plan created: {} ({} steps)", title, plan_steps.len());

            let checklist: Vec<String> = plan_steps
                .iter()
                .map(|s| {
                    format!(
                        "[ ] {}: {}",
                        s["id"].as_u64().unwrap_or(0),
                        s["description"].as_str().unwrap_or("")
                    )
                })
                .collect();

            Ok(format!(
                "Plan created: {}\n\n{}",
                title,
                checklist.join("\n")
            ))
        }
        "plan_update" => {
            let step_id = arguments["step_id"]
                .as_u64()
                .context("Missing 'step_id' argument")? as usize;
            let status = arguments["status"]
                .as_str()
                .context("Missing 'status' argument")?;
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let plan_path = sandbox_dir.join(".rustfox_plan.json");
            let content = tokio::fs::read_to_string(&plan_path)
                .await
                .context("No active plan found. Call plan_create first.")?;
            let mut plan: serde_json::Value = serde_json::from_str(&content)
                .context("Invalid plan file format")?;

            let steps = plan["steps"]
                .as_array_mut()
                .context("Invalid plan: missing steps array")?;
            let step = steps
                .get_mut(step_id)
                .with_context(|| format!("Step {} not found in plan", step_id))?;

            let description = step["description"]
                .as_str()
                .unwrap_or("")
                .to_string();
            step["status"] = json!(status);
            step["notes"] = json!(notes);

            tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)
                .await
                .context("Failed to update plan file")?;

            let icon = match status {
                "done" => "[x]",
                "failed" => "[!]",
                "in_progress" => "[>]",
                _ => "[ ]",
            };

            info!("Plan step {} -> {}", step_id, status);
            Ok(format!(
                "{} Step {}: {} [{}]{}",
                icon,
                step_id,
                description,
                status,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!(" -- {}", notes)
                }
            ))
        }
        "plan_view" => {
            let plan_path = sandbox_dir.join(".rustfox_plan.json");
            let content = tokio::fs::read_to_string(&plan_path)
                .await
                .context("No active plan found. Call plan_create first.")?;
            let plan: serde_json::Value =
                serde_json::from_str(&content).context("Invalid plan file format")?;

            let title = plan["title"].as_str().unwrap_or("Untitled Plan");
            let steps = plan["steps"]
                .as_array()
                .context("Invalid plan: missing steps array")?;

            let lines: Vec<String> = steps
                .iter()
                .map(|s| {
                    let icon = match s["status"].as_str().unwrap_or("todo") {
                        "done" => "[x]",
                        "failed" => "[!]",
                        "in_progress" => "[>]",
                        _ => "[ ]",
                    };
                    let desc = s["description"].as_str().unwrap_or("");
                    let notes = s["notes"].as_str().unwrap_or("");
                    if notes.is_empty() {
                        format!("{} {}", icon, desc)
                    } else {
                        format!("{} {} -- {}", icon, desc, notes)
                    }
                })
                .collect();

            Ok(format!("# {}\n\n{}", title, lines.join("\n")))
        }
        _ => anyhow::bail!("Unknown built-in tool: {}", tool_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_plan_create_writes_json() {
        let dir = tempdir().unwrap();
        let args = serde_json::json!({
            "title": "My Plan",
            "steps": ["Step A", "Step B"]
        });
        let result = execute_builtin_tool("plan_create", &args, dir.path())
            .await
            .unwrap();
        assert!(result.contains("My Plan"));
        assert!(result.contains("Step A"));
        assert!(result.contains("Step B"));

        let plan_path = dir.path().join(".rustfox_plan.json");
        assert!(plan_path.exists());
        let content = std::fs::read_to_string(plan_path).unwrap();
        let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(plan["title"].as_str().unwrap(), "My Plan");
        assert_eq!(plan["steps"][0]["status"].as_str().unwrap(), "todo");
        assert_eq!(plan["steps"][1]["description"].as_str().unwrap(), "Step B");
    }

    #[tokio::test]
    async fn test_plan_update_changes_step_status() {
        let dir = tempdir().unwrap();

        let create_args = serde_json::json!({
            "title": "Test Plan",
            "steps": ["Step A", "Step B"]
        });
        execute_builtin_tool("plan_create", &create_args, dir.path())
            .await
            .unwrap();

        let update_args = serde_json::json!({
            "step_id": 0,
            "status": "in_progress"
        });
        let result = execute_builtin_tool("plan_update", &update_args, dir.path())
            .await
            .unwrap();
        assert!(result.contains("in_progress") || result.contains("[>]"));

        let plan_path = dir.path().join(".rustfox_plan.json");
        let content = std::fs::read_to_string(plan_path).unwrap();
        let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(plan["steps"][0]["status"].as_str().unwrap(), "in_progress");
        assert_eq!(plan["steps"][1]["status"].as_str().unwrap(), "todo");
    }

    #[tokio::test]
    async fn test_plan_update_stores_notes() {
        let dir = tempdir().unwrap();

        let create_args = serde_json::json!({
            "title": "Test",
            "steps": ["Only step"]
        });
        execute_builtin_tool("plan_create", &create_args, dir.path())
            .await
            .unwrap();

        let update_args = serde_json::json!({
            "step_id": 0,
            "status": "done",
            "notes": "Completed successfully"
        });
        execute_builtin_tool("plan_update", &update_args, dir.path())
            .await
            .unwrap();

        let plan_path = dir.path().join(".rustfox_plan.json");
        let content = std::fs::read_to_string(plan_path).unwrap();
        let plan: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            plan["steps"][0]["notes"].as_str().unwrap(),
            "Completed successfully"
        );
    }

    #[tokio::test]
    async fn test_plan_view_renders_checklist() {
        let dir = tempdir().unwrap();

        let create_args = serde_json::json!({
            "title": "My Plan",
            "steps": ["Alpha", "Beta", "Gamma"]
        });
        execute_builtin_tool("plan_create", &create_args, dir.path())
            .await
            .unwrap();

        execute_builtin_tool(
            "plan_update",
            &serde_json::json!({ "step_id": 0, "status": "done", "notes": "ok" }),
            dir.path(),
        )
        .await
        .unwrap();
        execute_builtin_tool(
            "plan_update",
            &serde_json::json!({ "step_id": 1, "status": "in_progress" }),
            dir.path(),
        )
        .await
        .unwrap();

        let result = execute_builtin_tool("plan_view", &serde_json::json!({}), dir.path())
            .await
            .unwrap();

        assert!(result.contains("My Plan"));
        assert!(result.contains("[x]"));
        assert!(result.contains("[>]"));
        assert!(result.contains("[ ]"));
        assert!(result.contains("Alpha"));
        assert!(result.contains("ok"));
    }
}
