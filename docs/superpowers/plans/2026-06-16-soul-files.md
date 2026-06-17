# Soul Files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add SOUL.md (AI identity), AGENTS.md (learnings), and enhance USER.md with auto-update via AI-driven tools.

**Architecture:** Three markdown files under `<home>/` injected into system prompt every session. Three new built-in tools (`read_soul_file`, `update_soul_file`, `revert_soul_file`) for the AI to read/update them. Three-layer update mechanism: AI-driven via tools (primary), session-end reflection prompt (secondary), cron safety net (tertiary).

**Tech Stack:** Rust, Tokio, existing `Config::resolve()` pattern, existing `tools::execute_builtin_tool` pattern.

---

### Task 1: Add soul file paths to `home.rs` and `config.rs`

**Files:**
- Modify: `src/home.rs:99-108` — `ResolvedPaths` struct
- Modify: `src/home.rs:110-136` — `ensure_dirs()`
- Modify: `src/config.rs:484-506` — `Config::resolve()`
- Modify: `src/config.rs:246-260` — `LearningConfig` struct
- Test: `src/home.rs:265-287` — `ensure_dirs` test

- [ ] **Step 1: Add soul file fields to `ResolvedPaths`**

Replace existing `user_model: PathBuf` with three soul file paths:

```rust
#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub skills: PathBuf,
    pub agents: PathBuf,
    pub artifacts: PathBuf,
    pub soul: PathBuf,     // SOUL.md
    pub agents_md: PathBuf, // AGENTS.md
    pub user_model: PathBuf, // USER.md
}
```

- [ ] **Step 2: Update `ensure_dirs()` to handle soul files**

Soul files are in home dir (no subdirectory needed), but `ensure_dirs` already creates the home dir. No changes needed to directory creation — just update the test.

- [ ] **Step 3: Update `Config::resolve()` in `config.rs`**

Remove the old `resolve_one` call for `user_model_path`. Add three hardcoded paths:

```rust
// In Config::resolve(), replace the old resolve_one for user_model:
// Remove:
//   let user_model = resolve_one("learning.user_model_path", ...);

// Add:
let soul = home.join("SOUL.md");
let agents_md = home.join("AGENTS.md");
let user_model = home.join("USER.md");
```

Update `ResolvedPaths` construction:

```rust
let paths = ResolvedPaths {
    home: home.clone(),
    workspace: workspace.clone(),
    database: database.clone(),
    skills: skills.clone(),
    agents: agents.clone(),
    artifacts: artifacts.clone(),
    soul: soul.clone(),
    agents_md: agents_md.clone(),
    user_model: user_model.clone(),
};
```

Remove the old `self.learning.user_model_path = user_model;` line. Add:

```rust
// Migration: copy old user_model.md to USER.md if different
let old_user_model = home.join("user_model.md");
if old_user_model.exists() && !user_model.exists() {
    if let Ok(content) = std::fs::read_to_string(&old_user_model) {
        std::fs::write(&user_model, &content).ok();
        tracing::info!("Migrated old user_model.md to USER.md");
    }
}
```

- [ ] **Step 4: Update `LearningConfig` struct**

```rust
// In LearningConfig, REMOVE:
//   pub user_model_path: PathBuf,

// Keep everything else: skill_extraction_enabled, skill_extraction_threshold, user_model_update_interval
// user_model_update_interval stays but will be used differently (only for cron safety net)
```

Remove from default:

```rust
// Remove:
//   user_model_path: PathBuf::new(),
```

- [ ] **Step 5: Update test `ensure_dirs_creates_full_tree`**

```rust
let paths = ResolvedPaths {
    home: home.clone(),
    workspace: home.join("workspace"),
    database: home.join("rustfox.db"),
    skills: home.join("skills"),
    agents: home.join("agents"),
    artifacts: home.join("artifacts"),
    soul: home.join("SOUL.md"),
    agents_md: home.join("AGENTS.md"),
    user_model: home.join("USER.md"),
};
```

- [ ] **Step 6: Run build to verify compilation**

```bash
cargo check
```
Expected: passes

