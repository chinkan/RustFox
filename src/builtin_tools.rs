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
use crate::tools::{
    sandbox_list_dir, sandbox_open_for_send, sandbox_read_to_string, sandbox_write,
    validate_sandbox_path,
};

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
                    description: "Upgrade the bot to the latest version. Telegram /selfupgrade accepts only bare (auto), main (source+main), or release; other git branches need this tool with mode=source.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "branch": { "type": "string", "description": "Git branch to build from (source mode only, default: 'main'). Advanced: any branch with mode=source; Telegram UX is limited to main|release|bare." },
                            "mode": { "type": "string", "enum": ["auto", "source", "release"], "description": "Force a specific upgrade mode (default: 'auto'). Explicit source refuses if no Cargo.toml near the exe (ADR-0030)." }
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
                let sandbox = ctx.sandbox_dir.clone();
                let path_owned = path.to_string();
                let content = tokio::task::spawn_blocking(move || {
                    sandbox_read_to_string(&sandbox, &path_owned)
                })
                .await
                .context("read_file task failed")??;
                Ok(content)
            }
            "write_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let content = args["content"]
                    .as_str()
                    .context("Missing 'content' argument")?;
                let sandbox = ctx.sandbox_dir.clone();
                let path_owned = path.to_string();
                let content_owned = content.to_string();
                let resolved = tokio::task::spawn_blocking(move || {
                    sandbox_write(&sandbox, &path_owned, &content_owned)
                })
                .await
                .context("write_file task failed")??;
                Ok(format!(
                    "Wrote {} bytes to {}",
                    content.len(),
                    resolved.display()
                ))
            }
            "list_files" => {
                let path = args["path"].as_str().unwrap_or(".");
                let sandbox = ctx.sandbox_dir.clone();
                let path_owned = path.to_string();
                let entries =
                    tokio::task::spawn_blocking(move || sandbox_list_dir(&sandbox, &path_owned))
                        .await
                        .context("list_files task failed")??;
                Ok(entries.join("\n"))
            }
            "send_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let caption = args
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .filter(|c| !c.is_empty());
                let sandbox = ctx.sandbox_dir.clone();
                let path_owned = path.to_string();
                // validate + no-follow open (ADR-0025 §2 / 7a)
                let (resolved, size) = tokio::task::spawn_blocking(move || {
                    sandbox_open_for_send(&sandbox, &path_owned)
                })
                .await
                .context("send_file task failed")??;
                const TG_FILE_LIMIT: u64 = 50 * 1024 * 1024;
                if size > TG_FILE_LIMIT {
                    anyhow::bail!(
                        "File is {} MB — exceeds Telegram's 50 MB limit",
                        size / 1024 / 1024
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
                migrate_plans_dir(&plans_dir).await;
                // Overwrite an existing plan with this title in place (old- or
                // new-format file); otherwise use the encoded filename.
                let path = match find_plan_path(&plans_dir, title).await? {
                    Some(path) => path,
                    None => plan_file(&plans_dir, title),
                };
                tokio::fs::write(path, serde_json::to_string_pretty(&plan)?).await?;
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
                let (plan_path, content) = read_plan_file(&plans_dir, &title).await?;
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
                let (_, content) = read_plan_file(&plans_dir, &title).await?;
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

/// Longest encoded stem kept verbatim before truncating + hashing (ADR-0026).
const PLAN_STEM_MAX: usize = 120;

/// ADR-0026: `p_` + percent-encoded title bytes; only `[a-z0-9_%A-F-]` + `.json`.
/// Uppercase letters are encoded too, so names never collide on case-insensitive
/// filesystems (NTFS, APFS).
fn plan_file_name(title: &str) -> String {
    use std::fmt::Write;
    let mut stem = String::new();
    for b in title.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' {
            stem.push(b as char);
        } else {
            let _ = write!(stem, "%{b:02X}");
        }
    }
    if stem.len() + 2 > PLAN_STEM_MAX {
        use sha2::Digest;
        let mut cut = PLAN_STEM_MAX - 19;
        // Never split a `%XX` triple.
        while stem[..cut].ends_with('%') || stem[..cut - 1].ends_with('%') {
            cut -= 1;
        }
        let hash = sha2::Sha256::digest(title.as_bytes());
        stem.truncate(cut);
        // Distinct `h_` prefix: a short title can never equal a hashed name.
        let mut name = format!("h_{stem}_");
        for b in &hash[..8] {
            let _ = write!(name, "{b:02x}");
        }
        return name + ".json";
    }
    format!("p_{stem}.json")
}

