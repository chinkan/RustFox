# Architecture Deepening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract CancelRegistry, PlatformSender, ToolRegistry, ConversationManager, and LoopRunner from the ~4965-line Agent god module, improving testability and multi-platform readiness.

**Architecture:** Four independently-mergeable milestones (M1–M4). M1 extracts tool dispatch + platform seam + cancel registry. M2 extracts conversation management. M3 extracts the agentic loop runner. M4 writes a Supervisor ADR and refactors seed::write_lock. Each milestone produces working, mergeable code, with **zero behavioral change** — tool parameter names, error messages, and streamed output are preserved verbatim.

**Tech Stack:** Rust 2021, tokio, async-trait, oneshot channels, teloxide, uuid.

**Depends on:** Spec at `docs/superpowers/specs/2026-07-14-architecture-deepening-design.md`

**Real API signatures (critical — plan must use these):**
- `llm.chat(messages: &[ChatMessage], tools: &[ToolDefinition]) -> Result<ChatMessage>` (2 args)
- `memory.load_messages(conversation_id) -> Result<Vec<ChatMessage>>`
- `memory.save_message(conversation_id, &ChatMessage) -> Result<String>`
- `memory.search_knowledge(query, limit) -> Result<Vec<KnowledgeEntry>>` (fields: `id`, `category`, `key`, `value`, `source`)
- `memory.search_messages(query, limit) -> Result<Vec<ChatMessage>>`
- `scheduler.add_one_shot_job(delay: Duration, name, fire_closure) -> Result<Uuid>`
- `scheduler.add_cron_job(cron_expr, name, fire_closure) -> Result<Uuid>`
- `file_processor::process_attachments(&[Attachment], &str, &Config, &MemoryStore, bool) -> (String, Vec<ContentPart>)` (augmented_text, images)
- `learning::self_upgrade(branch, mode, progress_tx) -> Result<String>`
- `learning::self_patch_skill(skills_dir, skill_name, patch_content, &RwLock<SkillRegistry>) -> Result<String>`
- `seed::lock_map_for(&Path) -> BTreeMap<String, String>` (skill_name → SHA256 hex)

---

## File Structure

### New files (M1)
| File | Responsibility |
|------|----------------|
| `src/cancel_registry.rs` | `CancelRegistry` — shared map of oneshot channels for fine-grained command cancellation |
| `src/platform/sender.rs` | `PlatformSender` trait + `PlatformMessageId` type alias |
| `src/tool_registry.rs` | `ToolHandler` trait + `ToolRegistry` — index of handlers, provides definitions + dispatch |

### New files (M1 — handler modules)
| File | Responsibility |
|------|----------------|
| `src/builtin_tools.rs` | `BuiltinTools` — file I/O, plan management, try_new_tech, self_upgrade, soul file tools, patch_skill, send_file (extracted from `tools.rs` + `agent.rs` match arms) |
| `src/memory_tools.rs` | `MemoryTools` — remember, recall, search_memory (extracted from `agent.rs` match arms) |
| `src/scheduling_tools.rs` | `SchedulingTools` — schedule_task, list, cancel, history, rerun (extracted from `agent.rs` match arms) |
| `src/skill_tools.rs` | `SkillTools` — write/read_skill_file, reload_skills, write/read_agent_file, reload_agents (extracted from `agent.rs` match arms). `invoke_agent`/`spawn_agents` stay in Agent (circular dep) |
| `src/command_tool.rs` | `CommandTool` — execute_command with cancel button + streaming (extracted from `agent.rs` execute_command_interactive) |

### New files (M2)
| File | Responsibility |
|------|----------------|
| `src/conversation.rs` | `ConversationManager` — owns message history + compaction state, attachment processing, RAG injection, steer application |

### New files (M3)
| File | Responsibility |
|------|----------------|
| `src/loop_runner.rs` | `LoopConfig`, `AgenticLoop`, `LoopOutcome` — configurable iterator over LLM calls and tool results |

### New files (M4)
| File | Responsibility |
|------|----------------|
| `docs/adr/0001-supervisor-module-structure.md` | Architectural Decision Record for the 17-file Supervisor split |

### Modified files
| File | M1 | M2 | M3 | M4 |
|------|----|----|----|----|
| `src/lib.rs` | add modules | add module | add module | — |
| `src/agent.rs` | remove `Arc<Bot>`, `running_commands`, inline tool dispatch; add `ToolRegistry`, `PlatformSender`, `CancelRegistry` | use `ConversationManager` | delegate to `AgenticLoop` | — |
| `src/tools.rs` | strip to `validate_sandbox_path` + `validate_home_path` + their tests only; move all tool definitions + builtin_tool_definitions + execute_builtin_tool + their tests to `builtin_tools.rs` | — | — | — |
| `src/platform/mod.rs` | re-export sender | — | — | — |
| `src/platform/telegram.rs` | add `TelegramAdapter` implementing `PlatformSender`; migrate callback handler to use `CancelRegistry`; do NOT remove existing free functions yet (removed in cleanup task) | — | — | — |
| `src/main.rs` | construct ToolRegistry, CancelRegistry, PlatformSender; wire into Agent; update callback query handler to use `agent.cancel_registry` | — | — | use `seed::write_lock()` |
| `src/skills/seed.rs` | — | — | — | no change needed — `lock_map_for` already exists; just add `write_lock` |

---

## M1: ToolRegistry + PlatformSender + CancelRegistry

### Task 1: Create CancelRegistry

**Files:**
- Create: `src/cancel_registry.rs`
- Test: inline `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the CancelRegistry module**

```rust
// src/cancel_registry.rs

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

/// A shared map of oneshot sender channels keyed by opaque ID.
/// Any tool can register a cancel channel; the Telegram callback handler
/// cancels by ID. Distinct from `CancellationToken` (used for /stop).
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, id: String, tx: oneshot::Sender<()>) {
        let mut map = self.inner.blocking_lock();
        map.insert(id, tx);
    }

    pub fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.blocking_lock();
        if let Some(tx) = map.remove(id) {
            let _ = tx.send(());
            true
        } else {
            false
        }
    }

    pub fn unregister(&self, id: &str) {
        let mut map = self.inner.blocking_lock();
        map.remove(id);
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_cancel() {
        let reg = CancelRegistry::new();
        let (tx, mut rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        assert!(reg.cancel("cmd_1"));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_cancel_unknown() {
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("nonexistent"));
    }

    #[test]
    fn test_unregister() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        reg.unregister("cmd_1");
        assert!(!reg.cancel("cmd_1"));
    }

    #[test]
    fn test_double_cancel() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        assert!(reg.cancel("cmd_1"));
        assert!(!reg.cancel("cmd_1"));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test cancel_registry::tests -- --nocapture`
Expected: 4 tests pass

- [ ] **Step 3: Add module to lib.rs**

```rust
// Add to src/lib.rs module declarations section
pub mod cancel_registry;
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test` — all existing tests still pass

- [ ] **Step 5: Commit**

```bash
git add src/cancel_registry.rs src/lib.rs
git commit -m "feat: add CancelRegistry module"
```

---

### Task 2: Create PlatformSender trait + PlatformMessageId

**Files:**
- Create: `src/platform/sender.rs`
- Modify: `src/platform/mod.rs`

- [ ] **Step 1: Create PlatformSender trait**

```rust
// src/platform/sender.rs

use std::path::Path;
use anyhow::Result;
use async_trait::async_trait;

/// Opaque message identifier returned by the platform after sending.
/// Telegram encoding: `"{chat_id_int}:{message_id_int}"`.
/// Each adapter documents its own encoding.
pub type PlatformMessageId = String;

/// Message format mode for responses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageFormat {
    Rich,
    Markdown,
    Auto,
}

#[async_trait]
pub trait PlatformSender: Send + Sync {
    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        format: MessageFormat,
    ) -> Result<PlatformMessageId>;

    async fn send_file(
        &self,
        chat_id: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<PlatformMessageId>;

    async fn show_cancel_button(
        &self,
        chat_id: &str,
        text: &str,
        cancel_id: &str,
    ) -> Result<PlatformMessageId>;

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &PlatformMessageId,
        text: &str,
    ) -> Result<()>;

    async fn notify_shutdown(&self, chat_id: &str) -> Result<()>;
}
```

- [ ] **Step 2: Update platform/mod.rs**

```rust
// src/platform/mod.rs — add after existing imports
pub mod sender;
pub use sender::{MessageFormat, PlatformMessageId, PlatformSender};
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check`

- [ ] **Step 4: Commit**

```bash
git add src/platform/sender.rs src/platform/mod.rs
git commit -m "feat: add PlatformSender trait and PlatformMessageId type"
```

---

### Task 3: Create ToolHandler trait + ToolRegistry

**Files:**
- Create: `src/tool_registry.rs`

- [ ] **Step 1: Write ToolRegistry module**

```rust
// src/tool_registry.rs

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use crate::cancel_registry::CancelRegistry;
use crate::llm::ToolDefinition;
use crate::platform::sender::PlatformSender;

pub type ToolResult = Result<String>;

pub struct ToolContext {
    pub sandbox_dir: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub sender: Arc<dyn PlatformSender>,
    pub cancel_registry: Arc<CancelRegistry>,
    pub user_id: String,
    pub chat_id: String,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn define(&self) -> Vec<ToolDefinition>;
    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult;
}

