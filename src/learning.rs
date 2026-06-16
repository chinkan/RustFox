use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::llm::{ChatMessage, LlmClient, MessageContent};
use crate::skills::loader::load_skills_from_dir;
use crate::skills::SkillRegistry;

/// Minimum number of conversation messages needed before updating the user model.
const MIN_MESSAGES_FOR_USER_MODEL: usize = 3;

// ─── Self-Upgrade Helpers ───────────────────────────────────────────────────

/// Result of a self-upgrade operation.
pub struct UpgradeResult {
    pub log: String,
    pub mode: UpgradeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeMode {
    Source,
    Release,
}

/// Detect whether we are running from a source clone or a release binary.
/// Walks up from the current executable to find Cargo.toml.
fn detect_deployment_mode() -> UpgradeMode {
    if let Ok(exe) = std::env::current_exe() {
        let mut root = exe.clone();
        for _ in 0..10 {
            if root.join("Cargo.toml").exists() {
                return UpgradeMode::Source;
            }
            if !root.pop() {
                break;
            }
        }
    }
    UpgradeMode::Release
}

/// Detect if RustFox is running as a systemd/launchd service.
pub fn is_service_installed() -> bool {
    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir().map(|h| {
            h.join(".config")
                .join("systemd")
                .join("user")
                .join("rustfox.service")
        });
        if let Some(p) = service_path {
            if p.exists() {
                return true;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir().map(|h| {
            h.join("Library")
                .join("LaunchAgents")
                .join("com.rustfox.bot.plist")
        });
        if let Some(p) = plist_path {
            if p.exists() {
                return true;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("sc").args(["query", "RustFox"]).output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("STATE") {
                return true;
            }
        }
    }
    false
}

/// Restart the bot: for service mode, restart via systemd/launchd;
/// for foreground mode, spawn new binary and exit.
pub fn restart_bot() -> anyhow::Result<()> {
    if is_service_installed() {
        #[cfg(target_os = "linux")]
        {
            let status = std::process::Command::new("systemctl")
                .args(["--user", "restart", "rustfox.service"])
                .status()
                .context("Failed to run systemctl restart")?;
            if !status.success() {
                tracing::warn!("systemctl restart failed with exit: {:?}", status.code());
            }
        }
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("launchctl")
                .args(["stop", "com.rustfox.bot"])
                .status()
                .context("Failed to run launchctl stop")?;
            if !status.success() {
                tracing::warn!("launchctl stop failed with exit: {:?}", status.code());
            }
        }
        #[cfg(target_os = "windows")]
        {
            let status = std::process::Command::new("sc")
                .args(["stop", "RustFox"])
                .status()
                .context("Failed to run sc stop")?;
            if !status.success() {
                tracing::warn!("sc stop failed with exit: {:?}", status.code());
            }
            let status = std::process::Command::new("sc")
                .args(["start", "RustFox"])
                .status()
                .context("Failed to run sc start")?;
            if !status.success() {
                tracing::warn!("sc start failed with exit: {:?}", status.code());
            }
        }
    } else {
        let exe = std::env::current_exe().context("Failed to get current executable path")?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut child = std::process::Command::new(&exe)
            .args(&args)
            .spawn()
            .context("Failed to spawn new binary")?;
        // Brief pause to detect immediate startup failure.
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Ok(Some(status)) = child.try_wait() {
            anyhow::bail!(
                "New binary exited immediately with code {:?}",
                status.code()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::process::exit(0);
    }
    Ok(())
}

// ─── Feature 1: Post-task Skill Extractor ───────────────────────────────────

/// Analyze a completed agentic loop and, if the conversation used enough tool
/// calls and contains a reusable pattern, auto-generate a new skill in `skills/`.
///
/// This function performs the extraction work when awaited. Callers that want
/// it to run in the background should spawn it explicitly with `tokio::spawn`.
pub async fn post_task_skill_extractor(
    llm: &LlmClient,
    skills_dir: &Path,
    skills: &tokio::sync::RwLock<SkillRegistry>,
    messages: &[ChatMessage],
    tool_call_count: u32,
) {
    info!(
        "Skill extractor triggered: {} tool calls in conversation",
        tool_call_count
    );

    match extract_skill_from_conversation(llm, skills_dir, skills, messages).await {
        Ok(Some(name)) => info!("Auto-generated skill: {}", name),
        Ok(None) => info!("Skill extractor: no reusable pattern found"),
        Err(e) => warn!("Skill extractor failed: {:#}", e),
    }
}

/// Ask the LLM to analyze the conversation and decide whether a reusable skill
/// should be created. Returns `Some(skill_name)` if one was written, `None` if
/// the LLM decided the task was too ad-hoc.
async fn extract_skill_from_conversation(
    llm: &LlmClient,
    skills_dir: &Path,
    skills: &tokio::sync::RwLock<SkillRegistry>,
    messages: &[ChatMessage],
) -> Result<Option<String>> {
    // Build a condensed transcript of the conversation for the LLM.
    let transcript = build_transcript(messages);
    if transcript.is_empty() {
        return Ok(None);
    }

    // Collect existing skill names to avoid duplicates.
    let existing_names: Vec<String> = {
        let reg = skills.read().await;
        reg.list().iter().map(|s| s.name.clone()).collect()
    };

    let analysis_prompt = format!(
        "You are a skill-extraction engine for an AI assistant called RustFox.\n\
         \n\
         Analyze the following conversation transcript and decide if it contains a \
         **reusable, multi-step workflow** that should be saved as a new skill.\n\
         \n\
         Rules:\n\
         - Only create a skill if the pattern would help the assistant handle SIMILAR \
           future requests more efficiently.\n\
         - Do NOT create skills for: one-off questions, trivial lookups, simple math, \
           greetings, or tasks that are too specific to generalize.\n\
         - The skill name must be lowercase letters, numbers, and hyphens only (1-64 chars).\n\
         - These skills already exist (do NOT duplicate): {existing}\n\
         \n\
         If a skill SHOULD be created, respond with EXACTLY this format (no extra text):\n\
         ```\n\
         SKILL_NAME: <name>\n\
         SKILL_DESCRIPTION: <one-line description>\n\
         SKILL_TAGS: <comma-separated tags>\n\
         SKILL_BODY:\n\
         <full markdown body of the skill>\n\
         ```\n\
         \n\
         If NO skill should be created, respond with exactly: NO_SKILL\n\
         \n\
         TRANSCRIPT:\n{transcript}",
        existing = existing_names.join(", "),
        transcript = transcript,
    );

    let analysis_messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(analysis_prompt)),
        tool_calls: None,
        tool_call_id: None,
    }];

    let response = llm.chat(&analysis_messages, &[]).await?;
    let raw = response.content.unwrap_or_default().as_text();

    // Strip outer code fences if the model wrapped its entire response.
    let content = strip_code_fences(&raw);

    if content.trim() == "NO_SKILL" || !content.contains("SKILL_NAME:") {
        return Ok(None);
    }

    // Parse the structured response.
    let name = extract_line_value(&content, "SKILL_NAME:")
        .context("Missing SKILL_NAME in LLM response")?;
    let description = extract_line_value(&content, "SKILL_DESCRIPTION:")
        .unwrap_or_else(|| "Auto-generated skill".to_string());
    let tags_raw = extract_line_value(&content, "SKILL_TAGS:").unwrap_or_default();
    let tags: Vec<String> = tags_raw
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();

    let body = extract_body_after(&content, "SKILL_BODY:")
        .unwrap_or_else(|| "# Auto-generated skill\n\nNo instructions extracted.".to_string());

    // Validate the name.
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        warn!("Skill extractor: invalid name '{}', skipping", name);
        return Ok(None);
    }

