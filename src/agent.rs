use anyhow::Result;
use std::sync::{Arc, Weak};
use tracing::info;

use teloxide::Bot;

use crate::config::Config;
use crate::llm::{ChatMessage, FunctionDefinition, LlmClient, ToolDefinition};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::platform::IncomingMessage;
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::Scheduler;
use crate::skills::SkillRegistry;
use crate::tools;

/// Events emitted by the agent loop so callers can show live progress.
pub enum AgentEvent {
    ToolStarted { name: String },
}

/// Convenience alias — the sending half of an agent event channel.
pub type EventSender = tokio::sync::mpsc::UnboundedSender<AgentEvent>;

/// A request dispatched from a fire closure to the background job runner.
pub struct ScheduledJobRequest {
    pub incoming: IncomingMessage,
    pub bot: Arc<Bot>,
    pub task_id: String,
    pub is_recurring: bool,
    pub task_store: ScheduledTaskStore,
}

/// The core agent that processes messages through LLM + tools.
/// Platform-agnostic — receives IncomingMessage, returns response text.
pub struct Agent {
    pub llm: LlmClient,
    pub config: Config,
    pub mcp: McpManager,
    pub memory: MemoryStore,
    pub skills: tokio::sync::RwLock<SkillRegistry>,
    // Fields used by scheduling / job closures
    pub task_store: ScheduledTaskStore,
    pub scheduler: Arc<Scheduler>,
    pub bot: Arc<Bot>,
    #[allow(dead_code)]
    pub self_weak: Weak<Agent>,
    /// Sender for dispatching scheduled job work to the background runner.
    pub job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        mcp: McpManager,
        memory: MemoryStore,
        skills: SkillRegistry,
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        bot: Arc<Bot>,
        self_weak: Weak<Agent>,
        job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
    ) -> Self {
        let llm = LlmClient::new(config.openrouter.clone());
        Self {
            llm,
            config,
            mcp,
            memory,
            skills: tokio::sync::RwLock::new(skills),
            task_store,
            scheduler,
            bot,
            self_weak,
            job_tx,
        }
    }

    /// Build the system prompt, incorporating loaded skills
    async fn build_system_prompt(&self) -> String {
        let mut prompt = self.config.openrouter.system_prompt.clone();

        let skills = self.skills.read().await;
        let skill_context = skills.build_context();
        if !skill_context.is_empty() {
            prompt.push_str("\n\n# Available Skills\n\n");
            prompt.push_str(&skill_context);
        }
        drop(skills); // release read lock before further work

        // Append current timestamp and optional location
        let now = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        prompt.push_str(&format!("\n\nCurrent date and time: {}", now));
        if let Some(loc) = self.config.user_location() {
            prompt.push_str(&format!("\nUser location: {}", loc));
        }

        prompt
    }

    /// Process an incoming message and return the response text
    pub async fn process_message(
        &self,
        incoming: &IncomingMessage,
        events: Option<&EventSender>,
    ) -> Result<String> {
        let platform = &incoming.platform;
        let user_id = &incoming.user_id;
        let chat_id = &incoming.chat_id;

        // Get or create persistent conversation
        let conversation_id = self
            .memory
            .get_or_create_conversation(platform, user_id)
            .await?;

        // Load existing messages from memory
        let mut messages = self.memory.load_messages(&conversation_id).await?;

        // Always build the system prompt from the live registry.
        // For new conversations: save to DB and push.
        // For existing conversations: refresh messages[0] in-memory only
        //   (DB keeps the historical system message intact).
        let current_system_prompt = self.build_system_prompt().await;
        if messages.is_empty() {
            let system_msg = ChatMessage {
                role: "system".to_string(),
                content: Some(current_system_prompt),
                tool_calls: None,
                tool_call_id: None,
            };
            self.memory
                .save_message(&conversation_id, &system_msg)
                .await?;
            messages.push(system_msg);
        } else {
            // Refresh in-memory: new skills loaded by reload_skills take effect
            // on the very next message without restarting the bot.
            // Find the system message by role (defensive: don't assume messages[0] is system).
            if let Some(system_msg) = messages.iter_mut().find(|m| m.role == "system") {
                system_msg.content = Some(current_system_prompt);
            }
        }

        // Add user message
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(incoming.text.clone()),
            tool_calls: None,
            tool_call_id: None,
        };
        self.memory
            .save_message(&conversation_id, &user_msg)
            .await?;
        messages.push(user_msg);

        // Gather all tool definitions
        let mut all_tools: Vec<ToolDefinition> = tools::builtin_tool_definitions();
        all_tools.extend(self.mcp.tool_definitions());
        all_tools.extend(self.memory_tool_definitions());
        all_tools.extend(self.scheduling_tool_definitions());
        all_tools.extend(self.skill_tool_definitions());

        // Agentic loop — keep calling LLM until we get a non-tool response
        let max_iterations = self.config.max_iterations();
        let context_window = self.config.context_window();
        for iteration in 0..max_iterations {
            let windowed = apply_context_window(&messages, context_window);
            let response = self.llm.chat(&windowed, &all_tools).await?;

            if let Some(tool_calls) = &response.tool_calls {
                if !tool_calls.is_empty() {
                    info!(
                        "LLM requested {} tool call(s) (iteration {})",
                        tool_calls.len(),
                        iteration
                    );

                    // Save assistant message with tool calls
                    self.memory
                        .save_message(&conversation_id, &response)
                        .await?;
                    messages.push(response.clone());

                    // Notify listener about all tools about to run in this batch
                    if let Some(tx) = events {
                        for tc in tool_calls.iter() {
                            if tx
                                .send(AgentEvent::ToolStarted {
                                    name: tc.function.name.clone(),
                                })
                                .is_err()
                            {
                                tracing::debug!(
                                    "AgentEvent receiver dropped; tool hint not delivered"
                                );
                                break;
                            }
                        }
                    }

                    // Execute tool calls in parallel — each tool is independent
                    let tool_futures = tool_calls.iter().map(|tc| {
                        let id = tc.id.clone();
                        let name = tc.function.name.clone();
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        async move {
                            let result =
                                self.execute_tool(&name, &arguments, user_id, chat_id).await;
                            (id, name, result)
                        }
                    });
                    let tool_results = futures::future::join_all(tool_futures).await;

                    // Save results in order (preserves message ordering in DB)
                    for (tool_call_id, tool_name, tool_result) in tool_results {
                        info!(
                            "Tool '{}' result length: {} chars",
                            tool_name,
                            tool_result.len()
                        );
                        let tool_msg = ChatMessage {
                            role: "tool".to_string(),
                            content: Some(tool_result),
                            tool_calls: None,
                            tool_call_id: Some(tool_call_id),
                        };
                        self.memory
                            .save_message(&conversation_id, &tool_msg)
                            .await?;
                        messages.push(tool_msg);
                    }

                    continue;
                }
            }

            // Final response — no tool calls
            let content = response.content.clone().unwrap_or_default();
            self.memory
                .save_message(&conversation_id, &response)
                .await?;

            return Ok(content);
        }

        Ok("I've reached the maximum number of tool call iterations. Please try rephrasing your request.".to_string())
    }

    /// Re-register all active scheduled tasks from the DB into the scheduler.
    /// Called once at startup after the agent is constructed.
    pub async fn restore_scheduled_tasks(&self) {
        let tasks = match self.task_store.list_all_active().await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Failed to load scheduled tasks for restore: {}", e);
                return;
            }
        };

        let count = tasks.len();
        for task in tasks {
            // Build the same fire closure as in schedule_task handler
            let job_tx = self.job_tx.clone();
            let bot_clone = Arc::clone(&self.bot);
            let tid = task.id.clone();
            let uid = task.user_id.clone();
            let cid = task.chat_id.clone();
            let prompt_cap = task.prompt.clone();
            let is_recurring = task.trigger_type == "recurring";
            let store_clone = self.task_store.clone();

            let fire = move || {
                let tx = job_tx.clone();
                let bot = bot_clone.clone();
                let store = store_clone.clone();
                let tid = tid.clone();
                let uid = uid.clone();
                let cid = cid.clone();
                let prompt = prompt_cap.clone();
                let recurring = is_recurring;
                Box::pin(async move {
                    let incoming = crate::platform::IncomingMessage {
                        platform: "telegram".to_string(),
                        user_id: uid,
                        chat_id: cid,
                        user_name: String::new(),
                        text: prompt,
                    };
                    let req = ScheduledJobRequest {
                        incoming,
                        bot,
                        task_id: tid,
                        is_recurring: recurring,
                        task_store: store,
                    };
                    if let Err(e) = tx.send(req) {
                        tracing::error!("Failed to dispatch restored scheduled job: {}", e);
                    }
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            };

            // Register with the right scheduler method based on trigger_type
            let sched_result = if task.trigger_type == "one_shot" {
                match parse_one_shot_delay(&task.trigger_value) {
                    Ok(delay) => {
                        self.scheduler
                            .add_one_shot_job(delay, &task.description, fire)
                            .await
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Skipping restore of one-shot task {} (trigger has passed or invalid: {})",
                            task.id,
                            e
                        );
                        // Mark as completed since its time has passed
                        let _ = self.task_store.set_status(&task.id, "completed").await;
                        continue;
                    }
                }
            } else {
                self.scheduler
                    .add_cron_job(&task.trigger_value, &task.description, fire)
                    .await
            };

            match sched_result {
                Ok(sched_id) => {
                    if let Err(e) = self
                        .task_store
                        .update_scheduler_job_id(&task.id, &sched_id.to_string())
                        .await
                    {
                        tracing::warn!(
                            "Failed to update scheduler_job_id for restored task {}: {}",
                            task.id,
                            e
                        );
                    }
                    tracing::info!(
                        "Restored scheduled task: {} ({})",
                        task.id,
                        task.description
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to restore scheduled task {} ({}): {}",
                        task.id,
                        task.description,
                        e
                    );
                }
            }
        }

        if count > 0 {
            tracing::info!("Restored {} scheduled task(s) from DB", count);
        }
    }

    /// Clear conversation history for a user
    pub async fn clear_conversation(&self, platform: &str, user_id: &str) -> Result<()> {
        self.memory.clear_conversation(platform, user_id).await
    }

    /// Get all tool definitions for display
    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut all_tools = tools::builtin_tool_definitions();
        all_tools.extend(self.mcp.tool_definitions());
        all_tools.extend(self.memory_tool_definitions());
        all_tools.extend(self.scheduling_tool_definitions());
        all_tools.extend(self.skill_tool_definitions());
        all_tools
    }

    /// Memory-related tool definitions exposed to the LLM
    fn memory_tool_definitions(&self) -> Vec<ToolDefinition> {
        use serde_json::json;

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

    /// Scheduling-related tool definitions exposed to the LLM
    fn scheduling_tool_definitions(&self) -> Vec<ToolDefinition> {
        use serde_json::json;

        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "schedule_task".to_string(),
                    description: concat!(
                        "Schedule a task to run at a future time. The prompt will be executed by the AI agent ",
                        "at the scheduled time (full agentic loop). ",
                        "For one_shot: trigger_value is ISO 8601 datetime e.g. '2026-03-05T12:00:00'. ",
                        "For recurring: trigger_value is a 6-field cron expression ",
                        "(sec min hour day month weekday) e.g. '0 0 9 * * MON' for every Monday at 9am.\n\n",
                        "TIME INFERENCE RULES — follow these strictly, do not ask unnecessary questions:\n",
                        "- The current date and time is in your system prompt. Always use it as the reference.\n",
                        "- Time only, no date (e.g. '5:20', '9:30am'): assume TODAY. If the time is in the past today, use tomorrow.\n",
                        "- The user's AM/PM intent is clear from context: if it's currently 5:15pm and they say '5:20', ",
                        "that is obviously 5:20pm today — schedule it immediately without asking.\n",
                        "- '12:00' or 'noon' = 12:00pm. 'midnight' = 00:00.\n",
                        "- ONLY ask for AM/PM clarification when it is genuinely ambiguous: ",
                        "e.g. user says 'Friday 12:00' with no other context (could be noon or midnight).\n",
                        "- Day of week only (e.g. 'Friday'): assume the NEXT occurrence of that day.\n",
                        "- Never ask for information you can infer. Prefer acting over asking."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "trigger_type":  { "type": "string", "enum": ["one_shot", "recurring"] },
                            "trigger_value": { "type": "string", "description": "ISO 8601 datetime (one_shot) or 6-field cron expression (recurring)" },
                            "prompt":        { "type": "string", "description": "The message the agent will process at trigger time" },
                            "description":   { "type": "string", "description": "Human-readable label for this task" }
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
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string", "description": "The task ID from list_scheduled_tasks" }
                        },
                        "required": ["task_id"]
                    }),
                },
            },
        ]
    }

    /// Skill management tool definitions exposed to the LLM
    fn skill_tool_definitions(&self) -> Vec<ToolDefinition> {
        use serde_json::json;

        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "write_skill_file".to_string(),
                    description: concat!(
                        "Write a file into a skill directory under the configured skills folder. ",
                        "Use this to create SKILL.md and any supporting files (reference docs, templates, scripts). ",
                        "Call reload_skills after ALL files for the skill are written."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": {
                                "type": "string",
                                "description": "Skill directory name: lowercase letters, numbers, hyphens only, max 64 chars (e.g. 'creating-reports')"
                            },
                            "relative_path": {
                                "type": "string",
                                "description": "Path within the skill directory, e.g. 'SKILL.md', 'reference.md', 'scripts/helper.py'"
                            },
                            "content": {
                                "type": "string",
                                "description": "Full file content to write"
                            }
                        },
                        "required": ["skill_name", "relative_path", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "reload_skills".to_string(),
                    description: concat!(
                        "Reload all skills from the skills directory into memory. ",
                        "Call this after writing skill files to make the new skill immediately active ",
                        "without restarting the bot."
                    ).to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
        ]
    }

    /// Execute a tool call by routing to the right handler
    async fn execute_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        user_id: &str,
        chat_id: &str,
    ) -> String {
        match name {
            "remember" => {
                let category = arguments["category"].as_str().unwrap_or("general");
                let key = arguments["key"].as_str().unwrap_or("");
                let value = arguments["value"].as_str().unwrap_or("");
                match self.memory.remember(category, key, value, None).await {
                    Ok(()) => format!("Remembered: [{}] {} = {}", category, key, value),
                    Err(e) => format!("Failed to remember: {}", e),
                }
            }
            "recall" => {
                let category = arguments["category"].as_str().unwrap_or("general");
                let key = arguments["key"].as_str().unwrap_or("");
                match self.memory.recall(category, key).await {
                    Ok(Some(value)) => value,
                    Ok(None) => format!("No knowledge found for [{}] {}", category, key),
                    Err(e) => format!("Failed to recall: {}", e),
                }
            }
            "search_memory" => {
                let query = arguments["query"].as_str().unwrap_or("");
                let limit = arguments["limit"].as_u64().unwrap_or(5) as usize;

                // Compute the query embedding once and share it between both
                // searches to avoid two identical embedding API roundtrips.
                let shared_embedding = self.memory.embeddings.try_embed_one(query).await;

                // Run message and knowledge searches concurrently.
                // They contend on the DB mutex internally but the embedding
                // HTTP call (the slow part) is already done above.
                let (msgs_result, entries_result) = tokio::join!(
                    self.memory.search_messages_with_embedding(
                        query,
                        shared_embedding.clone(),
                        limit
                    ),
                    self.memory
                        .search_knowledge_with_embedding(query, shared_embedding, limit)
                );

                let mut results = Vec::new();

                // Search conversations (hybrid vector + FTS5)
                if let Ok(msgs) = msgs_result {
                    for msg in msgs {
                        if let Some(content) = &msg.content {
                            results.push(format!("[{}]: {}", msg.role, content));
                        }
                    }
                }

                // Search knowledge (hybrid vector + FTS5)
                if let Ok(entries) = entries_result {
                    for entry in entries {
                        results.push(format!(
                            "[knowledge:{}] {} = {}",
                            entry.category, entry.key, entry.value
                        ));
                    }
                }

                if results.is_empty() {
                    "No results found.".to_string()
                } else {
                    results.join("\n\n")
                }
            }
            "schedule_task" => {
                let trigger_type = match arguments["trigger_type"].as_str() {
                    Some(t) => t.to_string(),
                    None => return "Missing trigger_type".to_string(),
                };
                let trigger_value = match arguments["trigger_value"].as_str() {
                    Some(v) => v.to_string(),
                    None => return "Missing trigger_value".to_string(),
                };
                let prompt_text = match arguments["prompt"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing prompt".to_string(),
                };
                let description = match arguments["description"].as_str() {
                    Some(d) => d.to_string(),
                    None => return "Missing description".to_string(),
                };

                // Validate trigger and compute delay for one-shot
                let delay = if trigger_type == "one_shot" {
                    match parse_one_shot_delay(&trigger_value) {
                        Ok(d) => Some(d),
                        Err(e) => return format!("Invalid trigger: {}", e),
                    }
                } else if trigger_type == "recurring" {
                    if let Err(e) = validate_cron_expr(&trigger_value) {
                        return format!("Invalid cron expression: {}", e);
                    }
                    None
                } else {
                    return format!(
                        "Unknown trigger_type '{}'. Use 'one_shot' or 'recurring'.",
                        trigger_type
                    );
                };

                let next_run_at = trigger_value.clone();

                // Persist to DB
                let task_id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
                let task = crate::scheduler::reminders::ScheduledTask {
                    id: task_id.clone(),
                    scheduler_job_id: None,
                    user_id: user_id.to_string(),
                    chat_id: chat_id.to_string(),
                    platform: "telegram".to_string(),
                    trigger_type: trigger_type.clone(),
                    trigger_value: trigger_value.clone(),
                    prompt: prompt_text.clone(),
                    description: description.clone(),
                    status: "active".to_string(),
                    created_at: now,
                    next_run_at: Some(next_run_at),
                };
                if let Err(e) = self.task_store.create(&task).await {
                    return format!("Failed to save task: {}", e);
                }

                // Build closure captures — fire closure dispatches to background runner
                // via a channel so it can be `Send` without requiring process_message to be Send.
                let job_tx = self.job_tx.clone();
                let bot_clone = Arc::clone(&self.bot);
                let store_clone = self.task_store.clone();
                let tid = task_id.clone();
                let uid = user_id.to_string();
                let cid = chat_id.to_string();
                let prompt_cap = prompt_text.clone();
                let desc_cap = description.clone();
                let is_recurring = trigger_type == "recurring";
                let tv = trigger_value.clone();

                let fire = move || {
                    let tx = job_tx.clone();
                    let bot = bot_clone.clone();
                    let store = store_clone.clone();
                    let tid = tid.clone();
                    let uid = uid.clone();
                    let cid = cid.clone();
                    let prompt = prompt_cap.clone();
                    let recurring = is_recurring;
                    Box::pin(async move {
                        let incoming = crate::platform::IncomingMessage {
                            platform: "telegram".to_string(),
                            user_id: uid,
                            chat_id: cid,
                            user_name: String::new(),
                            text: prompt,
                        };
                        let req = ScheduledJobRequest {
                            incoming,
                            bot,
                            task_id: tid,
                            is_recurring: recurring,
                            task_store: store,
                        };
                        if let Err(e) = tx.send(req) {
                            tracing::error!("Failed to dispatch scheduled job: {}", e);
                        }
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                };

                // Register with scheduler
                let sched_result = if let Some(d) = delay {
                    self.scheduler.add_one_shot_job(d, &desc_cap, fire).await
                } else {
                    self.scheduler.add_cron_job(&tv, &desc_cap, fire).await
                };

                match sched_result {
                    Ok(sched_id) => {
                        if let Err(e) = self
                            .task_store
                            .update_scheduler_job_id(&task_id, &sched_id.to_string())
                            .await
                        {
                            tracing::warn!(
                                "Failed to persist scheduler_job_id for task {}: {}",
                                task_id,
                                e
                            );
                        }
                        format!(
                            "Task scheduled! ID: {} — {} ({})",
                            task_id, description, trigger_value
                        )
                    }
                    Err(e) => {
                        let _ = self.task_store.set_status(&task_id, "failed").await;
                        format!("Failed to register task with scheduler: {}", e)
                    }
                }
            }
            "list_scheduled_tasks" => match self.task_store.list_active_for_user(user_id).await {
                Ok(tasks) if tasks.is_empty() => "No active scheduled tasks.".to_string(),
                Ok(tasks) => {
                    let mut out = format!("Active scheduled tasks ({}):\n\n", tasks.len());
                    for t in tasks {
                        out.push_str(&format!(
                            "ID: {}\nDescription: {}\nType: {} | Trigger: {}\nPrompt: {}\n\n",
                            t.id, t.description, t.trigger_type, t.trigger_value, t.prompt
                        ));
                    }
                    out
                }
                Err(e) => format!("Failed to list tasks: {}", e),
            },
            "cancel_scheduled_task" => {
                let task_id = match arguments["task_id"].as_str() {
                    Some(id) => id.to_string(),
                    None => return "Missing task_id".to_string(),
                };
                // Fetch task to get scheduler_job_id
                let task = match self.task_store.get_by_id(&task_id).await {
                    Ok(Some(t)) => t,
                    Ok(None) => return format!("Task '{}' not found.", task_id),
                    Err(e) => return format!("Failed to look up task: {}", e),
                };
                // Remove from scheduler
                if let Some(ref sched_id_str) = task.scheduler_job_id {
                    if let Ok(sched_uuid) = sched_id_str.parse::<uuid::Uuid>() {
                        if let Err(e) = self.scheduler.remove_job(sched_uuid).await {
                            tracing::warn!(
                                "Failed to remove scheduler job for task {}: {}",
                                task_id,
                                e
                            );
                        }
                    }
                }
                // Mark cancelled in DB
                match self.task_store.set_status(&task_id, "cancelled").await {
                    Ok(()) => format!("Task '{}' ({}) cancelled.", task_id, task.description),
                    Err(e) => format!("Failed to update task status: {}", e),
                }
            }
            "write_skill_file" => {
                let skill_name = match arguments["skill_name"].as_str() {
                    Some(n) => n.to_string(),
                    None => return "Missing skill_name".to_string(),
                };
                let relative_path = match arguments["relative_path"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing relative_path".to_string(),
                };
                let content = arguments["content"].as_str().unwrap_or("").to_string();

                if let Err(e) = validate_skill_name(&skill_name) {
                    return format!("Invalid skill_name: {}", e);
                }
                if let Err(e) = validate_skill_path(&relative_path) {
                    return format!("Invalid relative_path: {}", e);
                }

                let target = self
                    .config
                    .skills
                    .directory
                    .join(&skill_name)
                    .join(&relative_path);

                if let Some(parent) = target.parent() {
                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                        return format!("Failed to create directories: {}", e);
                    }
                }

                match tokio::fs::write(&target, &content).await {
                    Ok(()) => {
                        info!("Skill file written: {}", target.display());
                        format!("Written: {}", target.display())
                    }
                    Err(e) => format!("Failed to write skill file: {}", e),
                }
            }
            "reload_skills" => {
                use crate::skills::loader::load_skills_from_dir;
                match load_skills_from_dir(&self.config.skills.directory).await {
                    Ok(new_registry) => {
                        let count = new_registry.len();
                        let mut skills = self.skills.write().await;
                        *skills = new_registry;
                        info!("Skills reloaded: {} skill(s) active", count);
                        format!("Skills reloaded. {} skill(s) now active.", count)
                    }
                    Err(e) => format!("Failed to reload skills: {}", e),
                }
            }
            _ if self.mcp.is_mcp_tool(name) => match self.mcp.call_tool(name, arguments).await {
                Ok(result) => result,
                Err(e) => format!("MCP tool error: {}", e),
            },
            _ => {
                match tools::execute_builtin_tool(
                    name,
                    arguments,
                    &self.config.sandbox.allowed_directory,
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => format!("Tool error: {}", e),
                }
            }
        }
    }
}

