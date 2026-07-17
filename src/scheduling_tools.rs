use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::ScheduledJobRequest;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::{reminders::ScheduledTask, Scheduler};
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
        Self {
            task_store,
            scheduler,
            job_tx,
            bot,
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
                let task_id_for_sched = task_id.clone();
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

                let job_tx = self.job_tx.clone();
                let bot_clone = self.bot.clone();
                let store_clone = self.task_store.clone();
                let uid = ctx.user_id.clone();
                let cid = ctx.chat_id.clone();
                let prompt_cap = prompt_text.clone();
                let is_recurring = trigger_type == "recurring";
                let tv = trigger_value.clone();

                let fire = move || {
                    let tx = job_tx.clone();
                    let bot = bot_clone.clone();
                    let store = store_clone.clone();
                    let uid = uid.clone();
                    let cid = cid.clone();
                    let prompt = prompt_cap.clone();
                    let tid = task_id_for_sched.clone();
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
                            task_store: store,
                        };
                        let _ = tx.send(req);
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };

                let sched_result = if let Some(d) = delay {
                    self.scheduler.add_one_shot_job(d, &description, fire).await
                } else {
                    self.scheduler.add_cron_job(&tv, &description, fire).await
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
                            incoming,
                            bot,
                            task_id: tid,
                            is_recurring: false,
                            task_store: store,
                        };
                        let _ = tx.send(req);
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };
                match self
                    .scheduler
                    .add_one_shot_job(std::time::Duration::from_secs(1), &task.description, fire)
                    .await
                {
                    Ok(_) => Ok(format!("Re-run scheduled for task {task_id}")),
                    Err(e) => Ok(format!("Failed to re-run task: {}", e)),
                }
            }
            _ => anyhow::bail!("SchedulingTools: unknown tool {name}"),
        }
    }
}