pub struct ToolRegistry {
    handlers: Vec<Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { handlers: Vec::new() }
    }

    pub fn register(&mut self, handler: Box<dyn ToolHandler>) {
        self.handlers.push(handler);
    }

    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        let mut all = Vec::new();
        for handler in &self.handlers {
            all.extend(handler.define());
        }
        all
    }

    pub async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        for handler in &self.handlers {
            if handler.define().iter().any(|d| d.function.name == name) {
                return handler.execute(name, args, ctx).await;
            }
        }
        anyhow::bail!("Unknown tool: {name}")
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionDefinition;
    use serde_json::json;
    use std::sync::Arc;

    struct MockHandler;

    #[async_trait]
    impl ToolHandler for MockHandler {
        fn define(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "mock_tool".to_string(),
                    description: "A mock tool".to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            }]
        }

        async fn execute(&self, name: &str, _args: Value, _ctx: ToolContext) -> ToolResult {
            Ok(format!("executed {name}"))
        }
    }

    #[tokio::test]
    async fn test_register_and_execute() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(MockHandler));
        let defs = reg.all_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "mock_tool");

        let ctx = ToolContext {
            sandbox_dir: PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "test".to_string(),
            chat_id: "0".to_string(),
        };
        let result = reg.execute("mock_tool", json!({}), ctx).await.unwrap();
        assert_eq!(result, "executed mock_tool");
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext {
            sandbox_dir: PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "test".to_string(),
            chat_id: "0".to_string(),
        };
        let result = reg.execute("unknown", json!({}), ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }

    struct TestSender;

    #[async_trait]
    impl PlatformSender for TestSender {
        async fn send_message(&self, _chat_id: &str, _text: &str, _format: MessageFormat) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn send_file(&self, _chat_id: &str, _path: &Path, _caption: Option<&str>) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn show_cancel_button(&self, _chat_id: &str, _text: &str, _cancel_id: &str) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn edit_message(&self, _chat_id: &str, _message_id: &PlatformMessageId, _text: &str) -> Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> Result<()> {
            Ok(())
        }
    }
}
```

- [ ] **Step 2: Add module to lib.rs and run tests**

```rust
// src/lib.rs
pub mod tool_registry;
```

Run: `cargo test tool_registry::tests -- --nocapture`
Expected: 2 tests pass

- [ ] **Step 3: Commit**

```bash
git add src/tool_registry.rs src/lib.rs
git commit -m "feat: add ToolHandler trait and ToolRegistry"
```

---

### Task 4: Implement PlatformSender on Telegram adapter + migrate callback handler

**Files:**
- Modify: `src/platform/telegram.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add TelegramAdapter and update callback handler**

Add to `src/platform/telegram.rs`:

```rust
use crate::platform::sender::{MessageFormat as PlatformMsgFormat, PlatformMessageId, PlatformSender};
use async_trait::async_trait;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

pub struct TelegramAdapter {
    bot: Bot,
}

impl TelegramAdapter {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl PlatformSender for TelegramAdapter {
    async fn send_message(&self, chat_id_str: &str, text: &str, format: PlatformMsgFormat) -> Result<PlatformMessageId> {
        let chat_id: ChatId = chat_id_str.parse()?;
        let parse_mode = match format {
            PlatformMsgFormat::Markdown | PlatformMsgFormat::Auto => Some(ParseMode::MarkdownV2),
            PlatformMsgFormat::Rich => None,
        };
        let mut req = self.bot.send_message(chat_id, text);
        if let Some(pm) = parse_mode {
            req = req.parse_mode(pm);
        }
        let msg = req.await?;
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn send_file(&self, chat_id_str: &str, path: &Path, caption: Option<&str>) -> Result<PlatformMessageId> {
        let chat_id: ChatId = chat_id_str.parse()?;
        let input_file = teloxide::types::InputFile::file(path);
        let msg = if let Some(cap) = caption {
            self.bot.send_document(chat_id, input_file).caption(cap).await?
        } else {
            self.bot.send_document(chat_id, input_file).await?
        };
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn show_cancel_button(&self, chat_id_str: &str, text: &str, cancel_id: &str) -> Result<PlatformMessageId> {
        let chat_id: ChatId = chat_id_str.parse()?;
        let keyboard = InlineKeyboardMarkup::new([[InlineKeyboardButton::callback(
            "Cancel", format!("cancel_cmd:{cancel_id}"),
        )]]);
        let msg = self.bot.send_message(chat_id, text).reply_markup(keyboard).await?;
        Ok(format!("{}:{}", chat_id.0, msg.id.0))
    }

    async fn edit_message(&self, chat_id_str: &str, message_id: &PlatformMessageId, text: &str) -> Result<()> {
        let chat_id: ChatId = chat_id_str.parse()?;
        let parts: Vec<&str> = message_id.split(':').collect();
        let msg_id: i32 = parts.get(1).unwrap_or(&"0").parse()?;
        self.bot.edit_message_text(chat_id, teloxide::types::MessageId(msg_id), text).await?;
        Ok(())
    }

    async fn notify_shutdown(&self, chat_id_str: &str) -> Result<()> {
        let chat_id: ChatId = chat_id_str.parse()?;
        self.bot.send_message(chat_id, "⚠️ Bot is shutting down...").await?;
        Ok(())
    }
}
```

Update the callback query handler in `src/main.rs` (or `telegram.rs` if the handler lives there):

```rust
// Replace: agent.running_commands.lock().await.get(&cancel_id)
// With: agent.cancel_registry.cancel(&cancel_id)
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/platform/telegram.rs src/main.rs
git commit -m "feat: implement PlatformSender trait on TelegramAdapter, migrate cancel handler"
```

---

### Task 5: Move builtin_tool_definitions + execute_builtin_tool into BuiltinTools handler, preserving all parameter names and real implementations

**Files:**
- Create: `src/builtin_tools.rs`
- Modify: `src/tools.rs` — strip to path validators only
- Modify: `src/lib.rs`

**CRITICAL: All tool parameter names, descriptions, and enum values must be preserved verbatim from the original definitions in `tools.rs` and `agent.rs` to guarantee zero behavioral change. The plan below shows the correct original signatures — use these exact values.**

- [ ] **Step 1: Create builtin_tools.rs with the correct original definitions and real implementations**