- [ ] **Step 7: Commit**

```bash
git add src/home.rs src/config.rs
git commit -m "feat: add soul file paths (SOUL.md, AGENTS.md, USER.md) to home resolution"
```

---

### Task 2: Add `validate_home_path()` to `tools.rs`

**Files:**
- Modify: `src/tools.rs` — add `validate_home_path()`
- Test: add test alongside existing sandbox test

- [ ] **Step 1: Add `validate_home_path()` function**

```rust
/// Validates that a path is within the RustFox home directory.
/// Returns the canonicalized path if valid.
pub fn validate_home_path(home_dir: &Path, requested: &str) -> Result<PathBuf> {
    let home_canonical = home_dir
        .canonicalize()
        .with_context(|| format!("Home directory not found: {}", home_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        home_dir.join(requested)
    };

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

    if !check_path.starts_with(&home_canonical) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the home directory '{}'",
            requested,
            home_dir.display()
        );
    }

    Ok(check_path)
}
```

- [ ] **Step 2: Write test**

```rust
#[test]
fn test_validate_home_path_allows_home_files() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".rustfox");
    std::fs::create_dir_all(&home).unwrap();
    let soul = home.join("SOUL.md");
    std::fs::write(&soul, "# Soul").unwrap();

    let result = validate_home_path(&home, "SOUL.md").unwrap();
    assert_eq!(result, soul.canonicalize().unwrap());
}

#[test]
fn test_validate_home_path_denies_outside() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".rustfox");
    std::fs::create_dir_all(&home).unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "data").unwrap();

    let result = validate_home_path(&home, "../outside.txt");
    assert!(result.is_err());
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p rustfox -- tools::test_validate_home_path --nocapture
```
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add validate_home_path() for soul file access control"
```

---

### Task 3: Add soul file tools to `agent.rs`

**Files:**
- Modify: `src/agent.rs` — add `read_soul_file`, `update_soul_file`, `revert_soul_file` to `execute_tool()`
- Modify: `src/tools.rs` — add tool definitions to `builtin_tool_definitions()`

- [ ] **Step 1: Add soul tool definitions to `builtin_tool_definitions()` in `tools.rs`**

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "read_soul_file".to_string(),
        description: "Read the full contents of a soul file (SOUL.md, AGENTS.md, or USER.md) from the home directory.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "file_name": {
                    "type": "string",
                    "enum": ["SOUL.md", "AGENTS.md", "USER.md"],
                    "description": "Which soul file to read"
                }
            },
            "required": ["file_name"]
        }),
    },
},
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "update_soul_file".to_string(),
        description: "Update a soul file. Use 'append' mode to add content at the end (safe, no data loss). Use 'replace' mode to rewrite the entire file (for consolidation).".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "file_name": {
                    "type": "string",
                    "enum": ["SOUL.md", "AGENTS.md", "USER.md"],
                    "description": "Which soul file to update"
                },
                "content": {
                    "type": "string",
                    "description": "The markdown content to append or replace with"
                },
                "mode": {
                    "type": "string",
                    "enum": ["append", "replace"],
                    "description": "'append' adds content after the frontmatter; 'replace' rewrites the entire file"
                }
            },
            "required": ["file_name", "content", "mode"]
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
                "file_name": {
                    "type": "string",
                    "enum": ["SOUL.md", "AGENTS.md", "USER.md"],
                    "description": "Which soul file to revert"
                }
            },
            "required": ["file_name"]
        }),
    },
},
```

- [ ] **Step 2: Add soul file path helpers**

Add a helper method to `ResolvedPaths` or to `Agent` to get the path for a given soul file name:

```rust
// In agent.rs, add a helper method:
fn soul_file_path(&self, file_name: &str) -> anyhow::Result<PathBuf> {
    let home = self.config.resolved_home()
        .context("Home directory not resolved")?;
    match file_name {
        "SOUL.md" => Ok(home.join("SOUL.md")),
        "AGENTS.md" => Ok(home.join("AGENTS.md")),
        "USER.md" => Ok(home.join("USER.md")),
        _ => anyhow::bail!("Invalid soul file name: {}", file_name),
    }
}
```