    // Skip if a skill with this name already exists.
    if existing_names.iter().any(|n| n == &name) {
        info!("Skill extractor: skill '{}' already exists, skipping", name);
        return Ok(None);
    }

    // Build SKILL.md content with frontmatter.
    // YAML-single-quote description and tag values so characters like `:`, `#`,
    // `]`, or commas don't break the simple frontmatter parser.
    fn yaml_single_quoted(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }
    let tags_yaml = if tags.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            tags.iter()
                .map(|tag| yaml_single_quoted(tag))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let skill_content = format!(
        "---\nname: {name}\ndescription: {description}\ntags: {tags}\n---\n\n{body}\n",
        name = name,
        description = yaml_single_quoted(&description),
        tags = tags_yaml,
        body = body,
    );

    // Write the skill file.
    let skill_dir = skills_dir.join(&name);
    tokio::fs::create_dir_all(&skill_dir)
        .await
        .with_context(|| format!("Failed to create skill directory: {}", skill_dir.display()))?;

    let skill_path = skill_dir.join("SKILL.md");
    tokio::fs::write(&skill_path, &skill_content)
        .await
        .with_context(|| format!("Failed to write skill file: {}", skill_path.display()))?;

    info!("Written auto-generated skill: {}", skill_path.display());

    // Hot-reload skills.
    match load_skills_from_dir(skills_dir, skills_dir.to_path_buf()).await {
        Ok(new_registry) => {
            let count = new_registry.len();
            let mut reg = skills.write().await;
            *reg = new_registry;
            info!("Skills reloaded after extraction: {} skill(s)", count);
        }
        Err(e) => warn!("Failed to reload skills after extraction: {:#}", e),
    }