/// Parse an ISO 8601 datetime string and return the Duration until it fires.
/// Returns Err if the string is invalid or the time is in the past.
fn parse_one_shot_delay(trigger_value: &str) -> anyhow::Result<std::time::Duration> {
    use chrono::{Local, NaiveDateTime, TimeZone};

    let dt = NaiveDateTime::parse_from_str(trigger_value, "%Y-%m-%dT%H:%M:%S")
        .map(|naive| Local.from_local_datetime(&naive).single())
        .ok()
        .flatten()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(trigger_value)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid datetime '{}'. Use ISO 8601 format e.g. '2026-03-05T12:00:00'",
                trigger_value
            )
        })?;

    let now = chrono::Utc::now();
    if dt <= now {
        anyhow::bail!(
            "That time has already passed ({}). Please provide a future datetime.",
            trigger_value
        );
    }

    let duration = (dt - now)
        .to_std()
        .map_err(|e| anyhow::anyhow!("Duration conversion failed: {}", e))?;
    Ok(duration)
}

/// Validate a 6-field cron expression (sec min hour day month weekday).
fn validate_cron_expr(expr: &str) -> anyhow::Result<()> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 6 {
        anyhow::bail!(
            "Cron expression must have 6 fields (sec min hour day month weekday), got {}: '{}'",
            fields.len(),
            expr
        );
    }
    Ok(())
}

/// Trim conversation history to at most `window` non-system messages, always
/// keeping the system message at index 0. Returns a `Vec` that is either a
/// clone of the full history (if within the window) or a truncated view.
/// `window == 0` means unlimited.
fn apply_context_window(messages: &[ChatMessage], window: usize) -> Vec<ChatMessage> {
    if window == 0 || messages.len() <= window + 1 {
        return messages.to_vec();
    }
    // Separate system message (if present) from the rest
    let (system_msgs, rest): (Vec<_>, Vec<_>) = messages.iter().partition(|m| m.role == "system");
    let trimmed_rest = if rest.len() > window {
        rest[rest.len() - window..].to_vec()
    } else {
        rest
    };
    let mut result: Vec<ChatMessage> = system_msgs.into_iter().cloned().collect();
    result.extend(trimmed_rest.into_iter().cloned());
    result
}

/// Split a long response string into chunks of at most `max_len` characters.
pub fn split_response_chunks(text: &str, max_len: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    let chars: Vec<char> = text.chars().collect();
    while start < chars.len() {
        let end = (start + max_len).min(chars.len());
        chunks.push(chars[start..end].iter().collect());
        start = end;
    }
    chunks
}

/// Validate skill directory name: lowercase letters, numbers, hyphens, 1–64 chars.
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name must not be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!(
            "Skill name too long ({} chars, max 64)",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "Skill name must contain only lowercase letters, numbers, and hyphens".to_string(),
        );
    }
    Ok(())
}

