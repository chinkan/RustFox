use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::learning;
use crate::llm::{FunctionDefinition, ToolDefinition};
#[allow(unused_imports)]
use crate::platform::sender::PlatformSender;
use crate::skills::SkillRegistry;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};
use crate::tools::validate_sandbox_path;

pub struct BuiltinTools {
    skills_dir: PathBuf,
    skills: Arc<RwLock<SkillRegistry>>,
    restart_pending: Arc<AtomicBool>,
    soul_updated: Arc<AtomicBool>,
}

impl BuiltinTools {
    pub fn new(
        skills_dir: PathBuf,
        skills: Arc<RwLock<SkillRegistry>>,
        restart_pending: Arc<AtomicBool>,
        soul_updated: Arc<AtomicBool>,
    ) -> Self {
        Self {
            skills_dir,
            skills,
            restart_pending,
            soul_updated,
        }
    }
}

#[async_trait]
impl ToolHandler for BuiltinTools {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_file".to_string(),
                    description: "Read the contents of a file within the sandbox directory".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "The file path (relative to sandbox or absolute within sandbox)" }
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
                            "path": { "type": "string", "description": "The file path (relative to sandbox or absolute within sandbox)" },
                            "content": { "type": "string", "description": "The content to write to the file" }
                        },
                        "required": ["path", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "list_files".to_string(),
                    description: "List files and directories within a path in the sandbox directory".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "The directory path (relative to sandbox or absolute within sandbox). Defaults to sandbox root." }
                        },
                        "required": []
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "send_file".to_string(),
                    description: "Send a file from the sandbox to the current chat. The file must already exist in the sandbox.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "The file path (relative to sandbox or absolute within sandbox)" },
                            "caption": { "type": "string", "description": "Optional caption for the file" }
                        },
                        "required": ["path"]
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
                            "title": { "type": "string", "description": "Short title describing the overall goal" },
                            "steps": { "type": "array", "items": { "type": "string" }, "description": "Ordered list of step descriptions" }
                        },
                        "required": ["title", "steps"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "plan_update".to_string(),
                    description: "Update a step in a plan. Omit title to use the active plan. Call before starting a step (in_progress) and after finishing (done or failed).".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "description": "Plan title. Omit to use the active plan." },
                            "step_id": { "type": "integer", "description": "Zero-based index of the step to update" },
                            "status": { "type": "string", "enum": ["todo", "in_progress", "done", "failed"], "description": "New status for the step" },
                            "notes": { "type": "string", "description": "Optional notes — result summary, error message, etc." }
                        },
                        "required": ["step_id", "status"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "plan_view".to_string(),
                    description: "View a plan, including notes. Omit title to use the active plan.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "description": "Plan title. Omit to use the active plan." }
                        },
                        "required": []
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "try_new_tech".to_string(),
                    description: "Run a sandboxed experiment with a new technology or approach.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "technology": { "type": "string", "description": "Name/description of the technology being tested" },
                            "experiment_code": { "type": "string", "description": "The source code for the experiment" },
                            "language": { "type": "string", "enum": ["rust", "javascript"], "description": "Programming language (default: rust)" }
                        },
                        "required": ["technology", "experiment_code"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "self_upgrade".to_string(),
                    description: "Upgrade the bot to the latest version.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "branch": { "type": "string", "description": "Git branch to build from (source mode only, default: 'main')" },
                            "mode": { "type": "string", "enum": ["auto", "source", "release"], "description": "Force a specific upgrade mode (default: 'auto')" }
                        },
                        "required": []
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "patch_skill".to_string(),
                    description: "Patch an existing skill's SKILL.md. Default mode is append (safe). Pass mode=replace only when intentionally overwriting the whole file.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string", "description": "Name of the skill to patch" },
                            "patch_content": { "type": "string", "description": "Content to append (body only preferred), or full SKILL.md when mode=replace. In append mode a leading YAML frontmatter block is stripped so the original frontmatter is kept." },
                            "mode": { "type": "string", "enum": ["append", "replace"], "description": "append (default) adds content after the existing body; replace overwrites the whole SKILL.md (backs up first)" }
                        },
                        "required": ["skill_name", "patch_content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_soul_file".to_string(),
                    description: "Read the full contents of a soul file (SOUL.md, AGENTS.md, or USER.md) from the home directory.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "file_name": { "type": "string", "enum": ["SOUL.md", "AGENTS.md", "USER.md"], "description": "Which soul file to read" }
                        },
                        "required": ["file_name"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "update_soul_file".to_string(),
                    description: "Update a soul file (SOUL.md, AGENTS.md, or USER.md) by appending or replacing content.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "file_name": { "type": "string", "enum": ["SOUL.md", "AGENTS.md", "USER.md"], "description": "Which soul file to update" },
                            "mode": { "type": "string", "enum": ["append", "replace"], "description": "append or replace content" },
                            "content": { "type": "string", "description": "Content to write" }
                        },
                        "required": ["file_name", "mode", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "revert_soul_file".to_string(),
                    description: "Restore a soul file from its most recent .bak backup.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "file_name": { "type": "string", "enum": ["SOUL.md", "AGENTS.md", "USER.md"], "description": "Which soul file to revert" }
                        },
                        "required": ["file_name"]
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        match name {
            "read_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let content = tokio::fs::read_to_string(&resolved)
                    .await
                    .with_context(|| format!("Failed to read file: {}", resolved.display()))?;
                Ok(content)
            }
            "write_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let content = args["content"]
                    .as_str()
                    .context("Missing 'content' argument")?;
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                if let Some(parent) = resolved.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&resolved, content).await?;
                Ok(format!(
                    "Wrote {} bytes to {}",
                    content.len(),
                    resolved.display()
                ))
            }
            "list_files" => {
                let path = args["path"].as_str().unwrap_or(".");
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let mut entries = Vec::new();
                let mut dir = tokio::fs::read_dir(&resolved).await?;
                while let Some(entry) = dir.next_entry().await? {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let kind = if entry.file_type().await?.is_dir() {
                        "dir"
                    } else {
                        "file"
                    };
                    entries.push(format!("[{kind}] {name}"));
                }
                entries.sort();
                Ok(entries.join("\n"))
            }
            "send_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let caption = args
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .filter(|c| !c.is_empty());
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let metadata = tokio::fs::metadata(&resolved)
                    .await
                    .with_context(|| format!("File not found: {}", resolved.display()))?;
                const TG_FILE_LIMIT: u64 = 50 * 1024 * 1024;
                if metadata.len() > TG_FILE_LIMIT {
                    anyhow::bail!(
                        "File is {} MB — exceeds Telegram's 50 MB limit",
                        metadata.len() / 1024 / 1024
                    );
                }
                ctx.sender
                    .send_file(&ctx.chat_id, &resolved, caption)
                    .await?;
                let file_name = resolved
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                Ok(format!("File '{}' sent successfully.", file_name))
            }
            "plan_create" => {
                let title = args["title"].as_str().context("Missing 'title' argument")?;
                reject_plan_title(title)?;
                let steps = args["steps"]
                    .as_array()
                    .context("Missing 'steps' argument")?;
                let plans_dir = ctx.sandbox_dir.join(".plans");
                tokio::fs::create_dir_all(&plans_dir).await?;
                let plan = json!({
                    "title": title,
                    "steps": steps,
                    "statuses": vec![json!("todo"); steps.len()],
                    "notes": vec![json!(""); steps.len()],
                });
                tokio::fs::write(
                    plan_file(&plans_dir, title),
                    serde_json::to_string_pretty(&plan)?,
                )
                .await?;
                write_active_plan(&plans_dir, title).await?;
                Ok(format!(
                    "Created plan '{}' with {} steps",
                    title,
                    steps.len()
                ))
            }
            "plan_update" => {
                let plans_dir = ctx.sandbox_dir.join(".plans");
                let title = resolve_plan_title(&plans_dir, &args).await?;
                let step_id = args["step_id"].as_u64().context("Missing 'step_id'")? as usize;
                let status = args["status"]
                    .as_str()
                    .context("Missing 'status' argument")?;
                let notes = args.get("notes").and_then(|v| v.as_str());
                let plan_path = plan_file(&plans_dir, &title);
                let content = read_plan_file(&plans_dir, &title).await?;
                let mut plan: Value = serde_json::from_str(&content)?;
                let len = plan
                    .get("statuses")
                    .and_then(|s| s.as_array())
                    .map(|s| s.len())
                    .unwrap_or(0);
                if step_id >= len {
                    anyhow::bail!("step_id {step_id} is out of range (0..{len})");
                }
                plan["statuses"][step_id] = json!(status);
                if let Some(note) = notes {
                    let arr = plan
                        .as_object_mut()
                        .context("plan is not an object")?
                        .entry("notes")
                        .or_insert_with(|| json!([]));
                    let arr = arr.as_array_mut().context("notes is not an array")?;
                    while arr.len() <= step_id {
                        arr.push(json!(""));
                    }
                    arr[step_id] = json!(note);
                }
                tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?).await?;
                write_active_plan(&plans_dir, &title).await?;
                Ok(format!("Updated step {step_id} to '{status}'"))
            }
            "plan_view" => {
                let plans_dir = ctx.sandbox_dir.join(".plans");
                let title = resolve_plan_title(&plans_dir, &args).await?;
                let content = read_plan_file(&plans_dir, &title).await?;
                let mut plan: Value = serde_json::from_str(&content)?;
                ensure_plan_notes(&mut plan);
                Ok(serde_json::to_string_pretty(&plan)?)
            }
            "try_new_tech" => {
                let technology = args["technology"]
                    .as_str()
                    .context("Missing 'technology'")?
                    .to_string();
                let experiment_code = args["experiment_code"]
                    .as_str()
                    .context("Missing 'experiment_code'")?
                    .to_string();
                let language = args["language"].as_str().unwrap_or("rust").to_string();

                let exp_id = uuid::Uuid::new_v4().to_string();
                let exp_dir = ctx.sandbox_dir.join("experiments").join(&exp_id);
                tokio::fs::create_dir_all(&exp_dir).await?;
                tracing::info!("Running experiment '{}'", technology);

                let (filename, check_cmd, check_args) = match language.as_str() {
                    "javascript" => ("experiment.js", "node", vec!["experiment.js".to_string()]),
                    _ => {
                        let cargo_toml = "[package]\nname = \"experiment\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_string();
                        let src_dir = exp_dir.join("src");
                        tokio::fs::create_dir_all(&src_dir).await?;
                        tokio::fs::write(exp_dir.join("Cargo.toml"), cargo_toml).await?;
                        tokio::fs::write(src_dir.join("main.rs"), &experiment_code).await?;
                        ("src/main.rs", "cargo", vec!["check".to_string()])
                    }
                };

                if language == "javascript" {
                    tokio::fs::write(exp_dir.join(filename), &experiment_code).await?;
                }

                let output = tokio::process::Command::new(check_cmd)
                    .args(&check_args)
                    .current_dir(&exp_dir)
                    .output()
                    .await?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);
                let success = output.status.success();

                let mut result = format!("Experiment: {}\nLanguage: {}\n", technology, language);
                if !stdout.is_empty() {
                    result.push_str(&format!("STDOUT:\n{}\n", stdout));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("STDERR:\n{}\n", stderr));
                }
                result.push_str(&format!(
                    "Exit code: {}\nResult: {}\n",
                    exit_code,
                    if success { "SUCCESS" } else { "FAILED" }
                ));

                if let Err(e) = tokio::fs::remove_dir_all(&exp_dir).await {
                    tracing::warn!(
                        "Failed to clean up experiment dir '{}': {}",
                        exp_dir.display(),
                        e
                    );
                }
                Ok(result)
            }
            "self_upgrade" => {
                let branch = args["branch"].as_str().unwrap_or("main").to_string();
                let mode = args["mode"].as_str().unwrap_or("auto").to_string();

                let is_valid_branch = !branch.is_empty()
                    && !branch.starts_with('-')
                    && !branch.starts_with('/')
                    && !branch.ends_with('/')
                    && !branch.ends_with('.')
                    && !branch.ends_with(".lock")
                    && !branch.contains("..")
                    && !branch.contains("@{")
                    && !branch.contains("//")
                    && branch != "@"
                    && branch.chars().all(|c| {
                        (c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
                            && !c.is_whitespace()
                            && !c.is_control()
                            && !matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
                    });
                if !is_valid_branch {
                    return Ok(format!(
                        "Self-upgrade failed: invalid branch name '{}'",
                        branch
                    ));
                }

                match learning::self_upgrade(&branch, &mode, None).await {
                    Ok(log) => {
                        self.restart_pending
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(log)
                    }
                    Err(e) => Ok(format!("Self-upgrade failed: {:#}", e)),
                }
            }
            "patch_skill" => {
                let skill_name = args["skill_name"]
                    .as_str()
                    .context("Missing 'skill_name'")?
                    .to_string();
                let patch_content = args["patch_content"]
                    .as_str()
                    .context("Missing 'patch_content'")?
                    .to_string();
                let mode = args["mode"].as_str();
                match learning::self_patch_skill(
                    &self.skills_dir,
                    &skill_name,
                    &patch_content,
                    &self.skills,
                    mode,
                )
                .await
                {
                    Ok(msg) => Ok(msg),
                    Err(e) => Ok(format!("Patch failed: {:#}", e)),
                }
            }
            "read_soul_file" => {
                let file_name = args["file_name"].as_str().context("Missing 'file_name'")?;
                let home = ctx
                    .home_dir
                    .as_ref()
                    .context("No home directory configured")?;
                let path =
                    validate_sandbox_path(home, file_name).unwrap_or_else(|_| home.join(file_name));
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => Ok(content),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Ok(format!("Soul file '{}' does not exist yet.", file_name))
                    }
                    Err(e) => Ok(format!("Error reading soul file: {}", e)),
                }
            }
            "update_soul_file" => {
                let file_name = args["file_name"].as_str().context("Missing 'file_name'")?;
                let content = args["content"].as_str().context("Missing 'content'")?;
                let mode = args["mode"].as_str().unwrap_or("append");

                if content.contains('\0') {
                    return Ok("Content contains null bytes and was rejected.".to_string());
                }
                if content.len() > 100_000 {
                    return Ok(
                        "Content too large (max 100KB). Please consolidate the file first."
                            .to_string(),
                    );
                }

                let home = ctx
                    .home_dir
                    .as_ref()
                    .context("No home directory configured")?;
                let path = home.join(file_name);

                let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();

                let new_content = match mode {
                    "append" => {
                        if existing.trim().is_empty() {
                            if content.starts_with("---") {
                                content.to_string()
                            } else {
                                format!(
                                    "---\nname: {}\nversion: 1\n---\n\n{}",
                                    file_name.trim_end_matches(".md"),
                                    content
                                )
                            }
                        } else {
                            if !existing.trim().starts_with("---") {
                                return Ok("Existing soul file has invalid format (missing frontmatter). Rejected.".to_string());
                            }
                            format!("{}\n{}", existing.trim_end(), content)
                        }
                    }
                    "replace" => {
                        if !content.trim().starts_with("---") {
                            return Ok(
                                "Replace mode requires content with YAML frontmatter".to_string()
                            );
                        }
                        content.to_string()
                    }
                    _ => return Ok("Invalid mode. Use 'append' or 'replace'.".to_string()),
                };

                if !learning::has_valid_frontmatter(&new_content) {
                    return Ok(
                        "Update would produce invalid soul file (missing frontmatter). Rejected."
                            .to_string(),
                    );
                }
                if !new_content.contains("name:") || !new_content.contains("version:") {
                    return Ok(
                        "Update rejected: frontmatter must contain 'name' and 'version' fields."
                            .to_string(),
                    );
                }

                fn bak_path(p: &std::path::Path, suffix: &str) -> PathBuf {
                    format!("{}{}", p.display(), suffix).into()
                }
                for (old, new) in [
                    (bak_path(&path, ".bak.2"), bak_path(&path, ".bak.3")),
                    (bak_path(&path, ".bak.1"), bak_path(&path, ".bak.2")),
                    (bak_path(&path, ".bak"), bak_path(&path, ".bak.1")),
                ] {
                    if old.exists() {
                        let _ = tokio::fs::rename(&old, &new).await;
                    }
                }
                if path.exists() {
                    let _ = tokio::fs::copy(&path, &bak_path(&path, ".bak")).await;
                }

                if let Err(e) = tokio::fs::write(&path, &new_content).await {
                    let bak = bak_path(&path, ".bak");
                    if bak.exists() {
                        let _ = tokio::fs::copy(&bak, &path).await;
                    }
                    return Ok(format!(
                        "Failed to write soul file (restored from backup): {}",
                        e
                    ));
                }

                match tokio::fs::read_to_string(&path).await {
                    Ok(read_back) if read_back == new_content => {
                        self.soul_updated
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(format!(
                            "{} updated successfully. Backup at {}.bak",
                            file_name,
                            path.display()
                        ))
                    }
                    Ok(_) => {
                        let bak = bak_path(&path, ".bak");
                        if bak.exists() {
                            let _ = tokio::fs::copy(&bak, &path).await;
                        }
                        Ok(
                            "Write verification failed (content mismatch). Restored from backup."
                                .to_string(),
                        )
                    }
                    Err(e) => {
                        let bak = bak_path(&path, ".bak");
                        if bak.exists() {
                            let _ = tokio::fs::copy(&bak, &path).await;
                        }
                        Ok(format!(
                            "Write verification error (restored from backup): {}",
                            e
                        ))
                    }
                }
            }
            "revert_soul_file" => {
                let file_name = args["file_name"].as_str().context("Missing 'file_name'")?;
                let home = ctx
                    .home_dir
                    .as_ref()
                    .context("No home directory configured")?;
                let path = home.join(file_name);
                let bak = {
                    let mut s = path.to_string_lossy().to_string();
                    s.push_str(".bak");
                    PathBuf::from(s)
                };
                if !bak.exists() {
                    return Ok(format!("No backup found for {}", file_name));
                }
                match tokio::fs::copy(&bak, &path).await {
                    Ok(_) => Ok(format!("{} restored from backup.", file_name)),
                    Err(e) => Ok(format!("Failed to restore backup: {}", e)),
                }
            }
            _ => anyhow::bail!("BuiltinTools: unknown tool {name}"),
        }
    }
}

