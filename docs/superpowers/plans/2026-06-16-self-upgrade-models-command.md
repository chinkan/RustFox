# Self-Upgrade & `/models` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add unified self-upgrade (source build + release binary download + service reinstall) and a `/models` command to change OpenRouter model at runtime.

**Architecture:** Two independent features sharing the Agent struct. Self-upgrade: rewrite `self_update()` in `learning.rs` → unified `self_upgrade()`, rename tool, add `/self-upgrade` command with inline progress. Models: add `ModelInfo` + `fetch_models()` to `llm.rs`, add `current_model: RwLock<String>` to Agent, add `/models` command with smart search.

**Tech Stack:** Rust, Tokio, Teloxide, `self_update` crate for GitHub release binary downloads.

---

### Task 1: Add `self_update` crate to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add self_update dependency**

Insert after the `reqwest` dependency:
```toml
self_update = { version = "0.44", features = ["archive-tar", "compression-flate2", "archive-zip"] }
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add self_update crate for release binary upgrades"
```

---

### Task 2: Add `ModelInfo` struct and `fetch_models()` to LlmClient

**Files:**
- Modify: `src/llm.rs`

- [ ] **Step 1: Add ModelInfo struct and fetch_models method**

At the end of `src/llm.rs`, before any `#[cfg(test)]` block, add:

```rust
/// Information about an OpenRouter model, deserialized from GET /api/v1/models.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub context_length: u64,
    #[serde(default)]
    pub pricing: ModelPricing,
    #[serde(default)]
    pub architecture: ModelArchitecture,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ModelPricing {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub completion: String,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ModelArchitecture {
    #[serde(default)]
    pub modality: String,
}

#[derive(Debug, serde::Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelInfo>,
}

impl LlmClient {
    /// Fetch the list of available models from OpenRouter.
    /// The endpoint is public (no auth required for GET /api/v1/models),
    /// but we send the API key in case of rate-limiting.
    pub async fn fetch_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.config.base_url);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send()
            .await
            .context("Failed to fetch model list from OpenRouter")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter models API error ({}): {}", status, body);
        }

        let list: ModelsListResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter model list response")?;

        Ok(list.data)
    }
}
```

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 3: Commit**

```bash
git add src/llm.rs
git commit -m "feat: add ModelInfo and fetch_models to LlmClient"
```

---

### Task 3: Rewrite `self_update()` into unified `self_upgrade()` in learning.rs

**Files:**
- Modify: `src/learning.rs`

- [ ] **Step 1: Add new imports to learning.rs**

At the top of `src/learning.rs`, add:
```rust
use std::path::PathBuf;
```

- [ ] **Step 2: Add detection helpers**

After the imports, before `post_task_skill_extractor`, add:
```rust
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
fn is_service_installed() -> bool {
    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir()
            .map(|h| h.join(".config").join("systemd").join("user").join("rustfox.service"));
        if let Some(p) = service_path {
            if p.exists() {
                return true;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .map(|h| h.join("Library").join("LaunchAgents").join("com.rustfox.bot.plist"));
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
            if output.status.success() {
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
            let mut child = std::process::Command::new("systemctl")
                .args(["--user", "restart", "rustfox.service"])
                .spawn()
                .context("Failed to spawn systemctl restart")?;
            let _ = child.wait();
        }
        #[cfg(target_os = "macos")]
        {
            let mut child = std::process::Command::new("launchctl")
                .args(["stop", "com.rustfox.bot"])
                .spawn()
                .context("Failed to spawn launchctl stop")?;
            let _ = child.wait();
        }
        #[cfg(target_os = "windows")]
        {
            let mut child = std::process::Command::new("sc")
                .args(["stop", "RustFox"])
                .spawn()
                .context("Failed to spawn sc stop")?;
            let _ = child.wait();
            let mut child = std::process::Command::new("sc")
                .args(["start", "RustFox"])
                .spawn()
                .context("Failed to spawn sc start")?;
            let _ = child.wait();
        }
    } else {
        let exe = std::env::current_exe().context("Failed to get current executable path")?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::Command::new(exe)
            .args(&args)
            .spawn()
            .context("Failed to spawn new binary")?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::process::exit(0);
    }
    Ok(())
}
```