/// Validate a relative path within a skill directory: no '..' components, non-empty.
fn validate_skill_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Relative path must not be empty".to_string());
    }
    if path.starts_with('/') {
        return Err("Relative path must not be absolute".to_string());
    }
    if path.split('/').any(|c| c == "..") {
        return Err("Path traversal ('..') is not allowed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_one_shot_delay_valid() {
        let result = parse_one_shot_delay("2099-12-31T23:59:59");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_one_shot_delay_past_returns_err() {
        let result = parse_one_shot_delay("2000-01-01T00:00:00");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already passed"));
    }

    #[test]
    fn test_parse_one_shot_delay_invalid_format() {
        let result = parse_one_shot_delay("next tuesday");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_cron_expr_valid() {
        assert!(validate_cron_expr("0 0 9 * * MON").is_ok());
        assert!(validate_cron_expr("0 30 8 * * *").is_ok());
    }

    #[test]
    fn test_validate_cron_expr_wrong_field_count() {
        assert!(validate_cron_expr("0 9 * * *").is_err()); // 5 fields
        assert!(validate_cron_expr("0 0 9 1 * * MON").is_err()); // 7 fields
    }

    #[test]
    fn test_validate_skill_name_valid() {
        assert!(validate_skill_name("creating-skills").is_ok());
        assert!(validate_skill_name("my-skill-123").is_ok());
        assert!(validate_skill_name("a").is_ok());
    }

    #[test]
    fn test_validate_skill_name_empty() {
        assert!(validate_skill_name("").is_err());
    }

    #[test]
    fn test_validate_skill_name_too_long() {
        let long = "a".repeat(65);
        assert!(validate_skill_name(&long).is_err());
    }

    #[test]
    fn test_validate_skill_name_invalid_chars() {
        assert!(validate_skill_name("My-Skill").is_err()); // uppercase
        assert!(validate_skill_name("my skill").is_err()); // space
        assert!(validate_skill_name("my_skill").is_err()); // underscore
        assert!(validate_skill_name("my/skill").is_err()); // slash
    }

    #[test]
    fn test_validate_skill_path_valid() {
        assert!(validate_skill_path("SKILL.md").is_ok());
        assert!(validate_skill_path("reference.md").is_ok());
        assert!(validate_skill_path("scripts/helper.py").is_ok());
        assert!(validate_skill_path("scripts/sub/tool.sh").is_ok());
    }

    #[test]
    fn test_validate_skill_path_traversal() {
        assert!(validate_skill_path("../other-skill/SKILL.md").is_err());
        assert!(validate_skill_path("scripts/../../../etc/passwd").is_err());
        assert!(validate_skill_path("..").is_err());
    }

    #[test]
    fn test_validate_skill_path_empty() {
        assert!(validate_skill_path("").is_err());
    }

    #[test]
    fn test_validate_skill_path_absolute() {
        assert!(validate_skill_path("/etc/passwd").is_err());
        assert!(validate_skill_path("/SKILL.md").is_err());
    }
}
