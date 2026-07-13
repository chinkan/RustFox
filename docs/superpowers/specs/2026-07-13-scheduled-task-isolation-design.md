# Scheduled Task Isolation — Execution History, Context Isolation, Rich Response Formatting

**Date:** 2026-07-13
**Feature:** Three fixes for scheduled tasks: (1) context isolation from user conversation, (2) execution history persistence with new tools, (3) rich message formatting for scheduled responses

## Problem

### Issue 1: Context mixing

When a scheduled task fires, it calls `agent.process_message()` with the user's `platform` (`"telegram"`) and `user_id`. This causes:

1. The scheduled task loads the **user's main conversation** (`get_or_create_conversation("telegram", user_id)`) — including all prior chat history
2. The scheduled task registers a **cancel token** under the user's user_id, causing `is_processing(uid)` to return `true` for the user
3. Any user message sent while the scheduled task runs gets queued as a **steer injection** into the scheduled task's processing loop
4. The scheduled task's execution messages (tool calls, results) get **saved into the user's main conversation history**

### Issue 2: No execution history or re-run capability

The `scheduled_tasks` table stores only task definitions (prompt, trigger, status). Once a task runs, there is no record of what happened — no response, no error, no timestamp. There is no tool to re-execute a past task.

### Issue 3: Raw markdown in scheduled responses

The background runner (`main.rs:268-275`) sends response text via `bot.send_message(chat, &chunk)` — plain raw markdown without any formatting conversion. All other message paths (normal chat, streaming) use the `sendRichMessage` → entity fallback pipeline for proper rendering.

## Architecture

```
  schedule_task tool fire / restore_scheduled_tasks()
         │
         ▼
  fire closure creates IncomingMessage
  ┌──────────────────────────────────────┐
  │ platform: "scheduled_task"           │
  │ user_id: "{real_uid}:{task_id}"      │ ← unique per task run
  │ chat_id: real_chat_id                │
  │ text: task prompt                    │
  └──────────────────────────────────────┘
         │
         ▼
  job_tx.send(ScheduledJobRequest)
         │
         ▼
  Background runner (main.rs tokio::spawn)
  ┌──────────────────────────────────────┐
  │ 1. Persist run record (status=running)│
  │    → capture run_at = fire time      │
  │                                      │
  │ 2. process_message()                 │
  │    → dedicated SQLite conversation   │
  │    → no cancel token for real user   │
  │    → no steer injection from user    │
  │                                      │
  │ 3. Update run record (status=done)   │
  │    → store response / error          │
  │                                      │
  │ 4. Send response via                 │
  │    send_markdown_message()           │
  │    → tries sendRichMessage first     │
  │    → falls back to entity sender     │
  └──────────────────────────────────────┘
```

### Database

New table `scheduled_task_runs` (auto-created via `CREATE TABLE IF NOT EXISTS` in `memory/mod.rs` alongside existing tables):