- [ ] **Step 3: Rewrite `self_update` into unified `self_upgrade` function**

Replace the existing `self_update` function (lines 431-472) with:

```rust
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
    let mut prog = || {
        let tx = progress.as_ref()?;
        let msg = log.lines().last().unwrap_or("").to_string();
        let _ = tx.send(msg);
        Some(())
    };

    let deployment = match mode {
        "source" => UpgradeMode::Source,
        "release" => UpgradeMode::Release,
        _ => detect_deployment_mode(),
    };

    match deployment {
        UpgradeMode::Source => {
            log.push_str(&format!("Mode: source (branch: {})\n", branch));

            // Determine project root from the current executable's location.
            let project_root = find_project_root().unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });

            // Step 0: Check for uncommitted changes.
            log.push_str("→ Checking for uncommitted changes...\n");
            prog();
            let status_output = run_git_command(&project_root, &["status", "--porcelain"]).await?;
            if !status_output.trim().is_empty() {
                let stash_result = run_git_command(
                    &project_root,
                    &["stash", "push", "-m", "rustfox-auto-stash-before-update"],
                ).await?;
                log.push_str(&format!("  ⚠ Stashed: {}\n", stash_result.trim()));
            }

            // Step 1: git fetch --all
            log.push_str("→ git fetch --all\n");
            prog();
            let fetch = run_git_command(&project_root, &["fetch", "--all"]).await?;
            log.push_str(&format!("  ✓ {}\n", fetch.trim()));

            // Step 2: git checkout <branch>
            log.push_str(&format!("→ git checkout {}\n", branch));
            prog();
            let checkout = run_git_command(&project_root, &["checkout", branch]).await?;
            log.push_str(&format!("  ✓ {}\n", checkout.trim()));

            // Step 3: git pull origin <branch>
            log.push_str(&format!("→ git pull origin {}\n", branch));
            prog();
            let pull = run_git_command(&project_root, &["pull", "origin", branch]).await?;
            log.push_str(&format!("  ✓ {}\n", pull.trim()));

            // Step 4: cargo build --release
            log.push_str("→ cargo build --release\n");
            prog();
            let build = run_cargo_build(&project_root).await?;
            log.push_str(&format!("  ✓ {}\n", build.trim()));

            // Step 5: cargo install --path . (if running as service)
            if is_service_installed() {
                log.push_str("→ cargo install --path . --force\n");
                prog();
                let install_output = tokio::process::Command::new("cargo")
                    .args(["install", "--path", ".", "--force"])
                    .current_dir(&project_root)
                    .output()
                    .await
                    .context("Failed to run cargo install --path .")?;
                let install_log = format!("{}{}",
                    String::from_utf8_lossy(&install_output.stdout),
                    String::from_utf8_lossy(&install_output.stderr));
                log.push_str(&format!("  ✓ {}\n", install_log.trim()));
            }

            log.push_str("✅ Build successful.");
            prog();
        }

        UpgradeMode::Release => {
            log.push_str("Mode: release binary\n→ Checking GitHub releases...\n");
            prog();

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

            log.push_str(&format!(
                "→ Updated to version: {}\n",
                update_result.version()
            ));
            log.push_str("✅ Download and replace successful.");
            prog();
        }
    }

    // Re-register service if installed.
    if is_service_installed() {
        log.push_str("\n→ Re-registering service...\n");
        prog();
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("rustfox"));
        let service_output = tokio::process::Command::new(&exe)
            .args(["--service", "install"])
            .output()
            .await
            .context("Failed to run rustfox --service install")?;
        let service_log = format!("{}{}",
            String::from_utf8_lossy(&service_output.stdout),
            String::from_utf8_lossy(&service_output.stderr));
        log.push_str(&format!("  ✓ {}\n", service_log.trim()));
    }

    log.push_str("\n✅ Upgrade complete. Restarting...");
    prog();
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
        root.pop()?;
    }
    None
}
```