const PLAN_POINTER: &str = "active";

fn reject_plan_title(title: &str) -> anyhow::Result<()> {
    let bad = title.trim().is_empty()
        || title.contains('/')
        || title.contains('\\')
        || title.split(['/', '\\']).any(|part| part == "..");
    if bad {
        anyhow::bail!("invalid plan title: {title:?}");
    }
    Ok(())
}

fn plan_file(plans_dir: &std::path::Path, title: &str) -> PathBuf {
    plans_dir.join(format!("{title}.json"))
}

fn ensure_plan_notes(plan: &mut Value) {
    let n = plan
        .get("steps")
        .and_then(|s| s.as_array())
        .map(|s| s.len())
        .unwrap_or(0);
    let Some(obj) = plan.as_object_mut() else {
        return;
    };
    let notes = obj.entry("notes").or_insert_with(|| json!([]));
    if let Some(arr) = notes.as_array_mut() {
        while arr.len() < n {
            arr.push(json!(""));
        }
    }
}

async fn list_plan_titles(plans_dir: &std::path::Path) -> Vec<String> {
    let mut titles = Vec::new();
    let mut dir = match tokio::fs::read_dir(plans_dir).await {
        Ok(dir) => dir,
        Err(_) => return titles,
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(title) = name.strip_suffix(".json") {
            titles.push(title.to_string());
        }
    }
    titles.sort();
    titles
}