    Ok(Some(name))
}

// ─── Feature 2: Skill Self-Patch ────────────────────────────────────────────

/// Safely patch an existing skill's SKILL.md by appending or replacing a section.
/// Creates a `.bak` backup before modifying. Returns a status message.
pub async fn self_patch_skill(
    skills_dir: &Path,
    skill_name: &str,
    patch_content: &str,
    skills: &tokio::sync::RwLock<SkillRegistry>,
) -> Result<String> {
    // Validate skill_name to prevent path traversal (e.g. "../secret").
    // Only allow safe directory-name characters.
    if skill_name.is_empty()
        || skill_name.contains('/')
        || skill_name.contains('\\')
        || skill_name.contains("..")
        || skill_name.starts_with('.')
    {
        anyhow::bail!("Invalid skill name: '{}'", skill_name);
    }

    let skill_dir = skills_dir.join(skill_name);

    // Canonicalize to verify the resolved path stays inside skills_dir.
    // We canonicalize skills_dir first (it must exist), then check the prefix.
    let canonical_skills_dir = tokio::fs::canonicalize(skills_dir)
        .await
        .with_context(|| format!("Cannot canonicalize skills dir: {}", skills_dir.display()))?;
    let canonical_skill_dir = tokio::fs::canonicalize(&skill_dir).await.ok();
    if let Some(ref resolved) = canonical_skill_dir {
        if !resolved.starts_with(&canonical_skills_dir) {
            anyhow::bail!("Skill path '{}' escapes the skills directory", skill_name);
        }
    }

    let skill_path = skill_dir.join("SKILL.md");

    if !skill_path.exists() {
        anyhow::bail!("Skill '{}' does not exist", skill_name);
    }

    // Read current content.
    let current = tokio::fs::read_to_string(&skill_path)
        .await
        .with_context(|| format!("Failed to read {}", skill_path.display()))?;

    // Create backup.
    let backup_path = skill_path.with_extension("md.bak");
    tokio::fs::write(&backup_path, &current)
        .await
        .with_context(|| format!("Failed to create backup: {}", backup_path.display()))?;

    // Apply patch: if the patch contains frontmatter (starts with ---), replace
    // the entire file. Otherwise append to the existing content.
    let new_content = if patch_content.starts_with("---") {
        patch_content.to_string()
    } else {
        format!("{}\n\n{}", current.trim_end(), patch_content)
    };

    // Validate that the result still contains frontmatter.
    if !new_content.starts_with("---") {
        anyhow::bail!(
            "Patched content would lose YAML frontmatter — aborting. \
             Backup preserved at {}",
            backup_path.display()
        );
    }

    tokio::fs::write(&skill_path, &new_content)
        .await
        .with_context(|| format!("Failed to write patched skill: {}", skill_path.display()))?;

    info!(
        "Skill '{}' patched ({} → {} bytes)",
        skill_name,
        current.len(),
        new_content.len()
    );

    // Hot-reload skills.
    match load_skills_from_dir(skills_dir, skills_dir.to_path_buf()).await {
        Ok(new_registry) => {
            let count = new_registry.len();
            let mut reg = skills.write().await;
            *reg = new_registry;
            info!("Skills reloaded after patch: {} skill(s)", count);
        }
        Err(e) => warn!("Failed to reload skills after patch: {:#}", e),
    }

    Ok(format!(
        "Skill '{}' patched successfully. Backup at {}",
        skill_name,
        backup_path.display()
    ))
}

