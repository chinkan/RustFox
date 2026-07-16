use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use teloxide::types::ChatId;
use teloxide::Bot;

use crate::agent_prompt::{
    prepare_messages_for_llm, recovery_nudge_for, ConversationMeta,
    PreparedPrompt,
};
use crate::config::Config;
use crate::langsmith::LangSmithClient;
use crate::llm::{
    is_empty_assistant_response, ChatMessage, ContentPart, LlmClient,
    MessageContent, ToolCall, ToolDefinition,
};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::platform::IncomingMessage;
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::Scheduler;
use crate::skills::{format_listed_section, SkillRegistry};
use crate::cancel_registry::CancelRegistry;
use crate::tool_registry::{ToolContext, ToolRegistry};
use crate::platform::sender::PlatformSender;
use std::collections::HashMap;

/// Mid-run mode determines how a user's message is handled when the agent
/// is already processing a previous turn. `Steer` injects the message into
/// the active run (interrupt the current trajectory). `Queue` stores it for
/// the next run instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidRunMode {
    Steer,
    Queue,
}

impl MidRunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            MidRunMode::Steer => "steer",
            MidRunMode::Queue => "queue",
        }
    }

    pub fn from_mode_str(s: &str) -> Option<Self> {
        match s {
            "steer" => Some(MidRunMode::Steer),
            "queue" => Some(MidRunMode::Queue),
            _ => None,
        }
    }
}

/// User's choice when a loop is detected and an inline keyboard is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopCallbackChoice {
    Continue,
    Stop,
    AddInstruction,
}



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
    pub registry: Arc<crate::provider::ProviderRegistry>,
    pub config: Config,
    pub mcp: McpManager,
    pub memory: MemoryStore,
    pub skills: tokio::sync::RwLock<SkillRegistry>,
    pub agents: tokio::sync::RwLock<SkillRegistry>,
    // Fields used by scheduling / job closures
    pub task_store: ScheduledTaskStore,
    pub scheduler: Arc<Scheduler>,
    pub bot: Arc<Bot>,
    pub self_weak: Weak<Agent>,
    /// Sender for dispatching scheduled job work to the background runner.
    pub job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
    pub langsmith: Arc<LangSmithClient>,
    pub restart_pending: AtomicBool,
    pub soul_updated: AtomicBool,
    pub current_model: tokio::sync::RwLock<String>,
    pub config_path: PathBuf,
    pub cancel_registry: Arc<CancelRegistry>,
    pub tool_registry: ToolRegistry,
    pub sender: Arc<dyn PlatformSender>,
    /// Per-user CancellationTokens for /stop — created at process_message entry,
    /// removed on exit. Checked at each iteration boundary.
    pub cancel_token_registry: Arc<tokio::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// Per-user pending injection messages (Steer/Inject), max 10 per user.
    /// When a non-command message arrives while processing is active, it's queued here.
    pub pending_injections: Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
    /// One-shot senders for loop detection callbacks, keyed by user_id.
    /// The agent loop creates a oneshot channel, stores the sender here,
    /// then awaits the receiver. The Telegram callback handler resolves
    /// the sender with the user's choice.
    pub pending_loop_callbacks: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<LoopCallbackChoice>>,
        >,
    >,
}

/// A task parsed from the spawn_agents tool arguments, after validation.
#[allow(dead_code)]
struct AdHocTask {
    system_prompt: String,
    prompt: String,
    model: Option<String>,
    tools: Option<Vec<String>>,
}

