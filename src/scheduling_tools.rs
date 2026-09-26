use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::ScheduledJobRequest;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::{reminders::ScheduledTask, reruns::RerunQueue, Scheduler};
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};
use teloxide::prelude::Bot;
use uuid::Uuid;

pub struct SchedulingTools {
    task_store: ScheduledTaskStore,
    scheduler: Arc<Scheduler>,
    job_tx: UnboundedSender<ScheduledJobRequest>,
    bot: Arc<Bot>,
    rerun_queue: RerunQueue,
}

impl SchedulingTools {
    pub fn new(
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        job_tx: UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
        rerun_queue: RerunQueue,
    ) -> Self {
        Self {
            task_store,
            scheduler,
            job_tx,
            bot,
            rerun_queue,
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
                    description: "List all active scheduled tasks for the current user."
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
        match name {
            "schedule_task" => {
                let trigger_type = args["trigger_type"]
                    .as_str()
                    .context("Missing 'trigger_type'")?
                    .to_string();
                let trigger_value = args["trigger_value"]
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

                use crate::agent::parse_one_shot_delay;
                use crate::agent::validate_cron_expr;

                let delay = if trigger_type == "one_shot" {
                    Some(
                        parse_one_shot_delay(&trigger_value)
                            .map_err(|e| anyhow::anyhow!("Invalid trigger: {e}"))?,
                    )
                } else if trigger_type == "recurring" {
                    validate_cron_expr(&trigger_value)
                        .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;
                    None
                } else {
                    anyhow::bail!(
                        "Unknown trigger_type '{trigger_type}'. Use 'one_shot' or 'recurring'."
                    );
                };

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
                    deleted_at: None,
                };
                if let Err(e) = self.task_store.create(&task).await {
                    return Ok(format!("Failed to save task: {}", e));
                }

                let fire = crate::agent::Agent::build_fire_closure(
                    self.job_tx.clone(),
                    Arc::clone(&self.bot),
                    self.task_store.clone(),
                    &task,
                );

                let sched_result = if let Some(d) = delay {
                    self.scheduler.add_one_shot_job(d, &description, fire).await
                } else {
                    self.scheduler
                        .add_cron_job(&trigger_value, &description, fire)
                        .await
                };

                match sched_result {
                    Ok(_sched_id) => Ok(format!(
                        "Task scheduled! ID: {} — {} ({})",
                        task_id, description, trigger_value
                    )),
                    Err(e) => Ok(format!("Failed to register task with scheduler: {}", e)),
                }
            }
            "list_scheduled_tasks" => {
                match self.task_store.list_active_for_user(&ctx.user_id).await {
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
                match self.task_store.set_status(task_id, "cancelled").await {
                    Ok(()) => Ok(format!("Cancelled task {task_id}")),
                    Err(e) => Ok(format!("Failed to cancel task: {}", e)),
                }
            }
            "get_scheduled_task_history" => {
                let task_id = args["task_id"].as_str().context("Missing 'task_id'")?;
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
                    Ok(Some(t)) => t,
                    Ok(None) => return Ok(format!("Task not found: {task_id}")),
                    Err(e) => return Ok(format!("Task not found: {}", e)),
                };
                let fire = crate::agent::Agent::build_fire_closure(
                    self.job_tx.clone(),
                    Arc::clone(&self.bot),
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
