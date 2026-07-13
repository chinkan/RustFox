# Scheduled Task Isolation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three scheduled task issues — context isolation from user conversations, execution history persistence with new tools, and rich message formatting for scheduled responses.

**Architecture:** Use dedicated `platform: "scheduled_task"` + `user_id: "{real_uid}:{task_id}"` in `IncomingMessage` to create isolated SQLite conversations. Persist execution results in a new `scheduled_task_runs` table. Use existing `send_markdown_message()` (rich→entity fallback) for sending responses from the background runner.

**Tech Stack:** Rust, rusqlite, tokio-cron-scheduler, teloxide, serde_json

---

## Files Summary

| File | Change |
|------|--------|
| `src/memory/mod.rs` | Add `CREATE TABLE scheduled_task_runs` + index |
| `src/scheduler/reminders.rs` | Add `ScheduledTaskRun` struct + `insert_run`, `update_run`, `get_task_runs` methods |
| `src/agent.rs` | Fix fire closures in 2 places; add tool definitions + dispatch for 2 new tools |
| `src/platform/telegram.rs` | Make `send_markdown_message` `pub` |
| `src/platform/tool_notifier.rs` | Add `friendly_tool_name` entries for 2 new tools |
| `src/main.rs` | Replace raw `send_message` with `send_markdown_message`; persist run records |

---

### Task 1: Add `scheduled_task_runs` table

**Files:**
- Modify: `src/memory/mod.rs` (after `scheduled_tasks` table, around line 220)

- [ ] **Step 1: Add the new table DDL**

In `src/memory/mod.rs`, after the `scheduled_tasks` index at line 220, add:

```rust
            -- Scheduled task execution history
            CREATE TABLE IF NOT EXISTS scheduled_task_runs (
                id          TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                run_at      TEXT NOT NULL,
                response    TEXT,
                error       TEXT,
                status      TEXT NOT NULL DEFAULT 'completed',
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id)
            );

            CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_task
                ON scheduled_task_runs(task_id, run_at);
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/memory/mod.rs
git commit -m "feat(scheduler): add scheduled_task_runs table for execution history"
```

---

### Task 2: Add `ScheduledTaskRun` struct and CRUD methods to `ScheduledTaskStore`

**Files:**
- Modify: `src/scheduler/reminders.rs`

- [ ] **Step 1: Add `ScheduledTaskRun` struct after `ScheduledTask`**

In `src/scheduler/reminders.rs`, after the existing `ScheduledTask` struct, add:

```rust
#[derive(Debug, Clone)]
pub struct ScheduledTaskRun {
    pub id: String,
    pub task_id: String,
    pub run_at: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub status: String,
    pub created_at: String,
}
```

- [ ] **Step 2: Add `insert_run` method**

Add after the existing `update_next_run_at` method:

```rust
    pub async fn insert_run(
        &self,
        id: &str,
        task_id: &str,
        run_at: &str,
        response: Option<&str>,
        error: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO scheduled_task_runs (id, task_id, run_at, response, error, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, task_id, run_at, response, error, status],
        )
        .context("Failed to insert scheduled task run")?;
        Ok(())
    }
```

- [ ] **Step 3: Add `update_run` method**

Add after `insert_run`:

```rust
    pub async fn update_run(
        &self,
        id: &str,
        response: Option<&str>,
        error: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE scheduled_task_runs SET response = ?1, error = ?2, status = ?3 WHERE id = ?4",
            rusqlite::params![response, error, status, id],
        )
        .context("Failed to update scheduled task run")?;
        Ok(())
    }
```

- [ ] **Step 4: Add `get_task_runs` method**

Add after `update_run`:

```rust
    pub async fn get_task_runs(&self, task_id: &str, limit: usize) -> Result<Vec<ScheduledTaskRun>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, run_at, response, error, status, created_at
                 FROM scheduled_task_runs
                 WHERE task_id = ?1
                 ORDER BY run_at DESC
                 LIMIT ?2",
            )
            .context("Failed to prepare get_task_runs query")?;
        let runs = stmt
            .query_map(rusqlite::params![task_id, limit as i64], |row| {
                Ok(ScheduledTaskRun {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    run_at: row.get(2)?,
                    response: row.get(3)?,
                    error: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .context("Failed to map rows")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect rows")?;
        Ok(runs)
    }
```