// ─── Feature 3: User Model ─────────────────────────────────────────────────

/// Default content for a new USER.md file.
const DEFAULT_USER_MODEL: &str = "\
---
name: user-model
description: Long-term user preferences and context learned across sessions.
tags: [user, preferences, context]
---

# User Model

<!-- Auto-maintained by RustFox. Updated periodically from conversation history. -->

user_name: ~
language: [en]
communication_style: []
preferences: []
interests: []
context: []
";

/// Read the user model file, or return empty string if it doesn't exist.
pub async fn read_user_model(user_model_path: &Path) -> String {
    match tokio::fs::read_to_string(user_model_path).await {
        Ok(content) => content,
        Err(e) => {
            // Only warn if the file exists but can't be read (permission errors, etc.)
            if user_model_path.exists() {
                warn!(
                    "User model file exists but could not be read ({}): {}",
                    user_model_path.display(),
                    e
                );
            }
            String::new()
        }
    }
}

/// Update the user model by summarizing recent conversations through the LLM.
pub async fn update_user_model(
    llm: &LlmClient,
    memory: &crate::memory::MemoryStore,
    user_model_path: &Path,
) {
    match update_user_model_inner(llm, memory, user_model_path).await {
        Ok(true) => info!("User model updated: {}", user_model_path.display()),
        Ok(false) => info!("User model: not enough data to update"),
        Err(e) => warn!("User model update failed: {:#}", e),
    }
}

async fn update_user_model_inner(
    llm: &LlmClient,
    memory: &crate::memory::MemoryStore,
    user_model_path: &Path,
) -> Result<bool> {
    // Load recent conversation messages for context.
    let recent = memory
        .search_messages("user preferences interests communication", 20)
        .await
        .unwrap_or_default();

    if recent.len() < MIN_MESSAGES_FOR_USER_MODEL {
        return Ok(false); // Not enough data yet
    }

    let conversation_snippets: String = recent
        .iter()
        .filter_map(|m| {
            m.content
                .as_ref()
                .map(|c| format!("[{}]: {}", m.role, c.as_text()))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Read existing model.
    let existing = if user_model_path.exists() {
        tokio::fs::read_to_string(user_model_path)
            .await
            .unwrap_or_default()
    } else {
        DEFAULT_USER_MODEL.to_string()
    };

    let prompt = format!(
        "You maintain a concise user profile for an AI assistant.\n\
         \n\
         Current user model:\n```\n{existing}\n```\n\
         \n\
         Recent conversation excerpts:\n```\n{snippets}\n```\n\
         \n\
         Update the user model based on the conversations. Rules:\n\
         - Keep the YAML frontmatter exactly as-is (name, description, tags)\n\
         - Update fields: user_name, language, communication_style, preferences, \
           interests, context\n\
         - Be concise — max 500 words total\n\
         - Only add information the user explicitly stated or strongly implied\n\
         - Do not remove existing valid entries — merge new info\n\
         - Output the COMPLETE updated file (frontmatter + body), nothing else",
        existing = existing,
        snippets = conversation_snippets,
    );

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(prompt)),
        tool_calls: None,
        tool_call_id: None,
    }];

    let response = llm.chat(&messages, &[]).await?;
    let new_content = response.content.unwrap_or_default().as_text();

    // Strict validation: must start with `---` and contain a closing `---`
    // delimiter so we don't write malformed or injection-bearing content into
    // USER.md (which is later injected into the system prompt).
    if !has_valid_frontmatter(&new_content) || new_content.trim().is_empty() {
        warn!("User model update returned invalid content, skipping");
        return Ok(false);
    }

    // Ensure parent directory exists.
    if let Some(parent) = user_model_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    tokio::fs::write(user_model_path, &new_content)
        .await
        .with_context(|| format!("Failed to write user model: {}", user_model_path.display()))?;

    Ok(true)
}