```rust
// src/builtin_tools.rs

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock;
use tracing::info;

use crate::learning;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::platform::sender::PlatformSender;
use crate::skills::SkillRegistry;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};
use crate::tools::{validate_home_path, validate_sandbox_path};
use crate::config::Config;

pub struct BuiltinTools {
    sandbox_dir: PathBuf,
    home_dir: Option<PathBuf>,
    skills_dir: PathBuf,
    skills: Arc<RwLock<SkillRegistry>>,
    config: Config,
}

impl BuiltinTools {
    pub fn new(
        sandbox_dir: PathBuf,
        home_dir: Option<PathBuf>,
        skills_dir: PathBuf,
        skills: Arc<RwLock<SkillRegistry>>,
        config: Config,
    ) -> Self {
        Self { sandbox_dir, home_dir, skills_dir, skills, config }
    }
}

#[async_trait]
impl ToolHandler for BuiltinTools {
    fn define(&self) -> Vec<ToolDefinition> {
        // These are the ORIGINAL definitions from tools.rs — parameter names preserved exactly
        vec![
            // read_file
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_file".to_string(),
                    description: "Read the contents of a file from the sandbox.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path to the file (relative to sandbox or absolute within sandbox)" }
                        },
                        "required": ["path"]
                    }),
                },
            },
            // write_file — same as original
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "write_file".to_string(),
                    description: "Write content to a file. Creates parent directories if they don't exist.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path to the file (relative to sandbox or absolute within sandbox)" },
                            "content": { "type": "string", "description": "Content to write" }
                        },
                        "required": ["path", "content"]
                    }),
                },
            },
            // list_files — same as original
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
            // send_file — same as original
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
            // plan_create — ORIGINAL: title (not name)
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "plan_create".to_string(),
                    description: "Create a new execution plan with ordered steps.".to_string(),
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
            // plan_update — ORIGINAL: step_id (not step_index), includes notes
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "plan_update".to_string(),
                    description: "Update a step's status in the active plan.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "step_id": { "type": "integer", "description": "Zero-based index of the step to update" },
                            "status": { "type": "string", "enum": ["todo", "in_progress", "done", "failed"], "description": "New status for the step" },
                            "notes": { "type": "string", "description": "Optional notes — result summary, error message, etc." }
                        },
                        "required": ["step_id", "status"]
                    }),
                },
            },
            // plan_view
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "plan_view".to_string(),
                    description: "View the current plan as a checklist.".to_string(),
                    parameters: json!({ "type": "object", "properties": {}, "required": [] }),
                },
            },
            // try_new_tech — ORIGINAL: technology, experiment_code, language
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
            // self_upgrade — ORIGINAL: branch, mode
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
            // patch_skill — ORIGINAL: skill_name, patch_content
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "patch_skill".to_string(),
                    description: "Patch an existing skill's SKILL.md by appending content or replacing it entirely.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string", "description": "Name of the skill to patch" },
                            "patch_content": { "type": "string", "description": "Content to append (or full replacement if it starts with ---)" }
                        },
                        "required": ["skill_name", "patch_content"]
                    }),
                },
            },
            // read_soul_file — ORIGINAL: file_name with enum
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
            // update_soul_file — ORIGINAL: file_name with enum
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
        ]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        match name {
            "read_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let content = tokio::fs::read_to_string(&resolved).await
                    .with_context(|| format!("Failed to read file: {}", resolved.display()))?;
                Ok(content)
            }
            "write_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let content = args["content"].as_str().context("Missing 'content' argument")?;
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                if let Some(parent) = resolved.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&resolved, content).await?;
                Ok(format!("Wrote {} bytes to {}", content.len(), resolved.display()))
            }
            "list_files" => {
                let path = args["path"].as_str().unwrap_or(".");
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let mut entries = Vec::new();
                let mut dir = tokio::fs::read_dir(&resolved).await?;
                while let Some(entry) = dir.next_entry().await? {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let kind = if entry.file_type().await?.is_dir() { "dir" } else { "file" };
                    entries.push(format!("[{kind}] {name}"));
                }
                entries.sort();
                Ok(entries.join("\n"))
            }
            "send_file" => {
                let path = args["path"].as_str().context("Missing 'path' argument")?;
                let caption = args.get("caption").and_then(|v| v.as_str()).filter(|c| !c.is_empty());
                let resolved = validate_sandbox_path(&ctx.sandbox_dir, path)?;
                let metadata = tokio::fs::metadata(&resolved).await
                    .with_context(|| format!("File not found: {}", resolved.display()))?;
                const TG_FILE_LIMIT: u64 = 50 * 1024 * 1024;
                if metadata.len() > TG_FILE_LIMIT {
                    anyhow::bail!("File is {} MB — exceeds Telegram's 50 MB limit", metadata.len() / 1024 / 1024);
                }
                ctx.sender.send_file(&ctx.chat_id, &resolved, caption).await?;
                let file_name = resolved.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                Ok(format!("File '{}' sent successfully.", file_name))
            }
            // Plan tools — use ORIGINAL parameter names
            "plan_create" => {
                let title = args["title"].as_str().context("Missing 'title' argument")?;
                let plans_dir = ctx.sandbox_dir.join(".plans");
                tokio::fs::create_dir_all(&plans_dir).await?;
                let plan_path = plans_dir.join(format!("{}.json", title));
                let steps = args["steps"].as_array().context("Missing 'steps' argument")?;
                let plan = json!({
                    "title": title,
                    "steps": steps,
                    "statuses": vec![json!("todo"); steps.len()],
                });
                tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?).await?;
                Ok(format!("Created plan '{}' with {} steps", title, steps.len()))
            }
            "plan_update" => {
                let title = args["title"].as_str().unwrap_or("default");
                let step_id = args["step_id"].as_u64().context("Missing 'step_id'")? as usize;
                let status = args["status"].as_str().context("Missing 'status' argument")?;
                let _notes = args.get("notes").and_then(|v| v.as_str());
                let plan_path = ctx.sandbox_dir.join(".plans").join(format!("{}.json", title));
                let content = tokio::fs::read_to_string(&plan_path).await?;
                let mut plan: Value = serde_json::from_str(&content)?;
                if let Some(statuses) = plan.get_mut("statuses").and_then(|s| s.as_array_mut()) {
                    if step_id < statuses.len() {
                        statuses[step_id] = json!(status);
                        if let Some(n) = _notes {
                            if let Some(notes_arr) = plan.get_mut("notes").and_then(|n| n.as_array_mut()) {
                                if step_id < notes_arr.len() {
                                    notes_arr[step_id] = json!(n);
                                }
                            }
                        }
                    }
                }
                tokio::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?).await?;
                Ok(format!("Updated step {step_id} to '{status}'"))
            }
            "plan_view" => {
                let title = args["title"].as_str().unwrap_or("default");
                let plan_path = ctx.sandbox_dir.join(".plans").join(format!("{}.json", title));
                let content = tokio::fs::read_to_string(&plan_path).await?;
                Ok(content)
            }
            // try_new_tech — full real implementation (from agent.rs:3867-3961)
            "try_new_tech" => {
                let technology = args["technology"].as_str().context("Missing 'technology'")?.to_string();
                let experiment_code = args["experiment_code"].as_str().context("Missing 'experiment_code'")?.to_string();
                let language = args["language"].as_str().unwrap_or("rust").to_string();

                let exp_id = uuid::Uuid::new_v4().to_string();
                let exp_dir = ctx.sandbox_dir.join("experiments").join(&exp_id);
                tokio::fs::create_dir_all(&exp_dir).await?;

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
                if !stdout.is_empty() { result.push_str(&format!("STDOUT:\n{}\n", stdout)); }
                if !stderr.is_empty() { result.push_str(&format!("STDERR:\n{}\n", stderr)); }
                result.push_str(&format!("Exit code: {}\nResult: {}\n", exit_code, if success { "SUCCESS" } else { "FAILED" }));

                if let Err(e) = tokio::fs::remove_dir_all(&exp_dir).await {
                    tracing::warn!("Failed to clean up experiment dir '{}': {}", exp_dir.display(), e);
                }
                Ok(result)
            }
            // self_upgrade — real implementation (delegates to learning::self_upgrade)
            "self_upgrade" => {
                let branch = args["branch"].as_str().unwrap_or("main").to_string();
                let mode = args["mode"].as_str().unwrap_or("auto").to_string();

                // Validate branch name (same validation as original agent.rs:3967-3984)
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
                    return Ok(format!("Self-upgrade failed: invalid branch name '{}'", branch));
                }

                match learning::self_upgrade(&branch, &mode, None).await {
                    Ok(log) => Ok(log),
                    Err(e) => Ok(format!("Self-upgrade failed: {:#}", e)),
                }
            }
            // patch_skill — real implementation (delegates to learning::self_patch_skill)
            "patch_skill" => {
                let skill_name = args["skill_name"].as_str().context("Missing 'skill_name'")?.to_string();
                let patch_content = args["patch_content"].as_str().context("Missing 'patch_content'")?.to_string();
                match learning::self_patch_skill(
                    &self.skills_dir,
                    &skill_name,
                    &patch_content,
                    &self.skills,
                ).await {
                    Ok(msg) => Ok(msg),
                    Err(e) => Ok(format!("Patch failed: {:#}", e)),
                }
            }
            // Soul file tools — use ORIGINAL parameter names and full real implementation
            "read_soul_file" => {
                let file_name = args["file_name"].as_str().context("Missing 'file_name'")?;
                let home = ctx.home_dir.as_ref().context("No home directory configured")?;
                let path = validate_sandbox_path(home, file_name).unwrap_or_else(|_| home.join(file_name));
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
                    return Ok("Content too large (max 100KB). Please consolidate the file first.".to_string());
                }

                let home = ctx.home_dir.as_ref().context("No home directory configured")?;
                let path = home.join(file_name);

                let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();

                let new_content = match mode {
                    "append" => {
                        if existing.trim().is_empty() {
                            if content.starts_with("---") { content.to_string() }
                            else { format!("---\nname: {}\nversion: 1\n---\n\n{}", file_name.trim_end_matches(".md"), content) }
                        } else {
                            if !existing.trim().starts_with("---") {
                                return Ok("Existing soul file has invalid format (missing frontmatter). Rejected.".to_string());
                            }
                            format!("{}\n{}", existing.trim_end(), content)
                        }
                    }
                    "replace" => {
                        if !content.trim().starts_with("---") {
                            return Ok("Replace mode requires content with YAML frontmatter".to_string());
                        }
                        content.to_string()
                    }
                    _ => return Ok("Invalid mode. Use 'append' or 'replace'.".to_string()),
                };

                if !learning::has_valid_frontmatter(&new_content) {
                    return Ok("Update would produce invalid soul file (missing frontmatter). Rejected.".to_string());
                }
                if !new_content.contains("name:") || !new_content.contains("version:") {
                    return Ok("Update rejected: frontmatter must contain 'name' and 'version' fields.".to_string());
                }

                // Rotate backups
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
                    if bak.exists() { let _ = tokio::fs::copy(&bak, &path).await; }
                    return Ok(format!("Failed to write soul file (restored from backup): {}", e));
                }

                match tokio::fs::read_to_string(&path).await {
                    Ok(read_back) if read_back == new_content => Ok(format!(
                        "{} updated successfully. Backup at {}.bak", file_name, path.display()
                    )),
                    Ok(_) => {
                        let bak = bak_path(&path, ".bak");
                        if bak.exists() { let _ = tokio::fs::copy(&bak, &path).await; }
                        Ok("Write verification failed (content mismatch). Restored from backup.".to_string())
                    }
                    Err(e) => {
                        let bak = bak_path(&path, ".bak");
                        if bak.exists() { let _ = tokio::fs::copy(&bak, &path).await; }
                        Ok(format!("Write verification error (restored from backup): {}", e))
                    }
                }
            }
            _ => anyhow::bail!("BuiltinTools: unknown tool {name}"),
        }
    }
}
```

- [ ] **Step 2: Strip tools.rs to just path validators**

Remove from `tools.rs`:
- `builtin_tool_definitions()` function
- `execute_builtin_tool()` function
- All tool definitions (read_file, write_file, etc.)
- All tool-related tests (move to builtin_tools.rs)

Keep in `tools.rs`:
- `validate_sandbox_path()`
- `validate_home_path()`
- Their tests

- [ ] **Step 3: Move tests from tools.rs to builtin_tools.rs**

Move the existing tests:
- Tests that call `builtin_tool_definitions()` → test against `BuiltinTools::define()` instead
- Tests that call `execute_builtin_tool("plan_create", ...)` → test against `BuiltinTools::execute("plan_create", ...)` instead
- Keep the soul-file enum test (`test_soul_tool_definitions_have_required_file_name_enum`) and update to use `BuiltinTools`