/// Build the unified `# Available Agents` section from the two line sources
/// (subagent-style skills and agents directory). Returns `None` when both
/// inputs are empty so the caller can skip the section entirely. The returned
/// string includes the leading `\n\n` separator so it can be appended
/// directly to a prompt that already ends with content.
fn format_available_agents_section(subagent_lines: &str, agent_lines: &str) -> Option<String> {
    if subagent_lines.is_empty() && agent_lines.is_empty() {
        return None;
    }

    let mut section = String::from("\n\n# Available Agents\n\n");
    section.push_str(&format_listed_section(
        "agent",
        "Delegate these tasks to specialized agents using `invoke_agent`:",
    ));

    if !subagent_lines.is_empty() {
        section.push_str(subagent_lines);
    }
    if !subagent_lines.is_empty() && !agent_lines.is_empty() {
        section.push('\n');
    }
    if !agent_lines.is_empty() {
        section.push_str(agent_lines);
    }
    section.push('\n');
    Some(section)
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        registry: Arc<crate::provider::ProviderRegistry>,
        mcp: McpManager,
        memory: MemoryStore,
        skills: SkillRegistry,
        agents: SkillRegistry,
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        bot: Arc<Bot>,
        self_weak: Weak<Agent>,
        job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
        langsmith: Arc<LangSmithClient>,
        config_path: PathBuf,
        cancel_registry: Arc<CancelRegistry>,
        tool_registry: ToolRegistry,
        sender: Arc<dyn PlatformSender>,
    ) -> Self {
        let llm = LlmClient::new(registry.clone());
        let initial_model = registry.default_qualified_model();
        Self {
            llm,
            registry,
            config,
            mcp,
            memory,
            skills: tokio::sync::RwLock::new(skills),
            agents: tokio::sync::RwLock::new(agents),
            task_store,
            scheduler,
            bot,
            self_weak,
            job_tx,
            langsmith,
            restart_pending: AtomicBool::new(false),
            soul_updated: AtomicBool::new(false),
            current_model: tokio::sync::RwLock::new(initial_model),
            config_path,
            cancel_registry,
            tool_registry,
            sender,
            cancel_token_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_injections: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_loop_callbacks: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// Build the system prompt, incorporating loaded skills and agents
    async fn build_system_prompt(&self) -> String {
        let mut prompt = self.config.openrouter.system_prompt.clone();

        let skills = self.skills.read().await;
        let skill_context = skills.build_context();
        if !skill_context.is_empty() {
            prompt.push_str("\n\n# Available Skills\n\n");
            prompt.push_str(&skill_context);
        }

        // Build unified "Available Agents" section from both subagent skills and agents/
        let subagent_skills = skills.build_subagent_lines();
        drop(skills);
        let agents = self.agents.read().await;
        let agent_lines = agents.build_agents_context();
        drop(agents);

        if let Some(section) = format_available_agents_section(&subagent_skills, &agent_lines) {
            prompt.push_str(&section);
        }

        // Work Verification Protocol
        prompt.push_str(
            "\n\n# Work Verification Protocol\n\n\
             BEFORE ending your response, you MUST verify your work:\n\n\
             1. Call `invoke_agent(agent=\"verifier\", prompt=\"TASK: ...\\nCRITERIA: ...\\nEVIDENCE: ...\")`\n\
                with the original task, your criteria, and a brief summary of what you did\n\
                including key file paths.\n\
             2. The verifier has READ-ONLY sandbox access — it will use read_file and\n\
                list_files to inspect the actual output. You do NOT need to dump file\n\
                contents into the prompt. Just tell it which files to look at.\n\
             3. If the verifier returns NEEDS_IMPROVEMENT or FAIL, do NOT end.\n\
                Use the feedback to continue working. You will get another iteration.\n\
             4. Only if the verifier returns PASS may you end.\n\
             5. You may also verify intermediate results during multi-step tasks."
        );

        // Soul file protocol — instruct the AI to maintain its own identity files
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
             giving your final response.",
        );

        // Append ambient system context (user model, timestamp, location).
        // `build_system_context` already includes the leading `\n\n` separators.
        prompt.push_str(&self.build_system_context().await);

        // Warn if system prompt is very large (tight on context window)
        if prompt.len() > 50_000 {
            warn!(
                "System prompt is large: {} bytes — consider reducing skill/agent descriptions",
                prompt.len()
            );
        }

        prompt
    }

    /// Build ambient system context (soul files, timestamp, location) shared by
    /// the main agent and subagents. Unlike build_system_prompt, this does NOT
    /// include skills/agents listings.
    async fn build_system_context(&self) -> String {
        let mut ctx = String::new();

        if let Some(home) = &self.config.resolved_home {
            // Inject SOUL.md
            let soul_path = home.join("SOUL.md");
            let soul_content = crate::learning::read_soul_file(&soul_path).await;
            if !soul_content.is_empty() {
                let truncated = crate::learning::truncate_to(&soul_content, 8_000);
                ctx.push_str("\n\n# My Identity\n<identity>\n");
                ctx.push_str(&truncated);
                ctx.push_str("\n</identity>");
                if truncated.len() < soul_content.len() {
                    ctx.push_str(
                        "\n[File truncated — use read_soul_file(\"SOUL.md\") for full content]",
                    );
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
                    ctx.push_str(
                        "\n[File truncated — use read_soul_file(\"AGENTS.md\") for full content]",
                    );
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
                    ctx.push_str(
                        "\n[File truncated — use read_soul_file(\"USER.md\") for full content]",
                    );
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

    /// Build the system prompt for an ad-hoc subagent by prepending system context
    /// (timestamp, user model, location) to the agent's specific instructions.
    #[allow(dead_code)]
    async fn build_subagent_system_prompt(&self, agent_instructions: &str) -> String {
        let mut prompt = self.build_system_context().await;
        prompt.push_str("\n\n");
        prompt.push_str(agent_instructions);
        prompt
    }

    /// Reload both skill and agent registries from their directories.
    /// Returns `(skills_count, agents_count)`.
    pub async fn reload_skills_and_agents(&self) -> (usize, usize) {
        use crate::skills::loader::load_skills_from_dir;

        let skills_dir = self.config.skills.directory.clone();
        let agents_dir = self.config.agents.directory.clone();

        if let Ok(reg) = load_skills_from_dir(&skills_dir, skills_dir.clone()).await {
            let count = reg.len();
            let mut s = self.skills.write().await;
            *s = reg;
            let a = if let Ok(reg) = load_skills_from_dir(&agents_dir, agents_dir.clone()).await {
                let count = reg.len();
                let mut a = self.agents.write().await;
                *a = reg;
                count
            } else {
                self.agents.read().await.len()
            };
            (count, a)
        } else {
            (
                self.skills.read().await.len(),
                self.agents.read().await.len(),
            )
        }
    }

    /// Change the active model and persist to config.toml.
    pub async fn set_model(&self, model_id: &str) -> anyhow::Result<()> {
        if model_id.is_empty() {
            anyhow::bail!("Model ID cannot be empty");
        }

        // Validate: resolve succeeds for any string
        let (provider, actual_model) = self.registry.resolve_model(model_id);
        if let Some((prefix, _)) = model_id.split_once('/') {
            if self.registry.get_provider(prefix).is_none() {
                tracing::warn!(
                    "Model '{}': prefix '{}' does not match any known provider \
                     (falling through to default '{}')",
                    model_id,
                    prefix,
                    self.registry.default_provider_name()
                );
            }
        }

        let content = tokio::fs::read_to_string(&self.config_path).await?;
        let mut doc: toml::value::Table = toml::from_str(&content)?;

        let provider_name = provider.name().to_string();

        // Try explicit [[provider]] array first
        let mut found_in_array = false;
        if let Some(provider_array) = doc.get_mut("provider").and_then(|v| v.as_array_mut()) {
            for entry in provider_array.iter_mut() {
                if let Some(table) = entry.as_table_mut() {
                    if table.get("name").and_then(|v| v.as_str()) == Some(&provider_name) {
                        table.insert(
                            "model".to_string(),
                            toml::Value::String(actual_model.to_string()),
                        );
                        found_in_array = true;
                    }
                }
            }
        }

        // Fall back to legacy [openrouter] section if not found in [[provider]] array
        if !found_in_array && provider_name == "openrouter" && doc.contains_key("openrouter") {
            if let Some(table) = doc.get_mut("openrouter").and_then(|v| v.as_table_mut()) {
                table.insert(
                    "model".to_string(),
                    toml::Value::String(actual_model.to_string()),
                );
            }
        }

        let new_content = toml::to_string_pretty(&doc)?;
        tokio::fs::write(&self.config_path, &new_content).await?;

        let mut current = self.current_model.write().await;
        *current = model_id.to_string();

        tracing::info!(model = %model_id, provider = %provider_name, "Model changed and persisted");
        Ok(())
    }

    /// Register a CancellationToken for the given user_id before processing starts.
    /// Called at the start of process_message. Returns the token for cancellation checks.
    pub async fn register_cancel_token(&self, user_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.cancel_token_registry
            .lock()
            .await
            .insert(user_id.to_string(), token.clone());
        token
    }

    /// Cancel processing for a user. Returns true if there was an active token.
    pub async fn cancel_processing(&self, user_id: &str) -> bool {
        let mut map = self.cancel_token_registry.lock().await;
        if let Some(token) = map.remove(user_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Check if a user has active processing.
    pub async fn is_processing(&self, user_id: &str) -> bool {
        self.cancel_token_registry
            .lock()
            .await
            .contains_key(user_id)
    }

    /// Queue an injection message for a user. Returns false if queue is full (max 10).
    pub async fn queue_injection(&self, user_id: &str, text: &str) -> bool {
        const MAX_INJECTIONS: usize = 10;
        let mut map = self.pending_injections.lock().await;
        let queue = map.entry(user_id.to_string()).or_default();
        if queue.len() >= MAX_INJECTIONS {
            false
        } else {
            queue.push(text.to_string());
            true
        }
    }

    /// Drain all pending injection messages for a user.
    pub async fn drain_injections(&self, user_id: &str) -> Vec<String> {
        let mut map = self.pending_injections.lock().await;
        map.remove(user_id).unwrap_or_default()
    }

    /// Drain pending steer/queue injections for the given user and push them
    /// into `messages`. Returns `true` when at least one injection was applied.
    ///
    /// Used both at the start of each outer iteration (before the LLM call)
    /// and right after a tool batch commits (so steer traffic that arrives
    /// during a long tool batch is still visible on the next turn).
    ///
    /// `Queue` mode additionally persists the message into conversation memory
    /// so it survives a process_message boundary.
    pub async fn drain_and_inject_steer(
        &self,
        user_id: &str,
        conversation_id: &str,
        messages: &mut Vec<ChatMessage>,
    ) {
        let inject_mode = self.get_mid_run_mode(user_id).await;
        let injections = self.drain_injections(user_id).await;
        for text in &injections {
            let label = if inject_mode == MidRunMode::Steer {
                "**[Steer]:** "
            } else {
                "**[User injected mid-processing]:** "
            };
            let msg = ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::from_text(format!("{}{}", label, text))),
                tool_calls: None,
                tool_call_id: None,
            };
            if inject_mode == MidRunMode::Queue {
                if let Err(e) = self.memory.save_message(conversation_id, &msg).await {
                    warn!("Failed to persist queued injection: {}", e);
                }
            }
            messages.push(msg);
        }
    }

    /// Register a oneshot sender for a user's loop detection callback.
    /// Returns the old sender if one was already registered (should not happen
    /// in practice since one user has one active process_message).
    pub async fn register_loop_callback(
        &self,
        user_id: &str,
        sender: tokio::sync::oneshot::Sender<LoopCallbackChoice>,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let mut map = self.pending_loop_callbacks.lock().await;
        map.insert(user_id.to_string(), sender)
    }

    /// Take the loop callback sender for a user, if any.
    pub async fn take_loop_callback(
        &self,
        user_id: &str,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let mut map = self.pending_loop_callbacks.lock().await;
        map.remove(user_id)
    }

    /// Get the current MidRunMode for a user. Defaults to Steer.
    pub async fn get_mid_run_mode(&self, user_id: &str) -> MidRunMode {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory
            .recall("settings", &key)
            .await
            .ok()
            .flatten()
            .and_then(|v| MidRunMode::from_mode_str(&v))
            .unwrap_or(MidRunMode::Steer)
    }

    /// Set the MidRunMode for a user.
    pub async fn set_mid_run_mode(&self, user_id: &str, mode: MidRunMode) {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory
            .remember("settings", &key, mode.as_str(), None)
            .await
            .ok();
    }

    /// Delete the MidRunMode for a user (resets to default).
    pub async fn delete_mid_run_mode(&self, user_id: &str) {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory.forget("settings", &key).await.ok();
    }

    /// Remove cancel token for a user (called on process_message exit).
    pub async fn clear_cancel_token(&self, user_id: &str) {
        self.cancel_token_registry.lock().await.remove(user_id);
    }

    /// Fetch the context window size for the current model from the
    /// provider API and cache it. Non-fatal — uses static fallback on
    /// failure.
    pub async fn refresh_context_window_cache(&self) {
        let model = self.current_model.read().await.clone();
        let (provider, actual_model) = self.registry.resolve_model(&model);
        let client = reqwest::Client::new();
        if let Some(ctx) = provider.fetch_context_window(&client, actual_model).await {
            let mut cache = provider.config().context_window_cache.write().await;
            *cache = Some(ctx);
            tracing::info!("Context window for {}: {} tokens", actual_model, ctx);
        }
    }

    /// Process an incoming message and return the response text
    pub(crate) fn now_iso8601_static() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Build LangSmith outputs for an LLM run, including completion metadata and prompt stats.
    #[allow(dead_code)]
    fn llm_run_outputs(
        completion: Option<&crate::llm::ChatCompletion>,
        prompt: &PreparedPrompt,
        retry_count: u32,
    ) -> serde_json::Value {
        let finish_reason = completion.and_then(|c| c.finish_reason.clone());
        let model = completion
            .map(|c| c.model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let message = completion.map(|c| &c.message);

        serde_json::json!({
            "choices": [{
                "finish_reason": finish_reason,
                "message": message.map(|message| serde_json::json!({
                    "role": message.role,
                    "content": message.content,
                    "tool_calls": message.tool_calls,
                }))
            }],
            "metadata": {
                "model": model,
                "message_count": prompt.stats.prepared_message_count,
                "original_message_count": prompt.stats.original_message_count,
                "prompt_chars": prompt.stats.prepared_prompt_chars,
                "original_prompt_chars": prompt.stats.original_prompt_chars,
                "prompt_compaction_applied": prompt.stats.compaction_applied,
                "empty_response_retry_count": retry_count,
            }
        })
    }

    pub async fn process_message(
        &self,
        incoming: &IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Result<String> {
        let platform = &incoming.platform;
        let user_id = &incoming.user_id;
        let _parsed_chat_id: ChatId = incoming
            .chat_id
            .parse::<i64>()
            .map(ChatId)
            .unwrap_or(ChatId(0));

        // Get or create persistent conversation
        let conversation_id = self
            .memory
            .get_or_create_conversation(platform, user_id)
            .await?;

        // Always build the system prompt from the live registry.
        let current_system_prompt = self.build_system_prompt().await;

        // Use ConversationManager for message construction and management
        let skills = self.skills.read().await;
        let mut cmgr = crate::conversation::ConversationManager::new(
            &self.memory,
            platform,
            user_id,
            current_system_prompt.clone(),
            &skills,
            &self.config,
        )
        .await?;
        drop(skills);

        // RAG: auto-retrieve relevant past messages and inject into system prompt
        if !incoming.text.is_empty() {
            let filtered_msgs: Vec<_> = cmgr.messages()
                .iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .cloned()
                .collect();
            let rewrite_start = filtered_msgs.len().saturating_sub(6);
            let recent_for_rewrite = filtered_msgs[rewrite_start..].to_vec();

            let per_user_setting = self
                .memory
                .recall(
                    "settings",
                    &format!("query_rewrite_enabled_{}", incoming.user_id),
                )
                .await
                .unwrap_or(None);
            let rewrite_enabled = match per_user_setting.as_deref() {
                Some("true") => true,
                Some("false") => false,
                _ => self.config.memory.query_rewriter_enabled,
            };
            let llm_for_rewrite = if rewrite_enabled {
                Some(&self.llm)
            } else {
                None
            };

            if let Ok(Some(rag_block)) = crate::memory::rag::auto_retrieve_context(
                &self.memory,
                llm_for_rewrite,
                &incoming.text,
                &recent_for_rewrite,
                &conversation_id,
                self.config.memory.rag_limit,
            )
            .await
            {
                cmgr.inject_rag_context(&rag_block);
            }
        }

        // Process attachments
        let supports_vision = {
            let current = self.current_model.read().await;
            let (provider, _) = self.registry.resolve_model(&current);
            provider.supports_vision()
        };

        let image_parts = cmgr.add_incoming(incoming, &self.config, supports_vision).await?;

        // Build user message content
        let user_msg_content = if image_parts.is_empty() {
            MessageContent::from_text(incoming.text.clone())
        } else {
            let mut parts: Vec<ContentPart> = Vec::new();
            if !incoming.text.is_empty() {
                parts.push(ContentPart::Text { text: incoming.text.clone() });
            }
            parts.extend(image_parts);
            MessageContent::Parts(parts)
        };

        // Push the user message to in-memory context
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(user_msg_content),
            tool_calls: None,
            tool_call_id: None,
        };
        cmgr.add_user_turn(user_msg);

        // Compaction state for this conversation session (persists across iterations)
        let _conv_meta = ConversationMeta::new();
// Gather all tool definitions
        let mut all_tools: Vec<ToolDefinition> = self.tool_registry.all_definitions();
        all_tools.extend(self.mcp.tool_definitions());

        // --- LangSmith: start root chain run ---
        let chain_run_id = uuid::Uuid::new_v4().to_string();
        let ls_project = self
            .config
            .langsmith
            .as_ref()
            .map(|l| l.project.as_str())
            .unwrap_or("default")
            .to_string();

        self.langsmith.start_run(crate::langsmith::RunParams {
            id: chain_run_id.clone(),
            name: "rustfox_request".to_string(),
            run_type: crate::langsmith::RunType::Chain,
            parent_run_id: None,
            inputs: serde_json::json!({ "message": incoming.text }),
            session_name: ls_project.clone(),
            start_time: Self::now_iso8601_static(),
        });

        // Reset soul-update flag for this session
        self.soul_updated
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Register cancel token for /stop support
        let cancel_token = self.register_cancel_token(user_id).await;

        // Build make_ctx closure for ToolContext construction
        let make_ctx = {
            let sandbox_dir = self.config.sandbox.allowed_directory.clone();
            let home_dir = self.config.resolved_home.clone();
            let sender = self.sender.clone();
            let cancel_registry = self.cancel_registry.clone();
            move |_user_id: &str, _chat_id: &str| ToolContext {
                sandbox_dir: sandbox_dir.clone(),
                home_dir: home_dir.clone(),
                sender: sender.clone(),
                cancel_registry: cancel_registry.clone(),
                user_id: _user_id.to_string(),
                chat_id: _chat_id.to_string(),
            }
        };

        let loop_config = crate::loop_runner::LoopConfig {
            max_iterations: self.config.max_iterations(),
            empty_response_retry_limit: self.config.empty_response_retry_limit(),
            compaction_enabled: true,
            loop_detection_enabled: true,
            interactive_loop_callback: true,
            allowed_tools: None,
            langsmith_project: Some(ls_project.clone()),
            tool_event_tx,
            stream_token_tx,
        };

        let outcome = crate::loop_runner::AgenticLoop::new(
            &self.llm,
            &self.tool_registry,
            &self.mcp,
            &loop_config,
            Some(cancel_token.clone()),
            Some(chain_run_id.clone()),
            Some(&self.langsmith),
            self.sender.as_ref() as &dyn PlatformSender,
            Box::new(make_ctx),
        )
        .run(
            &mut crate::loop_runner::MessageContainer::Conversation(Box::new(cmgr)),
            user_id,
            &incoming.chat_id,
        )
        .await;

        match outcome {
            Ok(crate::loop_runner::LoopOutcome::FinalResponse(final_content)) => {
                // Save the delivered content to persistent memory
                let save_msg = ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::from_text(final_content.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                };
                self.memory
                    .save_message(&conversation_id, &save_msg)
                    .await?;

                // --- LangSmith: end chain run (success) ---
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: chain_run_id,
                    outputs: Some(serde_json::json!({
                        "response": final_content,
                        "iterations": 0,
                    })),
                    error: None,
                    end_time: Self::now_iso8601_static(),
                });

                self.clear_cancel_token(user_id).await;
                Ok(final_content)
            }
            Ok(crate::loop_runner::LoopOutcome::Cancelled) => {
                info!(
                    user_id = %user_id,
                    "Processing cancelled by user — returning partial result"
                );
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: chain_run_id,
                    outputs: None,
                    error: Some("Cancelled by user".to_string()),
                    end_time: Self::now_iso8601_static(),
                });
                self.clear_cancel_token(user_id).await;
                Ok("Processing was cancelled.".to_string())
            }
            Ok(crate::loop_runner::LoopOutcome::MaxIterations) => {
                warn!(
                    user_id = %user_id,
                    max_iterations = self.config.max_iterations(),
                    "Reached max iterations without final text response"
                );
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: chain_run_id,
                    outputs: None,
                    error: Some(format!("Reached max iterations ({})", self.config.max_iterations())),
                    end_time: Self::now_iso8601_static(),
                });
                self.clear_cancel_token(user_id).await;
                Ok("I've reached the maximum number of tool call iterations. Please try rephrasing your request.".to_string())
            }
            Err(e) => {
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: chain_run_id,
                    outputs: None,
                    error: Some(format!("{:#}", e)),
                    end_time: Self::now_iso8601_static(),
                });
                self.clear_cancel_token(user_id).await;
                Err(e)
            }
        }
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
        self.memory.clear_conversation(platform, user_id).await?;
        // Reset mid-run mode to default (Steer)
        self.delete_mid_run_mode(user_id).await;
        Ok(())
    }

    /// Get all tool definitions for display
    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut all = self.tool_registry.all_definitions();
        all.extend(self.mcp.tool_definitions());
        all
    }

    /// Run a named skill/agent as an isolated subagent mini-loop.
    /// `kind` controls which registry to look up and which read tool to use in the bootstrap.
    /// Returns the subagent's final text response (or an error string).
    ///
    /// Ad-hoc mode (skill_name = None): use the provided system_prompt + user_prompt
    /// directly with a default sandbox tool whitelist. The system_prompt is augmented
    /// with ambient system context (timestamp, user model, location) via
    /// `build_subagent_system_prompt`.
    #[allow(dead_code)]
    pub(crate) async fn run_subagent(
        &self,
        skill_name: Option<&str>,
        system_prompt: &str,
        user_prompt: &str,
        model_override: Option<&str>,
        tools_override: Option<Vec<String>>,
    ) -> String {
        // --- Ad-hoc mode (no predefined skill/agent) ---
        if skill_name.is_none() {
            let model = model_override
                .map(str::to_string)
                .unwrap_or_else(|| self.config.openrouter.model.clone());

            let declared_tools = tools_override
                .or_else(|| self.config.subagents.default_tools.clone())
                .unwrap_or_else(|| {
                    vec![
                        "read_file".to_string(),
                        "write_file".to_string(),
                        "list_files".to_string(),
                        "execute_command".to_string(),
                    ]
                });
            let allowed_tools = declared_tools; // ad-hoc: no auto-injection of read_skill_file
            let max_iter = self.config.max_iterations();

            info!(
                "Ad-hoc subagent using model: {} (allowed_tools: {} tools)",
                model,
                allowed_tools.len()
            );

            let all_possible_tools: Vec<ToolDefinition> = {
                let mut t = self.tool_registry.all_definitions();
                t.extend(self.mcp.tool_definitions());
                t
            };

            let subagent_tools: Vec<ToolDefinition> = all_possible_tools
                .into_iter()
                .filter(|td| allowed_tools.contains(&td.function.name))
                .collect();

            let system_content = self.build_subagent_system_prompt(system_prompt).await;
            let mut messages = vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::from_text(system_content)),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::from_text(user_prompt)),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ];

            return self
                .run_subagent_loop(
                    &mut messages,
                    &subagent_tools,
                    &allowed_tools,
                    &model,
                    max_iter,
                    "_ad_hoc_",
                    None,
                )
                .await;
        }

        // --- Predefined agent path ---
        let skill_name = skill_name.unwrap(); // safe: we handled None above

        // Resolve model and tool list from registry metadata (or overrides).
        // For invoke_agent: check agents registry first, fall back to skills registry.
        let (resolved_model, declared_tools, max_iter) = {
            let default_model = self.config.openrouter.model.clone();

            let skill_opt = {
                let agents = self.agents.read().await;
                let from_agents = agents.get(skill_name).cloned();
                drop(agents);
                if from_agents.is_some() {
                    from_agents
                } else {
                    // fall back to skills registry
                    let skills = self.skills.read().await;
                    skills.get(skill_name).cloned()
                }
            };

            let model = model_override
                .map(str::to_string)
                .or_else(|| skill_opt.as_ref().and_then(|s| s.model.clone()))
                .unwrap_or_else(|| default_model.clone());
            if model == default_model && skill_opt.is_none() {
                warn!(
                    "Agent/skill '{}' not found in registry; using default model.",
                    skill_name
                );
            }
            let tools = tools_override
                .or_else(|| skill_opt.as_ref().map(|s| s.tools.clone()))
                .unwrap_or_default();
            let max_i = skill_opt
                .as_ref()
                .and_then(|s| s.max_iterations)
                .unwrap_or_else(|| self.config.max_iterations())
                .min(self.config.max_iterations());
            (model, tools, max_i)
        };

        let allowed_tools = effective_subagent_tools(&declared_tools);

        info!(
            "Agent/subagent '{}' using model: {} (allowed_tools: {} tools)",
            skill_name,
            resolved_model,
            allowed_tools.len()
        );

        // Build the subagent tool definitions (filtered to whitelist only)
        let all_possible_tools: Vec<ToolDefinition> = {
            let mut t = self.tool_registry.all_definitions();
            t.extend(self.mcp.tool_definitions());
            t
        };

        // Warn if any declared tool is not available at runtime (e.g. MCP server not configured).
        let available_names: Vec<String> = all_possible_tools
            .iter()
            .map(|td| td.function.name.clone())
            .collect();
        let missing = missing_subagent_tools(&allowed_tools, &available_names);
        if !missing.is_empty() {
            warn!(
                "Agent '{}': declared tools not available at runtime \
                 (MCP server not configured?): {:?}",
                skill_name, missing
            );
        }

        let subagent_tools: Vec<ToolDefinition> = all_possible_tools
            .into_iter()
            .filter(|td| allowed_tools.contains(&td.function.name))
            .collect();

        // Resolve the skill/agent metadata again so we can read its body for the
        // skip_bootstrap path. (Cheap HashMap lookups; the locks are dropped quickly.)
        let skill_opt = {
            let agents = self.agents.read().await;
            let from_agents = agents.get(skill_name).cloned();
            drop(agents);
            if from_agents.is_some() {
                from_agents
            } else {
                let skills = self.skills.read().await;
                skills.get(skill_name).cloned()
            }
        };

        // Check if agent has skip_bootstrap: true — use body as system message directly
        let skip_bootstrap = skill_opt
            .as_ref()
            .map(|s| s.skip_bootstrap)
            .unwrap_or(false);

        // Strip YAML frontmatter from content if present (between --- markers)
        let body = skill_opt.as_ref().map(|s| {
            let content = &s.content;
            let trimmed = content.trim_start();
            if let Some(after_first) = trimmed.strip_prefix("---") {
                if let Some(end_pos) = after_first.find("---") {
                    return after_first[end_pos + 3..].trim_start().to_string();
                }
            }
            content.clone()
        });

        let system_content = if skip_bootstrap {
            let agent_body = body.as_deref().unwrap_or("");
            format!("You are the '{}' agent.\n\n{}", skill_name, agent_body)
        } else {
            format!(
                    "You are the '{}' agent. Your first action MUST be to call \
                     read_agent_file with agent_name='{}' and relative_path='AGENT.md' to load your instructions.",
                    skill_name, skill_name
                )
        };

        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::from_text(system_content)),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::from_text(user_prompt)),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        self.run_subagent_loop(
            &mut messages,
            &subagent_tools,
            &allowed_tools,
            &resolved_model,
            max_iter,
            skill_name,
            None,
        )
        .await
    }

    /// Shared mini-agentic loop used by both ad-hoc and predefined subagents.
    /// Runs LLM calls, executes whitelisted tools, and returns the final text response.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    async fn run_subagent_loop(
        &self,
        messages: &mut Vec<ChatMessage>,
        subagent_tools: &[ToolDefinition],
        allowed_tools: &[String],
        model: &str,
        max_iter: u32,
        label: &str,
        cancel_token: Option<CancellationToken>,
    ) -> String {
        let empty_response_retry_limit = self.config.empty_response_retry_limit();

        let loop_config = self.config.loop_detection_config();
        let mut loop_detector_sub =
            crate::loop_detector::LoopDetector::new(loop_config.threshold, loop_config.enabled);

        for iteration in 0..max_iter {
            // CHECK: cancelled by /stop?
            if let Some(ref token) = cancel_token {
                if token.is_cancelled() {
                    return format!("Subagent '{}' cancelled by user.", label);
                }
            }
            // --- Empty response recovery: retry loop ---
            let mut retry_count = 0u32;
            let response: ChatMessage;

            // Prepare prompt with optional compaction (invariant across retries)
            let context_window = {
                let (provider, _) = self.registry.resolve_model(model);
                provider.config().context_window
            };
            // TODO: consider adding Tier 3/4 to subagent loops in a follow-up
            let base_prompt = prepare_messages_for_llm(messages, context_window);

            loop {
                let mut prompt_prepared = base_prompt.clone();
                if retry_count > 0 {
                    let nudge = recovery_nudge_for(messages);
                    prompt_prepared.messages.push(nudge);
                }

                let completion = match self
                    .llm
                    .chat_completion_with_model(&prompt_prepared.messages, subagent_tools, model)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        error!("Subagent '{}' API call failed: {}", label, e);
                        return format!("Subagent '{}' error: {}", label, e);
                    }
                };

                if is_empty_assistant_response(&completion.message) {
                    if retry_count >= empty_response_retry_limit {
                        return format!(
                            "Subagent '{}' returned an empty response after {} attempts.",
                            label,
                            retry_count + 1
                        );
                    }
                    retry_count += 1;
                    continue;
                }

                if retry_count > 0 {
                    info!(
                        "Subagent '{}' recovered from empty response after retry",
                        label
                    );
                }

                response = completion.message;
                break;
            }

            // --- Subagent loop detection: auto-recover with nudge ---
            if loop_config.enabled {
                if let Some(ref tool_calls) = response.tool_calls {
                    loop_detector_sub.record(tool_calls, iteration as usize);
                    if let Some(loop_info) = loop_detector_sub.detect_loop() {
                        warn!(
                            subagent = %label,
                            tool = %loop_info.tool_name,
                            count = loop_info.call_count,
                            "Subagent loop detected — injecting recovery nudge"
                        );

                        // Persist the assistant tool-call message so the LLM sees it
                        // on the next iteration — required because a `tool`-role
                        // nudge without a preceding assistant tool_use would be
                        // an orphan message that APIs reject.
                        messages.push(response.clone());

                        // Inject recovery message as a user-role turn so it
                        // doesn't depend on a matching tool_call_id.
                        let nudge_text = format!(
                            "Error: You have called {} {} times with the same arguments. \
                             The result has not changed. Try a different approach.",
                            loop_info.tool_name, loop_info.call_count,
                        );
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: Some(MessageContent::from_text(nudge_text)),
                            tool_calls: None,
                            tool_call_id: None,
                        });

                        loop_detector_sub.clear();
                        continue;
                    }
                }
            }
            // --- End subagent loop detection ---

            if let Some(tool_calls) = &response.tool_calls {
                if !tool_calls.is_empty() {
                    messages.push(response.clone());

                    for tool_call in tool_calls {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                        if is_compacted_regurgitation(&tool_call.function.arguments, &arguments) {
                            messages.push(ChatMessage {
                                role: "tool".to_string(),
                                content: Some(MessageContent::from_text(REGURGITATION_ERROR_MSG)),
                                tool_calls: None,
                                tool_call_id: Some(tool_call.id.clone()),
                            });
                            continue;
                        }

                        let result = if allowed_tools.contains(&tool_call.function.name) {
                            self.execute_tool(
                                &tool_call.function.name,
                                &arguments,
                                "",        // agent has no user_id context
                                ChatId(0), // agent has no chat_id context
                            )
                            .await
                        } else {
                            format!(
                                "Tool '{}' is not available to this agent.",
                                tool_call.function.name
                            )
                        };

                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: Some(MessageContent::from_text(result)),
                            tool_calls: None,
                            tool_call_id: Some(tool_call.id.clone()),
                        });
                    }

                    continue;
                }
            }

            return response.content.map(|c| c.as_text()).unwrap_or_default();
        }

        format!(
            "Subagent '{}' reached the maximum number of iterations ({}).",
            label, max_iter
        )
    }

    /// Execute a tool call by routing to the right handler
    #[allow(dead_code)]
    async fn execute_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        user_id: &str,
        chat_id: ChatId,
    ) -> String {
        if name.starts_with("mcp_") {
            return self.mcp.call_tool(name, arguments).await
                .unwrap_or_else(|e| format!("Error: {e}"));
        }
        match name {
            "invoke_agent" => {
                let agent = match arguments["agent"]
                    .as_str()
                    .or_else(|| arguments["skill"].as_str())
                {
                    Some(a) => a.to_string(),
                    None => return "Missing agent".to_string(),
                };
                let prompt = match arguments["prompt"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing prompt".to_string(),
                };
                let model_override = arguments["model"].as_str().map(str::to_string);
                let tools_override = arguments["tools"].as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                });
                info!(
                    "Invoking agent '{}' (model_override: {:?})",
                    agent, model_override
                );
                Box::pin(self.run_subagent(
                    Some(&agent),
                    "",
                    &prompt,
                    model_override.as_deref(),
                    tools_override,
                ))
                .await
            }
            "spawn_agents" => {
                let parsed_tasks: Vec<AdHocTask> = if let Some(tasks) =
                    arguments["tasks"].as_array()
                {
                    if tasks.is_empty() {
                        return "tasks array is empty".to_string();
                    }
                    let mut parsed = Vec::with_capacity(tasks.len());
                    for (i, task) in tasks.iter().enumerate() {
                        let system_prompt = match task["system_prompt"].as_str() {
                            Some(s) => s.to_string(),
                            None => return format!("Task at index {}: missing system_prompt", i),
                        };
                        let prompt = match task["prompt"].as_str() {
                            Some(p) => p.to_string(),
                            None => return format!("Task at index {}: missing prompt", i),
                        };
                        parsed.push(AdHocTask {
                            system_prompt,
                            prompt,
                            model: task["model"].as_str().map(str::to_string),
                            tools: task["tools"].as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            }),
                        });
                    }
                    parsed
                } else {
                    let system_prompt = match arguments["system_prompt"].as_str() {
                        Some(s) => s.to_string(),
                        None => return "Missing system_prompt or tasks".to_string(),
                    };
                    let prompt = match arguments["prompt"].as_str() {
                        Some(p) => p.to_string(),
                        None => return "Missing prompt".to_string(),
                    };
                    vec![AdHocTask {
                        system_prompt,
                        prompt,
                        model: arguments["model"].as_str().map(str::to_string),
                        tools: arguments["tools"].as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        }),
                    }]
                };
                let futures: Vec<_> = parsed_tasks
                    .into_iter()
                    .map(|task| {
                        let sp = task.system_prompt.clone();
                        let p = task.prompt.clone();
                        let m = task.model.clone();
                        let t = task.tools.clone();
                        Box::pin(async move {
                            self.run_subagent(
                                None,
                                &sp,
                                &p,
                                m.as_deref(),
                                t,
                            ).await
                        })
                    })
                    .collect();
                let results = futures::future::join_all(futures).await;
                let mut output = String::from("Spawned agents results:

");
                for (i, result) in results.iter().enumerate() {
                    output.push_str(&format!("--- Agent {} ---
{}

", i + 1, result));
                }
                output
            }
            _ => {
                let ctx = ToolContext {
                    sandbox_dir: self.config.sandbox.allowed_directory.clone(),
                    home_dir: self.config.resolved_home.clone(),
                    sender: self.sender.clone(),
                    cancel_registry: self.cancel_registry.clone(),
                    user_id: user_id.to_string(),
                    chat_id: chat_id.to_string(),
                };
                self.tool_registry.execute(name, arguments.clone(), ctx).await
                    .unwrap_or_else(|e| format!("Error: {e}"))
            }
        }
    }
}