- [ ] **Step 5: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
git add src/scheduler/reminders.rs
git commit -m "feat(scheduler): add ScheduledTaskRun struct with insert/update/get methods"
```

---

### Task 3: Fix fire closures in `schedule_task` handler and `restore_scheduled_tasks()`

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Fix `schedule_task` handler fire closure (line ~3305)**

In the `"schedule_task"` match arm, change the `IncomingMessage` construction inside the fire closure:

Old:
```rust
let incoming = crate::platform::IncomingMessage {
    platform: "telegram".to_string(),
    user_id: uid,
    ...
};
```

New:
```rust
let incoming = crate::platform::IncomingMessage {
    platform: "scheduled_task".to_string(),
    user_id: format!("{uid}:{tid}"),
    ...
};
```

Note: the local `tid` is `task_id` (the UUID of the task). The `uid` is the user's real ID. The closure captures `tid` and `uid` by clone. Since `fire` already captures `uid` and `tid` (as `uid` and `tid` variables), the format string uses those names. Verify the captured variable names match the actual closure code.

- [ ] **Step 2: Fix `restore_scheduled_tasks()` fire closure (line ~2053)**

In `restore_scheduled_tasks()`, same change to the `IncomingMessage` construction inside the fire closure:

Old:
```rust
let incoming = crate::platform::IncomingMessage {
    platform: "telegram".to_string(),
    user_id: uid,
    ...
};
```

New:
```rust
let incoming = crate::platform::IncomingMessage {
    platform: "scheduled_task".to_string(),
    user_id: format!("{uid}:{tid}"),
    ...
};
```

Here `uid` and `tid` are already captured variables (`let uid = task.user_id.clone()` and `let tid = task.id.clone()`). Verify exact captured variable names against the actual code.

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "fix(scheduler): isolate scheduled task conversations with dedicated platform/user_id"
```

---

### Task 4: Add `get_scheduled_task_history` and `rerun_scheduled_task` tool definitions

**Files:**
- Modify: `src/agent.rs` (in `scheduling_tool_definitions()` around line 2208)

- [ ] **Step 1: Add two new tool definitions**

In `scheduling_tool_definitions()`, after the `cancel_scheduled_task` entry, add:

```rust
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "get_scheduled_task_history".to_string(),
                    description: "Retrieve execution history for a scheduled task, including run timestamps, status, and response text.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string", "description": "The task ID from list_scheduled_tasks" }
                        },
                        "required": ["task_id"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "rerun_scheduled_task".to_string(),
                    description: "Execute a scheduled task immediately, regardless of its normal schedule. Does not cancel future occurrences.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string", "description": "The task ID to execute now" }
                        },
                        "required": ["task_id"]
                    }),
                },
            },
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/agent.rs
git commit -m "feat(scheduler): add get_scheduled_task_history and rerun_scheduled_task tool defs"
```

---

### Task 5: Add dispatch handlers for the two new tools

**Files:**
- Modify: `src/agent.rs` (in `execute_tool()` around line 3400)

- [ ] **Step 1: Add `get_scheduled_task_history` handler**

After the `cancel_scheduled_task` match arm (around line 3399), add:

```rust
            "get_scheduled_task_history" => {
                let task_id = match arguments["task_id"].as_str() {
                    Some(id) => id.to_string(),
                    None => return "Missing task_id".to_string(),
                };
                match self.task_store.get_task_runs(&task_id, 20).await {
                    Ok(runs) if runs.is_empty() => {
                        format!("No execution history for task '{}'.", task_id)
                    }
                    Ok(runs) => {
                        let mut out = format!("Execution history for task '{}' ({} runs):\n\n", task_id, runs.len());
                        for r in &runs {
                            let resp = r.response.as_deref().unwrap_or("(no response)");
                            let err = r.error.as_deref().map(|e| format!("\nError: {}", e)).unwrap_or_default();
                            let truncated = if resp.len() > 2000 {
                                format!("{}... (truncated)", &resp[..2000])
                            } else {
                                resp.to_string()
                            };
                            out.push_str(&format!(
                                "Run at: {} | Status: {}\n{}{}\n\n",
                                r.run_at, r.status, truncated, err
                            ));
                        }
                        out
                    }
                    Err(e) => format!("Failed to query task history: {}", e),
                }
            }
```