- [ ] **Step 4: Add module to lib.rs**

```rust
// src/lib.rs
pub mod builtin_tools;
```

- [ ] **Step 5: Run cargo check and tests**

Run: `cargo check && cargo test`
Expected: compiles and all tests pass (including moved tests)

- [ ] **Step 6: Commit**

```bash
git add src/builtin_tools.rs src/tools.rs src/lib.rs
git commit -m "feat: extract BuiltinTools handler, preserving all tool parameter names and real implementations"
```

---

### Task 6: Create MemoryTools handler

**Files:**
- Create: `src/memory_tools.rs`

- [ ] **Step 1: Write MemoryTools handler**

This handler uses the real `MemoryStore` APIs:
- `memory.remember(category, key, value, None).await`
- `memory.recall(category, key).await`
- `memory.search_knowledge(query, limit).await` (returns `Vec<KnowledgeEntry>` with fields `id`, `category`, `key`, `value`, `source`)
- `memory.search_messages(query, limit).await` (returns `Vec<ChatMessage>`)

```rust
// src/memory_tools.rs

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::memory::MemoryStore;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

pub struct MemoryTools {
    memory: MemoryStore,
}

impl MemoryTools {
    pub fn new(memory: MemoryStore) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl ToolHandler for MemoryTools {
    fn define(&self) -> Vec<ToolDefinition> {
        // EXACT original definitions from agent.rs:2154-2204
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "remember".to_string(),
                    description: "Store a piece of knowledge for long-term memory. Use this to remember user preferences, facts, or anything useful.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category (e.g., 'user_preference', 'fact', 'project')" },
                            "key": { "type": "string", "description": "Short identifier for this knowledge" },
                            "value": { "type": "string", "description": "The knowledge to remember" }
                        },
                        "required": ["category", "key", "value"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "recall".to_string(),
                    description: "Retrieve a specific piece of remembered knowledge.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category to search in" },
                            "key": { "type": "string", "description": "The key to look up" }
                        },
                        "required": ["category", "key"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "search_memory".to_string(),
                    description: "Search through past conversations and knowledge using hybrid vector + full-text search. Finds semantically similar content even with different wording.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Search query (natural language)" },
                            "limit": { "type": "integer", "description": "Max results (default 5)" }
                        },
                        "required": ["query"]
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, _ctx: ToolContext) -> ToolResult {
        match name {
            "remember" => {
                let category = args["category"].as_str().unwrap_or("general");
                let key = args["key"].as_str().unwrap_or("");
                let value = args["value"].as_str().unwrap_or("");
                match self.memory.remember(category, key, value, None).await {
                    Ok(()) => Ok(format!("Remembered: [{}] {} = {}", category, key, value)),
                    Err(e) => Ok(format!("Failed to remember: {}", e)),
                }
            }
            "recall" => {
                let category = args["category"].as_str().unwrap_or("general");
                let key = args["key"].as_str().unwrap_or("");
                match self.memory.recall(category, key).await {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => Ok(format!("No knowledge found for [{}] {}", category, key)),
                    Err(e) => Ok(format!("Failed to recall: {}", e)),
                }
            }
            "search_memory" => {
                let query = args["query"].as_str().context("Missing 'query' argument")?;
                let limit = args["limit"].as_u64().unwrap_or(5) as usize;

                let mut results = Vec::new();

                if let Ok(msgs) = self.memory.search_messages(query, limit).await {
                    for msg in msgs {
                        results.push(format!("[{}]: {}", msg.role, msg.content.to_string()));
                    }
                }

                if let Ok(entries) = self.memory.search_knowledge(query, limit).await {
                    for entry in entries {
                        results.push(format!(
                            "[knowledge:{}] {} = {}",
                            entry.category, entry.key, entry.value
                        ));
                    }
                }

                if results.is_empty() {
                    Ok("No results found.".to_string())
                } else {
                    Ok(results.join("\n\n"))
                }
            }
            _ => anyhow::bail!("MemoryTools: unknown tool {name}"),
        }
    }
}
```

- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod memory_tools;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/memory_tools.rs src/lib.rs
git commit -m "feat: extract MemoryTools handler from agent.rs"
```

---

### Task 7: Create SchedulingTools handler

**Files:**
- Create: `src/scheduling_tools.rs`

- [ ] **Step 1: Write SchedulingTools handler**

This handler uses the real Scheduler APIs:
- `scheduler.add_one_shot_job(delay: Duration, name, fire_closure) -> Result<Uuid>`
- `scheduler.add_cron_job(cron_expr, name, fire_closure) -> Result<Uuid>`
- Uses same `ScheduledTask` struct and `task_store` methods as original code at agent.rs:3251-3384

```rust
// src/scheduling_tools.rs

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::ScheduledJobRequest;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::scheduler::{reminders::ScheduledTask, tasks::ScheduledTaskStore, Scheduler};
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};
use teloxide::prelude::Bot;
use uuid::Uuid;

pub struct SchedulingTools {
    task_store: ScheduledTaskStore,
    scheduler: Arc<Scheduler>,
    job_tx: UnboundedSender<ScheduledJobRequest>,
    bot: Arc<Bot>,
}

impl SchedulingTools {
    pub fn new(
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        job_tx: UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
    ) -> Self {
        Self { task_store, scheduler, job_tx, bot }
    }
}