fn plan_file(plans_dir: &std::path::Path, title: &str) -> PathBuf {
    plans_dir.join(plan_file_name(title))
}

/// Title stored inside a plan file, if it parses.
fn plan_json_title(content: &str) -> Option<String> {
    let v: Value = serde_json::from_str(content).ok()?;
    v.get("title")?.as_str().map(str::to_string)
}

/// All `(path, title)` pairs: every `*.json` in the dir, title from its JSON.
async fn scan_plans(plans_dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(mut dir) = tokio::fs::read_dir(plans_dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            if let Some(title) = plan_json_title(&content) {
                out.push((path, title));
            }
        }
    }
    out
}

/// Encoded file first, then any old-format file whose JSON title matches.
/// An encoded-name file holding a different title is a conflict (ADR-0026).
async fn find_plan_path(
    plans_dir: &std::path::Path,
    title: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let encoded = plan_file(plans_dir, title);
    if tokio::fs::try_exists(&encoded).await.unwrap_or(false) {
        let content = tokio::fs::read_to_string(&encoded).await?;
        let found = plan_json_title(&content);
        if found.as_deref() != Some(title) {
            let existing = found.unwrap_or_else(|| encoded.display().to_string());
            anyhow::bail!(
                "Another plan named \"{existing}\" already exists. Please choose a different name."
            );
        }
        return Ok(Some(encoded));
    }
    Ok(scan_plans(plans_dir)
        .await
        .into_iter()
        .find(|(_, t)| t == title)
        .map(|(p, _)| p))
}

/// ADR-0026: recover plans hidden in NTFS alternate data streams (Windows only).
/// Runs once per plans dir per process.
async fn migrate_plans_dir(plans_dir: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static DONE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
        let dir = plans_dir.to_path_buf();
        let first = DONE
            .get_or_init(Default::default)
            .lock()
            .map(|mut set| set.insert(dir.clone()))
            .unwrap_or(false);
        if !first {
            return;
        }
        if let Err(err) = tokio::task::spawn_blocking(move || ads_migrate::run(&dir)).await {
            tracing::warn!("plan ADS migration task failed: {err}");
        }
    }
    #[cfg(not(windows))]
    let _ = plans_dir;
}