// ─── Feature 4: Self-Update ─────────────────────────────────────────────────

/// Unified self-upgrade: auto-detects deployment mode (source or release binary),
/// upgrades the binary, re-registers the service if running as one, and restarts.
///
/// Returns a status log string. The caller should set `restart_pending` and
/// call `restart_bot()` after the response is delivered to the user.
///
/// If `progress` is `Some(tx)`, sends per-step updates for inline progress display.
///
/// For release binary mode, the `self_update` crate downloads the latest
/// GitHub release matching the current platform. `branch` is ignored.
/// For source mode, `branch` specifies which git branch to build from.
pub async fn self_upgrade(
    branch: &str,
    mode: &str,
    progress: Option<tokio::sync::mpsc::UnboundedSender<String>>,
) -> Result<String> {
    let mut log = String::new();
    // Send a progress update with the most recent line of the log.
    // The closure takes `&mut String` instead of capturing `log` so the
    // immutable borrow does not live across later `log.push_str` calls.
    let send_progress = |log: &mut String| {
        if let Some(tx) = progress.as_ref() {
            let msg = log.lines().last().unwrap_or("").to_string();
            let _ = tx.send(msg);
        }
    };

    let deployment = match mode {
        "source" => UpgradeMode::Source,
        "release" => UpgradeMode::Release,
        _ => detect_deployment_mode(),
    };

    let service_installed = is_service_installed();

    match deployment {
        UpgradeMode::Source => {
            log.push_str(&format!("Mode: source (branch: {})\n", branch));

            let project_root = find_project_root().unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });

            // Step 0: Check for uncommitted changes.
            log.push_str("→ Checking for uncommitted changes...\n");
            send_progress(&mut log);
            let status_output = run_git_command(&project_root, &["status", "--porcelain"]).await?;
            if !status_output.trim().is_empty() {
                let stash_result = run_git_command(
                    &project_root,
                    &["stash", "push", "-m", "rustfox-auto-stash-before-update"],
                )
                .await?;
                log.push_str(&format!("  ⚠ Stashed: {}\n", stash_result.trim()));
            }

            // Step 1: git fetch --all
            log.push_str("→ git fetch --all\n");
            send_progress(&mut log);
            let fetch = run_git_command(&project_root, &["fetch", "--all"]).await?;
            log.push_str(&format!("  ✓ {}\n", fetch.trim()));

            // Step 2: git checkout <branch>
            log.push_str(&format!("→ git checkout {}\n", branch));
            send_progress(&mut log);
            let checkout = run_git_command(&project_root, &["checkout", branch]).await?;
            log.push_str(&format!("  ✓ {}\n", checkout.trim()));

            // Step 3: git pull origin <branch>
            log.push_str(&format!("→ git pull origin {}\n", branch));
            send_progress(&mut log);
            let pull = run_git_command(&project_root, &["pull", "origin", branch]).await?;
            log.push_str(&format!("  ✓ {}\n", pull.trim()));

            // Step 4: cargo build --release
            log.push_str("→ cargo build --release\n");
            send_progress(&mut log);
            let build = run_cargo_build(&project_root).await?;
            log.push_str(&format!("  ✓ {}\n", build.trim()));

            // Step 5: cargo install --path . (if running as service)
            if service_installed {
                log.push_str("→ cargo install --path . --force\n");
                send_progress(&mut log);
                let install_output = tokio::process::Command::new("cargo")
                    .args(["install", "--path", ".", "--force"])
                    .current_dir(&project_root)
                    .output()
                    .await
                    .context("Failed to run cargo install --path .")?;
                let install_log = format!(
                    "{}{}",
                    String::from_utf8_lossy(&install_output.stdout),
                    String::from_utf8_lossy(&install_output.stderr)
                );
                log.push_str(&format!("  ✓ {}\n", install_log.trim()));
            }

            log.push_str("✅ Build successful.");
            send_progress(&mut log);
        }

        UpgradeMode::Release => {
            log.push_str("Mode: release binary\n→ Checking GitHub releases...\n");
            send_progress(&mut log);

            let update_result = tokio::task::spawn_blocking(|| {
                self_update::backends::github::Update::configure()
                    .repo_owner("chinkan")
                    .repo_name("RustFox")
                    .bin_name("rustfox")
                    .show_download_progress(false)
                    .current_version(self_update::cargo_crate_version!())
                    .build()
                    .and_then(|updater| updater.update())
            })
            .await
            .context("spawn_blocking failed")?
            .context("self_update failed")?;

            if update_result.updated() {
                log.push_str(&format!(
                    "→ Updated to version: {}\n",
                    update_result.version()
                ));
                log.push_str("✅ Download and replace successful.");
            } else {
                log.push_str(&format!(
                    "→ Already up to date ({})\n",
                    update_result.version()
                ));
                log.push_str("✅ Already at latest version.");
            }
            send_progress(&mut log);
        }
    }

    // Re-register service if installed.
    if service_installed {
        log.push_str("\n→ Re-registering service...\n");
        send_progress(&mut log);
        let exe = std::env::current_exe().context("Failed to get current executable path")?;
        let service_output = tokio::process::Command::new(&exe)
            .args(["--service", "install"])
            .output()
            .await
            .context("Failed to run rustfox --service install")?;
        if !service_output.status.success() {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&service_output.stdout),
                String::from_utf8_lossy(&service_output.stderr)
            );
            anyhow::bail!(
                "rustfox --service install failed (exit {:?}): {}",
                service_output.status.code(),
                combined.trim()
            );
        }
        let service_log = format!(
            "{}{}",
            String::from_utf8_lossy(&service_output.stdout),
            String::from_utf8_lossy(&service_output.stderr)
        );
        log.push_str(&format!("  ✓ {}\n", service_log.trim()));
    }

    log.push_str("\n✅ Upgrade complete. Restarting...");
    send_progress(&mut log);
    Ok(log)
}