- [ ] **Step 3: Add `validate_home_path` check helper to `validate_soul_file_path`**

```rust
async fn validate_soul_file_path(&self, file_name: &str) -> anyhow::Result<PathBuf> {
    let home = self.config.resolved_home()
        .context("Home directory not resolved")?;
    let path = self.soul_file_path(file_name)?;
    tools::validate_home_path(&home, &path.to_string_lossy())
}
```

- [ ] **Step 4: Add `read_soul_file` handler to `execute_tool`**

Before the `_ if self.mcp.is_mcp_tool(name)` match arm, add:

```rust
"read_soul_file" => {
    let file_name = arguments["file_name"]
        .as_str()
        .context("Missing 'file_name' argument")?;
    let path = self.validate_soul_file_path(file_name).await?;
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            format!("Soul file '{}' does not exist yet. It will be created on first update.", file_name)
        }
        Err(e) => format!("Error reading soul file: {}", e),
    }
}
```

- [ ] **Step 5: Add `update_soul_file` handler**

```rust
"update_soul_file" => {
    let file_name = arguments["file_name"]
        .as_str()
        .context("Missing 'file_name'")?;
    let content = arguments["content"]
        .as_str()
        .context("Missing 'content'")?;
    let mode = arguments["mode"]
        .as_str()
        .unwrap_or("append");
    let path = self.validate_soul_file_path(file_name).await?;

    // Validate content: no null bytes
    if content.contains('\0') {
        return "Content contains null bytes and was rejected.".to_string();
    }

    // Hard size limit (checked first — unambiguous rejection)
    if content.len() > 100_000 {
        return "Content too large (max 100KB). Please consolidate the file first.".to_string();
    }
    // Soft size warning: content >50KB is allowed but flagged
    let size_warning = if content.len() > 50_000 {
        format!("\n\n(Warning: content is {} bytes >50KB. Consider consolidating if it keeps growing.)", content.len())
    } else {
        String::new()
    };

    // Read existing content
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();

    // Build new content
    let new_content = match mode {
        "append" => {
            if existing.trim().is_empty() {
                // File doesn't exist or is empty — create with frontmatter
                if content.starts_with("---") {
                    content.to_string()
                } else {
                    format!("---\nname: {}\nversion: 1\n---\n\n{}",
                        file_name.trim_end_matches(".md").to_lowercase(),
                        content)
                }
            } else {
                // Validate existing has frontmatter
                if !existing.trim().starts_with("---") {
                    return "Existing soul file has invalid format (missing frontmatter)".to_string();
                }
                format!("{}\n{}", existing.trim_end(), content)
            }
        }
        "replace" => {
            if !content.trim().starts_with("---") {
                return "Replace mode requires content with YAML frontmatter".to_string();
            }
            content.to_string()
        }
        _ => return "Invalid mode. Use 'append' or 'replace'.".to_string(),
    };

    // Make `has_valid_frontmatter` in `learning.rs` public if not already (add `pub` to fn)
    // Validate frontmatter of new content — check both delimiter AND required fields
    if !crate::learning::has_valid_frontmatter(&new_content) {
        return "Update would produce invalid soul file (missing frontmatter). Rejected.".to_string();
    }
    // Verify required frontmatter fields exist
    if !new_content.contains("name:") || !new_content.contains("version:") {
        return "Update rejected: frontmatter must contain 'name' and 'version' fields.".to_string();
    }

    // Helper: append .bak suffix to path (not with_extension, which replaces the extension)
    fn bak_path(path: &Path, suffix: &str) -> PathBuf {
        let mut s = path.to_string_lossy().to_string();
        s.push_str(suffix);
        PathBuf::from(s)
    }

    // Rotate backups: .bak → .bak.1 → .bak.2 → .bak.3
    // Step 1: shift .bak.2 → .bak.3
    let bak_2 = bak_path(&path, ".bak.2");
    let bak_3 = bak_path(&path, ".bak.3");
    if bak_2.exists() {
        let _ = tokio::fs::rename(&bak_2, &bak_3).await;
    }
    // Step 2: shift .bak.1 → .bak.2
    let bak_1 = bak_path(&path, ".bak.1");
    let bak_2_new = bak_path(&path, ".bak.2");
    if bak_1.exists() {
        let _ = tokio::fs::rename(&bak_1, &bak_2_new).await;
    }
    // Step 3: shift .bak → .bak.1
    let bak_current = bak_path(&path, ".bak");
    let bak_1_new = bak_path(&path, ".bak.1");
    if bak_current.exists() {
        let _ = tokio::fs::rename(&bak_current, &bak_1_new).await;
    }
    // Step 4: copy current file to .bak
    if path.exists() {
        let _ = tokio::fs::copy(&path, &bak_current).await;
    }

    // Write
    if let Err(e) = tokio::fs::write(&path, &new_content).await {
        // Restore from backup on write failure
        if bak_current.exists() {
            let _ = tokio::fs::copy(&bak_current, &path).await;
        }
        return format!("Failed to write soul file (restored from backup): {}", e);
    }

    // Post-write validation: read back and verify
    match tokio::fs::read_to_string(&path).await {
        Ok(read_back) if read_back == new_content => {
            format!("{} updated successfully. Backup at {}{}", file_name, bak_current.display(), size_warning)
        }
        Ok(_) => {
            // Content mismatch — restore from backup
            if bak_current.exists() {
                let _ = tokio::fs::copy(&bak_current, &path).await;
            }
            "Write verification failed (content mismatch). Restored from backup.".to_string()
        }
        Err(e) => {
            if bak_current.exists() {
                let _ = tokio::fs::copy(&bak_current, &path).await;
            }
            format!("Write verification error (restored from backup): {}", e)
        }
    }
}
```