fn plans_list(titles: &[String]) -> String {
    if titles.is_empty() {
        "(none)".to_string()
    } else {
        titles.join(", ")
    }
}

async fn read_active_plan(plans_dir: &std::path::Path) -> Option<String> {
    let raw = tokio::fs::read_to_string(plans_dir.join(PLAN_POINTER))
        .await
        .ok()?;
    serde_json::from_str(&raw).ok()
}

async fn write_active_plan(plans_dir: &std::path::Path, title: &str) -> anyhow::Result<()> {
    tokio::fs::write(plans_dir.join(PLAN_POINTER), serde_json::to_string(title)?).await?;
    Ok(())
}

async fn resolve_plan_title(plans_dir: &std::path::Path, args: &Value) -> anyhow::Result<String> {
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        reject_plan_title(title)?;
        return Ok(title.to_string());
    }
    match read_active_plan(plans_dir).await {
        Some(title) => Ok(title),
        None => {
            let existing = list_plan_titles(plans_dir).await;
            anyhow::bail!("no active plan. existing plans: {}", plans_list(&existing))
        }
    }
}

async fn read_plan_file(plans_dir: &std::path::Path, title: &str) -> anyhow::Result<String> {
    match tokio::fs::read_to_string(plan_file(plans_dir, title)).await {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let existing = list_plan_titles(plans_dir).await;
            anyhow::bail!(
                "plan {title:?} not found. existing plans: {}",
                plans_list(&existing)
            )
        }
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tools() -> BuiltinTools {
        BuiltinTools::new(
            PathBuf::from("/tmp/skills"),
            Arc::new(RwLock::new(SkillRegistry::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn test_builtin_tool_definitions_includes_soul_tools() {
        let tools = make_tools();
        let defs = tools.define();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            names.contains(&"read_soul_file"),
            "read_soul_file must be in BuiltinTools definitions"
        );
        assert!(
            names.contains(&"update_soul_file"),
            "update_soul_file must be in BuiltinTools definitions"
        );
        assert!(
            names.contains(&"revert_soul_file"),
            "revert_soul_file must be in BuiltinTools definitions"
        );
    }

    struct NopSender;

    #[async_trait]
    impl crate::platform::sender::PlatformSender for NopSender {
        async fn send_message(
            &self,
            _: &str,
            _: &str,
            _: crate::platform::sender::MessageFormat,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _: &str,
            _: &std::path::Path,
            _: Option<&str>,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn edit_message(
            &self,
            _: &str,
            _: &crate::platform::sender::PlatformMessageId,
            _: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _: &str,
            _: &crate::platform::sender::PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn plan_ctx(sandbox: &std::path::Path) -> ToolContext {
        ToolContext {
            sandbox_dir: sandbox.to_path_buf(),
            home_dir: None,
            sender: Arc::new(NopSender),
            cancel_registry: Arc::new(crate::cancel_registry::CancelRegistry::new()),
            user_id: "u".into(),
            chat_id: "1".into(),
            bot_id: crate::platform::DEFAULT_BOT_ID.into(),
            tool_ui_mode: crate::tool_registry::ToolUiMode::Silent,
        }
    }

    #[tokio::test]
    async fn plan_pointer_notes_and_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let tools = make_tools();
        let ctx = || plan_ctx(dir.path());

        let created = tools
            .execute(
                "plan_create",
                json!({"title": "第一份: plan", "steps": ["a", "b"]}),
                ctx(),
            )
            .await
            .unwrap();
        assert!(created.contains("第一份: plan"));

        let viewed = tools.execute("plan_view", json!({}), ctx()).await.unwrap();
        let plan: Value = serde_json::from_str(&viewed).unwrap();
        assert_eq!(plan["title"], "第一份: plan");
        assert_eq!(plan["notes"], json!(["", ""]));

        tools
            .execute(
                "plan_create",
                json!({"title": "second plan", "steps": ["only"]}),
                ctx(),
            )
            .await
            .unwrap();
        let active = tools.execute("plan_view", json!({}), ctx()).await.unwrap();
        assert!(active.contains("second plan"));
        let first = tools
            .execute("plan_view", json!({"title": "第一份: plan"}), ctx())
            .await
            .unwrap();
        assert!(first.contains("第一份: plan"));
        let still = tools.execute("plan_view", json!({}), ctx()).await.unwrap();
        assert!(still.contains("second plan"));

        tools
            .execute(
                "plan_update",
                json!({"step_id": 0, "status": "done", "notes": "ok"}),
                ctx(),
            )
            .await
            .unwrap();
        let updated: Value =
            serde_json::from_str(&tools.execute("plan_view", json!({}), ctx()).await.unwrap())
                .unwrap();
        assert_eq!(updated["statuses"][0], "done");
        assert_eq!(updated["notes"][0], "ok");

        let missed = tools
            .execute(
                "plan_update",
                json!({"title": "第一份: plan", "step_id": 9, "status": "failed"}),
                ctx(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(missed.contains("out of range"), "{missed}");
        let unchanged: Value = serde_json::from_str(
            &tools
                .execute("plan_view", json!({"title": "第一份: plan"}), ctx())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(unchanged["statuses"][0], "todo");
        let pointer = tools.execute("plan_view", json!({}), ctx()).await.unwrap();
        assert!(pointer.contains("second plan"));

        tools
            .execute(
                "plan_create",
                json!({"title": "second plan", "steps": ["replaced"]}),
                ctx(),
            )
            .await
            .unwrap();
        let replaced: Value =
            serde_json::from_str(&tools.execute("plan_view", json!({}), ctx()).await.unwrap())
                .unwrap();
        assert_eq!(replaced["steps"], json!(["replaced"]));
        assert_eq!(replaced["notes"], json!([""]));

        for title in ["", "   ", "a/b", "a\\b", "..", "foo/.."] {
            let err = tools
                .execute(
                    "plan_create",
                    json!({"title": title, "steps": ["x"]}),
                    ctx(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("invalid plan title"), "{title:?} -> {err}");
        }

        let missing = tools
            .execute("plan_view", json!({"title": "nope"}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(missing.contains("not found"), "{missing}");
        assert!(missing.contains("第一份: plan"), "{missing}");
        assert!(
            !missing.to_ascii_lowercase().contains("os error"),
            "{missing}"
        );
    }

    #[test]
    fn test_soul_tool_definitions_have_required_file_name_enum() {
        let tools = make_tools();
        let defs = tools.define();
        for name in ["read_soul_file", "update_soul_file", "revert_soul_file"] {
            let def = defs
                .iter()
                .find(|d| d.function.name == name)
                .unwrap_or_else(|| panic!("missing tool: {name}"));
            let file_name_schema = &def.function.parameters["properties"]["file_name"];
            assert_eq!(file_name_schema["type"].as_str(), Some("string"));
            let allowed = file_name_schema["enum"].as_array().expect("enum array");
            let allowed_strs: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
            assert!(allowed_strs.contains(&"SOUL.md"));
            assert!(allowed_strs.contains(&"AGENTS.md"));
            assert!(allowed_strs.contains(&"USER.md"));
        }
    }
}