```sql
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

### Tools

Two new tools added to `scheduling_tool_definitions()` in `agent.rs`:

**`get_scheduled_task_history(task_id)`**

Returns all execution records for a task, most recent first. Each record shows: run_at, status (completed/failed), response (truncated to 2000 chars), error (if any). Allows the LLM to answer "what happened with my scheduled task?".

**`rerun_scheduled_task(task_id)`**

Fetches the task from DB, creates a new one-shot job with 0-second delay (fires immediately). Returns the new run's ID. Does NOT cancel any future recurring occurrences — only adds an extra execution.

### Changes per file

#### `src/memory/mod.rs`

- Add `scheduled_task_runs` table + index creation inside the existing initialization block (after `scheduled_tasks` table)

#### `src/agent.rs`

**`schedule_task` handler** (line 3223):

- Change `IncomingMessage` construction in the fire closure:
  - `platform: "scheduled_task".to_string()` (was `"telegram"`)
  - `user_id: format!("{user_id}:{task_id}")` (was `user_id.to_string()`)
  - (chat_id, text, attachments unchanged)

**`scheduling_tool_definitions()`** (line 2208):

- Add two new `ToolDefinition` entries: `get_scheduled_task_history` and `rerun_scheduled_task`

**`execute_tool()` dispatch** (around line 3400):

- Add match arms for `"get_scheduled_task_history"` and `"rerun_scheduled_task"`:
  - `get_scheduled_task_history`: query `scheduled_task_runs` for task_id, format as text
  - `rerun_scheduled_task`: fetch scheduled_task, build fire closure + dispatch one-shot

**`restore_scheduled_tasks()`** (line 2022):

- Apply the same `IncomingMessage` changes to the fire closure inside `restore_scheduled_tasks()` (lines 2053-2060):
  - `platform: "scheduled_task".to_string()` (was `"telegram"`)
  - `user_id: format!("{uid}:{tid}")` (was `uid` directly)
- **Do NOT skip this function** — it builds identical fire closures for tasks restored after bot restart. Without these changes, restored tasks would still share conversation context even though newly-created tasks from `schedule_task` handler are fixed.

> **Design note:** Both `schedule_task` (line 3295) and `restore_scheduled_tasks()` (line 2043) contain nearly identical fire closure code. Consider extracting a shared helper method to prevent future divergence.

#### `src/platform/telegram.rs`

- Make `send_markdown_message` function `pub` (currently private) so it can be called from the background runner in `main.rs`

#### `src/main.rs`

- Add import: `use rustfox::platform::telegram::send_markdown_message;`

**Background runner** (line 238):

- BEFORE calling `process_message`, capture `run_at` timestamp and persist a run record with `status = 'running'`:
  ```rust
  let run_id = uuid::Uuid::new_v4().to_string();
  let run_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
  let _ = req.task_store.insert_run(&run_id, &req.task_id, &run_at, None, None, "running").await;
  ```
- AFTER `process_message` completes (both success and error), update the run record:
  ```rust
  let _ = req.task_store.update_run(&run_id, &response, error_opt, "completed").await;
  ```
- Replace raw `bot.send_message(chat, &chunk).await` loop with:
  ```rust
  let chat = teloxide::types::ChatId(chat_id_val);
  if let Err(e) = send_markdown_message(&req.bot, chat, &response).await {
      tracing::error!("Failed to send scheduled response: {}", e);
  }
  ```
  Note: `send_markdown_message` returns `ResponseResult<()>` (teloxide error type), not `anyhow::Result`. The `if let Err(e)` pattern handles this correctly.

- Error path: update run record with `status = "failed"` and error text, send error via `send_markdown_message`

#### `src/scheduler/reminders.rs`

- Add `ScheduledTaskRun` struct:
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
- Add `insert_run(id, task_id, run_at, response, error, status)` method — creates a new run row with given id
- Add `update_run(id, response, error, status)` method — updates an existing run record
- Add `get_task_runs(task_id, limit)` method returning `Vec<ScheduledTaskRun>` ordered by `run_at DESC`

### Error handling

| Scenario | Behaviour |
|----------|-----------|
| `send_markdown_message` fails for scheduled response | Log error, skip (degradation: user doesn't see result) |
| `scheduled_task_runs` insert fails | Log warning, response still sent to user |
| `rerun_scheduled_task` on unknown task_id | Return error string to LLM |
| `get_scheduled_task_history` on unknown task_id | Return empty history |
| Scheduled task fails during `process_message` | Persist record with status="failed" + error text, send error to user via `send_markdown_message` |

### Testing

- No existing tests for scheduled task execution; manual verification recommended
- Unit test for `ScheduledTaskStore.insert_run()` and `get_task_runs()` in `reminders.rs`
- `friendly_tool_name()` entries for the two new tools must be added to `tool_notifier.rs` so they show human-friendly labels when verbose tool UI is enabled

### Dependencies

No new crate dependencies. `uuid`, `chrono`, `rusqlite` already in `Cargo.toml`.