- [ ] **Step 6: Add `revert_soul_file` handler**

```rust
"revert_soul_file" => {
    let file_name = arguments["file_name"]
        .as_str()
        .context("Missing 'file_name'")?;
    let path = self.validate_soul_file_path(file_name).await?;
    let bak = {
        let mut s = path.to_string_lossy().to_string();
        s.push_str(".bak");
        PathBuf::from(s)
    };
    if !bak.exists() {
        return format!("No backup found for {}", file_name);
    }
    match tokio::fs::copy(&bak, &path).await {
        Ok(_) => format!("{} restored from backup.", file_name),
        Err(e) => format!("Failed to restore backup: {}", e),
    }
}
```

- [ ] **Step 7: Add `resolved_home()` method to `Config`** (if not already existing)

```rust
// In config.rs or as an Agent method
pub fn resolved_home(&self) -> Option<&PathBuf> {
    self.resolved_home.as_ref()
}
```

- [ ] **Step 8: Build to verify**

```bash
cargo check
```
Expected: passes

- [ ] **Step 9: Commit**

```bash
git add src/agent.rs src/tools.rs
git commit -m "feat: add read_soul_file, update_soul_file, revert_soul_file tools"
```

---

### Task 4: Inject soul files into system prompt

**Files:**
- Modify: `src/agent.rs:191-217` — `build_system_context()`

- [ ] **Step 1: Update `build_system_context()` to inject SOUL.md + AGENTS.md + USER.md**

