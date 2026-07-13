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
  schedule_task tool fire
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
  Background runner (tokio::spawn)
  ┌──────────────────────────────────────┐
  │ 1. process_message()                 │
  │    → dedicated SQLite conversation   │
  │    → no cancel token for real user   │
  │    → no steer injection from user    │
  │                                      │
  │ 2. Send response via                 │
  │    send_markdown_message()           │
  │    → tries sendRichMessage first     │
  │    → falls back to entity sender     │
  │                                      │
  │ 3. INSERT INTO scheduled_task_runs   │
  │    → persist execution result        │
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

**Background runner in `restore_scheduled_tasks()`** (line 2022):

- No changes needed — the fire closures built here already use `job_tx.send()`, which routes through the same background runner. The fix is in the fire closure itself (platform + user_id).

#### `src/platform/telegram.rs`

- Make `send_markdown_message` function `pub` (currently private) so it can be called from the background runner in `main.rs`

#### `src/main.rs`

- Add import: `use rustfox::platform::telegram::send_markdown_message;`

**Background runner** (line 238):

- After `process_message` returns, persist execution result via `req.task_store.insert_run()`
- Replace raw `bot.send_message(chat, &chunk).await` loop with:
  ```rust
  let chat = teloxide::types::ChatId(chat_id_val);
  if let Err(e) = send_markdown_message(&req.bot, chat, &response).await {
      tracing::error!("Failed to send scheduled response: {}", e);
  }
  ```

- Error path: set status to "failed", insert run record with error, send error via `send_markdown_message`

#### `src/scheduler/reminders.rs`

- Add `insert_run(task_id, run_at, response, error, status)` method to `ScheduledTaskStore` for persisting execution results. Creates new UUID for run id.
- Add `get_task_runs(task_id, limit)` method returning `Vec<ScheduledTaskRun>` for history queries
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
- Unit test for new tool definitions parsing

### Dependencies

No new crate dependencies. `uuid`, `chrono`, `rusqlite` already in `Cargo.toml`.