#[cfg(windows)]
mod ads_migrate {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[repr(C)]
    struct Win32FindStreamData {
        stream_size: i64,
        stream_name: [u16; 296],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn FindFirstStreamW(
            file_name: *const u16,
            info_level: i32,
            data: *mut Win32FindStreamData,
            flags: u32,
        ) -> isize;
        fn FindNextStreamW(handle: isize, data: *mut Win32FindStreamData) -> i32;
        fn FindClose(handle: isize) -> i32;
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    const INVALID_HANDLE_VALUE: isize = -1;
    const ERROR_HANDLE_EOF: i32 = 38;
    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    /// Move without replacing an existing target (std::fs::rename replaces).
    fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        let (f, t) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated UTF-16.
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Named stream names (without leading `:` and trailing `:$DATA`).
    pub(super) fn named_streams(path: &Path) -> Vec<String> {
        let wide = wide(path);
        let mut data = Win32FindStreamData {
            stream_size: 0,
            stream_name: [0; 296],
        };
        let mut names = Vec::new();
        // SAFETY: wide is NUL-terminated; data is a valid out-buffer.
        let handle = unsafe { FindFirstStreamW(wide.as_ptr(), 0, &mut data, 0) };
        if handle == INVALID_HANDLE_VALUE {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_HANDLE_EOF) {
                tracing::warn!("plan migration: FindFirstStreamW {path:?}: {err}");
            }
            return names;
        }
        loop {
            let len = data.stream_name.iter().position(|&c| c == 0).unwrap_or(296);
            let raw = String::from_utf16_lossy(&data.stream_name[..len]);
            if let Some(name) = raw.strip_prefix(':').and_then(|r| r.strip_suffix(":$DATA")) {
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
            // SAFETY: handle came from FindFirstStreamW.
            if unsafe { FindNextStreamW(handle, &mut data) } == 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_HANDLE_EOF) {
                    tracing::warn!("plan migration: FindNextStreamW {path:?}: {err}");
                }
                break;
            }
        }
        // SAFETY: valid find handle.
        unsafe { FindClose(handle) };
        names
    }

    const MIGRATION_TMP_SUFFIX: &str = ".rfmig.tmp";

    pub(super) fn run(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        // Only migration-owned temp files (`*.rfmig.tmp`); other `*.tmp` files are left alone.
        let is_tmp = |p: &Path| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(MIGRATION_TMP_SUFFIX))
        };
        let entries: Vec<_> = entries.flatten().collect();
        for entry in &entries {
            let path = entry.path();
            if is_tmp(&path) {
                if let Err(err) = std::fs::remove_file(&path) {
                    tracing::warn!("plan migration: remove leftover {path:?}: {err}");
                }
            }
        }
        for entry in entries {
            if is_tmp(&entry.path()) {
                continue;
            }
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let base = entry.path();
            let Some(base_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let mut migrated = false;
            for stream in named_streams(&base) {
                let Some(rest) = stream.strip_suffix(".json") else {
                    continue;
                };
                if migrate_stream(dir, &base_name, &stream, rest) {
                    migrated = true;
                }
            }
            if migrated && named_streams(&base).is_empty() {
                match std::fs::metadata(&base) {
                    Ok(m) if m.len() == 0 => {
                        if let Err(err) = std::fs::remove_file(&base) {
                            tracing::warn!("plan migration: remove {base:?}: {err}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// True when the stream was migrated and deleted. On a name collision the
    /// plan is recovered as "<title> (recovered)", "<title> (recovered 2)", ...
    fn migrate_stream(dir: &Path, base_name: &str, stream: &str, rest: &str) -> bool {
        let stream_path = dir.join(format!("{base_name}:{stream}"));
        let content = match std::fs::read_to_string(&stream_path) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!("plan migration: read {stream_path:?}: {err}");
                return false;
            }
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            tracing::warn!("plan migration: {stream_path:?} is not valid JSON; left as-is");
            return false;
        };
        let original =
            super::plan_json_title(&content).unwrap_or_else(|| format!("{base_name}:{rest}"));
        for n in 0..1000u32 {
            let title = match n {
                0 => original.clone(),
                1 => format!("{original} (recovered)"),
                n => format!("{original} (recovered {n})"),
            };
            let target = super::plan_file(dir, &title);
            if target.exists() {
                tracing::warn!("plan migration: {target:?} already exists; not overwriting");
                continue;
            }
            let mut plan = value.clone();
            let Some(obj) = plan.as_object_mut() else {
                tracing::warn!("plan migration: {stream_path:?} is not a JSON object; kept");
                return false;
            };
            obj.insert("title".into(), serde_json::Value::String(title.clone()));
            let body = match serde_json::to_string_pretty(&plan) {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!("plan migration: serialize {stream_path:?}: {err}");
                    return false;
                }
            };
            // Temp file (`*.rfmig.tmp`, never listed) -> move without replace -> verify.
            let mut tmp_name = target.clone().into_os_string();
            tmp_name.push(MIGRATION_TMP_SUFFIX);
            let tmp = std::path::PathBuf::from(tmp_name);
            let moved = std::fs::write(&tmp, body).and_then(|_| move_no_replace(&tmp, &target));
            if let Err(err) = moved {
                let _ = std::fs::remove_file(&tmp);
                if matches!(
                    err.raw_os_error(),
                    Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS)
                ) {
                    tracing::warn!("plan migration: {target:?} already exists; not overwriting");
                    continue;
                }
                tracing::warn!("plan migration: write {target:?}: {err}; stream kept");
                return false;
            }
            let verified = std::fs::read_to_string(&target)
                .ok()
                .and_then(|c| super::plan_json_title(&c));
            if verified.as_deref() != Some(title.as_str()) {
                tracing::warn!("plan migration: {target:?} failed verification; stream kept");
                let _ = std::fs::remove_file(&target);
                return false;
            }
            if let Err(err) = std::fs::remove_file(&stream_path) {
                tracing::warn!("plan migration: delete stream {stream_path:?}: {err}");
                return false;
            }
            tracing::info!("plan migration: recovered {title:?} from {stream_path:?}");
            return true;
        }
        tracing::warn!("plan migration: no free name for {original:?}; stream kept");
        false
    }
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
    migrate_plans_dir(plans_dir).await;
    let mut titles: Vec<String> = scan_plans(plans_dir)
        .await
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    titles.sort();
    titles.dedup();
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

async fn read_plan_file(
    plans_dir: &std::path::Path,
    title: &str,
) -> anyhow::Result<(PathBuf, String)> {
    migrate_plans_dir(plans_dir).await;
    if let Some(path) = find_plan_path(plans_dir, title).await? {
        let content = tokio::fs::read_to_string(&path).await?;
        return Ok((path, content));
    }
    let existing = list_plan_titles(plans_dir).await;
    anyhow::bail!(
        "plan {title:?} not found. existing plans: {}",
        plans_list(&existing)
    )
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

    /// #173: the shipped verifier agent gets exactly the built-in tools its
    /// AGENT.md `tools:` lists, and can read a workspace file inside the sandbox.
    #[tokio::test]
    async fn verifier_subagent_reads_workspace_file_with_declared_tools_only() {
        let agents_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("agents");
        let agents = crate::skills::loader::load_skills_from_dir(&agents_dir, agents_dir.clone())
            .await
            .unwrap();
        let verifier = agents.get("verifier").expect("verifier agent ships");
        assert_eq!(verifier.tools, vec!["read_file", "list_files", "plan_view"]);

        // Same whitelist + filter run_subagent applies to the tool registry.
        let allowed = crate::agent::effective_subagent_tools(&verifier.tools);
        let tools = make_tools();
        let exposed: Vec<String> = tools
            .define()
            .into_iter()
            .map(|d| d.function.name)
            .filter(|n| allowed.contains(n))
            .collect();
        for want in ["read_file", "list_files", "plan_view"] {
            assert!(exposed.contains(&want.to_string()), "{want} missing");
        }
        for unlisted in ["write_file", "send_file", "plan_create", "plan_update"] {
            assert!(
                !exposed.contains(&unlisted.to_string()),
                "{unlisted} leaked"
            );
        }

        // Reads go through the shared sandbox tool and stay confined.
        let root = tempfile::tempdir().unwrap();
        let sandbox = root.path().join("workspace");
        std::fs::create_dir_all(sandbox.join("daily")).unwrap();
        std::fs::write(sandbox.join("daily/note.md"), "2026-10-08 ok").unwrap();
        std::fs::write(root.path().join("secret.txt"), "nope").unwrap();
        let ctx = || plan_ctx(&sandbox);

        let body = tools
            .execute("read_file", json!({"path": "daily/note.md"}), ctx())
            .await
            .unwrap();
        assert_eq!(body, "2026-10-08 ok");
        let abs = sandbox.join("daily/note.md").display().to_string();
        let body = tools
            .execute("read_file", json!({"path": abs}), ctx())
            .await
            .unwrap();
        assert!(body.contains("2026-10-08"));
        let listing = tools
            .execute("list_files", json!({"path": "daily"}), ctx())
            .await
            .unwrap();
        assert!(listing.contains("note.md"));

        for escape in [
            "../secret.txt".to_string(),
            root.path().join("secret.txt").display().to_string(),
        ] {
            assert!(
                tools
                    .execute("read_file", json!({"path": escape}), ctx())
                    .await
                    .is_err(),
                "read outside sandbox must fail: {escape}"
            );
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

    /// Test-only inverse of `plan_file_name` (non-truncated names).
    fn decode_plan_file_name(name: &str) -> Option<String> {
        let stem = name.strip_prefix("p_")?.strip_suffix(".json")?;
        let bytes = stem.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(u8::from_str_radix(stem.get(i + 1..i + 3)?, 16).ok()?);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).ok()
    }

    #[test]
    fn plan_file_name_encodes_and_round_trips() {
        for title in [
            "Plan: A",
            "a:b:c",
            "x/y\\z",
            "v1.0.6 release",
            "第一份: plan",
            "CON",
            "NUL",
            "com1",
            "..",
            "plain_title-1",
        ] {
            let name = plan_file_name(title);
            assert!(name.starts_with("p_") && name.ends_with(".json"), "{name}");
            let stem = name.strip_suffix(".json").unwrap();
            assert!(
                stem.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b"_%-ABCDEF".contains(&b)),
                "{name}"
            );
            assert_eq!(decode_plan_file_name(&name).as_deref(), Some(title));
        }
        // A crafted short title shaped like a hashed stem stays in `p_` space.
        let long = plan_file_name(&"x".repeat(300));
        let crafted = long
            .strip_prefix("h_")
            .unwrap()
            .strip_suffix(".json")
            .unwrap();
        let crafted_name = plan_file_name(crafted);
        assert!(crafted_name.starts_with("p_"), "{crafted_name}");
        assert_ne!(crafted_name, long);
        let crafted2 = long.strip_suffix(".json").unwrap();
        assert_ne!(plan_file_name(crafted2), long);
        let literal = plan_file_name("h_abc_0123456789abcdef");
        assert_eq!(literal, "p_h_abc_0123456789abcdef.json");
        assert!(!literal.starts_with("h_"));
        assert_eq!(plan_file_name("Plan: A"), "p_%50lan%3A%20%41.json");
        assert_eq!(plan_file_name("CON"), "p_%43%4F%4E.json");
        assert_eq!(plan_file_name("plan"), "p_plan.json");
        assert_ne!(
            plan_file_name("Plan").to_lowercase(),
            plan_file_name("plan").to_lowercase()
        );
    }

    #[test]
    fn long_plan_titles_truncate_with_hash() {
        let a = "计划".repeat(200);
        let b = format!("{a}x");
        let (na, nb) = (plan_file_name(&a), plan_file_name(&b));
        assert!(na.len() <= PLAN_STEM_MAX + 5, "{}", na.len());
        assert_ne!(na, nb);
        assert_eq!(na, plan_file_name(&a));
        let stem = na.strip_suffix(".json").unwrap();
        assert!(na.starts_with("h_"), "{na}");
        let (head, hash) = stem.rsplit_once('_').unwrap();
        assert_eq!(hash.len(), 16);
        assert!(!head.ends_with('%') && !head[..head.len() - 1].ends_with('%'));
    }

    #[tokio::test]
    async fn colon_titles_create_list_view_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let tools = make_tools();
        let ctx = || plan_ctx(dir.path());
        for title in ["Plan: A", "Plan: B", "a:b:c", "CON", "计划：一"] {
            tools
                .execute(
                    "plan_create",
                    json!({"title": title, "steps": ["s"]}),
                    ctx(),
                )
                .await
                .unwrap();
        }
        let plans_dir = dir.path().join(".plans");
        assert!(plans_dir.join("p_%50lan%3A%20%41.json").is_file());
        assert!(plans_dir.join("p_a%3Ab%3Ac.json").is_file());
        for name in std::fs::read_dir(&plans_dir).unwrap() {
            let name = name.unwrap().file_name().into_string().unwrap();
            assert!(!name.contains(':'), "{name}");
        }
        let titles = list_plan_titles(&plans_dir).await;
        assert_eq!(
            titles,
            vec!["CON", "Plan: A", "Plan: B", "a:b:c", "计划：一"]
        );
        for title in &titles {
            let viewed = tools
                .execute("plan_view", json!({"title": title}), ctx())
                .await
                .unwrap();
            let plan: Value = serde_json::from_str(&viewed).unwrap();
            assert_eq!(plan["title"], title.as_str());
        }
        tools
            .execute(
                "plan_update",
                json!({"title": "a:b:c", "step_id": 0, "status": "done"}),
                ctx(),
            )
            .await
            .unwrap();
        let err = tools
            .execute("plan_view", json!({"title": "missing"}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("a:b:c") && err.contains("Plan: A"), "{err}");
    }

    #[tokio::test]
    async fn case_only_titles_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let tools = make_tools();
        let ctx = || plan_ctx(dir.path());
        for (title, step) in [("Plan", "upper"), ("plan", "lower")] {
            tools
                .execute(
                    "plan_create",
                    json!({"title": title, "steps": [step]}),
                    ctx(),
                )
                .await
                .unwrap();
        }
        let plans_dir = dir.path().join(".plans");
        assert_eq!(list_plan_titles(&plans_dir).await, vec!["Plan", "plan"]);
        for (title, step) in [("Plan", "upper"), ("plan", "lower")] {
            let plan: Value = serde_json::from_str(
                &tools
                    .execute("plan_view", json!({"title": title}), ctx())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(plan["title"], title);
            assert_eq!(plan["steps"], json!([step]));
        }
    }

    #[tokio::test]
    async fn mismatched_title_at_encoded_path_is_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let path = plans_dir.join(plan_file_name("mine"));
        let other =
            json!({"title": "someone else", "steps": ["x"], "statuses": ["todo"]}).to_string();
        std::fs::write(&path, &other).unwrap();
        let tools = make_tools();
        let ctx = || plan_ctx(dir.path());
        for (tool, args) in [
            ("plan_create", json!({"title": "mine", "steps": ["a"]})),
            ("plan_view", json!({"title": "mine"})),
            (
                "plan_update",
                json!({"title": "mine", "step_id": 0, "status": "done"}),
            ),
        ] {
            let err = tools
                .execute(tool, args, ctx())
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("Another plan named \"someone else\" already exists"),
                "{tool}: {err}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), other);
    }

    #[tokio::test]
    async fn old_format_plan_files_still_listed_and_updated_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let old = plans_dir.join("legacy plan.json");
        std::fs::write(
            &old,
            json!({"title": "legacy plan", "steps": ["a"], "statuses": ["todo"]}).to_string(),
        )
        .unwrap();
        std::fs::write(plans_dir.join("README"), "not a plan").unwrap();
        std::fs::write(
            plans_dir.join("p_x.json.tmp"),
            json!({"title": "temp", "steps": []}).to_string(),
        )
        .unwrap();
        let tools = make_tools();
        let ctx = || plan_ctx(dir.path());
        assert_eq!(list_plan_titles(&plans_dir).await, vec!["legacy plan"]);
        tools
            .execute(
                "plan_update",
                json!({"title": "legacy plan", "step_id": 0, "status": "done"}),
                ctx(),
            )
            .await
            .unwrap();
        let plan: Value = serde_json::from_str(&std::fs::read_to_string(&old).unwrap()).unwrap();
        assert_eq!(plan["statuses"][0], "done");
        assert!(!plans_dir.join(plan_file_name("legacy plan")).exists());
        // Re-creating the same title overwrites the old file, no duplicate.
        tools
            .execute(
                "plan_create",
                json!({"title": "legacy plan", "steps": ["b"]}),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(list_plan_titles(&plans_dir).await, vec!["legacy plan"]);
        assert_eq!(
            std::fs::read_to_string(plans_dir.join("README")).unwrap(),
            "not a plan"
        );
    }

    /// Real NTFS alternate-data-stream fixture; reproduces v1.0.5 writes.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_ads_hidden_plans_are_migrated_once() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan = |t: &str| json!({"title": t, "steps": ["s"], "statuses": ["todo"]}).to_string();
        // v1.0.5 wrote `{title}.json`: these land in streams of base file `Plan`.
        std::fs::write(plans_dir.join("Plan: A.json"), plan("Plan: A")).unwrap();
        std::fs::write(plans_dir.join("Plan: B.json"), plan("Plan: B")).unwrap();
        // Stream on base `Busy` whose target encoded name already exists.
        std::fs::write(plans_dir.join("Busy: x.json"), plan("Busy: x")).unwrap();
        std::fs::write(
            plans_dir.join("p_%42usy%3A%20x.json"),
            "{\"title\":\"Busy: x\",\"keep\":1}",
        )
        .unwrap();
        // Leftover temp file from an interrupted run: removed on first pass.
        std::fs::write(plans_dir.join("p_x.json.rfmig.tmp"), "{\"title\":\"x\"}").unwrap();
        // Unrelated `*.tmp` (not migration-owned): must stay byte-for-byte.
        std::fs::write(plans_dir.join("foo.tmp"), "user data").unwrap();
        // Ordinary extensionless files with no named streams: untouched.
        std::fs::write(plans_dir.join("notes"), "").unwrap();
        std::fs::write(plans_dir.join("notes2"), "data").unwrap();

        let visible: Vec<String> = std::fs::read_dir(&plans_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        if visible.iter().any(|n| n.contains(':')) {
            eprintln!("skipping: filesystem has no NTFS alternate data streams (e.g. Wine)");
            return;
        }
        assert!(ads_migrate::named_streams(&plans_dir.join("Plan")).len() == 2);

        let titles = list_plan_titles(&plans_dir).await;
        assert_eq!(
            titles,
            vec!["Busy: x", "Busy: x (recovered)", "Plan: A", "Plan: B"]
        );
        assert!(
            !plans_dir.join("Plan").exists(),
            "0-byte stream-free base removed"
        );
        assert!(plans_dir.join("p_%50lan%3A%20%41.json").is_file());
        // Collision: existing file untouched, stream recovered under a new title.
        assert_eq!(
            std::fs::read_to_string(plans_dir.join("p_%42usy%3A%20x.json")).unwrap(),
            "{\"title\":\"Busy: x\",\"keep\":1}"
        );
        let (_, recovered) = read_plan_file(&plans_dir, "Busy: x (recovered)")
            .await
            .unwrap();
        let recovered: Value = serde_json::from_str(&recovered).unwrap();
        assert_eq!(recovered["steps"], json!(["s"]));
        assert!(!plans_dir.join("Busy").exists());
        assert!(
            !plans_dir.join("p_x.json.rfmig.tmp").exists(),
            "leftover migration tmp cleaned"
        );
        assert_eq!(
            std::fs::read_to_string(plans_dir.join("foo.tmp")).unwrap(),
            "user data",
            "unrelated tmp untouched"
        );
        assert_eq!(
            std::fs::read_to_string(plans_dir.join("notes")).unwrap(),
            ""
        );
        assert_eq!(
            std::fs::read_to_string(plans_dir.join("notes2")).unwrap(),
            "data"
        );

        let snapshot = |d: &std::path::Path| {
            let mut v: Vec<_> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    (e.file_name(), std::fs::read(e.path()).unwrap())
                })
                .collect();
            v.sort();
            v
        };
        let before = snapshot(&plans_dir);
        ads_migrate::run(&plans_dir);
        migrate_plans_dir(&plans_dir).await;
        assert_eq!(before, snapshot(&plans_dir), "second run is a no-op");
    }
}