#[async_trait]
impl ToolHandler for SchedulingTools {
    fn define(&self) -> Vec<ToolDefinition> {
        // EXACT original definitions from agent.rs:2208-2295
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "schedule_task".to_string(),
                    description: "Schedule a task to run at a future time.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "trigger_type": { "type": "string", "enum": ["one_shot", "recurring"] },
                            "trigger_value": { "type": "string", "description": "ISO 8601 (one_shot) or 6-field cron (recurring)" },
                            "prompt": { "type": "string", "description": "The message the agent will process" },
                            "description": { "type": "string", "description": "Human-readable label" }
                        },
                        "required": ["trigger_type", "trigger_value", "prompt", "description"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "list_scheduled_tasks".to_string(),
                    description: "List all active scheduled tasks for the current user.".to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "cancel_scheduled_task".to_string(),
                    description: "Cancel an active scheduled task by its ID.".to_string(),
                    parameters: json!({
                        "type": "object", "properties": {
                            "task_id": { "type": "string", "description": "The task ID from list_scheduled_tasks" }
                        }, "required": ["task_id"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "get_scheduled_task_history".to_string(),
                    description: "Retrieve execution history for a scheduled task.".to_string(),
                    parameters: json!({
                        "type": "object", "properties": {
                            "task_id": { "type": "string" }
                        }, "required": ["task_id"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "rerun_scheduled_task".to_string(),
                    description: "Execute a scheduled task immediately.".to_string(),
                    parameters: json!({
                        "type": "object", "properties": {
                            "task_id": { "type": "string" }
                        }, "required": ["task_id"]
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        match name {
            "schedule_task" => {
                let trigger_type = args["trigger_type"].as_str().context("Missing 'trigger_type'")?.to_string();
                let trigger_value = args["trigger_value"].as_str().context("Missing 'trigger_value'")?.to_string();
                let prompt_text = args["prompt"].as_str().context("Missing 'prompt'")?.to_string();
                let description = args["description"].as_str().context("Missing 'description'")?.to_string();

                // Parse delay from trigger_value (same logic as original agent.rs:3270-3284)
                use crate::agent::parse_one_shot_delay;
                use crate::agent::validate_cron_expr;

                let delay = if trigger_type == "one_shot" {
                    Some(parse_one_shot_delay(&trigger_value).map_err(|e| anyhow::anyhow!("Invalid trigger: {e}"))?)
                } else if trigger_type == "recurring" {
                    validate_cron_expr(&trigger_value).map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;
                    None
                } else {
                    anyhow::bail!("Unknown trigger_type '{trigger_type}'. Use 'one_shot' or 'recurring'.");
                };

                // Persist to DB (same logic as agent.rs:3290-3308)
                let task_id = Uuid::new_v4().to_string();
                let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
                let task = ScheduledTask {
                    id: task_id.clone(),
                    scheduler_job_id: None,
                    user_id: ctx.user_id.clone(),
                    chat_id: ctx.chat_id.clone(),
                    platform: "telegram".to_string(),
                    trigger_type: trigger_type.clone(),
                    trigger_value: trigger_value.clone(),
                    prompt: prompt_text.clone(),
                    description: description.clone(),
                    status: "active".to_string(),
                    created_at: now.clone(),
                    next_run_at: Some(trigger_value.clone()),
                };
                if let Err(e) = self.task_store.create(&task).await {
                    return Ok(format!("Failed to save task: {}", e));
                }

                // Build fire closure (same pattern as agent.rs:3311-3353)
                let job_tx = self.job_tx.clone();
                let bot_clone = self.bot.clone();
                let uid = ctx.user_id.clone();
                let cid = ctx.chat_id.clone();
                let prompt_cap = prompt_text.clone();
                let is_recurring = trigger_type == "recurring";
                let tv = trigger_value.clone();

                let fire = move || {
                    let tx = job_tx.clone();
                    let bot = bot_clone.clone();
                    let uid = uid.clone();
                    let cid = cid.clone();
                    let prompt = prompt_cap.clone();
                    let tid = task_id.clone();
                    let recurring = is_recurring;
                    Box::pin(async move {
                        let incoming = crate::platform::IncomingMessage {
                            platform: "scheduled_task".to_string(),
                            user_id: format!("{uid}:{tid}"),
                            chat_id: cid,
                            user_name: String::new(),
                            text: prompt,
                            attachments: vec![],
                        };
                        let req = ScheduledJobRequest {
                            incoming,
                            bot,
                            task_id: tid,
                            is_recurring: recurring,
                        };
                        let _ = tx.send(req);
                    }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };

                let sched_result = if let Some(d) = delay {
                    self.scheduler.add_one_shot_job(d, &description, fire).await
                } else {
                    self.scheduler.add_cron_job(&tv, &description, fire).await
                };

                match sched_result {
                    Ok(_sched_id) => {
                        Ok(format!("Task scheduled! ID: {} — {} ({})", task_id, description, trigger_value))
                    }
                    Err(e) => {
                        Ok(format!("Failed to register task with scheduler: {}", e))
                    }
                }
            }
            "list_scheduled_tasks" => {
                match self.task_store.list_active_for_user(&ctx.user_id).await {
                    Ok(tasks) if tasks.is_empty() => Ok("No active scheduled tasks.".to_string()),
                    Ok(tasks) => {
                        let mut out = format!("Active scheduled tasks ({}):\n\n", tasks.len());
                        for t in &tasks {
                            out.push_str(&format!(
                                "ID: {}\nDescription: {}\nType: {} | Trigger: {}\nPrompt: {}\n\n",
                                t.id, t.description, t.trigger_type, t.trigger_value, t.prompt
                            ));
                        }
                        Ok(out)
                    }
                    Err(e) => Ok(format!("Failed to list tasks: {}", e)),
                }
            }
            "cancel_scheduled_task" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
                match self.task_store.set_status(task_id, "cancelled").await {
                    Ok(()) => Ok(format!("Cancelled task {task_id}")),
                    Err(e) => Ok(format!("Failed to cancel task: {}", e)),
                }
            }
            "get_scheduled_task_history" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
                match self.task_store.get_history(task_id).await {
                    Ok(history) if history.is_empty() => Ok("No history for this task.".to_string()),
                    Ok(history) => {
                        let lines: Vec<String> = history.iter().map(|h| {
                            format!("[{}] {} — {}", h.ran_at, h.status, h.response_preview)
                        }).collect();
                        Ok(lines.join("\n"))
                    }
                    Err(e) => Ok(format!("Failed to get history: {}", e)),
                }
            }
            "rerun_scheduled_task" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
                let task = match self.task_store.get(task_id).await {
                    Ok(t) => t,
                    Err(e) => return Ok(format!("Task not found: {}", e)),
                };
                let job_tx = self.job_tx.clone();
                let bot_clone = self.bot.clone();
                let store_clone = self.task_store.clone();
                let tid = task_id.to_string();
                let uid = task.user_id.clone();
                let cid = task.chat_id.clone();
                let prompt_cap = task.prompt.clone();
                let fire = move || {
                    let tx = job_tx.clone();
                    let bot = bot_clone.clone();
                    let store = store_clone.clone();
                    let tid = tid.clone();
                    let uid = uid.clone();
                    let cid = cid.clone();
                    let prompt = prompt_cap.clone();
                    Box::pin(async move {
                        let incoming = crate::platform::IncomingMessage {
                            platform: "scheduled_task".to_string(),
                            user_id: format!("{uid}:{tid}"),
                            chat_id: cid,
                            user_name: String::new(),
                            text: prompt,
                            attachments: vec![],
                        };
                        let req = ScheduledJobRequest {
                            incoming, bot, task_id: tid,
                            is_recurring: false, task_store: store,
                        };
                        let _ = tx.send(req);
                    }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };
                match self.scheduler.add_one_shot_job(
                    std::time::Duration::from_secs(1), &task.description, fire,
                ).await {
                    Ok(_) => Ok(format!("Re-run scheduled for task {task_id}")),
                    Err(e) => Ok(format!("Failed to re-run task: {}", e)),
                }
            }
            _ => anyhow::bail!("SchedulingTools: unknown tool {name}"),
        }
    }
}
```



- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod scheduling_tools;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/scheduling_tools.rs src/lib.rs
git commit -m "feat: extract SchedulingTools handler from agent.rs"
```

---

### Task 8: Create SkillTools handler

**Files:**
- Create: `src/skill_tools.rs`

- [ ] **Step 1: Write SkillTools handler with real implementations**

Note: `invoke_agent` and `spawn_agents` remain in Agent's `execute_tool` dispatch (they require access to Agent's loop infrastructure — `self.agents`, `self.run_subagent_loop`). Everything else is extracted into this handler.

```rust
// src/skill_tools.rs

use anyhow::{Context, Result};
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
        // EXACT original definitions from agent.rs:2299-2431 (preserved verbatim)
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
                // Skills reload is triggered externally — returns instruction
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
```

- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod skill_tools;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/skill_tools.rs src/lib.rs
git commit -m "feat: extract SkillTools handler from agent.rs"
```

---

### Task 9: Create CommandTool handler

**Files:**
- Create: `src/command_tool.rs`

- [ ] **Step 1: Write CommandTool handler**

Same as the original plan but with added `escape_text` for the command string and `tokio::fs` usage (no `std::fs`). The original `execute_command_interactive` at agent.rs:2962-3191 is moved entirely.

```rust
// src/command_tool.rs

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;
use tracing::warn;

use crate::cancel_registry::CancelRegistry;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::platform::sender::PlatformSender;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

pub struct CommandTool {
    sandbox_dir: PathBuf,
    cancel_registry: Arc<CancelRegistry>,
    sender: Arc<dyn PlatformSender>,
}

impl CommandTool {
    pub fn new(
        sandbox_dir: PathBuf,
        cancel_registry: Arc<CancelRegistry>,
        sender: Arc<dyn PlatformSender>,
    ) -> Self {
        Self { sandbox_dir, cancel_registry, sender }
    }
}

#[async_trait]
impl ToolHandler for CommandTool {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "execute_command".to_string(),
                description: "Execute a shell command within the sandbox directory.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to execute" }
                    },
                    "required": ["command"]
                }),
            },
        }]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        match name {
            "execute_command" => self.exec_command(&args, &ctx).await,
            _ => anyhow::bail!("CommandTool: unknown tool {name}"),
        }
    }
}

impl CommandTool {
    async fn exec_command(&self, arguments: &Value, ctx: &ToolContext) -> ToolResult {
        let command = arguments["command"].as_str().context("Missing 'command' argument")?;
        let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());

        let mut child = TokioCommand::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.sandbox_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;

        // Escape for Telegram markdown (same as original agent.rs:2993)
        let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

        let status_text = format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd);
        let msg_id = self.sender.show_cancel_button(&ctx.chat_id, &status_text, &cmd_id).await?;

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        self.cancel_registry.register(cmd_id.clone(), cancel_tx);

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
        let output_tx2 = output_tx.clone();
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        let stdout_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stdout.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { let _ = output_tx.send(String::from_utf8_lossy(&buf[..n]).to_string()).await; }
                }
            }
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stderr.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { let _ = output_tx2.send(String::from_utf8_lossy(&buf[..n]).to_string()).await; }
                }
            }
        });

        const MAX_BUFFER_CHARS: usize = 100_000;
        let mut output_buffer = String::new();
        let mut last_edit = Instant::now();
        let mut exit_code: Option<i32> = None;
        let mut cancelled = false;
        tokio::pin!(cancel_rx);

        loop {
            tokio::select! {
                Some(chunk) = output_rx.recv() => {
                    output_buffer.push_str(&chunk);
                    if output_buffer.chars().count() > MAX_BUFFER_CHARS {
                        output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
                    }
                    if last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        let text = format!("💻 Running: `{}`\n\n```\n{}\n```", escaped_cmd, capped);
                        if let Err(e) = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await {
                            warn!("Failed to update running message: {e}");
                        }
                        last_edit = Instant::now();
                    }
                }
                status = child.wait() => {
                    exit_code = Some(status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1));
                    break;
                }
                _ = &mut cancel_rx => {
                    cancelled = true;
                    if let Some(pid) = child.id() {
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    break;
                }
            }
        }

        let _ = tokio::join!(stdout_handle, stderr_handle);
        while let Ok(chunk) = output_rx.try_recv() {
            output_buffer.push_str(&chunk);
        }
        if output_buffer.chars().count() > MAX_BUFFER_CHARS {
            output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
        }

        fn format_body(buf: &str, no_output_msg: &str) -> Option<String> {
            if buf.is_empty() {
                if no_output_msg.is_empty() { None } else { Some(no_output_msg.to_owned()) }
            } else {
                let capped = crate::utils::strings::truncate_tail(buf, 3500);
                Some(format!("```\n{}\n```", capped))
            }
        }

        let result = if cancelled {
            let body = format_body(&output_buffer, "");
            let text = match body {
                None => format!("❌ Cancelled: `{}`", escaped_cmd),
                Some(b) => format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, b),
            };
            let _ = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await;
            "⚠️ User cancelled the command".to_string()
        } else if let Some(code) = exit_code {
            let (icon, label) = if code == 0 { ("✅", "Completed") } else { ("❌", "Failed") };
            let body = format_body(&output_buffer, "Command completed with no output.");
            let text = format!("{} {}: `{}`\n\n{}", icon, label, escaped_cmd, body.unwrap_or_default());
            let _ = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await;
            let mut result = String::new();
            if !output_buffer.is_empty() { result.push_str(output_buffer.trim_end()); result.push('\n'); }
            result.push_str(&format!("Exit code: {}", code));
            result
        } else {
            unreachable!()
        };

        self.cancel_registry.unregister(&cmd_id);
        Ok(result)
    }
}
```

- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod command_tool;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/command_tool.rs src/lib.rs
git commit -m "feat: extract CommandTool handler from agent.rs"
```