- [ ] **Step 2: Add `rerun_scheduled_task` handler**

After the `get_scheduled_task_history` arm, add:

```rust
            "rerun_scheduled_task" => {
                let task_id = match arguments["task_id"].as_str() {
                    Some(id) => id.to_string(),
                    None => return "Missing task_id".to_string(),
                };
                let task = match self.task_store.get_by_id(&task_id).await {
                    Ok(Some(t)) => t,
                    Ok(None) => return format!("Task '{}' not found.", task_id),
                    Err(e) => return format!("Failed to look up task: {}", e),
                };
                // Build fire closure (same pattern as schedule_task handler)
                let job_tx = self.job_tx.clone();
                let bot_clone = Arc::clone(&self.bot);
                let store_clone = self.task_store.clone();
                let tid = task.id.clone();
                let uid = task.user_id.clone();
                let cid = task.chat_id.clone();
                let prompt_cap = task.prompt.clone();
                let is_recurring = false;

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
                        let req = crate::agent::ScheduledJobRequest {
                            incoming,
                            bot,
                            task_id: tid,
                            is_recurring,
                            task_store: store,
                        };
                        if let Err(e) = tx.send(req) {
                            tracing::error!("Failed to dispatch rerun scheduled job: {}", e);
                        }
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };

                // Fire immediately with a 1-second delay to allow the response to return
                match self.scheduler.add_one_shot_job(
                    std::time::Duration::from_secs(1),
                    &format!("rerun-{}", task.description),
                    fire,
                ).await {
                    Ok(sched_id) => {
                        format!("Task '{}' scheduled for immediate re-execution (scheduler ID: {}).", task_id, sched_id)
                    }
                    Err(e) => format!("Failed to re-run task: {}", e),
                }
            }
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "feat(scheduler): add handler dispatch for get_scheduled_task_history and rerun_scheduled_task"
```

---

### Task 6: Make `send_markdown_message` public

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Change `send_markdown_message` from private to `pub`**

Find `async fn send_markdown_message` at line 248 and change it to:

```rust
pub async fn send_markdown_message(bot: &Bot, chat_id: ChatId, markdown: &str) -> ResponseResult<()> {
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(telegram): make send_markdown_message pub for scheduled task use"
```

---

### Task 7: Update background runner in `main.rs` for rich formatting + run persistence

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add import for `send_markdown_message`**

At the top of `src/main.rs`, add to the existing `use` block:

```rust
use rustfox::platform::telegram::send_markdown_message;
```

- [ ] **Step 2: Replace the background runner body**

Replace the entire background runner `tokio::spawn` block (lines 236-277) with:

```rust
    // Spawn background runner: receives ScheduledJobRequest, calls process_message, persists result, sends reply
    let agent_for_runner = Arc::clone(&agent);
    tokio::spawn(async move {
        use teloxide::prelude::*;
        while let Some(req) = job_rx.recv().await {
            let agent = Arc::clone(&agent_for_runner);

            // Persist run record BEFORE processing (capture fire time)
            let run_id = uuid::Uuid::new_v4().to_string();
            let run_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
            let _ = req.task_store.insert_run(
                &run_id, &req.task_id, &run_at, None, None, "running",
            ).await;

            let response = match agent.process_message(&req.incoming, None, None).await {
                Ok(r) => {
                    let _ = req.task_store.update_run(
                        &run_id, Some(&r), None, "completed",
                    ).await;
                    r
                }
                Err(e) => {
                    tracing::error!("Scheduled task {} failed: {}", req.task_id, e);
                    let err_str = format!("{:#}", e);
                    let _ = req.task_store.update_run(
                        &run_id, None, Some(&err_str), "failed",
                    ).await;
                    if !req.is_recurring {
                        let _ = req.task_store.set_status(&req.task_id, "failed").await;
                    }
                    // Send error to user via rich message
                    let chat_id_val: i64 = match req.incoming.chat_id.parse() {
                        Ok(v) => v,
                        Err(_) => {
                            tracing::error!(
                                "Unparseable chat_id '{}' for task {}",
                                req.incoming.chat_id,
                                req.task_id
                            );
                            continue;
                        }
                    };
                    let chat = teloxide::types::ChatId(chat_id_val);
                    let error_msg = format!("**Scheduled task failed:** {}", e);
                    let _ = send_markdown_message(&req.bot, chat, &error_msg).await;
                    continue;
                }
            };

            let chat_id_val: i64 = match req.incoming.chat_id.parse() {
                Ok(v) => v,
                Err(_) => {
                    tracing::error!(
                        "Unparseable chat_id '{}' for task {}",
                        req.incoming.chat_id,
                        req.task_id
                    );
                    continue;
                }
            };
            let chat = teloxide::types::ChatId(chat_id_val);
            if let Err(e) = send_markdown_message(&req.bot, chat, &response).await {
                tracing::error!("Failed to send scheduled response: {}", e);
            }
        }
    });
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "fix(scheduler): use send_markdown_message and persist run records in background runner"
```