- [ ] **Step 4: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 5: Commit**

```bash
git add src/learning.rs
git commit -m "feat: replace self_update with unified self_upgrade (source + release + service)"
```

---

### Task 4: Update tool definition in tools.rs

**Files:**
- Modify: `src/tools.rs`

- [ ] **Step 1: Rename `self_update_to_branch` to `self_upgrade`**

Replace the tool definition (around line 233-249) with:

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "self_upgrade".to_string(),
        description: "Upgrade the bot to the latest version. \
Auto-detects whether running from source code (git pull + cargo build --release) \
or from a pre-built release binary (downloads latest from GitHub). If running as \
a systemd/launchd service, re-registers the service unit. Restarts the bot \
after successful upgrade. Use this when the user asks to update/upgrade the bot.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Git branch to build from (source mode only, default: 'main')"
                },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "source", "release"],
                    "description": "Force a specific upgrade mode (default: 'auto')"
                }
            },
            "required": []
        }),
    },
},
```

- [ ] **Step 2: Commit**

```bash
git add src/tools.rs
git commit -m "feat: rename self_update_to_branch tool to self_upgrade with mode param"
```

---

### Task 5: Update tool_notifier.rs for new tool name

**Files:**
- Modify: `src/platform/tool_notifier.rs`

- [ ] **Step 1: Rename display string**

Replace:
```rust
"self_update_to_branch" => return "🔄 Self-updating".to_string(),
```
with:
```rust
"self_upgrade" => return "🔄 Self-upgrading".to_string(),
```

- [ ] **Step 2: Commit**

```bash
git add src/platform/tool_notifier.rs
git commit -m "chore: update tool_notifier display for self_upgrade"
```

---

### Task 6: Update systemd service template

**Files:**
- Modify: `scripts/services/rustfox.service.template`

- [ ] **Step 1: Change Restart policy**

Replace `Restart=on-failure` with `Restart=always` on line 9 of the template.

- [ ] **Step 2: Commit**

```bash
git add scripts/services/rustfox.service.template
git commit -m "chore: change systemd Restart from on-failure to always"
```

---

### Task 7: Add fields and methods to Agent in agent.rs

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add new imports at top of agent.rs**

Add to imports (note: `PathBuf` is already imported at line 2, only add what's missing):
```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

- [ ] **Step 2: Add new fields to Agent struct (after line 54)**

```rust
pub restart_pending: AtomicBool,
pub current_model: tokio::sync::RwLock<String>,
pub config_path: PathBuf,
```

- [ ] **Step 3: Initialize new fields in Agent::new()**

After `langsmith,` in the Self block, add:
```rust
restart_pending: AtomicBool::new(false),
current_model: tokio::sync::RwLock::new(config.openrouter.model.clone()),
config_path,
```

Update the `new()` function signature to accept `config_path: PathBuf`:
```rust
pub fn new(
    config: Config,
    mcp: McpManager,
    memory: MemoryStore,
    skills: SkillRegistry,
    agents: SkillRegistry,
    task_store: ScheduledTaskStore,
    scheduler: Arc<Scheduler>,
    bot: Arc<Bot>,
    self_weak: Weak<Agent>,
    job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
    langsmith: Arc<LangSmithClient>,
    config_path: PathBuf,  // NEW
) -> Self {
```

- [ ] **Step 4: Update chat_completion call site (line ~558)**

Replace:
```rust
let completion_result =
    self.llm.chat_completion(&prompt.messages, &all_tools).await;
```
with:
```rust
let model = self.current_model.read().await.clone();
let completion_result =
    self.llm.chat_completion_with_model(&prompt.messages, &all_tools, &model).await;
```

- [ ] **Step 5: Add set_model method to Agent**

Add after the `reload_skills_and_agents` method:
```rust
/// Change the active model and persist to config.toml.
pub async fn set_model(&self, model_id: &str) -> anyhow::Result<()> {
    if model_id.is_empty() {
        anyhow::bail!("Model ID cannot be empty");
    }

    // Persist to config.toml (reuse TOML-edit pattern from mcp.rs).
    let content = tokio::fs::read_to_string(&self.config_path)
        .await
        .context("Failed to read config.toml")?;
    let mut doc: toml::value::Table = toml::from_str(&content)
        .context("Failed to parse config.toml")?;

    doc.entry("openrouter")
        .and_modify(|section| {
            if let Some(table) = section.as_table_mut() {
                table.insert(
                    "model".to_string(),
                    toml::Value::String(model_id.to_string()),
                );
            }
        });

    let new_content = toml::to_string_pretty(&doc)
        .context("Failed to serialize config.toml")?;
    tokio::fs::write(&self.config_path, &new_content)
        .await
        .with_context(|| format!("Failed to write {}", self.config_path.display()))?;

    // Update in-memory model.
    let mut current = self.current_model.write().await;
    *current = model_id.to_string();

    tracing::info!(model = %model_id, "Model changed and persisted");
    Ok(())
}
```

- [ ] **Step 6: Rename tool dispatch arm (line ~2482)**

Replace `"self_update_to_branch" => {` with `"self_upgrade" => {`, and replace the body (lines 2482-2541):

```rust
"self_upgrade" => {
    let branch = arguments["branch"].as_str().unwrap_or("main").to_string();
    let mode = arguments["mode"].as_str().unwrap_or("auto").to_string();

    // Validate branch name (same checks as before).
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
        return format!("Self-upgrade failed: invalid branch name '{}'", branch);
    }

    info!("Self-upgrade requested: branch '{}', mode '{}'", branch, mode);

    match crate::learning::self_upgrade(&branch, &mode, None).await {
        Ok(log) => {
            self.restart_pending.store(true, Ordering::Release);
            log
        }
        Err(e) => format!("Self-upgrade failed: {:#}", e),
    }
}
```

- [ ] **Step 7: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add restart_pending, current_model, set_model; update chat_completion; rename tool dispatch"
```

---

### Task 8: Update main.rs to pass config_path to Agent

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Pass config_path to Agent::new()**

In the `Agent::new()` call inside `Arc::new_cyclic`, add `config_path.clone()` as the new last argument:

```rust
let agent = Arc::new_cyclic(|weak| {
    Agent::new(
        config.clone(),
        mcp_manager,
        memory.clone(),
        skills,
        agents,
        task_store.clone(),
        Arc::clone(&scheduler),
        Arc::clone(&bot),
        weak.clone(),
        job_tx,
        Arc::clone(&langsmith),
        config_path.clone(),  // NEW
    )
});
```

- [ ] **Step 2: Commit**

```bash
git add src/main.rs
git commit -m "feat: pass config_path to Agent::new()"
```

---

### Task 9: Add `/self-upgrade` command + restart logic to telegram.rs

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add new command handler before the LLM section (after `/queryrewrite` block at line ~424)**

Insert after line 424 (`return Ok(());`):

```rust
if let Some((cmd, arg)) = parse_command(&text) {
    if cmd == "self-upgrade" || cmd == "selfupgrade" {
        let branch = if arg.is_empty() { "main" } else { &arg };

        // Set up progress channel for per-step inline updates.
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<String>();

        let sent = bot
            .send_message(msg.chat.id, "🔄 Starting self-upgrade...")
            .await?;

        // Run the upgrade in a background task so we can update the message.
        let bot_clone = bot.clone();
        let bot_progress = bot.clone();
        let chat_id = msg.chat.id;
        let msg_id = sent.id;
        let branch_owned = branch.to_string();

        // Spawn a progress listener that edits the message on each update.
        let progress_handle = tokio::spawn(async move {
            let mut buffer = String::from("🔄 Self-upgrading...\n");
            while let Some(step) = progress_rx.recv().await {
                buffer.push_str(&format!("{}\n", step));
                // Keep message under 4000 chars.
                if buffer.len() > 3500 {
                    let suffix = "\n...(truncated)";
                    let trunc = buffer.len() - 3500 + suffix.len();
                    buffer = format!("...{}", &buffer[buffer.len().saturating_sub(trunc)..]);
                    buffer.push_str(suffix);
                }
                let _ = bot_progress.edit_message_text(chat_id, msg_id, &buffer).await;
            }
        });

        // Run the upgrade.
        let result = crate::learning::self_upgrade(&branch_owned, "auto", Some(progress_tx)).await;

        // Wait for progress to be fully displayed.
        drop(progress_rx);
        progress_handle.await.ok();

        match result {
            Ok(log) => {
                let display = if log.len() > 3500 {
                    format!("{}...\n(truncated)", &log[..3500])
                } else {
                    log
                };
                bot_clone.edit_message_text(chat_id, msg_id, &format!("✅ Upgrade successful!\n\n{}", display)).await.ok();
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let _ = crate::learning::restart_bot();
            }
            Err(e) => {
                bot_clone.edit_message_text(chat_id, msg_id, &format!("❌ Upgrade failed:\n{}", e)).await.ok();
            }
        }

        return Ok(());
    }
}
```

- [ ] **Step 2: Add restart check after process_message completes**

After line ~665 (the `Ok(())` at the end of `handle_message`), just before it, add the restart check. Insert after `// Success: response already delivered via streaming`:

```rust
    // Check if a self-upgrade tool call requested a restart.
    if agent.restart_pending.load(std::sync::atomic::Ordering::Acquire) {
        agent.restart_pending.store(false, std::sync::atomic::Ordering::Release);
        let _ = bot.send_message(msg.chat.id, "🔄 Self-upgrade complete. Restarting...").await;
        // Spawn restart with a delay to allow message delivery.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = crate::learning::restart_bot();
        });
    }
```

- [ ] **Step 3: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: add /self-upgrade command with inline progress and restart logic"
```

---

### Task 10: Add `/models` command handler to telegram.rs

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add ModelInfo import to telegram.rs**

Add at the top of `src/platform/telegram.rs`, alongside the existing imports:
```rust
use crate::llm::ModelInfo;
```

- [ ] **Step 2: Add /models command handler**

Add this after the `/self-upgrade` block (inserted in Task 9), but before the `// Send "typing" indicator` line:

```rust
    if let Some((cmd, arg)) = parse_command(&text) {
        if cmd == "models" {
            if arg.is_empty() {
                let current = agent.current_model.read().await;
                let reply = format!(
                    "Current model: `{}`\n\nTo change model, use:\n\
                     `/models <model-id>` — exact model ID\n\
                     `/models <keyword>` — search by name\n\
                     Example: `/models claude` to search for Claude models",
                    *current
                );
                bot.send_message(msg.chat.id, escape_text(&reply))
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }

            // Fetch model list.
            let models = match agent.llm.fetch_models().await {
                Ok(list) => list,
                Err(e) => {
                    bot.send_message(
                        msg.chat.id,
                        escape_text(&format!("Failed to fetch model list: {:#}", e)),
                    )
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                    return Ok(());
                }
            };

            // Try exact match first.
            if let Some(model) = models.iter().find(|m| m.id == arg) {
                match agent.set_model(&model.id).await {
                    Ok(()) => {
                        let reply = format!(
                            "✅ Model changed to `{}` ({})",
                            model.id, model.name
                        );
                        bot.send_message(msg.chat.id, escape_text(&reply))
                            .parse_mode(ParseMode::MarkdownV2)
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(
                            msg.chat.id,
                            escape_text(&format!("Failed to save model: {:#}", e)),
                        )
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                    }
                }
                return Ok(());
            }

            // Fuzzy search: case-insensitive match on id or name.
            let query = arg.to_lowercase();
            let mut matches: Vec<&ModelInfo> = models
                .iter()
                .filter(|m| {
                    m.id.to_lowercase().contains(&query)
                        || m.name.to_lowercase().contains(&query)
                })
                .collect();
            matches.sort_by(|a, b| {
                // Prefer name matches over id matches.
                let a_name = a.name.to_lowercase().contains(&query);
                let b_name = b.name.to_lowercase().contains(&query);
                b_name.cmp(&a_name).then(a.id.cmp(&b.id))
            });
            matches.truncate(10);

            if matches.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    escape_text(&format!(
                        "No models found matching '{}'. Try a different search term.",
                        arg
                    )),
                )
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
                return Ok(());
            }

            if matches.len() == 1 {
                // Auto-select if single result.
                let model = &matches[0];
                match agent.set_model(&model.id).await {
                    Ok(()) => {
                        let reply = format!(
                            "✅ Model changed to `{}` ({})",
                            model.id, model.name
                        );
                        bot.send_message(msg.chat.id, escape_text(&reply))
                            .parse_mode(ParseMode::MarkdownV2)
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(
                            msg.chat.id,
                            escape_text(&format!("Failed to save model: {:#}", e)),
                        )
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                    }
                }
                return Ok(());
            }

            // Multiple results: show list.
            let mut reply = format!(
                "Found {} models matching '{}':\n\n",
                matches.len(),
                arg
            );
            for model in &matches {
                reply.push_str(&format!(
                    "`{}` — {} ({} context)\n",
                    model.id,
                    model.name,
                    if model.context_length > 0 {
                        format!("{}K", model.context_length / 1024)
                    } else {
                        "??".to_string()
                    }
                ));
            }
            reply.push_str(&format!(
                "\nSelect by typing: `/models <model-id>`"
            ));
            bot.send_message(msg.chat.id, escape_text(&reply))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
            return Ok(());
        }
    }
```