```rust
async fn build_system_context(&self) -> String {
    let mut ctx = String::new();
    let home = self.config.resolved_home();

    // Inject SOUL.md
    if let Some(home) = home {
        let soul_path = home.join("SOUL.md");
        let soul_content = crate::learning::read_soul_file(&soul_path).await;
        if !soul_content.is_empty() {
            let truncated = crate::learning::truncate_to(&soul_content, 8_000);
            ctx.push_str("\n\n# My Identity\n<identity>\n");
            ctx.push_str(&truncated);
            ctx.push_str("\n</identity>");
            if truncated.len() < soul_content.len() {
                ctx.push_str("\n[File truncated — use read_soul_file(\"SOUL.md\") for full content]");
            }
        }

        // Inject AGENTS.md
        let agents_path = home.join("AGENTS.md");
        let agents_content = crate::learning::read_soul_file(&agents_path).await;
        if !agents_content.is_empty() {
            let truncated = crate::learning::truncate_to(&agents_content, 8_000);
            ctx.push_str("\n\n# What I've Learned\n<agent_memory>\n");
            ctx.push_str(&truncated);
            ctx.push_str("\n</agent_memory>");
            if truncated.len() < agents_content.len() {
                ctx.push_str("\n[File truncated — use read_soul_file(\"AGENTS.md\") for full content]");
            }
        }

        // Inject USER.md
        let user_path = home.join("USER.md");
        let user_content = crate::learning::read_soul_file(&user_path).await;
        if !user_content.is_empty() {
            let truncated = crate::learning::truncate_to(&user_content, 8_000);
            ctx.push_str("\n\n# User Model\n<user_model>\n");
            ctx.push_str(&truncated);
            ctx.push_str("\n</user_model>");
            if truncated.len() < user_content.len() {
                ctx.push_str("\n[File truncated — use read_soul_file(\"USER.md\") for full content]");
            }
        }
    }

    let now = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    ctx.push_str(&format!("\n\nCurrent date and time: {}", now));
    if let Some(loc) = self.config.user_location() {
        ctx.push_str(&format!("\nUser location: {}", loc));
    }

    ctx
}
```

- [ ] **Step 2: Add `read_soul_file` and `truncate_to` helpers to `learning.rs`**

```rust
/// Read a soul file, returning empty string if it doesn't exist.
pub async fn read_soul_file(path: &Path) -> String {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => String::new(),
    }
}

/// Truncate a string to at most `max_chars` characters at a char boundary.
pub fn truncate_to(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
```

Make `has_valid_frontmatter` public if it isn't already.

- [ ] **Step 3: Build to verify**