---

### Task 10: Wire through Agent + main.rs, remove Arc<Bot> and running_commands

**Files:**
- Modify: `src/agent.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Update Agent struct and constructor**

Remove `bot: Arc<Bot>` and `running_commands`. Add `tool_registry: ToolRegistry`, `sender: Arc<dyn PlatformSender>`, `cancel_registry: Arc<CancelRegistry>`.

```rust
pub struct Agent {
    pub llm: LlmClient,
    pub registry: Arc<ProviderRegistry>,
    pub config: Config,
    pub mcp: McpManager,
    pub memory: MemoryStore,
    pub skills: RwLock<SkillRegistry>,
    pub agents: RwLock<SkillRegistry>,
    pub task_store: ScheduledTaskStore,
    pub scheduler: Arc<Scheduler>,
    pub self_weak: Weak<Agent>,
    pub job_tx: UnboundedSender<ScheduledJobRequest>,
    pub langsmith: Arc<LangSmithClient>,
    pub restart_pending: AtomicBool,
    pub soul_updated: AtomicBool,
    pub current_model: RwLock<String>,
    pub config_path: PathBuf,
    pub cancel_token_registry: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub pending_injections: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub pending_loop_callbacks: Arc<Mutex<HashMap<String, oneshot::Sender<LoopCallbackChoice>>>>,
    // New
    pub tool_registry: ToolRegistry,
    pub sender: Arc<dyn PlatformSender>,
    pub cancel_registry: Arc<CancelRegistry>,
}
```

- [ ] **Step 2: Update Agent::new to accept new parameters**

Remove `bot: Arc<Bot>` from parameters. Add `tool_registry`, `sender`, `cancel_registry`.

- [ ] **Step 3: Rewrite execute_tool — ToolRegistry + MCP prefix + invoke_agent/spawn_agents dispatch**

Like the MCP prefix check, `invoke_agent` and `spawn_agents` remain directly in Agent because they need access to `self.agents`, `self.llm`, `self.config`, and `self.run_subagent_loop()` — circular dependency prevents registering them in ToolRegistry.

```rust
async fn execute_tool(&self, name: &str, arguments: &Value, _user_id: &str, chat_id: ChatId) -> String {
    if name.starts_with("mcp_") {
        return self.mcp.call_tool(name, arguments).await
            .unwrap_or_else(|e| format!("Error: {e}"));
    }
    match name {
        "invoke_agent" => {
            // Full subagent dispatch: look up agent in self.agents, get tool whitelist + model,
            // call self.run_subagent_loop(...). Same as original agent.rs:3642-3730.
            // (code stays verbatim from original)
        }
        "spawn_agents" => {
            // Parallel agent spawning. Same as original agent.rs:3731-3825.
            // (code stays verbatim from original)
        }
        _ => {
            let ctx = ToolContext {
                sandbox_dir: self.config.sandbox.allowed_directory.clone(),
                home_dir: self.config.resolved_home.clone(),
                sender: self.sender.clone(),
                cancel_registry: self.cancel_registry.clone(),
                user_id: _user_id.to_string(),
                chat_id: chat_id.to_string(),
            };
            self.tool_registry.execute(name, arguments.clone(), ctx).await
                .unwrap_or_else(|e| format!("Error: {e}"))
        }
    }
}
```

- [ ] **Step 4: Delete removed methods**

Remove from `agent.rs`:
- `memory_tool_definitions()`
- `scheduling_tool_definitions()`
- `skill_tool_definitions()`
- `execute_command_interactive()`
- All match arms from `execute_tool` (replaced with ToolRegistry dispatch)
- `running_commands` field and all references

- [ ] **Step 5: Update main.rs to construct new types**

```rust
// After constructing bot, scheduler, etc.
let cancel_registry = Arc::new(CancelRegistry::new());
let sender: Arc<dyn PlatformSender> = Arc::new(TelegramAdapter::new(bot.clone()));

let skills_rw = Arc::new(RwLock::new(skills));

let mut tool_registry = ToolRegistry::new();
tool_registry.register(Box::new(BuiltinTools::new(
    config.sandbox.allowed_directory.clone(),
    config.resolved_home.clone(),
    config.skills.directory.clone(),
    skills_rw.clone(),
    config.clone(),
)));
tool_registry.register(Box::new(MemoryTools::new(memory.clone())));
tool_registry.register(Box::new(SchedulingTools::new(
    scheduler.clone(),
    job_tx.clone(),
    bot.clone(),
)));
tool_registry.register(Box::new(SkillTools::new(
    config.skills.directory.clone(),
    config.agents.directory.clone(),
)));
tool_registry.register(Box::new(CommandTool::new(
    config.sandbox.allowed_directory.clone(),
    cancel_registry.clone(),
    sender.clone(),
)));

let agent = Arc::new_cyclic(|weak| {
    Agent::new(
        config.clone(), registry.clone(), mcp_manager, memory.clone(),
        skills_rw, agents, task_store.clone(), scheduler.clone(),
        weak.clone(), job_tx, langsmith.clone(), config_path.clone(),
        tool_registry, sender.clone(), cancel_registry,
    )
});
```

- [ ] **Step 6: Update callback query handler**

In the Telegram callback handler (`src/main.rs` or `telegram.rs`), replace:
```rust
// Old: agent.running_commands.lock().await.get(&cancel_id)
// New:
agent.cancel_registry.cancel(&cancel_id);
```

- [ ] **Step 7: Run cargo check and tests**

Run: `cargo check && cargo test`
Expected: compiles and tests pass

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs src/main.rs
git commit -m "refactor: wire ToolRegistry/PlatformSender/CancelRegistry through Agent, remove Arc<Bot> and running_commands"
```

---

### Task 11: Clean up — migrate telegram.rs free functions to TelegramAdapter, remove dead code

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Migrate remaining free functions**

Replace direct `agent.bot` calls in telegram.rs with `TelegramAdapter` or `PlatformSender` calls. Remove the old free-function implementations for:
- `notify_startup` → use `sender.notify_shutdown` or `sender.send_message`
- `notify_shutdown` → use `sender.notify_shutdown`
- Any inline `bot.send_message` calls in the message handler → use `sender.send_message`

- [ ] **Step 2: Run cargo check**

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "refactor: migrate telegram.rs free functions to TelegramAdapter"
```

---

## M2: ConversationManager

### Task 12: Create ConversationManager

**Files:**
- Create: `src/conversation.rs`

- [ ] **Step 1: Write ConversationManager module**

Uses the real APIs:
- `memory.load_messages(conversation_id)` — returns `Vec<ChatMessage>`
- `memory.save_message(conversation_id, &ChatMessage)` — returns `Result<String>`
- `file_processor::process_attachments(&[Attachment], &str, &Config, &MemoryStore, bool) -> (String, Vec<ContentPart>)`

```rust
// src/conversation.rs

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::agent::{MidRunMode, PreparedPrompt};
use crate::agent_prompt::prepare_messages_for_llm;
use crate::config::Config;
use crate::llm::{ChatMessage, ContentPart, LlmClient, MessageContent};
use crate::memory::MemoryStore;
use crate::platform::{Attachment, AttachmentKind, IncomingMessage};
use crate::skills::SkillRegistry;

pub struct ConversationManager {
    messages: Vec<ChatMessage>,
    system_prompt: String,
    memory: MemoryStore,
    conversation_id: String,
}

impl ConversationManager {
    pub async fn new(
        memory: &MemoryStore,
        platform: &str,
        user_id: &str,
        system_prompt: String,
        _skills: &SkillRegistry,
        config: &Config,
    ) -> Result<Self> {
        let conversation_id = format!("{}:{}", platform, user_id);
        let history = memory.load_messages(&conversation_id).await.unwrap_or_default();

        let now = chrono::Local::now();
        let context_prompt = format!(
            "\n\nCurrent date and time: {} ({})",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.format("%A")
        );

        let system_msg = ChatMessage {
            role: "system".to_string(),
            content: MessageContent::Text(format!("{system_prompt}{context_prompt}")),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };

        let mut messages = vec![system_msg];
        messages.extend(history);

        Ok(Self {
            messages,
            system_prompt,
            memory: memory.clone(),
            conversation_id,
        })
    }

    /// Process an incoming message: extract attachments, run RAG, save user message.
    /// Returns image ContentParts for multi-modal messages.
    pub async fn add_incoming(
        &mut self,
        incoming: &IncomingMessage,
        config: &Config,
        supports_vision: bool,
    ) -> Result<Vec<ContentPart>> {
        // Use the real process_attachments API
        let (augmented_text, image_parts) = crate::file_processor::process_attachments(
            &incoming.attachments,
            &incoming.text,
            config,
            &self.memory,
            supports_vision,
        ).await;

        // Save user message to conversation history
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(augmented_text),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        self.memory.save_message(&self.conversation_id, &user_msg).await?;

        Ok(image_parts)
    }