Note: The `parse_command` helper already exists in `telegram.rs:57-67`. The `/self-upgrade` and `/models` handlers should share a single `parse_command` dispatch block to avoid redundant parsing. Place them together after the existing command chain (before `// Send "typing" indicator`):

```rust
// Combined parse_command dispatch for /self-upgrade and /models.
if let Some((cmd, arg)) = parse_command(&text) {
    match cmd.as_str() {
        "self-upgrade" | "selfupgrade" => {
            // ... self-upgrade handler from Task 9 ...
        }
        "models" => {
            // ... models handler from Task 10 ...
        }
        _ => {} // ignore unknown commands
    }
}
```

The existing simple `==` checks (`/clear`, `/start`, `/tools`, etc.) remain as-is for backward compatibility.

- [ ] **Step 3: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: add /models command with smart search and hot-reload"
```

---

### Task 11: Register new commands in supported_commands()

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Add new BotCommands to supported_commands()**

In the `supported_commands()` function (line 74-87), add after the `queryrewrite` entry:

```rust
BotCommand::new("self-upgrade", "Upgrade the bot to the latest version (source or release)"),
BotCommand::new("models", "Browse and change the OpenRouter model"),
```

The full function will return 8 entries.

- [ ] **Step 2: Verify cargo check passes**

Run: `cargo check`
Expected: Compilation succeeds

- [ ] **Step 3: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: register /self-upgrade and /models in supported_commands"
```

---

### Self-Review

After all tasks are complete, verify:

1. **Spec coverage:** Every requirement from the spec has a corresponding task:
   - Unified `self_upgrade()` with source + release + service modes → Task 3
   - `self_upgrade` tool (renamed) → Tasks 4, 5, 7
   - `/self-upgrade` command with inline progress → Task 9
   - Restart logic (foreground + service) → Task 3 (restart_bot), Task 9 (command), Task 7 (restart_pending)
   - Service template update → Task 6
   - `ModelInfo` + `fetch_models()` → Task 2
   - `current_model: RwLock<String>` → Task 7
   - `set_model()` method → Task 7
   - `/models` command with smart search → Task 10
   - `supported_commands()` → Task 11
   - `config_path` → Task 7, 8

2. **No placeholders:** All code blocks are concrete.

3. **Type consistency:** Function signatures match between tasks. `self_upgrade(branch, mode, Option<UnboundedSender<String>>)` returns `Result<String>`, `restart_bot()` returns `Result<()>`, `set_model(model_id)` returns `Result<()>`.

4. **Run cargo clippy and cargo test:**

```bash
cargo clippy -- -D warnings
cargo test
```