```bash
cargo check
```
Expected: passes

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs src/learning.rs
git commit -m "feat: inject SOUL.md, AGENTS.md, USER.md into system prompt every session"
```

---

### Task 5: Session-end reflection

**Files:**
- Modify: `src/agent.rs:40-60` — add `AtomicBool` field to `Agent`
- Modify: `src/agent.rs:538` — add local soul_updated tracking
- Modify: `src/agent.rs:1860` — populate flag in `execute_tool`
- Modify: `src/agent.rs:170` — system prompt instructions
- Modify: `src/agent.rs:1006` — post-loop reflection LLM call

**Approach:** Two-layer reflection:
1. **Inline system prompt** — tells the AI to call `update_soul_file()` before the final answer if anything is worth recording
2. **Post-loop safety net** — if the AI still didn't update any soul file, make one additional LLM call to decide if reflection is needed

- [ ] **Step 1: Add `soul_updated: AtomicBool` field to `Agent` struct**

```rust
// Add to Agent struct:
pub soul_updated: AtomicBool,
```

Initialize in `Agent::new()`:

```rust
soul_updated: AtomicBool::new(false),
```

- [ ] **Step 2: Set flag to true in `update_soul_file` handler**

In the `"update_soul_file"` arm of `execute_tool`, add at the very start:

```rust
self.soul_updated.store(true, std::sync::atomic::Ordering::Relaxed);
```

- [ ] **Step 3: Reset flag before agentic loop and check after**

Before the agentic loop (around line 538), add:

```rust
self.soul_updated.store(false, std::sync::atomic::Ordering::Relaxed);
```

- [ ] **Step 3: Add system prompt instructions in `build_system_prompt()`**

After the Work Verification Protocol section (around line 170), add:

```rust
// Soul file protocol
prompt.push_str(
    "\n\n# Soul Files\n\n\
     You maintain three soul files in your home directory:\n\
     - SOUL.md — your identity, values, and boundaries\n\
     - AGENTS.md — what you've learned across sessions\n\
     - USER.md — the user's preferences and context\n\n\
     When you discover something worth remembering:\n\
     1. Call `update_soul_file()` during the conversation\n\
     2. Use 'append' mode for new observations\n\
     3. Use 'replace' mode only when consolidating\n\n\
     If you reach your final answer and haven't updated any soul file\n\
     but learned something significant, call `update_soul_file()` before\n\
     giving your final response."
);
```

- [ ] **Step 4: Add post-loop reflection LLM call**

After the existing post-loop code (around line 1006), before `return Ok(final_content)`:

```rust
// --- Self-learning: session-end soul reflection ---
// If the AI did not update any soul file during the loop, make one
// additional lightweight LLM call to check if reflection is needed.
if !self.soul_updated.load(std::sync::atomic::Ordering::Relaxed) {
    let reflection_prompt = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(
            "Review the conversation above. Did you learn anything about the \
             user or yourself that should be recorded in SOUL.md, AGENTS.md, \
             or USER.md? If yes, respond with EXACTLY:\n\
             UPDATE_SOUL: <file_name>\n\
             CONTENT:\n\
             <content to append>\n\n\
             If nothing worth recording, respond with: NO_UPDATE".to_string()
        )),
        tool_calls: None,
        tool_call_id: None,
    }];

    if let Ok(reflection_response) = self.llm.chat(&reflection_prompt, &[]).await {
        if let Some(content) = reflection_response.content {
            let text = content.as_text();
            if let Some(rest) = text.strip_prefix("UPDATE_SOUL:") {
                let parts: Vec<&str> = rest.splitn(2, '\n').collect();
                if parts.len() == 2 {
                    let file_name = parts[0].trim();
                    let append_content = parts[1]
                        .strip_prefix("CONTENT:\n")
                        .or_else(|| parts[1].strip_prefix("CONTENT:"))
                        .unwrap_or(parts[1])
                        .trim();
                    // Call update_soul_file with append mode
                    let args = serde_json::json!({
                        "file_name": file_name,
                        "content": append_content,
                        "mode": "append"
                    });
                    let _ = self.execute_tool(
                        "update_soul_file", &args, user_id, parsed_chat_id
                    ).await;
                }
            }
        }
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add soul file protocol and session-end reflection"
```

---

### Task 6: Update cron safety net in `scheduler/tasks.rs`

**Files:**
- Modify: `src/scheduler/tasks.rs` — update cron to check mtime
- Modify: `src/learning.rs` — add `.bak` backup to `update_user_model_inner()`

- [ ] **Step 1: Add `.bak` backup and diff logging to `update_user_model_inner()`**

Before the write at line 552:

```rust
// Helper: append suffix to path
fn bak_path(p: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = p.to_string_lossy().to_string();
    s.push_str(suffix);
    std::path::PathBuf::from(s)
}

// Create backup before overwriting
if user_model_path.exists() {
    let bak_path = bak_path(user_model_path, ".bak");
    let _ = tokio::fs::copy(user_model_path, &bak_path).await;
}

// Compute diff summary for logging
let old_content = if user_model_path.exists() {
    tokio::fs::read_to_string(user_model_path).await.unwrap_or_default()
} else {
    String::new()
};
```

After the existing `tokio::fs::write(...)` (which is followed by `Ok(true)`), add diff logging:

```rust
// Log diff summary (added/removed lines)
let old_lines: usize = old_content.lines().count();
let new_lines: usize = new_content.lines().count();
let added = new_lines.saturating_sub(old_lines);
let removed = old_lines.saturating_sub(new_lines);
info!(
    "User model updated: {} ({} lines, +{}/-{})",
    user_model_path.display(),
    new_lines,
    added,
    removed
);
```

- [ ] **Step 2: Update cron to only fire if mtime > 24h**

```rust
// Weekly user model update (only if no soul updates in 24h)
{
    let memory_clone = _memory.clone();
    let llm_clone = llm.clone();
    let home_path = home.clone(); // home directory (replaces old user_model_path)
    scheduler
        .add_cron_job(&user_model_cron, "weekly-user-model-update", move || {
            let store = memory_clone.clone();
            let llm = llm_clone.clone();
            let path = home_path.join("USER.md");
            let home = home_path.clone();
            Box::pin(async move {
                // Check if any soul file was updated in the last 24h
                let recent_update = ["SOUL.md", "AGENTS.md", "USER.md"].iter().any(|name| {
                    let p = home.join(name);
                    if let Ok(meta) = std::fs::metadata(&p) {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(duration) = modified.elapsed() {
                                return duration < std::time::Duration::from_secs(86400);
                            }
                        }
                    }
                    false
                });

                if recent_update {
                    tracing::info!("Soul files recently updated — skipping cron user model update");
                    return;
                }
                crate::learning::update_user_model(&llm, &store, &path).await;
            })
        })
        .await?;
}
```

Update `register_builtin_tasks` signature in `src/scheduler/tasks.rs:7-15`:

```rust
pub async fn register_builtin_tasks(
    scheduler: &Scheduler,
    _memory: MemoryStore,
    llm: crate::llm::LlmClient,
    summarize_cron: String,
    summarize_threshold: usize,
    user_model_cron: String,
    home: std::path::PathBuf,  // was: user_model_path: PathBuf
) -> anyhow::Result<()> {
```

Inside the cron closure, derive the USER.md path from `home` instead of using the old `model_path`:

```rust
let user_model_path = home.join("USER.md");
```

And replace the closure's `let path = model_path.clone();` with `let path = user_model_path.clone();`.

And update `main.rs:253-261` call site from:

```rust
register_builtin_tasks(
    &scheduler,
    memory.clone(),
    rustfox::llm::LlmClient::new(config.openrouter.clone()),
    config.memory.summarize_cron.clone(),
    config.memory.summarize_threshold,
    config.learning.user_model_cron.clone(),
    config.learning.user_model_path.clone(),  // REMOVE this line
)
```

To:

```rust
let home = config.resolved_home.clone().unwrap_or_else(|| PathBuf::from("."));
register_builtin_tasks(
    &scheduler,
    memory.clone(),
    rustfox::llm::LlmClient::new(config.openrouter.clone()),
    config.memory.summarize_cron.clone(),
    config.memory.summarize_threshold,
    config.learning.user_model_cron.clone(),
    home,
)
```

- [ ] **Step 3: Commit**

```bash
git add src/scheduler/tasks.rs src/learning.rs
git commit -m "feat: add .bak backup to user model updates, mtime check in cron"
```

---

### Task 7: Seed initial soul files on first run

**Files:**
- Modify: `src/home.rs` — seed default SOUL.md and AGENTS.md

- [ ] **Step 1: Add seed constants**

```rust
const DEFAULT_SOUL: &str = "\
---
name: soul
version: 1
---
# Soul

## Who I Am
I'm RustFox, a Telegram AI assistant. I use tools to help the user.

## My Values
- Be genuinely helpful, not performatively helpful
- Have opinions; disagree when warranted
- Earn trust through competence

## My Boundaries
- Private things stay private
- Never send half-baked replies
- I'm a guest in the user's life

## Continuity
Each session I wake up fresh. These files _are_ my memory.
I read them at start, update them at end.
";

const DEFAULT_AGENTS: &str = "\
---
name: agents
version: 1
---
# Agent Memory

## What I've Learned
(Updated by the AI after each session.)

## Repeated Patterns
(Observed workflows, preferences, habits.)
";
```

- [ ] **Step 2: Write default files if not exist in `ensure_dirs()`**

```rust
// After directory creation, write default soul files if missing
for (path, content) in [
    (&paths.soul, DEFAULT_SOUL),
    (&paths.agents_md, DEFAULT_AGENTS),
] {
    if !path.exists() {
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write default {}", path.display()))?;
        tracing::info!("Created default soul file: {}", path.display());
    }
}
```

Add `use tracing;` at top if not already there (it's already used elsewhere in the file).

- [ ] **Step 3: Run build**

```bash
cargo check
```
Expected: passes

- [ ] **Step 4: Commit**

```bash
git add src/home.rs
git commit -m "feat: seed default SOUL.md and AGENTS.md on first run"
```

---

### Task 8: Update all references to old `user_model_path`

**Files:**
- Modify: `src/main.rs:253-261` — `register_builtin_tasks` call
- Modify: `src/agent.rs:979-1006` — remove periodic passive trigger
- Modify: `src/agent.rs:191-196` — replace `user_model_path` with `home.join("USER.md")`
- Modify: `src/setup/wizard.rs:102-104,192-195,866-868` — remove references

- [ ] **Step 1: Update `main.rs` — replace `user_model_path` with `home` in `register_builtin_tasks` call**

Before:
```rust
register_builtin_tasks(
    &scheduler,
    memory.clone(),
    rustfox::llm::LlmClient::new(config.openrouter.clone()),
    config.memory.summarize_cron.clone(),
    config.memory.summarize_threshold,
    config.learning.user_model_cron.clone(),
    config.learning.user_model_path.clone(),  // REMOVE
)
```

After:
```rust
let home = config.resolved_home.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
register_builtin_tasks(
    &scheduler,
    memory.clone(),
    rustfox::llm::LlmClient::new(config.openrouter.clone()),
    config.memory.summarize_cron.clone(),
    config.memory.summarize_threshold,
    config.learning.user_model_cron.clone(),
    home,
)
```

Note: `config.resolved_home` is `Option<PathBuf>`. It will always be `Some` after `config.resolve()` runs (which is called before this point in `main.rs`), so the fallback to `"."` is only a safety measure.

- [ ] **Step 2: Update `agent.rs` — remove the periodic passive trigger (lines ~979-1006)**

Remove this entire block:

```rust
// --- Self-learning: periodic user model update (background) ---
{
    let msg_count = messages.iter().filter(|m| m.role == "user").count();
    let update_interval = self.config.learning.user_model_update_interval;
    if update_interval > 0 && msg_count % update_interval == 0 && msg_count > 0 {
        info!(
            "Triggering periodic user model update ({} user messages)",
            msg_count
        );
        if let Some(agent) = self.self_weak.upgrade() {
            tokio::spawn(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    crate::learning::update_user_model(
                        &agent.llm,
                        &agent.memory,
                        &agent.config.learning.user_model_path,
                    ),
                )
                .await
                {
                    Ok(()) => debug!("Periodic user model update completed"),
                    Err(_) => warn!("Periodic user model update timed out"),
                }
            });
        }
    }
}
```

- [ ] **Step 3: Update `agent.rs` — replace `user_model_path` in `build_system_context`**

Before (line ~194):
```rust
let user_model = crate::learning::read_user_model(&self.config.learning.user_model_path).await;
```

After (already done in Task 4 — uses `home.join("USER.md")` instead):
```rust
// Already updated in Task 4 — no further changes needed here.
```

- [ ] **Step 4: Update `wizard.rs` — remove `user_model_path` from WizardConfig structs**

In `RawLearning` struct (~line 192):
```rust
// REMOVE:
// pub user_model_path: Option<String>,
```

In the handler that maps `RawLearning` → config (~line 866):
```rust
// REMOVE:
// cfg.learning_skill_extraction_enabled = learning.skill_extraction_enabled.unwrap_or(false);
// cfg.learning_skill_extraction_threshold = learning.skill_extraction_threshold.unwrap_or(0);
// cfg.learning_user_model_update_interval = learning.user_model_update_interval.unwrap_or(0);

// The above should stay! Only remove reference to user_model_path.
```

In `SetupWizardConfig` struct (~line 102):
```rust
// REMOVE:
// pub user_model_path: Option<String>,
```

- [ ] **Step 5: Build**

```bash
cargo check
```
Expected: passes

- [ ] **Step 6: Run all tests**

```bash
cargo test
```
Expected: all pass

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/agent.rs src/setup/wizard.rs
git commit -m "refactor: remove old user_model_path config key and passive trigger"
```

---

### Self-Review

**Spec coverage check:**
1. Files & Locations — Task 1 (paths), Task 7 (seeding)
2. Prompt Injection — Task 4 (build_system_context)
3. AI Tools — Task 2 (validate_home_path), Task 3 (tool handlers + definitions)
4. Update Mechanism — Task 5 (system prompt instructions), Task 6 (cron)
5. Safety & Validation — covered in update_soul_file handler (backup rotation, frontmatter check, size limit)
6. Integration Points — Task 8 (remove old references)
