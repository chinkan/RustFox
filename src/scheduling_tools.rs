use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::ScheduledJobRequest;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::schedule::SchedulingOps;
use crate::scheduler::{reminders::ScheduledTask, reruns::RerunQueue, Scheduler};
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};
use teloxide::prelude::Bot;
use uuid::Uuid;

pub struct SchedulingTools {
    task_store: ScheduledTaskStore,
    scheduler: Arc<Scheduler>,
    /// Arms / disarms live jobs and persists the job id (issue #109, Bug 2).
    /// Injected so tests can drive the whole tool with a fake, without an
    /// `Agent` (production wiring passes `AgentOpsScheduling`).
    ops: Arc<dyn SchedulingOps>,
    job_tx: UnboundedSender<ScheduledJobRequest>,
    bot: Arc<Bot>,
    telegram_bots: Arc<std::sync::RwLock<std::collections::HashMap<String, Arc<Bot>>>>,
    rerun_queue: RerunQueue,
    /// `(shim bot id, owner Telegram id)` for turns with no Telegram chat
    /// (portal web chat, #163): their schedules live on the owner's DM.
    telegram_owner: Option<(String, String)>,
}

impl SchedulingTools {
    pub fn new(
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        ops: Arc<dyn SchedulingOps>,
        job_tx: UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
        telegram_bots: Arc<std::sync::RwLock<std::collections::HashMap<String, Arc<Bot>>>>,
        rerun_queue: RerunQueue,
    ) -> Self {
        Self {
            task_store,
            scheduler,
            ops,
            job_tx,
            bot,
            telegram_bots,
            rerun_queue,
            telegram_owner: None,
        }
    }

    pub fn with_telegram_owner(mut self, bot_id: String, chat_id: String) -> Self {
        self.telegram_owner = Some((bot_id, chat_id));
        self
    }

    /// A non-Telegram turn (chat_id not numeric) acts as the owner on the
    /// shim bot, so create, list and cancel all see the same rows.
    fn telegram_scope(&self, mut ctx: ToolContext) -> ToolContext {
        if let Some((bot_id, owner)) = &self.telegram_owner {
            if ctx.chat_id.trim().parse::<i64>().is_err() {
                ctx.bot_id = bot_id.clone();
                ctx.user_id = owner.clone();
                ctx.chat_id = owner.clone();
            }
        }
        ctx
    }

    async fn rerun_owned_by(&self, queue_id: &str, bot_id: &str) -> anyhow::Result<bool> {
        let Some(row) = self.rerun_queue.get(queue_id).await? else {
            return Ok(false);
        };
        match self.task_store.get_by_id(&row.task_id).await? {
            Some(task) => Ok(task.bot_id == bot_id),
            None => Ok(false),
        }
    }
}