---

### Task 8: Add `friendly_tool_name` entries for new tools

**Files:**
- Modify: `src/platform/tool_notifier.rs`

- [ ] **Step 1: Add entries in `friendly_tool_name()`**

In `friendly_tool_name()` at line 282, after the `"cancel_scheduled_task"` entry, add:

```rust
        "get_scheduled_task_history" => return "📋 Checking task history".to_string(),
        "rerun_scheduled_task" => return "🔄 Re-running scheduled task".to_string(),
```

- [ ] **Step 2: Add unit tests for the new entries**

In `tool_notifier.rs`, after the `#[cfg(test)]` section, the existing test module `mod tests` at the bottom of the file. After the relevant test functions (around line 1131), add:

```rust
    #[test]
    fn test_friendly_tool_name_get_scheduled_task_history() {
        assert_eq!(
            friendly_tool_name("get_scheduled_task_history"),
            "📋 Checking task history"
        );
    }

    #[test]
    fn test_friendly_tool_name_rerun_scheduled_task() {
        assert_eq!(
            friendly_tool_name("rerun_scheduled_task"),
            "🔄 Re-running scheduled task"
        );
    }
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p rustfox tool_notifier -- --test-threads=1`
Expected: All tests pass including the two new ones

- [ ] **Step 4: Commit**

```bash
git add src/platform/tool_notifier.rs
git commit -m "feat(notifier): add friendly_tool_name entries for new scheduled task tools"
```

---

### Task 9: Add unit tests for `ScheduledTaskStore` run methods

**Files:**
- Modify: `src/scheduler/reminders.rs` (add `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add test module**

Append to `src/scheduler/reminders.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    #[tokio::test]
    async fn test_insert_and_get_task_runs() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .insert_run("run-1", "task-1", "2026-07-13T10:00:00", Some("hello"), None, "completed")
            .await
            .unwrap();
        store
            .insert_run("run-2", "task-1", "2026-07-13T11:00:00", None, Some("error"), "failed")
            .await
            .unwrap();

        let runs = store.get_task_runs("task-1", 10).await.unwrap();
        assert_eq!(runs.len(), 2);
        // Most recent first
        assert_eq!(runs[0].id, "run-2");
        assert_eq!(runs[1].id, "run-1");
        assert_eq!(runs[0].response, None);
        assert_eq!(runs[0].error.as_deref(), Some("error"));
        assert_eq!(runs[1].response.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn test_get_task_runs_empty() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        let runs = store.get_task_runs("nonexistent", 10).await.unwrap();
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn test_update_run() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());

        store
            .insert_run("run-x", "task-x", "2026-07-13T12:00:00", None, None, "running")
            .await
            .unwrap();

        store
            .update_run("run-x", Some("result"), None, "completed")
            .await
            .unwrap();

        let runs = store.get_task_runs("task-x", 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].response.as_deref(), Some("result"));
        assert_eq!(runs[0].status, "completed");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rustfox reminders -- --test-threads=1`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/scheduler/reminders.rs
git commit -m "test(scheduler): add unit tests for ScheduledTaskStore run methods"
```

---

### Task 10: Final verification

**Files:** (no changes)

- [ ] **Step 1: Run full build**

Run: `cargo build`
Expected: Compiles with no errors

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass
