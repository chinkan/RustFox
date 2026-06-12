# Send File from Sandbox — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `send_file` built-in tool so the AI agent can send sandbox files to the Telegram user.

**Architecture:** Tool defined in `tools.rs` (schema only), executed in `agent.rs::execute_tool()` (needs `Bot` + `ChatId`). Reuses existing `validate_sandbox_path`. Signature change: `chat_id: &str` → `ChatId` on `execute_tool`.

**Tech Stack:** Rust, teloxide, tokio

---

### Task 1: Tool definition in tools.rs

**Files:**
- Modify: `src/tools.rs:10` (make `validate_sandbox_path` pub)
- Modify: `src/tools.rs:48-249` (add `send_file` to `builtin_tool_definitions()`)

- [ ] **Step 1: Make `validate_sandbox_path` pub**

Change `fn validate_sandbox_path` to `pub fn validate_sandbox_path` at `src/tools.rs:10`.

- [ ] **Step 2: Add `send_file` to `builtin_tool_definitions()`**

Insert a new `ToolDefinition` entry after `list_files` (around line 105, grouping it with the other file-related tools read/write/list):

```rust
ToolDefinition {
    tool_type: "function".to_string(),
    function: FunctionDefinition {
        name: "send_file".to_string(),
        description: "Send a file from the sandbox to the current chat. The file must already exist in the sandbox."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path (relative to sandbox or absolute within sandbox)"
                },
                "caption": {
                    "type": "string",
                    "description": "Optional caption for the file"
                }
            },
            "required": ["path"]
        }),
    },
},
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: Compiles with no errors.

- [ ] **Step 4: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add send_file tool definition and make validate_sandbox_path pub"
```

---

### Task 2: Execution in agent.rs

**Files:**
- Modify: `src/agent.rs:325-326` (parse ChatId once)
- Modify: `src/agent.rs:724-792` (parallel call site — pass parsed ChatId)
- Modify: `src/agent.rs:794-861` (sequential call site — pass parsed ChatId)
- Modify: `src/agent.rs:1824` (change signature)
- Modify: `src/agent.rs:1824-2690` (add `"send_file"` arm)

- [ ] **Step 1: Parse chat_id once at the top of process_message**

At `src/agent.rs:326`, after `let chat_id = &incoming.chat_id;`, add:

```rust
let parsed_chat_id: ChatId = incoming.chat_id.parse::<i64>().map(ChatId).unwrap_or(ChatId(0));
```

The `parse::<i64>()` works because Telegram chat IDs are numeric (positive for private chats, negative for groups/supergroups). The `ChatId(0)` fallback is safe because `send_file` will fail gracefully on invalid IDs.

- [ ] **Step 2: Pass parsed_chat_id to execute_tool in parallel agent group**

At `src/agent.rs:758-759`, change:
```rust
let result =
    self.execute_tool(&name, &args, user_id, chat_id).await;
```
to:
```rust
let result =
    self.execute_tool(&name, &args, user_id, parsed_chat_id).await;
```

The closure captures `parsed_chat_id` by copy (`ChatId` is `Copy`).

- [ ] **Step 3: Pass parsed_chat_id in sequential tool group**

At `src/agent.rs:833`, change:
```rust
let result = self.execute_tool(&name, &args, user_id, chat_id).await;
```
to:
```rust
let result = self.execute_tool(&name, &args, user_id, parsed_chat_id).await;
```

- [ ] **Step 4: Change execute_tool signature**

At `src/agent.rs:1824`, change:
```rust
async fn execute_tool(
    &self,
    name: &str,
    arguments: &serde_json::Value,
    user_id: &str,
    chat_id: &str,
) -> String {
```
to:
```rust
async fn execute_tool(
    &self,
    name: &str,
    arguments: &serde_json::Value,
    user_id: &str,
    chat_id: ChatId,
) -> String {
```

Add `use teloxide::types::ChatId;` to the imports at the top of the file (alongside the existing `use teloxide::Bot;` at line 6).

Also update the subagent call site at `src/agent.rs:1792` — change `""` to `ChatId(0)` to match the new signature:
```rust
"", // agent has no user_id context
ChatId(0), // agent has no chat_id context
```

- [ ] **Step 5: Add send_file arm in execute_tool**

Add a new arm **before** the fallthrough to `tools::execute_builtin_tool()` (before the `_ =>` arm at line ~2677). Place it after the `"patch_skill"` arm and before the MCP check:

```rust
"send_file" => {
    match async {
        let path = arguments["path"]
            .as_str()
            .context("Missing 'path' argument")?;
        let caption = arguments
            .get("caption")
            .and_then(|v| v.as_str())
            .filter(|c| !c.is_empty());

        let full_path = tools::validate_sandbox_path(
            &self.config.sandbox.allowed_directory,
            path,
        )?;

        let metadata = tokio::fs::metadata(&full_path).await
            .with_context(|| format!("File not found: {}", full_path.display()))?;
        const TG_FILE_LIMIT: u64 = 50 * 1024 * 1024;
        if metadata.len() > TG_FILE_LIMIT {
            anyhow::bail!(
                "File is {} MB — exceeds Telegram's 50 MB limit",
                metadata.len() / 1024 / 1024
            );
        }

        let bytes = tokio::fs::read(&full_path).await
            .with_context(|| format!("Failed to read file: {}", full_path.display()))?;

        let file_name = full_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let input_file = teloxide::types::InputFile::memory(bytes).file_name(file_name.clone());
        let mut req = self.bot.send_document(chat_id, input_file);
        if let Some(c) = caption {
            req = req.caption(c);
        }
        req.await
            .with_context(|| "Telegram API failed to send document")?;

        Ok(format!("File '{}' sent successfully.", file_name))
    }.await {
        Ok(msg) => msg,
        Err(e) => format!("Error sending file: {:#}", e),
    }
}
```

- [ ] **Step 6: Add ChatId and InputFile imports**

Add these alongside the existing imports at the top of `src/agent.rs` (near `use anyhow::Result;` at line 1 and `use teloxide::Bot;` at line 6):
```rust
use anyhow::Context;
use teloxide::types::{ChatId, InputFile};
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo check`
Expected: Compiles with no errors.

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs
git commit -m "feat: add send_file execution in agent.rs execute_tool"
```

---

### Task 3: Friendly tool name in tool_notifier.rs

**Files:**
- Modify: `src/platform/tool_notifier.rs:285`

- [ ] **Step 1: Add friendly name for send_file**

At `src/platform/tool_notifier.rs`, add a new arm in the `friendly_tool_name` function (near the other built-in tools at ~line 285):

```rust
"send_file" => return "📤 Sending a file".to_string(),
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: Compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/platform/tool_notifier.rs
git commit -m "feat: add friendly tool name for send_file"
```

---

### Task 4: Final build and clippy

- [ ] **Step 1: Full release build**

Run: `cargo build --release`
Expected: Builds successfully.

- [ ] **Step 2: Clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Format**

Run: `cargo fmt`
Expected: No changes.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass (existing tool tests, nothing new for send_file yet).

- [ ] **Step 5: Final commit**

```bash
git add -A && git commit -m "chore: final cleanup after send_file implementation"
```