#[async_trait]
impl ToolHandler for SchedulingTools {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "schedule_task".to_string(),
                    description: "Schedule a task for the current bot. Every schedule has that bot's id; there is no global or shared cron.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "trigger_type": { "type": "string", "enum": ["one_shot", "recurring"] },
                            "trigger_value": { "type": "string", "description": "ISO 8601 (one_shot) or cron (recurring): 5 fields 'min hour day month weekday' or 6 fields 'sec min hour day month weekday'. Evaluated in UTC. Stored as 6 fields (seconds added as 0)." },
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
                    description: "List active scheduled tasks for the current bot only."
                        .to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "cancel_scheduled_task".to_string(),
                    description: "Cancel an active scheduled task by its ID.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
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
                        "type": "object",
                        "properties": {
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
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" }
                        }, "required": ["task_id"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "task_reruns".to_string(),
                    description: "Inspect and resolve the dead-letter queue of scheduled tasks that died on a transient provider failure or hit their iteration cap. `list` shows what is waiting; `retry` re-arms one for another attempt; `cancel` gives up on one.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["list", "retry", "cancel"],
                                "description": "list active queue rows, or resolve one by id"
                            },
                            "queue_id": {
                                "type": "string",
                                "description": "Queue row id (required for retry/cancel)"
                            }
                        },
                        "required": ["action"]
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        let ctx = self.telegram_scope(ctx);
        match name {
            "schedule_task" => {
                let trigger_type = args["trigger_type"]
                    .as_str()
                    .context("Missing 'trigger_type'")?
                    .to_string();
                let mut trigger_value = args["trigger_value"]
                    .as_str()
                    .context("Missing 'trigger_value'")?
                    .to_string();
                let prompt_text = args["prompt"]
                    .as_str()
                    .context("Missing 'prompt'")?
                    .to_string();
                let description = args["description"]
                    .as_str()
                    .context("Missing 'description'")?
                    .to_string();

                use crate::agent::normalize_cron_expr;
                use crate::agent::parse_one_shot_delay;

                if trigger_type == "one_shot" {
                    parse_one_shot_delay(&trigger_value)
                        .map_err(|e| anyhow::anyhow!("Invalid trigger: {e}"))?;
                } else if trigger_type == "recurring" {
                    trigger_value = normalize_cron_expr(&trigger_value)
                        .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;
                } else {
                    anyhow::bail!(
                        "Unknown trigger_type '{trigger_type}'. Use 'one_shot' or 'recurring'."
                    );
                }

                let task_id = Uuid::new_v4().to_string();
                let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
                let bot_id = ctx.bot_id.trim().to_string();
                if bot_id.is_empty() {
                    anyhow::bail!("schedule_task requires the current bot_id");
                }
                let task = ScheduledTask {
                    id: task_id.clone(),
                    scheduler_job_id: None,
                    user_id: ctx.user_id.clone(),
                    chat_id: ctx.chat_id.clone(),
                    platform: "telegram".to_string(),
                    bot_id,
                    trigger_type: trigger_type.clone(),
                    trigger_value: trigger_value.clone(),
                    prompt: prompt_text.clone(),
                    description: description.clone(),
                    status: "active".to_string(),
                    created_at: now.clone(),
                    // Issue #111 (Part A): the true next fire instant is not
                    // known until the task is armed — `arm_task` computes and
                    // persists it. Writing the raw cron expression here (as the
                    // tool used to) made `next_run_at` lie for recurring tasks.
                    next_run_at: None,
                    deleted_at: None,
                };
                if let Err(e) = self.task_store.create(&task).await {
                    return Ok(format!("Failed to save task: {}", e));
                }

                // Route through the shared arm path so the returned job id is
                // persisted onto the row (issue #109, Bug 2). Without this a
                // recurring task could neither be disarmed nor safely re-armed.
                match self.ops.arm_task(&task).await {
                    Ok(_job_id) => {
                        // Bookkeeping (issue #111, Part A): recompute the real
                        // next fire instant now the task is armed. Idempotent
                        // with `Agent::arm_task`'s own refresh; here it also
                        // makes the value observable in tests that drive the
                        // tool with a fake `SchedulingOps`.
                        let _ = self.task_store.refresh_next_run_at(&task_id).await;
                        Ok(format!(
                            "Task scheduled! ID: {} — {} ({})",
                            task_id, description, trigger_value
                        ))
                    }
                    Err(e) => Ok(format!("Failed to register task with scheduler: {}", e)),
                }
            }
            "list_scheduled_tasks" => {
                // Current bot only. Another bot's crons are not listed.
                match self.task_store.list_active_for_bot(&ctx.bot_id).await {
                    Ok(tasks) if tasks.is_empty() => Ok("No active scheduled tasks.".to_string()),
                    Ok(tasks) => {
                        let tasks: Vec<ScheduledTask> = tasks;
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
                // Disarm the *live* job first (issue #109, Bug 2), then flip
                // the DB status. Before the fix this only set status, so a
                // "cancelled" recurring task kept firing forever.
                match self.task_store.get_by_id(task_id).await {
                    Ok(Some(task)) => {
                        if task.bot_id != ctx.bot_id {
                            return Ok(format!("Task not found: {task_id}"));
                        }
                        self.ops.disarm_task(&task).await;
                    }
                    Ok(None) => {}
                    Err(e) => return Ok(format!("Failed to look up task: {}", e)),
                }
                match self.task_store.set_status(task_id, "cancelled").await {
                    Ok(()) => Ok(format!("Cancelled task {task_id}")),
                    Err(e) => Ok(format!("Failed to cancel task: {}", e)),
                }
            }
            "get_scheduled_task_history" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
                match self.task_store.get_by_id(task_id).await {
                    Ok(Some(task)) if task.bot_id == ctx.bot_id => {}
                    Ok(_) => return Ok(format!("Task not found: {task_id}")),
                    Err(e) => return Ok(format!("Failed to get history: {}", e)),
                }
                let runs: Vec<crate::scheduler::reminders::ScheduledTaskRun> =
                    match self.task_store.get_task_runs(task_id, 50).await {
                        Ok(r) => r,
                        Err(e) => return Ok(format!("Failed to get history: {}", e)),
                    };
                if runs.is_empty() {
                    Ok("No history for this task.".to_string())
                } else {
                    let lines: Vec<String> = runs
                        .iter()
                        .map(|r| {
                            let response_preview = r
                                .response
                                .as_deref()
                                .unwrap_or("")
                                .chars()
                                .take(100)
                                .collect::<String>();
                            format!("[{}] {} — {}", r.run_at, r.status, response_preview)
                        })
                        .collect();
                    Ok(lines.join("\n"))
                }
            }
            "rerun_scheduled_task" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
                let task = match self.task_store.get_by_id(task_id).await {
                    Ok(Some(t)) if t.bot_id == ctx.bot_id => t,
                    Ok(_) => return Ok(format!("Task not found: {task_id}")),
                    Err(e) => return Ok(format!("Task not found: {}", e)),
                };
                let fire = crate::agent::Agent::build_fire_closure(
                    self.job_tx.clone(),
                    Arc::clone(&self.bot),
                    Arc::clone(&self.telegram_bots),
                    self.task_store.clone(),
                    &task,
                );
                match self
                    .scheduler
                    .add_one_shot_job(std::time::Duration::from_secs(1), &task.description, fire)
                    .await
                {
                    Ok(_) => Ok(format!("Re-run scheduled for task {task_id}")),
                    Err(e) => Ok(format!("Failed to re-run task: {}", e)),
                }
            }
            "task_reruns" => {
                let action = args["action"].as_str().context("Missing 'action'")?;
                match action {
                    "list" => {
                        let rows = self.rerun_queue.list_active().await?;
                        let mut rows = rows;
                        let mut owned = Vec::new();
                        for row in rows.drain(..) {
                            match self.task_store.get_by_id(&row.task_id).await {
                                Ok(Some(task)) if task.bot_id == ctx.bot_id => owned.push(row),
                                _ => {}
                            }
                        }
                        let rows = owned;
                        if rows.is_empty() {
                            return Ok("No scheduled tasks are waiting on you.".to_string());
                        }
                        let lines: Vec<String> = rows
                            .iter()
                            .map(|r| {
                                format!(
                                    "- `{}` · task {} · {} · attempt(s) {} · next {} · {}",
                                    r.id,
                                    r.task_id,
                                    r.state.as_str(),
                                    r.attempts,
                                    r.next_eligible_at,
                                    r.fail_reason.chars().take(120).collect::<String>()
                                )
                            })
                            .collect();
                        Ok(format!(
                            "{} task(s) in the dead-letter queue:\n{}",
                            rows.len(),
                            lines.join("\n")
                        ))
                    }
                    "retry" => {
                        let id = args["queue_id"]
                            .as_str()
                            .context("Missing 'queue_id' for retry")?;
                        if !self.rerun_owned_by(id, &ctx.bot_id).await? {
                            return Ok(format!(
                                "No actionable queue row `{id}` (already done, or never existed)."
                            ));
                        }
                        if self.rerun_queue.retry(id).await? {
                            Ok(format!(
                                "Queued `{id}` for another attempt — the watchdog will pick it up within the hour."
                            ))
                        } else {
                            Ok(format!(
                                "No actionable queue row `{id}` (already done, or never existed)."
                            ))
                        }
                    }
                    "cancel" => {
                        let id = args["queue_id"]
                            .as_str()
                            .context("Missing 'queue_id' for cancel")?;
                        if !self.rerun_owned_by(id, &ctx.bot_id).await? {
                            return Ok(format!(
                                "No actionable queue row `{id}` (already done, or never existed)."
                            ));
                        }
                        if self.rerun_queue.cancel(id).await? {
                            Ok(format!("Cancelled `{id}` — it will not run again."))
                        } else {
                            Ok(format!(
                                "No cancellable queue row `{id}` (already done, or never existed)."
                            ))
                        }
                    }
                    other => Ok(format!(
                        "Unknown action '{other}'. Use list, retry, or cancel."
                    )),
                }
            }
            _ => anyhow::bail!("SchedulingTools: unknown tool {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel_registry::CancelRegistry;
    use crate::memory::MemoryStore;
    use crate::platform::sender::MessageFormat;
    use crate::platform::sender::PlatformSender;
    use crate::scheduler::reminders::ScheduledTaskStore;
    use crate::scheduler::schedule::SchedulingOps;
    use crate::tool_registry::ToolUiMode;
    use anyhow::Result;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;
    use uuid::Uuid;

    struct NopSender;

    #[async_trait]
    impl PlatformSender for NopSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> Result<crate::platform::sender::PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &crate::platform::sender::PlatformMessageId,
            _text: &str,
        ) -> Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _chat_id: &str,
            _message_id: &crate::platform::sender::PlatformMessageId,
        ) -> Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> Result<()> {
            Ok(())
        }
    }

    /// Records arm/disarm calls so a test can observe the tool's intent
    /// without a live `Agent`, and mimics `Agent::arm_task`'s contract of
    /// persisting the job id back onto the row.
    #[derive(Default)]
    struct FakeScheduling {
        armed: AsyncMutex<Vec<Uuid>>,
        disarmed: AsyncMutex<Vec<Uuid>>,
        store: Option<ScheduledTaskStore>,
    }

    #[async_trait]
    impl SchedulingOps for FakeScheduling {
        async fn arm_task(&self, task: &ScheduledTask) -> Result<Uuid> {
            let id = Uuid::new_v4();
            self.armed.lock().await.push(id);
            if let Some(store) = &self.store {
                store
                    .update_scheduler_job_id(&task.id, &id.to_string())
                    .await?;
            }
            Ok(id)
        }
        async fn disarm_task(&self, task: &ScheduledTask) -> bool {
            if let Ok(job) = Uuid::parse_str(task.scheduler_job_id.as_deref().unwrap_or("")) {
                self.disarmed.lock().await.push(job);
            }
            true
        }
    }

    fn make_ctx(user_id: &str) -> ToolContext {
        ToolContext {
            sandbox_dir: std::path::PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(NopSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: user_id.to_string(),
            chat_id: "555".to_string(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            tool_ui_mode: ToolUiMode::Minimal,
        }
    }

    async fn build_tools() -> (
        SchedulingTools,
        ScheduledTaskStore,
        Arc<FakeScheduling>,
        RerunQueue,
    ) {
        let memory = MemoryStore::open_in_memory().unwrap();
        let store = ScheduledTaskStore::new(memory.connection());
        let rerun_queue = RerunQueue::new(memory.connection());
        let scheduler = Arc::new(Scheduler::new().await.unwrap());
        let (job_tx, _job_rx) = tokio::sync::mpsc::unbounded_channel();
        let bot = Arc::new(teloxide::Bot::new("TEST_TOKEN"));
        let mut bots = std::collections::HashMap::new();
        bots.insert(
            crate::platform::DEFAULT_BOT_ID.to_string(),
            Arc::clone(&bot),
        );
        let telegram_bots = Arc::new(std::sync::RwLock::new(bots));
        let fake = Arc::new(FakeScheduling {
            store: Some(store.clone()),
            ..Default::default()
        });
        let tools = SchedulingTools::new(
            store.clone(),
            scheduler,
            fake.clone() as Arc<dyn SchedulingOps>,
            job_tx,
            bot,
            telegram_bots,
            rerun_queue.clone(),
        );
        (tools, store, fake, rerun_queue)
    }

    /// Regression (#163): a portal web-chat turn (chat_id "web") must store
    /// the reminder on the shim bot's owner DM, and its list/cancel must see
    /// it. Telegram turns keep their own bot/chat.
    #[tokio::test]
    async fn web_turn_schedules_on_owner_dm_and_can_list_and_cancel() {
        let (tools, store, _fake, _queue) = build_tools().await;
        let tools = tools.with_telegram_owner("main".into(), "42".into());
        let web = || {
            let mut c = make_ctx("web");
            c.chat_id = "web".into();
            c
        };
        let args = json!({
            "trigger_type": "one_shot",
            "trigger_value": "2099-01-01T00:00:00Z",
            "prompt": "ping",
            "description": "web ping"
        });

        tools
            .execute("schedule_task", args.clone(), web())
            .await
            .unwrap();
        let task = store.list_all_active().await.unwrap().pop().unwrap();
        assert_eq!(
            (
                task.bot_id.as_str(),
                task.user_id.as_str(),
                task.chat_id.as_str()
            ),
            ("main", "42", "42")
        );

        let listed = tools
            .execute("list_scheduled_tasks", json!({}), web())
            .await
            .unwrap();
        assert!(listed.contains(&task.id), "web list must see it: {listed}");
        let out = tools
            .execute("cancel_scheduled_task", json!({"task_id": task.id}), web())
            .await
            .unwrap();
        assert!(
            out.starts_with("Cancelled"),
            "web cancel must find it: {out}"
        );

        tools
            .execute("schedule_task", args, make_ctx("7"))
            .await
            .unwrap();
        let tg = store.list_all_active().await.unwrap().pop().unwrap();
        assert_eq!(
            (tg.bot_id.as_str(), tg.user_id.as_str(), tg.chat_id.as_str()),
            (crate::platform::DEFAULT_BOT_ID, "7", "555")
        );
    }

    /// ADR-0021: a 5-field cron is padded with seconds `0`, stored, and shown.
    #[tokio::test]
    async fn schedule_task_pads_five_field_cron() {
        let (tools, store, _fake, _queue) = build_tools().await;
        let out = tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 9 * * *",
                    "prompt": "say hi",
                    "description": "daily hi"
                }),
                make_ctx("80180742"),
            )
            .await
            .unwrap();
        assert!(out.contains("(0 0 9 * * *)"), "unexpected output: {out}");
        let tasks = store.list_all_active().await.unwrap();
        assert_eq!(tasks[0].trigger_value, "0 0 9 * * *");
    }

    /// Regression (issue #109, Bug 2): `schedule_task` must persist the live
    /// scheduler job id — before the fix it discarded it and the row stayed
    /// `scheduler_job_id = NULL`, so a cancel could never disarm the job.
    #[tokio::test]
    async fn schedule_task_persists_scheduler_job_id() {
        let (tools, store, fake, _queue) = build_tools().await;
        let ctx = make_ctx("80180742");

        let out = tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "say hi",
                    "description": "daily hi"
                }),
                ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("Task scheduled!"), "unexpected output: {out}");

        assert_eq!(
            fake.armed.lock().await.len(),
            1,
            "arm must be routed via ops"
        );
        let tasks = store.list_all_active().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(
            tasks[0].scheduler_job_id.is_some(),
            "the arm path must persist the live job id (was NULL before the fix)"
        );
    }

    /// Regression (issue #111, Part A): creating a recurring task through the
    /// tool must store a concrete *timestamp* as `next_run_at`, never the raw
    /// cron expression (which is what the tool used to write).
    #[tokio::test]
    async fn schedule_task_stores_real_next_run_timestamp() {
        let (tools, store, _fake, _queue) = build_tools().await;

        tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "say hi",
                    "description": "daily hi"
                }),
                make_ctx("80180742"),
            )
            .await
            .unwrap();

        let task = store
            .list_all_active()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let val = task
            .next_run_at
            .expect("next_run_at must be populated after arm");
        assert_ne!(val, "0 0 4 * * *", "raw cron string must not be stored");
        assert!(
            chrono::NaiveDateTime::parse_from_str(&val, "%Y-%m-%dT%H:%M:%S").is_ok(),
            "next_run_at must be an ISO timestamp, got: {val}"
        );
    }

    /// Regression (issue #109, Bug 1): a scheduled task's own run context
    /// (`user_id = "{owner}:{task_id}"`) must list its task, not answer
    /// "No active scheduled tasks.".
    #[tokio::test]
    async fn list_from_scheduled_run_context_is_not_empty() {
        let (tools, store, _fake, _queue) = build_tools().await;

        // Create the task under the owner, then list as the composite run id.
        tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "say hi",
                    "description": "daily hi"
                }),
                make_ctx("80180742"),
            )
            .await
            .unwrap();
        let task_id = store.list_all_active().await.unwrap()[0].id.clone();

        let composite = format!("80180742:{task_id}");
        let out = tools
            .execute("list_scheduled_tasks", json!({}), make_ctx(&composite))
            .await
            .unwrap();
        assert!(
            out.contains(&task_id),
            "a scheduled run must see its own task; got: {out}"
        );
        assert!(!out.contains("No active scheduled tasks."));
    }

    /// Regression (issue #109, Bug 2): cancelling must route a disarm so the
    /// live job is actually removed (not just the DB status flipped).
    #[tokio::test]
    async fn cancel_scheduled_task_disarms_live_job() {
        let (tools, store, fake, _queue) = build_tools().await;
        let ctx = make_ctx("80180742");
        tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "say hi",
                    "description": "daily hi"
                }),
                ctx,
            )
            .await
            .unwrap();
        let task = store
            .list_all_active()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let armed_job = task.scheduler_job_id.clone().unwrap();

        let out = tools
            .execute(
                "cancel_scheduled_task",
                json!({ "task_id": task.id }),
                make_ctx("80180742"),
            )
            .await
            .unwrap();
        assert!(out.contains("Cancelled task"), "got: {out}");

        let disarmed = fake.disarmed.lock().await.clone();
        assert_eq!(
            disarmed,
            vec![Uuid::parse_str(&armed_job).unwrap()],
            "the exact live job id must be disarmed"
        );
        // Row is now cancelled and drops out of active listings.
        assert!(store.list_all_active().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_scheduled_tasks_is_scoped_to_the_current_bot() {
        let (tools, _store, _fake, _queue) = build_tools().await;
        tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "alpha only",
                    "description": "alpha"
                }),
                make_ctx("owner"),
            )
            .await
            .unwrap();
        let other = || {
            let mut ctx = make_ctx("owner");
            ctx.bot_id = "researcher".into();
            ctx
        };
        let hidden = tools
            .execute("list_scheduled_tasks", json!({}), other())
            .await
            .unwrap();
        assert!(
            hidden.contains("No active scheduled tasks."),
            "bot B must not see bot A's cron: {hidden}"
        );
        let mine = tools
            .execute("list_scheduled_tasks", json!({}), make_ctx("owner"))
            .await
            .unwrap();
        assert!(
            mine.contains("alpha"),
            "current bot must see its cron: {mine}"
        );
    }

    /// A bot that does not own the row cannot cancel it, read its history,
    /// rerun it, or retry/cancel its dead-letter row.
    #[tokio::test]
    async fn other_bot_cannot_cancel_history_rerun_or_dead_letter() {
        let (tools, store, fake, queue) = build_tools().await;
        tools
            .execute(
                "schedule_task",
                json!({
                    "trigger_type": "recurring",
                    "trigger_value": "0 0 4 * * *",
                    "prompt": "owner only",
                    "description": "owned"
                }),
                make_ctx("owner"),
            )
            .await
            .unwrap();
        let task = store
            .list_active_for_bot(crate::platform::DEFAULT_BOT_ID)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        store
            .insert_run(
                "run-owned",
                &task.id,
                "2026-10-03T00:00:00",
                Some("owner-only-history"),
                None,
                "completed",
            )
            .await
            .unwrap();
        let queue_id = queue
            .enqueue_manual(&task.id, "run-owned", "stalled")
            .await
            .unwrap();

        let other = || {
            let mut ctx = make_ctx("owner");
            ctx.bot_id = "researcher".into();
            ctx
        };

        let cancel = tools
            .execute(
                "cancel_scheduled_task",
                json!({ "task_id": task.id }),
                other(),
            )
            .await
            .unwrap();
        assert!(
            cancel.contains("Task not found"),
            "other bot must not cancel: {cancel}"
        );
        assert!(!cancel.contains("Cancelled task"));
        assert!(
            fake.disarmed.lock().await.is_empty(),
            "other bot must not disarm the live job"
        );
        assert_eq!(
            store.get_by_id(&task.id).await.unwrap().unwrap().status,
            "active"
        );

        let history = tools
            .execute(
                "get_scheduled_task_history",
                json!({ "task_id": task.id }),
                other(),
            )
            .await
            .unwrap();
        assert!(
            history.contains("Task not found"),
            "other bot must not read history: {history}"
        );
        assert!(!history.contains("owner-only-history"));

        let rerun = tools
            .execute(
                "rerun_scheduled_task",
                json!({ "task_id": task.id }),
                other(),
            )
            .await
            .unwrap();
        assert!(
            rerun.contains("Task not found"),
            "other bot must not rerun: {rerun}"
        );
        assert!(!rerun.contains("Re-run scheduled"));

        let retry = tools
            .execute(
                "task_reruns",
                json!({ "action": "retry", "queue_id": queue_id }),
                other(),
            )
            .await
            .unwrap();
        assert!(
            retry.contains("No actionable queue row"),
            "other bot must not retry the dead-letter row: {retry}"
        );
        assert!(!retry.contains("Queued"));

        let qcancel = tools
            .execute(
                "task_reruns",
                json!({ "action": "cancel", "queue_id": queue_id }),
                other(),
            )
            .await
            .unwrap();
        assert!(
            qcancel.contains("No actionable queue row"),
            "other bot must not cancel the dead-letter row: {qcancel}"
        );
        assert!(!qcancel.contains("will not run again"));

        let row = queue.get(&queue_id).await.unwrap().unwrap();
        assert_eq!(
            row.state,
            crate::scheduler::reruns::RerunState::AwaitingUser
        );

        let own_history = tools
            .execute(
                "get_scheduled_task_history",
                json!({ "task_id": task.id }),
                make_ctx("owner"),
            )
            .await
            .unwrap();
        assert!(
            own_history.contains("owner-only-history"),
            "owner must still see the run: {own_history}"
        );
    }
}