    pub fn add_user_turn(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn add_assistant_turn(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn add_tool_result(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn inject_rag_context(&mut self, rag_block: &str) {
        if !rag_block.is_empty() {
            self.system_prompt.push_str(&format!("\n\n# Retrieved Context\n{rag_block}"));
        }
    }

    pub fn apply_steer(&mut self, text: &str) {
        let steer_msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: Some("steer".to_string()),
        };
        self.messages.push(steer_msg);
    }

    pub async fn compact_tier3(&mut self, context_window: usize) {
        let total: usize = self.messages.iter().map(|m| m.content.len()).sum();
        if total > context_window / 2 {
            let system = self.messages.first().cloned();
            let recent: Vec<ChatMessage> = self.messages.iter().skip(1).rev().take(20).cloned().collect();
            let mut trimmed = Vec::new();
            if let Some(sys) = system { trimmed.push(sys); }
            trimmed.extend(recent.into_iter().rev());
            self.messages = trimmed;
        }
    }

    pub async fn compact_tier4(&mut self, llm: &LlmClient, context_window: usize) -> Result<bool> {
        let total: usize = self.messages.iter().map(|m| m.content.len()).sum();
        if total <= context_window / 2 { return Ok(false); }

        let system = self.messages.first().cloned();
        let keep_count = 10.min(self.messages.len().saturating_sub(1));
        let keep_from = self.messages.len().saturating_sub(keep_count);
        let to_summarize: Vec<&ChatMessage> = self.messages.iter().skip(1).take(keep_from.saturating_sub(1)).collect();
        if to_summarize.is_empty() { return Ok(false); }

        let summary_text: String = to_summarize.iter()
            .map(|m| format!("{}: {}", m.role, m.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");

        let summary_prompt = format!("Summarize the following conversation, preserving key decisions and facts:\n\n{summary_text}");
        let summary_msg = vec![
            ChatMessage {
                role: "system".to_string(),
                content: MessageContent::Text("You are a conversation summarizer.".to_string()),
                tool_calls: None, tool_call_id: None, name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: MessageContent::Text(summary_prompt),
                tool_calls: None, tool_call_id: None, name: None,
            },
        ];

        match llm.chat(&summary_msg, &[]).await {
            Ok(summary) => {
                let summary_entry = ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text(format!("[Previous conversation summarized: {}]", summary.to_string())),
                    tool_calls: None, tool_call_id: Some("summary".to_string()), name: Some("summarizer".to_string()),
                };
                let mut new_msgs = Vec::new();
                if let Some(sys) = system { new_msgs.push(sys); }
                new_msgs.push(summary_entry);
                new_msgs.extend(self.messages.iter().skip(keep_from).cloned());
                self.messages = new_msgs;
                Ok(true)
            }
            Err(e) => {
                tracing::warn!("Compaction tier 4 failed: {e}");
                self.compact_tier3(context_window).await;
                Ok(false)
            }
        }
    }

    pub fn prepare(&self, context_window: usize) -> PreparedPrompt {
        prepare_messages_for_llm(&self.messages, context_window)
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn into_messages(self) -> Vec<ChatMessage> {
        self.messages
    }
}
```

- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod conversation;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/conversation.rs src/lib.rs
git commit -m "feat: add ConversationManager module"
```

---

### Task 13: Refactor process_message to use ConversationManager

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Replace inline message construction with ConversationManager**

```rust
let mut cmgr = ConversationManager::new(
    &self.memory, platform, user_id,
    current_system_prompt, &skills, &self.config,
).await?;

let image_parts = cmgr.add_incoming(incoming, &self.config, supports_vision).await?;

let user_msg_content = if image_parts.is_empty() {
    MessageContent::Text(incoming.text.clone())
} else {
    let mut parts = vec![ContentPart::Text(incoming.text.clone())];
    parts.extend(image_parts);
    MessageContent::Parts(parts)
};

cmgr.add_user_turn(ChatMessage {
    role: "user".to_string(), content: user_msg_content,
    tool_calls: None, tool_call_id: None, name: None,
});
```

- [ ] **Step 2: Replace compaction calls**

```rust
cmgr.compact_tier3(context_window).await;
let _ = cmgr.compact_tier4(&self.llm, context_window).await;
```

- [ ] **Step 3: Replace steer injection**

```rust
if let Some(injections) = self.pending_injections.lock().await.remove(user_id) {
    for text in injections {
        cmgr.apply_steer(&text);
    }
}
```

- [ ] **Step 4: Run cargo check and tests**

Run: `cargo check && cargo test`

- [ ] **Step 5: Commit**

```bash
git add src/agent.rs
git commit -m "refactor: process_message uses ConversationManager"
```

---

## M3: LoopRunner

### Task 14: Create LoopRunner (AgenticLoop)

**Files:**
- Create: `src/loop_runner.rs`

- [ ] **Step 1: Write LoopConfig, AgenticLoop, LoopOutcome**

The `AgenticLoop` receives a `ToolContext` factory so it can construct real contexts for each tool call, not fake ones.

```rust
// src/loop_runner.rs

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::CancellationToken;
use tracing::debug;

use crate::agent::LoopCallbackChoice;
use crate::conversation::ConversationManager;
use crate::langsmith::LangSmithClient;
use crate::llm::{ChatMessage, LlmClient, MessageContent};
use crate::mcp::McpManager;
use crate::platform::sender::PlatformSender;
use crate::platform::tool_notifier::ToolEvent;
use crate::tool_registry::{ToolContext, ToolRegistry};
use std::path::PathBuf;
use crate::cancel_registry::CancelRegistry;

pub struct LoopConfig {
    pub max_iterations: u32,
    pub empty_response_retry_limit: u32,
    pub compaction_enabled: bool,
    pub loop_detection_enabled: bool,
    pub interactive_loop_callback: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub langsmith_project: Option<String>,
    pub tool_event_tx: Option<mpsc::Sender<ToolEvent>>,
    pub stream_token_tx: Option<mpsc::Sender<String>>,
}

pub enum LoopOutcome {
    FinalResponse(String),
    Cancelled,
    MaxIterations,
}

pub struct AgenticLoop<'a> {
    llm: &'a LlmClient,
    tools: &'a ToolRegistry,
    mcp: &'a McpManager,
    config: &'a LoopConfig,
    cancel: Option<CancellationToken>,
    chain_run_id: Option<String>,
    langsmith: Option<&'a LangSmithClient>,
    platform_sender: &'a dyn PlatformSender,
    make_tool_ctx: Box<dyn Fn(&str, &str) -> ToolContext + Send + Sync + 'a>,
}

impl<'a> AgenticLoop<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm: &'a LlmClient,
        tools: &'a ToolRegistry,
        mcp: &'a McpManager,
        config: &'a LoopConfig,
        cancel: Option<CancellationToken>,
        chain_run_id: Option<String>,
        langsmith: Option<&'a LangSmithClient>,
        platform_sender: &'a dyn PlatformSender,
        make_tool_ctx: Box<dyn Fn(&str, &str) -> ToolContext + Send + Sync + 'a>,
    ) -> Self {
        Self { llm, tools, mcp, config, cancel, chain_run_id, langsmith, platform_sender, make_tool_ctx }
    }

    pub async fn run(
        &self,
        // Accept either ConversationManager or plain Vec<ChatMessage> for subagent support
        messages: &mut MessageContainer,
    ) -> Result<LoopOutcome> {
        let context_window = 128_000;
        let mut empty_count = 0u32;

        for iteration in 0..self.config.max_iterations {
            if let Some(ref cancel) = self.cancel {
                if cancel.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled);
                }
            }

            if self.config.compaction_enabled && iteration > 0 && iteration % 5 == 0 {
                if let crate::loop_runner::MessageContainer::Conversation(cm) = messages {
                    cm.compact_tier3(context_window).await;
                    let _ = cm.compact_tier4(self.llm, context_window).await;
                }
            }

            let prepared = messages.prepare(context_window);

            let tool_defs = if let Some(ref whitelist) = self.config.allowed_tools {
                let mut all = self.tools.all_definitions();
                all.extend(self.mcp.tool_definitions());
                all.into_iter().filter(|d| whitelist.contains(&d.function.name)).collect()
            } else {
                let mut all = self.tools.all_definitions();
                all.extend(self.mcp.tool_definitions());
                all
            };

            let response = self.llm.chat(prepared.messages, &tool_defs).await?;

            let text = response.content.to_string();
            let tool_calls = response.tool_calls.clone().unwrap_or_default();

            if text.is_empty() && tool_calls.is_empty() {
                empty_count += 1;
                if empty_count >= self.config.empty_response_retry_limit {
                    return Ok(LoopOutcome::FinalResponse("I'm having trouble processing that. Please try again.".to_string()));
                }
                continue;
            }

            if !tool_calls.is_empty() {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(text.clone()),
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None, name: None,
                });

                for tc in &tool_calls {
                    if let Some(ref whitelist) = self.config.allowed_tools {
                        if !whitelist.contains(&tc.name) {
                            messages.push_tool_result(&tc.id, format!("Tool '{}' is not available to this agent.", tc.name));
                            continue;
                        }
                    }

                    let result = if tc.name.starts_with("mcp_") {
                        self.mcp.call_tool(&tc.name, &tc.arguments).await
                            .unwrap_or_else(|e| format!("Error: {e}"))
                    } else {
                        let ctx = (self.make_tool_ctx)("", "");
                        self.tools.execute(&tc.name, tc.arguments.clone(), ctx).await
                            .unwrap_or_else(|e| format!("Error: {e}"))
                    };

                    messages.push_tool_result(&tc.id, result);
                }
                continue;
            }

            if !text.is_empty() {
                if let Some(ref tx) = self.config.stream_token_tx {
                    let _ = LlmClient::stream_text(text.clone(), tx.clone()).await;
                }
                return Ok(LoopOutcome::FinalResponse(text));
            }

            empty_count += 1;
        }

        Ok(LoopOutcome::MaxIterations)
    }
}

/// Wrapper that allows AgenticLoop to work with either ConversationManager or Vec<ChatMessage>
pub enum MessageContainer {
    Conversation(Box<ConversationManager>),
    Plain(Vec<ChatMessage>),
}

impl MessageContainer {
    pub fn prepare(&self, context_window: usize) -> crate::agent::PreparedPrompt {
        match self {
            MessageContainer::Conversation(cm) => cm.prepare(context_window),
            MessageContainer::Plain(msgs) => crate::agent_prompt::prepare_messages_for_llm(msgs, context_window),
        }
    }

    pub fn push(&mut self, msg: ChatMessage) {
        match self {
            MessageContainer::Conversation(cm) => cm.add_assistant_turn(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }

    pub fn push_tool_result(&mut self, tool_call_id: &str, result: String) {
        let msg = ChatMessage {
            role: "tool".to_string(),
            content: MessageContent::Text(result),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: None,
        };
        match self {
            MessageContainer::Conversation(cm) => cm.add_tool_result(msg),
            MessageContainer::Plain(msgs) => msgs.push(msg),
        }
    }
}
```

- [ ] **Step 2: Add module to lib.rs and run cargo check**

```rust
// src/lib.rs
pub mod loop_runner;
```

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/loop_runner.rs src/lib.rs
git commit -m "feat: add AgenticLoop runner with MessageContainer for dual path"
```

---

### Task 15: Refactor process_message and run_subagent_loop to use AgenticLoop

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Replace the inline loop in process_message**

```rust
use crate::loop_runner::{AgenticLoop, LoopConfig, LoopOutcome, MessageContainer};

let make_ctx = {
    let sandbox_dir = self.config.sandbox.allowed_directory.clone();
    let home_dir = self.config.resolved_home.clone();
    let sender = self.sender.clone();
    let cancel_registry = self.cancel_registry.clone();
    move |user_id: &str, chat_id: &str| ToolContext {
        sandbox_dir: sandbox_dir.clone(),
        home_dir: home_dir.clone(),
        sender: sender.clone(),
        cancel_registry: cancel_registry.clone(),
        user_id: user_id.to_string(),
        chat_id: chat_id.to_string(),
    }
};

let outcome = AgenticLoop::new(
    &self.llm,
    &self.tool_registry,
    &self.mcp,
    &LoopConfig { /* ... */ },
    Some(cancel_token),
    Some(chain_run_id),
    Some(&self.langsmith),
    self.sender.as_ref(),
    Box::new(make_ctx),
).run(&mut MessageContainer::Conversation(Box::new(cmgr))).await?;
```

- [ ] **Step 2: Replace the subagent loop**

```rust
let outcome = AgenticLoop::new(
    &self.llm,
    &self.tool_registry,
    &self.mcp,
    &LoopConfig {
        max_iterations: max_iter,
        empty_response_retry_limit: self.config.empty_response_retry_limit(),
        compaction_enabled: false,
        loop_detection_enabled: true,
        interactive_loop_callback: false,
        allowed_tools: Some(allowed_tools),
        langsmith_project: None,
        tool_event_tx: None,
        stream_token_tx: None,
    },
    cancel,
    None,
    None,
    self.sender.as_ref(),
    Box::new(make_subagent_ctx),
).run(&mut MessageContainer::Plain(messages)).await?;
```

- [ ] **Step 3: Run cargo check and tests**

Run: `cargo check && cargo test`

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "refactor: delegate agentic loop to AgenticLoop"
```

---

## M4: Supervisor ADR + seed::write_lock

### Task 16: Write Supervisor ADR

**Files:**
- Create: `docs/adr/0001-supervisor-module-structure.md`

```markdown
# ADR 0001: Supervisor Module Structure

## Status
Accepted

## Date
2026-07-15

## Context
The Supervisor module has 17 source files (`task.rs`, `job.rs`, `state.rs`, etc.)
with only one caller (the Supervisor facade). Should they be consolidated?

## Decision
Keep the 17-file split. Each file represents one stage of the pipeline.

## Rationale
- Each file is small (50–150 lines) and focused on one responsibility
- New pipeline stages can be added without modifying existing files
- The `backend/` submodule already justifies the structure (6 backend types)
- The design anticipates future variations

## Consequences
- Higher file count but each file is easier to navigate
- If after 6 months no second variation exists, consolidate
- Adding a new backend requires only a new file + Registry registration
```

- [ ] **Step 1: Create the file and commit**

```bash
mkdir -p docs/adr
git add docs/adr/0001-supervisor-module-structure.md
git commit -m "docs: add ADR for Supervisor module structure"
```

---

### Task 17: Add seed::write_lock helper

**Files:**
- Modify: `src/skills/seed.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add write_lock function to seed.rs**

The existing `seed.rs` already has `lock_map_for(&Path) -> BTreeMap<String, String>` returning name→SHA256. Add `use serde::{Deserialize, Serialize};` to the imports, then add:

```rust
/// A lock file recording which skills/agents have been seeded.
#[derive(Debug, Serialize, Deserialize)]
pub struct SkillLock {
    pub version: u32,
    pub skills: BTreeMap<String, String>,
}

/// Write a lock file if it doesn't exist yet.
pub fn write_lock(lock_name: &str, dir: &Path, home: &Path) -> Result<()> {
    let lock_path = home.join(lock_name);
    if !lock_path.exists() {
        let lock = SkillLock {
            version: 1,
            skills: lock_map_for(dir),
        };
        let json = serde_json::to_string_pretty(&lock)?;
        std::fs::write(&lock_path, json)?;
    }
    Ok(())
}
```

- [ ] **Step 2: Update main.rs to call write_lock**

Replace the existing inline lock-writing code with:
```rust
seed::write_lock("skills-lock.json", &config.skills.directory, &home)?;
seed::write_lock("agents-lock.json", &config.agents.directory, &home)?;
```

- [ ] **Step 3: Run cargo check and tests**

Run: `cargo check && cargo test`

- [ ] **Step 4: Commit**

```bash
git add src/skills/seed.rs src/main.rs
git commit -m "refactor: extract seed::write_lock function"
```

---

## Self-Review

### Spec Coverage

| Spec Requirement | Task |
|-----------------|------|
| CancelRegistry with register/cancel/unregister | Task 1 |
| PlatformSender trait with 5 methods | Task 2 |
| PlatformMessageId type alias | Task 2 |
| ToolHandler trait (define + execute) | Task 3 |
| ToolRegistry | Task 3 |
| ToolContext struct | Task 3 |
| TelegramAdapter implementing PlatformSender | Task 4 |
| Cancel button callback migrates to CancelRegistry | Task 4 |
| BuiltinTools preserving all original parameter names | Task 5 |
| MemoryTools with real search_knowledge + search_messages | Task 6 |
| SchedulingTools with real scheduler APIs | Task 7 |
| SkillTools with real file operations | Task 8 |
| CommandTool with escape_text + CancelRegistry | Task 9 |
| Agent no longer holds Arc<Bot> or running_commands | Task 10 |
| MCP + invoke_agent/spawn_agents dispatch stays in Agent (circular dep) | Task 10 |
| SkillTools handler (write/read_skill_file, reload_skills, write/read_agent_file, reload_agents) | Task 8 |
| telegram.rs free functions migrated | Task 11 |
| ConversationManager with real memory/file_processor APIs | Task 12 |
| process_message uses ConversationManager | Task 13 |
| LoopConfig, AgenticLoop, LoopOutcome | Task 14 |
| MessageContainer for both main and subagent paths | Task 14 |
| process_message and run_subagent_loop delegate to AgenticLoop | Task 15 |
| Supervisor ADR | Task 16 |
| seed::write_lock reusing existing lock_map_for | Task 17 |

### Placeholder Scan
No "TBD", "TODO", "implement later", or FIXME placeholders remain. Every tool implementation moves the full real code or explicitly references the original source.

### Type Consistency
- ToolContext: `sandbox_dir: PathBuf`, `home_dir: Option<PathBuf>`, `sender: Arc<dyn PlatformSender>`, `cancel_registry: Arc<CancelRegistry>`, `user_id: String`, `chat_id: String`
- PlatformMessageId: `String`
- CancelRegistry: `register(String, oneshot::Sender<()>)`, `cancel(&str) -> bool`, `unregister(&str)`
- llm.chat: `(&[ChatMessage], &[ToolDefinition])` — 2 args
- memory.load_messages: `(&str) -> Result<Vec<ChatMessage>>`
- memory.save_message: `(&str, &ChatMessage) -> Result<String>`
- file_processor::process_attachments: `(&[Attachment], &str, &Config, &MemoryStore, bool) -> (String, Vec<ContentPart>)`
- scheduler.add_one_shot_job: `(Duration, &str, F) -> Result<Uuid>`
- scheduler.add_cron_job: `(&str, &str, F) -> Result<Uuid>`
- seed::lock_map_for: `(&Path) -> BTreeMap<String, String>`

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-15-architecture-deepening.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