/// Build a context-forked message list for a /btw side question.
///
/// Follows Claude Code's pattern: fork the current conversation messages,
/// strip orphaned tool_use blocks (no matching tool_result), and append a
/// strict system-reminder that constrains the model to answer from context
/// only, with no tools and no follow-up turns.
///
/// The returned messages are ephemeral — they are NOT saved to conversation
/// history and the /btw response is sent asynchronously.
///
/// This is a free function (not a method) because it only uses its arguments.
pub fn build_btw_context(messages: &[ChatMessage], question: &str) -> Vec<ChatMessage> {
    // 1. Collect all tool_call_ids that have a matching tool_result.
    let mut resolved_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for msg in messages.iter().rev() {
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                resolved_ids.insert(id.as_str());
            }
        }
    }

    // 2. Walk messages and strip orphaned tool_use blocks from assistant messages.
    let forked: Vec<ChatMessage> = messages
        .iter()
        .map(|msg| {
            if msg.role == "assistant" {
                if let Some(ref calls) = msg.tool_calls {
                    let kept: Vec<ToolCall> = calls
                        .iter()
                        .filter(|tc| resolved_ids.contains(tc.id.as_str()))
                        .cloned()
                        .collect();
                    if kept.len() != calls.len() {
                        let mut stripped = msg.clone();
                        if kept.is_empty() {
                            stripped.tool_calls = None;
                        } else {
                            stripped.tool_calls = Some(kept);
                        }
                        return stripped;
                    }
                }
            }
            msg.clone()
        })
        .collect();

    // 3. Append strict system-reminder with the question.
    let reminder = format!(
        r#"<system-reminder>
This is a side question from the user. You must answer this question directly in a single response.

CRITICAL CONSTRAINTS:
- You have NO tools available — you cannot read files, run commands, search, or take any actions
- This is a one-off response — there will be no follow-up turns
- You can ONLY provide information based on what you already know from the conversation context
- NEVER say things like "Let me try...", "I'll now...", "Let me check...", or promise to take any action
- If you don't know the answer, say so — do not offer to look it up or investigate

Simply answer the question with the information you have.
</system-reminder>

{}"#,
        question
    );

    let mut result = forked;
    result.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::from_text(reminder)),
        tool_calls: None,
        tool_call_id: None,
    });
    result
}