/// Walk up from the current executable to find the project root containing Cargo.toml.
fn find_project_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut root = exe.clone();
    for _ in 0..10 {
        if root.join("Cargo.toml").exists() {
            return Some(root);
        }
        if !root.pop() {
            return None;
        }
    }
    None
}

/// Run a git command in the given directory and return combined stdout+stderr.
async fn run_git_command(dir: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .with_context(|| format!("Failed to execute: git {}", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    if !output.status.success() {
        anyhow::bail!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            combined.trim()
        );
    }

    Ok(combined)
}

/// Run `cargo build --release` in the given directory.
async fn run_cargo_build(dir: &Path) -> Result<String> {
    let output = tokio::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(dir)
        .output()
        .await
        .context("Failed to execute cargo build --release")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    if !output.status.success() {
        anyhow::bail!(
            "cargo build --release failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            combined.trim()
        );
    }

    Ok(combined)
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a condensed transcript from conversation messages for analysis.
fn build_transcript(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant" || m.role == "tool")
        .filter_map(|m| {
            m.content
                .as_ref()
                .map(|c| format!("[{}]: {}", m.role, truncate(&c.as_text(), 500)))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncate a string to at most `max_len` characters.
fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}…", truncated)
    }
}

/// Extract the value after a `KEY:` prefix on a single line.
fn extract_line_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(key) {
            let value = rest.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Extract everything after a `KEY:` line as the body text.
fn extract_body_after(text: &str, key: &str) -> Option<String> {
    if let Some(pos) = text.find(key) {
        let after = &text[pos + key.len()..];
        let body = after.trim().to_string();
        if !body.is_empty() {
            return Some(body);
        }
    }
    None
}

/// Strip outer code fences (```` ``` ```` or ```` ```rust ```` etc.) from a string.
/// If the entire content is wrapped in a single fenced block, return just the
/// inner text; otherwise return the trimmed original.
fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(after_open) = trimmed.strip_prefix("```") {
        // Skip optional language hint on the opening fence line.
        let after_hint =
            after_open.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
        // The rest must start with a newline for this to be a real fence.
        if let Some(inner) = after_hint.strip_prefix('\n') {
            if let Some(close_pos) = inner.rfind("```") {
                return inner[..close_pos].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Return `true` if `content` has a valid YAML frontmatter block, i.e. it
/// starts with `---` and contains a second `---` delimiter on its own line.
pub fn has_valid_frontmatter(content: &str) -> bool {
    let trimmed = content.trim();
    let mut lines = trimmed.lines();
    // The very first line must be exactly "---".
    if lines.next() != Some("---") {
        return false;
    }
    // Look for a closing "---" on its own line.
    for line in lines {
        if line.trim() == "---" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_extract_line_value() {
        let text = "SKILL_NAME: my-new-skill\nSKILL_DESCRIPTION: Does things\n";
        assert_eq!(
            extract_line_value(text, "SKILL_NAME:"),
            Some("my-new-skill".to_string())
        );
        assert_eq!(
            extract_line_value(text, "SKILL_DESCRIPTION:"),
            Some("Does things".to_string())
        );
        assert_eq!(extract_line_value(text, "MISSING:"), None);
    }

    #[test]
    fn test_extract_body_after() {
        let text = "SKILL_BODY:\n# My Skill\n\nDo things step by step.";
        let body = extract_body_after(text, "SKILL_BODY:").unwrap();
        assert!(body.starts_with("# My Skill"));
        assert!(body.contains("step by step"));
    }

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        let result = truncate("hello world", 5);
        assert!(result.starts_with("hello"));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_build_transcript_filters_roles() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("System prompt".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hello".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("Hi there".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let transcript = build_transcript(&messages);
        assert!(!transcript.contains("System prompt"));
        assert!(transcript.contains("[user]: Hello"));
        assert!(transcript.contains("[assistant]: Hi there"));
    }

    #[tokio::test]
    async fn test_read_user_model_nonexistent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.md");
        let content = read_user_model(&path).await;
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn test_read_user_model_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("USER.md");
        tokio::fs::write(&path, "# Test Model\nuser_name: Alice")
            .await
            .unwrap();
        let content = read_user_model(&path).await;
        assert!(content.contains("Alice"));
    }

    #[tokio::test]
    async fn test_self_patch_skill_creates_backup() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();

        let skill_path = skill_dir.join("SKILL.md");
        tokio::fs::write(
            &skill_path,
            "---\nname: test-skill\ndescription: Test\ntags: []\n---\n\n# Original",
        )
        .await
        .unwrap();

        let registry = tokio::sync::RwLock::new(SkillRegistry::new());

        let result = self_patch_skill(
            dir.path(),
            "test-skill",
            "\n## New Section\nAdded.",
            &registry,
        )
        .await
        .unwrap();

        assert!(result.contains("patched successfully"));

        // Verify backup exists.
        let backup = skill_dir.join("SKILL.md.bak");
        assert!(backup.exists());
        let backup_content = tokio::fs::read_to_string(&backup).await.unwrap();
        assert!(backup_content.contains("# Original"));

        // Verify patch was applied.
        let patched = tokio::fs::read_to_string(&skill_path).await.unwrap();
        assert!(patched.contains("# Original"));
        assert!(patched.contains("## New Section"));
    }

    #[tokio::test]
    async fn test_self_patch_nonexistent_skill_fails() {
        let dir = tempdir().unwrap();
        let registry = tokio::sync::RwLock::new(SkillRegistry::new());
        let result = self_patch_skill(dir.path(), "no-such-skill", "patch", &registry).await;
        assert!(result.is_err());
    }
}