/// Parse an ISO 8601 datetime string and return the Duration until it fires.
/// Returns Err if the string is invalid or the time is in the past.
pub(crate) fn parse_one_shot_delay(trigger_value: &str) -> anyhow::Result<std::time::Duration> {
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
pub(crate) fn validate_cron_expr(expr: &str) -> anyhow::Result<()> {
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Build the effective tool whitelist for a subagent/agent.
/// Always includes `read_skill_file` and `read_agent_file`; deduplicates.
#[allow(dead_code)]
fn effective_subagent_tools(declared: &[String]) -> Vec<String> {
    let mut tools = vec!["read_skill_file".to_string(), "read_agent_file".to_string()];
    for t in declared {
        if t != "read_skill_file" && t != "read_agent_file" {
            tools.push(t.clone());
        }
    }
    tools
}

/// Return declared tools that are not present in the set of all available tool names.
/// Used to warn at subagent launch when the whitelist references unavailable tools.
#[allow(dead_code)]
fn missing_subagent_tools(declared: &[String], available_names: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|t| !available_names.contains(t))
        .cloned()
        .collect()
}

/// Error message returned when the main agent or a subagent produces a tool call
/// whose arguments are a regurgitated compaction marker rather than real JSON.
#[allow(dead_code)]
const REGURGITATION_ERROR_MSG: &str = "Error: Your tool call arguments are in compacted format \
    (reproduced from a compressed history entry). \
    Please regenerate the complete call with all required fields.";

/// Detect when the LLM directly reproduces a compaction-marker string as its own
/// tool call arguments.  This happens when the model learns the marker from a
/// compacted history entry and outputs it verbatim instead of real JSON.
///
/// Handles two formats:
/// - Old (backward compat): JSON object with `_rustfox_compacted_arguments: true`
/// - New: plain-text that starts with `COMPACTION_MARKER_PREFIX`
#[allow(dead_code)]
fn is_compacted_regurgitation(raw: &str, parsed: &serde_json::Value) -> bool {
    // Old JSON format — lookup the marker key in the parsed object.
    if parsed
        .get("_rustfox_compacted_arguments")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    // New plain-text format — the raw string itself starts with the marker.
    if raw.starts_with(crate::agent_prompt::COMPACTION_MARKER_PREFIX) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_subagent_tools_includes_read_tools() {
        let tools = effective_subagent_tools(&[]);
        assert!(tools.contains(&"read_skill_file".to_string()));
        assert!(tools.contains(&"read_agent_file".to_string()));
    }

    #[test]
    fn test_effective_subagent_tools_dedup() {
        let tools = effective_subagent_tools(&["execute_command".to_string()]);
        assert!(tools.contains(&"execute_command".to_string()));
        assert!(tools.contains(&"read_skill_file".to_string()));
        assert!(tools.contains(&"read_agent_file".to_string()));
        // Ensure no duplicates of the auto-injected tools
        let count_rsf = tools.iter().filter(|t| *t == "read_skill_file").count();
        let count_raf = tools.iter().filter(|t| *t == "read_agent_file").count();
        assert_eq!(count_rsf, 1, "read_skill_file should appear only once");
        assert_eq!(count_raf, 1, "read_agent_file should appear only once");
    }

    #[test]
    fn test_effective_subagent_tools_skips_declared_read_tools() {
        let tools = effective_subagent_tools(&[
            "read_skill_file".to_string(),
            "read_agent_file".to_string(),
            "read_file".to_string(),
        ]);
        assert!(tools.contains(&"read_skill_file".to_string()));
        assert!(tools.contains(&"read_agent_file".to_string()));
        assert!(tools.contains(&"read_file".to_string()));
        // Should not have duplicates
        assert_eq!(tools.iter().filter(|t| *t == "read_skill_file").count(), 1);
    }

    #[test]
    fn test_tool_status_is_not_streamed_to_answer_channel() {
        let source = include_str!("agent.rs");
        let status_line_call = ["format_tool_status", "_line("].concat();
        let stream_status_var = ["stream", "_status_tx"].concat();

        assert!(
            !source.contains(&status_line_call),
            "agent.rs must not format tool-status lines for the assistant answer stream"
        );
        assert!(
            !source.contains(&stream_status_var),
            "agent.rs must not clone a separate stream-status sender for tool progress"
        );
    }

    #[test]
    fn test_reloads_replace_registry_not_just_instance_skills() {
        // Ensure reload paths use `*registry = new_reg` (full replacement)
        // rather than only updating instance_skills while leaving stale bundled entries.
        let source = include_str!("agent.rs");
        // Each reload/write handler should do `*skills = new_reg` or `*agents = new_reg`
        let skills_replace = source.matches("*skills = new_reg").count();
        let agents_replace = source.matches("*agents = new_reg").count();
        assert!(
            skills_replace >= 2,
            "all skill reload paths must replace the entire registry: found {skills_replace}"
        );
        assert!(
            agents_replace >= 2,
            "all agent reload paths must replace the entire registry: found {agents_replace}"
        );
    }

    #[test]
    fn test_now_iso8601_is_valid_rfc3339() {
        let ts = Agent::now_iso8601_static();
        chrono::DateTime::parse_from_rfc3339(&ts).unwrap();
        assert!(ts.ends_with('Z'), "timestamp must be UTC: {}", ts);
    }

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

    #[test]
    fn test_read_skill_file_validates_skill_name() {
        // validate_skill_name is reused — just verify the boundary
        assert!(validate_skill_name("valid-skill").is_ok());
        assert!(validate_skill_name("../evil").is_err());
        assert!(validate_skill_name("").is_err());
    }

    #[test]
    fn test_read_skill_file_validates_relative_path() {
        assert!(validate_skill_path("SKILL.md").is_ok());
        assert!(validate_skill_path("style-guide.md").is_ok());
        assert!(validate_skill_path("../other-skill/SKILL.md").is_err());
        assert!(validate_skill_path("/etc/passwd").is_err());
        assert!(validate_skill_path("").is_err());
    }

    #[test]
    fn test_subagent_tool_whitelist_always_includes_read_skill_file() {
        // read_skill_file is always available to subagents regardless of whitelist
        let declared: Vec<String> = vec!["mcp_threads_post".to_string()];
        let effective = effective_subagent_tools(&declared);
        assert!(effective.contains(&"read_skill_file".to_string()));
        assert!(effective.contains(&"mcp_threads_post".to_string()));
    }

    #[test]
    fn test_subagent_tool_whitelist_empty_gets_read_tools() {
        let declared: Vec<String> = vec![];
        let effective = effective_subagent_tools(&declared);
        assert!(effective.contains(&"read_skill_file".to_string()));
        assert!(effective.contains(&"read_agent_file".to_string()));
    }

    #[test]
    fn test_subagent_tool_whitelist_deduplicates_read_skill_file() {
        // If the skill already lists read_skill_file, it shouldn't appear twice
        let declared = vec!["read_skill_file".to_string(), "mcp_something".to_string()];
        let effective = effective_subagent_tools(&declared);
        let count = effective.iter().filter(|t| *t == "read_skill_file").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_missing_subagent_tools_detected() {
        // If a declared tool is not in all_possible, it should be detectable.
        let declared = vec![
            "read_skill_file".to_string(),
            "mcp_nonexistent_tool".to_string(),
        ];
        let available: Vec<String> = vec!["read_skill_file".to_string()]; // mcp_nonexistent_tool missing
        let missing = missing_subagent_tools(&declared, &available);
        assert_eq!(missing, vec!["mcp_nonexistent_tool".to_string()]);
    }

    #[test]
    fn test_missing_subagent_tools_empty_when_all_present() {
        let declared = vec!["read_skill_file".to_string()];
        let available = vec!["read_skill_file".to_string(), "write_file".to_string()];
        let missing = missing_subagent_tools(&declared, &available);
        assert!(missing.is_empty());
    }

    #[test]
    fn test_assemble_tokens_joins_correctly() {
        let tokens = ["Hello", " ", "world", "!"];
        let assembled: String = tokens.concat();
        assert_eq!(assembled, "Hello world!");
    }

    #[tokio::test]
    async fn test_load_skills_from_single_instance_dir() {
        let dir = tempfile::tempdir().unwrap();
        let instance_dir = dir.path().join("instance-skills");

        tokio::fs::create_dir_all(instance_dir.join("my-skill"))
            .await
            .unwrap();
        tokio::fs::write(instance_dir.join("my-skill/SKILL.md"), "instance content")
            .await
            .unwrap();

        let registry =
            crate::skills::loader::load_skills_from_dir(&instance_dir, instance_dir.clone())
                .await
                .unwrap();

        assert_eq!(registry.len(), 1);
        let skill = registry.get("my-skill").unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "instance content");
    }

    #[test]
    fn test_is_compacted_regurgitation_new_plain_text_format_detected() {
        let raw = "[RustFox compacted: previous invoke_subagent call with 1200 bytes of arguments]";
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        assert!(is_compacted_regurgitation(raw, &parsed));
    }

    #[test]
    fn test_is_compacted_regurgitation_old_json_format_detected() {
        let raw = r#"{"_rustfox_compacted_arguments": true, "tool_name": "invoke_subagent", "original_char_count": 1200, "preview": "{\"skill\": \"test\"}"}"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(is_compacted_regurgitation(raw, &parsed));
    }

    #[test]
    fn test_is_compacted_regurgitation_old_json_false_not_detected() {
        let raw = r#"{"_rustfox_compacted_arguments": false, "tool_name": "invoke_subagent"}"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(!is_compacted_regurgitation(raw, &parsed));
    }

    #[test]
    fn test_is_compacted_regurgitation_old_json_missing_field_not_detected() {
        let raw = r#"{"tool_name": "invoke_subagent", "original_char_count": 1200}"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(!is_compacted_regurgitation(raw, &parsed));
    }

    #[test]
    fn test_is_compacted_regurgitation_normal_json_not_detected() {
        let raw = r#"{"skill": "novel-writer", "prompt": "write a chapter"}"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(!is_compacted_regurgitation(raw, &parsed));
    }

    #[test]
    fn test_is_compacted_regurgitation_empty_string_not_detected() {
        let raw = "";
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        assert!(!is_compacted_regurgitation(raw, &parsed));
    }

    // ---- Available Agents section builder ----

    #[test]
    fn test_format_available_agents_section_both_empty_returns_none() {
        let section = format_available_agents_section("", "");
        assert!(
            section.is_none(),
            "expected None when both inputs are empty"
        );
    }

    #[test]
    fn test_format_available_agents_section_only_subagent_nonempty() {
        let section = format_available_agents_section("- sub line", "").expect("expected Some");
        assert!(section.contains("# Available Agents"));
        assert!(section.contains("- sub line"));
        assert!(section.contains("All available agents are listed below"));
        assert!(section.contains("invoke_agent"));
        // No separator needed when only one source is present
        assert!(!section.contains("- sub line\n\n"));
    }

    #[test]
    fn test_format_available_agents_section_only_agents_nonempty() {
        let section = format_available_agents_section("", "- agent line").expect("expected Some");
        assert!(section.contains("# Available Agents"));
        assert!(section.contains("- agent line"));
        assert!(section.contains("All available agents are listed below"));
    }

    #[test]
    fn test_format_available_agents_section_both_nonempty_merged() {
        let section =
            format_available_agents_section("- sub line", "- agent line").expect("expected Some");

        // Header and preamble are present
        assert!(section.contains("# Available Agents"));
        assert!(section.contains("All available agents are listed below"));
        assert!(section.contains("DO NOT try to list agent directories"));

        // Both line sources appear
        assert!(section.contains("- sub line"));
        assert!(section.contains("- agent line"));

        // Subagent block appears before agent block, separated by at least one newline
        let sub_idx = section.find("- sub line").expect("subagent line present");
        let agent_idx = section.find("- agent line").expect("agent line present");
        assert!(
            sub_idx < agent_idx,
            "subagent lines must precede agent lines"
        );
        let between = &section[sub_idx..agent_idx];
        assert!(
            between.contains('\n'),
            "expected a newline separator between subagent and agent lines, got {between:?}"
        );
    }

    #[test]
    fn test_format_available_agents_section_uses_shared_preamble() {
        // The preamble should come from `format_listed_section("agent", ...)`.
        let section = format_available_agents_section("- sub", "- ag").expect("expected Some");
        let shared = format_listed_section(
            "agent",
            "Delegate these tasks to specialized agents using `invoke_agent`:",
        );
        assert!(
            section.contains(&shared),
            "section should embed the shared preamble exactly"
        );
    }

    #[test]
    fn test_build_btw_context_removes_orphaned_tool_use() {
        use crate::llm::{FunctionCall, ToolCall};
        let assistant = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "orphaned_call".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"x"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let msgs = vec![assistant];
        let result = build_btw_context(&msgs, "test question");
        let forked = &result[..result.len() - 1];
        for msg in forked {
            assert!(
                msg.tool_calls.as_ref().is_none_or(|c| c.is_empty()),
                "orphaned tool_use should be stripped"
            );
        }
    }

    #[test]
    fn test_build_btw_context_preserves_matched_tool_calls() {
        use crate::llm::{FunctionCall, ToolCall};
        let tool_msg = ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::from_text("result")),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
        };
        let assistant = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"x"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let msgs = vec![tool_msg, assistant];
        let result = build_btw_context(&msgs, "test question");
        let forked = &result[..result.len() - 1];
        let has_tool_calls = forked
            .iter()
            .any(|m| m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()));
        assert!(has_tool_calls, "matched tool_use should be preserved");
    }

    #[test]
    fn test_build_btw_context_text_only_messages_unchanged() {
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text("hello")),
            tool_calls: None,
            tool_call_id: None,
        }];
        let result = build_btw_context(&msgs, "question");
        assert!(result.len() > msgs.len(), "should append question");
        assert_eq!(
            result[0].content.as_ref().map(|c| c.as_text()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_build_btw_context_empty_list() {
        let result = build_btw_context(&[], "question");
        assert_eq!(result.len(), 1, "only the question message");
        assert!(result[0]
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default()
            .contains("question"));
    }
}
