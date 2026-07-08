use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use teloxide::payloads::{SendDocumentSetters, SendMessageSetters};
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, InputFile};
use teloxide::Bot;

use crate::agent_prompt::{
    build_compact_boundary_marker, build_compact_summary_prompt, estimate_prompt_bytes,
    prepare_messages_for_llm, recovery_nudge_for, should_auto_compact, ConversationMeta,
    PreparedPrompt, PRESERVED_TOOL_GROUPS,
};
use crate::config::Config;
use crate::langsmith::LangSmithClient;
use crate::llm::{
    is_empty_assistant_response, ChatMessage, ContentPart, FunctionDefinition, LlmClient,
    MessageContent, ToolDefinition,
};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::platform::IncomingMessage;
use crate::scheduler::reminders::ScheduledTaskStore;
use crate::scheduler::Scheduler;
use crate::skills::{format_listed_section, SkillRegistry};
use crate::tools;
use std::collections::HashMap;
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;

/// Number of context snippets to retrieve from conversation history for
/// compaction summarization.
const COMPACTION_RAG_LIMIT: usize = 5;

/// A request dispatched from a fire closure to the background job runner.
pub struct ScheduledJobRequest {
    pub incoming: IncomingMessage,
    pub bot: Arc<Bot>,
    pub task_id: String,
    pub is_recurring: bool,
    pub task_store: ScheduledTaskStore,
}

/// A running shell command that can be cancelled by the user via a callback button.
pub struct RunningCommand {
    pub cancel_tx: oneshot::Sender<()>,
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
    #[allow(dead_code)]
    pub self_weak: Weak<Agent>,
    /// Sender for dispatching scheduled job work to the background runner.
    pub job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
    pub langsmith: Arc<LangSmithClient>,
    pub restart_pending: AtomicBool,
    pub soul_updated: AtomicBool,
    pub current_model: tokio::sync::RwLock<String>,
    pub config_path: PathBuf,
    pub running_commands: Arc<tokio::sync::Mutex<HashMap<String, RunningCommand>>>,
    /// Per-user CancellationTokens for /stop — created at process_message entry,
    /// removed on exit. Checked at each iteration boundary.
    pub cancel_token_registry: Arc<tokio::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// Per-user pending injection messages (Steer/Inject), max 10 per user.
    /// When a non-command message arrives while processing is active, it's queued here.
    pub pending_injections: Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
}

/// A task parsed from the spawn_agents tool arguments, after validation.
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
            running_commands: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cancel_token_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_injections: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
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
    async fn build_subagent_system_prompt(&self, agent_instructions: &str) -> String {
        let mut prompt = self.build_system_context().await;
        prompt.push_str("\n\n");
        prompt.push_str(agent_instructions);
        prompt
    }

    /// Resolve the base directory for a skill/agent by checking the registry.
    /// Falls back to the configured directory if not found (for newly-created skills).
    fn resolve_skill_base_dir(
        &self,
        name: &str,
        config_dir: &Path,
        skills_lock: &SkillRegistry,
    ) -> PathBuf {
        skills_lock
            .base_dir(name)
            .unwrap_or(config_dir)
            .to_path_buf()
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

    /// Remove cancel token for a user (called on process_message exit).
    pub async fn clear_cancel_token(&self, user_id: &str) {
        self.cancel_token_registry
            .lock()
            .await
            .remove(user_id);
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
        let parsed_chat_id: ChatId = incoming
            .chat_id
            .parse::<i64>()
            .map(ChatId)
            .unwrap_or(ChatId(0));

        // Get or create persistent conversation
        let conversation_id = self
            .memory
            .get_or_create_conversation(platform, user_id)
            .await?;

        // Load existing messages from memory
        let mut messages = self
            .memory
            .load_messages_with_limit(&conversation_id, self.config.memory.max_raw_messages)
            .await?;

        // Always build the system prompt from the live registry.
        // For new conversations: save to DB and push.
        // For existing conversations: refresh messages[0] in-memory only
        //   (DB keeps the historical system message intact).
        let current_system_prompt = self.build_system_prompt().await;
        if messages.is_empty() {
            let system_msg = ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::from_text(current_system_prompt)),
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
                system_msg.content = Some(MessageContent::from_text(current_system_prompt));
            }
        }

        // RAG: auto-retrieve relevant past messages and inject into system prompt
        if !incoming.text.is_empty() {
            // Take last 6 messages for rewrite context (skip system messages)
            let filtered_msgs: Vec<_> = messages
                .iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .cloned()
                .collect();
            let rewrite_start = filtered_msgs.len().saturating_sub(6);
            let recent_for_rewrite = filtered_msgs[rewrite_start..].to_vec();

            // Determine if query rewriting is enabled: per-user setting overrides config default.
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
                if let Some(system_msg) = messages.iter_mut().find(|m| m.role == "system") {
                    if let Some(MessageContent::Text(ref mut s)) = system_msg.content {
                        s.push_str("\n\n");
                        s.push_str(&rag_block);
                    }
                }
            }
        }

        // Process attachments (images → vision parts or OCR text; PDFs/DOCXs → extracted text)
        let supports_vision = {
            let current = self.current_model.read().await;
            let (provider, _) = self.registry.resolve_model(&current);
            provider.supports_vision()
        };

        let (attachment_text, image_parts) = if !incoming.attachments.is_empty() {
            crate::file_processor::process_attachments(
                &incoming.attachments,
                &incoming.text,
                &self.config,
                &self.memory,
                supports_vision, // NEW parameter
            )
            .await
        } else {
            (String::new(), vec![])
        };

        // Build user message content
        let user_msg_content = if image_parts.is_empty() {
            // Text-only path: combine user text with any extracted document text
            let mut combined = incoming.text.clone();
            if !attachment_text.is_empty() {
                combined.push_str("\n\n");
                combined.push_str(&attachment_text);
            }
            MessageContent::from_text(combined)
        } else {
            // Multi-modal path: text part + image content parts
            let mut parts: Vec<ContentPart> = Vec::new();
            let mut text_content = incoming.text.clone();
            if !attachment_text.is_empty() {
                text_content.push_str("\n\n");
                text_content.push_str(&attachment_text);
            }
            if !text_content.is_empty() {
                parts.push(ContentPart::Text { text: text_content });
            }
            parts.extend(image_parts);
            MessageContent::Parts(parts)
        };

        // Save a text-only version to DB (avoid storing base64 image data in message history)
        let db_content = if incoming.attachments.is_empty() {
            user_msg_content.clone()
        } else {
            let mut db_text = incoming.text.clone();
            if !attachment_text.is_empty() {
                db_text.push_str("\n\n[Attachment processed]");
            }
            MessageContent::from_text(db_text)
        };
        let db_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(db_content),
            tool_calls: None,
            tool_call_id: None,
        };
        self.memory.save_message(&conversation_id, &db_msg).await?;

        // Push the full message (with image parts if any) to in-memory context
        let user_msg = ChatMessage {
            role: "user".to_string(),
            content: Some(user_msg_content),
            tool_calls: None,
            tool_call_id: None,
        };
        messages.push(user_msg);

        // Compaction state for this conversation session (persists across iterations)
        let mut conv_meta = ConversationMeta::new();

        // Gather all tool definitions
        let mut all_tools: Vec<ToolDefinition> = tools::builtin_tool_definitions();
        all_tools.extend(self.mcp.tool_definitions());
        all_tools.extend(self.memory_tool_definitions());
        all_tools.extend(self.scheduling_tool_definitions());
        all_tools.extend(self.skill_tool_definitions());

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

        // Agentic loop — keep calling LLM until we get a non-tool response
        let max_iterations = self.config.max_iterations();
        let empty_response_retry_limit = self.config.empty_response_retry_limit();
        let mut iteration_count = 0u32;
        let mut tool_call_count = 0u32;

        // Reset soul-update flag for this session
        self.soul_updated
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Register cancel token for /stop support
        let cancel_token = self.register_cancel_token(user_id).await;

        // Resolve context_window once before the loop (can't .await inside the loop)
        let context_window = {
            let model = self.current_model.read().await;
            self.registry.effective_context_window(&model)
        };

        'outer: for iteration in 0..max_iterations {
            debug!(
                "Trying iteration {}: messages length: {}",
                iteration,
                messages.len()
            );

            // CHECK: cancelled by /stop?
            if cancel_token.is_cancelled() {
                info!(
                    user_id = %user_id,
                    iteration,
                    "Processing cancelled by user via /stop"
                );
                break;
            }

            // CHECK: pending injections from user?
            let injections = self.drain_injections(user_id).await;
            for text in &injections {
                let inject_msg = ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::from_text(format!(
                        "**[User injected mid-processing]:** {}",
                        text
                    ))),
                    tool_calls: None,
                    tool_call_id: None,
                };
                // Save to persistent memory
                self.memory
                    .save_message(&conversation_id, &inject_msg)
                    .await
                    .ok();
                messages.push(inject_msg);
            }

            // --- Empty response recovery: retry loop ---
            let mut retry_count = 0u32;
            let response: ChatMessage;

            // Tier 3: auto-compact (async, LLM call) — before Tiers 1-2
            conv_meta.current_turn = iteration as usize;
            if should_auto_compact(&messages, &conv_meta, context_window) {
                // Capture pre-compact metrics
                let messages_before = messages.clone();
                let messages_before_bytes = estimate_prompt_bytes(&messages_before);

                if let Ok(compacted) = Self::auto_compact_conversation(
                    &self.llm,
                    &self.memory,
                    &conversation_id,
                    &messages,
                    context_window,
                )
                .await
                {
                    conv_meta.last_compact_turn = iteration as usize;
                    messages = compacted;
                    info!(
                        "Tier 3 auto-compact applied: {} messages reduced",
                        messages.len(),
                    );

                    // LangSmith logging for Tier 3
                    let compacted_bytes = estimate_prompt_bytes(&messages);
                    let compact_run_id = uuid::Uuid::new_v4().to_string();
                    self.langsmith.start_run(crate::langsmith::RunParams {
                        id: compact_run_id.clone(),
                        name: "auto_compact".to_string(),
                        run_type: crate::langsmith::RunType::Chain,
                        parent_run_id: Some(chain_run_id.clone()),
                        inputs: serde_json::json!({
                            "tier": 3,
                            "pre_bytes": messages_before_bytes,
                            "post_bytes": compacted_bytes,
                            "pre_count": messages_before.len(),
                            "post_count": messages.len(),
                        }),
                        session_name: ls_project.clone(),
                        start_time: Self::now_iso8601_static(),
                    });
                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                        id: compact_run_id,
                        outputs: Some(serde_json::json!({
                            "tier": 3,
                            "delta_bytes": (messages_before_bytes as i64 - compacted_bytes as i64).max(0),
                            "delta_messages": messages_before.len() as i64 - messages.len() as i64,
                        })),
                        error: None,
                        end_time: Self::now_iso8601_static(),
                    });
                }
            }

            // Tiers 1-2: sync compaction
            let base_prompt = prepare_messages_for_llm(&messages, context_window);

            loop {
                // CHECK: cancelled while retrying?
                if cancel_token.is_cancelled() {
                    info!("Cancelled during retry loop — breaking");
                    break 'outer;
                }

                // Clone the base prompt for this retry attempt
                let mut prompt = base_prompt.clone();

                // On retry, append recovery nudge to in-memory prompt only
                if retry_count > 0 {
                    let nudge = recovery_nudge_for(&messages);
                    prompt.messages.push(nudge);
                    // Recompute stats after adding nudge
                    prompt.stats.prepared_message_count = prompt.messages.len();
                    prompt.stats.prepared_prompt_chars = estimate_prompt_bytes(&prompt.messages);
                }

                // Log prompt compaction if applied
                if prompt.stats.compaction_applied {
                    info!(
                        original_message_count = prompt.stats.original_message_count,
                        prepared_message_count = prompt.stats.prepared_message_count,
                        original_prompt_chars = prompt.stats.original_prompt_chars,
                        prepared_prompt_chars = prompt.stats.prepared_prompt_chars,
                        "Prompt compaction applied"
                    );
                }

                // --- LangSmith: start llm run (child of chain) ---
                let llm_run_id = uuid::Uuid::new_v4().to_string();
                let llm_start = Self::now_iso8601_static();
                self.langsmith.start_run(crate::langsmith::RunParams {
                    id: llm_run_id.clone(),
                    name: "llm_call".to_string(),
                    run_type: crate::langsmith::RunType::Llm,
                    parent_run_id: Some(chain_run_id.clone()),
                    inputs: serde_json::json!({
                        "messages": prompt.messages,
                        "metadata": {
                            "retry_count": retry_count,
                            "message_count": prompt.stats.prepared_message_count,
                            "prompt_chars": prompt.stats.prepared_prompt_chars,
                        }
                    }),
                    session_name: ls_project.clone(),
                    start_time: llm_start,
                });

                // Call LLM with prepared prompt (with fallback chain)
                let model = self.current_model.read().await.clone();
                let fallback_chain = &self.config.fallback.chain;
                let mut last_error = None;

                let completion_result = 'fallback: {
                    for attempt in 0..=fallback_chain.len() {
                        let current_model = if attempt == 0 {
                            model.clone()
                        } else {
                            fallback_chain[attempt - 1].clone()
                        };

                        match self
                            .llm
                            .chat_completion_with_model(
                                &prompt.messages,
                                &all_tools,
                                &current_model,
                            )
                            .await
                        {
                            Ok(c) => {
                                if attempt > 0 {
                                    tracing::info!(
                                        "Fallback succeeded: switched from '{}' to '{}'",
                                        model,
                                        current_model
                                    );
                                    *self.current_model.write().await = current_model.clone();
                                }
                                break 'fallback Ok(c);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Model '{}' failed (attempt {}/{}): {}",
                                    current_model,
                                    attempt,
                                    fallback_chain.len(),
                                    e
                                );
                                last_error = Some(e);
                                if attempt == fallback_chain.len() {
                                    break 'fallback Err(last_error.unwrap());
                                }
                                continue;
                            }
                        }
                    }
                    Err(last_error.unwrap())
                };

                // Handle LLM transport/API errors
                let mut recovered_from_413 = false;

                let completion = match completion_result {
                    Ok(c) => c,
                    Err(e) => {
                        let err_str = format!("{:#}", e);
                        let is_413 = err_str.contains("413")
                            || err_str.to_lowercase().contains("prompt too long")
                            || err_str.to_lowercase().contains("context length")
                            || err_str.to_lowercase().contains("maximum context");
                        if is_413 && !conv_meta.has_attempted_reactive_compact {
                            conv_meta.has_attempted_reactive_compact = true;
                            let messages_before_compact = messages.len();
                            match Self::reactive_compact(
                                &self.llm,
                                &self.memory,
                                &conversation_id,
                                &messages,
                                context_window,
                            )
                            .await
                            {
                                Ok(compacted) => {
                                    let compacted_len = compacted.len();
                                    messages = compacted;
                                    recovered_from_413 = true;

                                    // LangSmith logging for Tier 4
                                    let compact_run_id = uuid::Uuid::new_v4().to_string();
                                    self.langsmith.start_run(crate::langsmith::RunParams {
                                        id: compact_run_id.clone(),
                                        name: "reactive_compact".to_string(),
                                        run_type: crate::langsmith::RunType::Chain,
                                        parent_run_id: Some(chain_run_id.clone()),
                                        inputs: serde_json::json!({
                                            "tier": 4,
                                            "reason": err_str,
                                            "pre_count": messages_before_compact,
                                        }),
                                        session_name: ls_project.clone(),
                                        start_time: Self::now_iso8601_static(),
                                    });
                                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                                        id: compact_run_id,
                                        outputs: Some(serde_json::json!({
                                            "tier": 4,
                                            "post_count": compacted_len,
                                        })),
                                        error: None,
                                        end_time: Self::now_iso8601_static(),
                                    });
                                }
                                Err(compact_err) => {
                                    warn!("Reactive compact failed: {}", compact_err);
                                }
                            }
                        }
                        if !recovered_from_413 {
                            self.langsmith.end_run(crate::langsmith::EndRunParams {
                                id: llm_run_id,
                                outputs: None,
                                error: Some(err_str.clone()),
                                end_time: Self::now_iso8601_static(),
                            });
                            self.langsmith.end_run(crate::langsmith::EndRunParams {
                                id: chain_run_id,
                                outputs: None,
                                error: Some(err_str),
                                end_time: Self::now_iso8601_static(),
                            });
                            self.clear_cancel_token(user_id).await;
                            return Err(e);
                        }
                        // recovered_from_413 is true but the compiler can't see this;
                        // return a dummy completion (never used because of continue below)
                        crate::llm::ChatCompletion {
                            message: ChatMessage {
                                role: String::new(),
                                content: None,
                                tool_calls: None,
                                tool_call_id: None,
                            },
                            finish_reason: None,
                            model: String::new(),
                        }
                    }
                };

                if recovered_from_413 {
                    // End the leaked llm_run before continuing
                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                        id: llm_run_id,
                        outputs: None,
                        error: Some(
                            "413 context exceeded — recovered via Tier 4 compact".to_string(),
                        ),
                        end_time: Self::now_iso8601_static(),
                    });
                    continue;
                }

                // Check if response is empty (no content and no tool calls)
                if is_empty_assistant_response(&completion.message) {
                    warn!(
                        user_id = %user_id,
                        iteration,
                        retry_count,
                        "LLM returned empty content with no tool calls"
                    );

                    // End LLM run with error and diagnostic outputs
                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                        id: llm_run_id,
                        outputs: Some(Self::llm_run_outputs(
                            Some(&completion),
                            &prompt,
                            retry_count,
                        )),
                        error: Some(
                            "Empty assistant response (no content and no tool calls)".to_string(),
                        ),
                        end_time: Self::now_iso8601_static(),
                    });

                    // Check retry limit
                    if retry_count >= empty_response_retry_limit {
                        warn!(
                            user_id = %user_id,
                            retry_count,
                            limit = empty_response_retry_limit,
                            "Exhausted empty response retry limit"
                        );

                        // End chain run with error
                        self.langsmith.end_run(crate::langsmith::EndRunParams {
                            id: chain_run_id,
                            outputs: None,
                            error: Some(format!(
                                "Unable to get valid response after {} attempts",
                                retry_count + 1
                            )),
                            end_time: Self::now_iso8601_static(),
                        });

                        self.clear_cancel_token(user_id).await;
                        return Err(anyhow::anyhow!(
                            "Unable to get a valid response from the AI model after {} attempts. \
                             Your conversation history has been saved. Please try rephrasing your \
                             request or continue from where we left off.",
                            retry_count + 1
                        ));
                    }

                    // Retry
                    retry_count += 1;
                    continue;
                }

                // Valid response received
                if retry_count > 0 {
                    info!(
                        user_id = %user_id,
                        retry_count,
                        "Recovered from empty response after retry"
                    );
                }

                // --- LangSmith: end llm run (success) ---
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: llm_run_id,
                    outputs: Some(Self::llm_run_outputs(
                        Some(&completion),
                        &prompt,
                        retry_count,
                    )),
                    error: None,
                    end_time: Self::now_iso8601_static(),
                });

                response = completion.message;
                break;
            }

            if let Some(tool_calls) = &response.tool_calls {
                if !tool_calls.is_empty() {
                    tool_call_count += tool_calls.len() as u32;
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

                    // --- Parallel-aware tool execution ---
                    // Clone tool call data to avoid lifetime issues in async move blocks
                    let tool_call_data: Vec<(usize, String, serde_json::Value, String)> =
                        tool_calls
                            .iter()
                            .enumerate()
                            .map(|(i, tc)| {
                                let name = tc.function.name.clone();
                                let args = serde_json::from_str(&tc.function.arguments)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                                let id = tc.id.clone();
                                (i, name, args, id)
                            })
                            .collect();

                    // Classify: agent-spawning calls run in parallel, others sequential
                    let is_agent_tool =
                        |name: &str| -> bool { matches!(name, "spawn_agents" | "invoke_agent") };

                    let mut agent_group: Vec<(usize, String, serde_json::Value, String)> =
                        Vec::new();
                    let mut other_group: Vec<(usize, String, serde_json::Value, String)> =
                        Vec::new();

                    for (i, name, args, id) in tool_call_data {
                        if is_agent_tool(&name) {
                            agent_group.push((i, name, args, id));
                        } else {
                            other_group.push((i, name, args, id));
                        }
                    }

                    let mut all_results: Vec<(usize, ChatMessage)> = Vec::new();

                    // --- Run subagent-spawning calls in PARALLEL ---
                    if !agent_group.is_empty() {
                        let futs: Vec<_> = agent_group
                            .into_iter()
                            .map(|(idx, name, args, id)| {
                                let chain_run_id = chain_run_id.clone();
                                let ls_project = ls_project.clone();
                                let tool_event_tx = tool_event_tx.clone();
                                async move {
                                    let tool_run_id = uuid::Uuid::new_v4().to_string();
                                    self.langsmith.start_run(crate::langsmith::RunParams {
                                        id: tool_run_id.clone(),
                                        name: name.clone(),
                                        run_type: crate::langsmith::RunType::Tool,
                                        parent_run_id: Some(chain_run_id.clone()),
                                        inputs: serde_json::json!({ "arguments": args }),
                                        session_name: ls_project.clone(),
                                        start_time: Self::now_iso8601_static(),
                                    });

                                    if let Some(ref tx) = tool_event_tx {
                                        let args_preview =
                                            crate::platform::tool_notifier::format_args_preview(
                                                &args.to_string(),
                                            );
                                        let _ = tx.try_send(
                                            crate::platform::tool_notifier::ToolEvent::Started {
                                                name: name.clone(),
                                                args_preview,
                                                arguments_json: args.to_string(),
                                            },
                                        );
                                    }

                                    let result = self
                                        .execute_tool(&name, &args, user_id, parsed_chat_id)
                                        .await;

                                    if let Some(ref tx) = tool_event_tx {
                                        let success = !result.starts_with("Error");
                                        let _ = tx.try_send(
                                            crate::platform::tool_notifier::ToolEvent::Completed {
                                                name: name.clone(),
                                                success,
                                            },
                                        );
                                    }

                                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                                        id: tool_run_id,
                                        outputs: Some(serde_json::json!({ "result": result })),
                                        error: None,
                                        end_time: Self::now_iso8601_static(),
                                    });

                                    (
                                        idx,
                                        ChatMessage {
                                            role: "tool".to_string(),
                                            content: Some(MessageContent::from_text(result)),
                                            tool_calls: None,
                                            tool_call_id: Some(id),
                                        },
                                    )
                                }
                            })
                            .collect();
                        let parallel_results = futures::future::join_all(futs).await;
                        all_results.extend(parallel_results);
                    }

                    // --- Non-agent tool calls run SEQUENTIALLY ---
                    for (idx, name, args, id) in other_group {
                        // Compaction regurgitation check
                        if is_compacted_regurgitation(&args.to_string(), &args) {
                            let tool_msg = ChatMessage {
                                role: "tool".to_string(),
                                content: Some(MessageContent::from_text(REGURGITATION_ERROR_MSG)),
                                tool_calls: None,
                                tool_call_id: Some(id),
                            };
                            all_results.push((idx, tool_msg));
                            continue;
                        }

                        // LangSmith: start tool run
                        let tool_run_id = uuid::Uuid::new_v4().to_string();
                        self.langsmith.start_run(crate::langsmith::RunParams {
                            id: tool_run_id.clone(),
                            name: name.clone(),
                            run_type: crate::langsmith::RunType::Tool,
                            parent_run_id: Some(chain_run_id.clone()),
                            inputs: serde_json::json!({ "arguments": args }),
                            session_name: ls_project.clone(),
                            start_time: Self::now_iso8601_static(),
                        });

                        // Notify tool start
                        if let Some(ref tx) = tool_event_tx {
                            let args_preview = crate::platform::tool_notifier::format_args_preview(
                                &args.to_string(),
                            );
                            let _ =
                                tx.try_send(crate::platform::tool_notifier::ToolEvent::Started {
                                    name: name.clone(),
                                    args_preview,
                                    arguments_json: args.to_string(),
                                });
                        }

                        let result = self
                            .execute_tool(&name, &args, user_id, parsed_chat_id)
                            .await;

                        // Notify tool completion
                        if let Some(ref tx) = tool_event_tx {
                            let success = !result.starts_with("Error");
                            let _ =
                                tx.try_send(crate::platform::tool_notifier::ToolEvent::Completed {
                                    name: name.clone(),
                                    success,
                                });
                        }

                        info!("Tool '{}' result length: {} chars", name, result.len());
                        debug!("Tool '{}' result: {}", name, result);

                        // LangSmith: end tool run
                        self.langsmith.end_run(crate::langsmith::EndRunParams {
                            id: tool_run_id,
                            outputs: Some(serde_json::json!({ "result": result })),
                            error: None,
                            end_time: Self::now_iso8601_static(),
                        });

                        let tool_msg = ChatMessage {
                            role: "tool".to_string(),
                            content: Some(MessageContent::from_text(result)),
                            tool_calls: None,
                            tool_call_id: Some(id),
                        };
                        all_results.push((idx, tool_msg));
                    }

                    // Sort results by original index and push to memory + messages
                    all_results.sort_by_key(|(i, _)| *i);
                    for (_idx, tool_msg) in all_results {
                        self.memory
                            .save_message(&conversation_id, &tool_msg)
                            .await?;
                        messages.push(tool_msg);
                    }

                    iteration_count = iteration + 1;
                    continue;
                }
            }

            // Final response — no tool calls
            let content = response
                .content
                .as_ref()
                .map(|c| c.as_text())
                .unwrap_or_default();

            // Stream the final response directly from the already-complete content.
            // Previously this made a second chat_stream() API call, which could return
            // Ok(partial) if the SSE connection was dropped mid-generation (e.g. after an
            // 11-minute kimi-k2.5 response), silently saving a truncated reply.
            // Now we pipe the guaranteed-complete content through the channel in small
            // chunks so Telegram still sees tokens arrive progressively.
            let final_content = if let Some(tx) = stream_token_tx {
                LlmClient::stream_text(content.clone(), tx).await.ok();
                content.clone()
            } else {
                content.clone()
            };

            // Save the delivered content to persistent memory
            let save_msg = crate::llm::ChatMessage {
                role: response.role.clone(),
                content: Some(crate::llm::MessageContent::Text(final_content.clone())),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: response.tool_call_id.clone(),
            };
            self.memory
                .save_message(&conversation_id, &save_msg)
                .await?;

            // --- LangSmith: end chain run (success) ---
            self.langsmith.end_run(crate::langsmith::EndRunParams {
                id: chain_run_id,
                outputs: Some(serde_json::json!({
                    "response": final_content,
                    "iterations": iteration,
                })),
                error: None,
                end_time: Self::now_iso8601_static(),
            });

            // --- Self-learning: post-task skill extraction (background) ---
            if self.config.learning.skill_extraction_enabled
                && tool_call_count >= self.config.learning.skill_extraction_threshold
            {
                if let Some(agent) = self.self_weak.upgrade() {
                    let msgs_clone = messages.clone();
                    tokio::spawn(async move {
                        let _extraction_result = tokio::time::timeout(
                            std::time::Duration::from_secs(60),
                            crate::learning::post_task_skill_extractor(
                                &agent.llm,
                                &agent.config.skills.directory,
                                &agent.skills,
                                &msgs_clone,
                                tool_call_count,
                            ),
                        )
                        .await;
                    });
                }
            }

            // --- Self-learning: session-end soul reflection (background) ---
            if !self.soul_updated.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(agent) = self.self_weak.upgrade() {
                    let msgs = messages.clone();
                    let uid = user_id.to_string();
                    let cid = parsed_chat_id;
                    tokio::spawn(async move {
                        let mut reflection_messages = msgs;
                        reflection_messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: Some(MessageContent::Text(
                                "Review the conversation above. Did you learn anything about the \
                                 user or yourself that should be recorded in SOUL.md, AGENTS.md, \
                                 or USER.md? If yes, respond with EXACTLY:\n\
                                 UPDATE_SOUL: <file_name>\n\
                                 CONTENT:\n\
                                 <content to append>\n\n\
                                 If nothing worth recording, respond with: NO_UPDATE"
                                    .to_string(),
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        });

                        if let Ok(reflection_response) =
                            agent.llm.chat(&reflection_messages, &[]).await
                        {
                            if let Some(content) = reflection_response.content {
                                let text = content.as_text();
                                if let Some(rest) = text.strip_prefix("UPDATE_SOUL:") {
                                    let parts: Vec<&str> = rest.splitn(2, '\n').collect();
                                    if parts.len() == 2 {
                                        let file_name = parts[0].trim();
                                        let append_content = parts[1]
                                            .strip_prefix("CONTENT:\n")
                                            .or_else(|| parts[1].strip_prefix("CONTENT:"))
                                            .unwrap_or(parts[1])
                                            .trim();
                                        let args = serde_json::json!({
                                            "file_name": file_name,
                                            "content": append_content,
                                            "mode": "append"
                                        });
                                        let _ = agent
                                            .execute_tool("update_soul_file", &args, &uid, cid)
                                            .await;
                                    }
                                }
                            }
                        }
                    });
                }
            }

            self.clear_cancel_token(user_id).await;
            return Ok(final_content);
        }

        // Reached max iterations
        warn!(
            user_id = %user_id,
            max_iterations = max_iterations,
            iteration_count = iteration_count,
            "Reached max iterations without final text response"
        );

        // --- LangSmith: end chain run (max iterations) ---
        self.langsmith.end_run(crate::langsmith::EndRunParams {
            id: chain_run_id,
            outputs: None,
            error: Some(format!("Reached max iterations ({})", max_iterations)),
            end_time: Self::now_iso8601_static(),
        });

        self.clear_cancel_token(user_id).await;
        Ok("I've reached the maximum number of tool call iterations. Please try rephrasing your request.".to_string())
    }

    /// Tier 3: Auto-compact via LLM summarization.
    ///
    /// `_context_window` is reserved for future threshold-based
    /// split-point calculation — the summarization LLM handles
    /// split decisions internally.
    async fn auto_compact_conversation(
        llm: &LlmClient,
        memory: &MemoryStore,
        conversation_id: &str,
        messages: &[ChatMessage],
        _context_window: usize,
    ) -> Result<Vec<ChatMessage>> {
        // 1. Separate system messages from the rest
        let mut system_msgs: Vec<ChatMessage> = Vec::new();
        let non_system: Vec<ChatMessage> = messages
            .iter()
            .filter(|&msg| {
                if msg.role == "system" {
                    system_msgs.push(msg.clone());
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        let tool_groups = crate::agent_prompt::find_tool_groups(&non_system);

        let preserve_count = PRESERVED_TOOL_GROUPS.min(tool_groups.len());
        let preserved_groups_start = tool_groups.len().saturating_sub(preserve_count);

        let summary_end = if preserved_groups_start > 0 {
            let last_summary = &tool_groups[preserved_groups_start - 1];
            *last_summary
                .tool_result_indices
                .last()
                .unwrap_or(&last_summary.assistant_idx)
                + 1
        } else {
            return Ok(messages.to_vec());
        };

        let to_summarize = &non_system[..summary_end];
        let preserved = &non_system[summary_end..];

        // 2. Summarize with RAG-aware compaction
        let mut compacted = Self::summarize_and_replace(
            llm,
            memory,
            conversation_id,
            to_summarize,
            preserved,
            "Auto-compact",
            "★ COMPACT SUMMARY ★",
        )
        .await?;

        // 3. Prepend system messages back
        let mut result = system_msgs;
        result.append(&mut compacted);
        Ok(result)
    }

    /// Tier 4: Reactive compact — emergency 413 recovery.
    ///
    /// `_context_window` is reserved for future threshold-based
    /// split-point calculation — the summarization LLM handles
    /// split decisions internally.
    async fn reactive_compact(
        llm: &LlmClient,
        memory: &MemoryStore,
        conversation_id: &str,
        messages: &[ChatMessage],
        _context_window: usize,
    ) -> Result<Vec<ChatMessage>> {
        const PRESERVE_COUNT: usize = 4;

        // 1. Separate system messages
        let mut system_msgs: Vec<ChatMessage> = Vec::new();
        let non_system: Vec<ChatMessage> = messages
            .iter()
            .filter(|&msg| {
                if msg.role == "system" {
                    system_msgs.push(msg.clone());
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        if non_system.len() <= PRESERVE_COUNT {
            anyhow::bail!("Too few non-system messages for reactive compact");
        }

        let split = non_system.len().saturating_sub(PRESERVE_COUNT);
        let to_summarize = &non_system[..split];
        let preserved = &non_system[split..];

        let mut compacted = Self::summarize_and_replace(
            llm,
            memory,
            conversation_id,
            to_summarize,
            preserved,
            "Reactive compact",
            "★ COMPACT SUMMARY (EMERGENCY) ★",
        )
        .await?;

        let mut result = system_msgs;
        result.append(&mut compacted);
        Ok(result)
    }

    /// Shared helper for Tiers 3 and 4: send messages to LLM for
    /// summarization, then assemble the compacted result.
    async fn summarize_and_replace(
        llm: &LlmClient,
        memory: &MemoryStore,
        conversation_id: &str,
        to_summarize: &[ChatMessage],
        preserved: &[ChatMessage],
        error_label: &str,
        summary_label: &str,
    ) -> Result<Vec<ChatMessage>> {
        // NEW: RAG retrieval for compaction (non-fatal — warn on error, continue)
        let retrieved = match crate::memory::rag::retrieve_context_for_compaction(
            memory,
            to_summarize,
            preserved,
            conversation_id,
            COMPACTION_RAG_LIMIT,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("RAG retrieval for compaction failed: {}", e);
                None
            }
        };

        // Build compact messages: summary prompt + optional retrieved context + truncated input
        let mut compact_msgs = Vec::new();
        compact_msgs.push(build_compact_summary_prompt());

        if let Some(ref ctx) = retrieved {
            compact_msgs.push(ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(ctx.clone())),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Tool-group-aware truncation
        let groups = crate::agent_prompt::find_tool_groups(to_summarize);
        let bookend_groups = 1usize;
        let tail_groups = 3usize.min(groups.len().saturating_sub(bookend_groups));

        let mut seen_indices = std::collections::HashSet::new();

        // First bookend groups (conversation origin)
        for group in groups.iter().take(bookend_groups) {
            seen_indices.insert(group.assistant_idx);
            for &ti in &group.tool_result_indices {
                seen_indices.insert(ti);
            }
        }

        // Last tail groups (recent flow)
        for group in groups.iter().rev().take(tail_groups) {
            seen_indices.insert(group.assistant_idx);
            for &ti in &group.tool_result_indices {
                seen_indices.insert(ti);
            }
        }

        // Always include non-assistant/non-tool messages
        for (idx, msg) in to_summarize.iter().enumerate() {
            if msg.role != "assistant" && !msg.has_tool_calls() {
                seen_indices.insert(idx);
            }
        }

        // Build sampled list in original order with truncation notice
        let mut sampled: Vec<ChatMessage> = Vec::new();
        let mut inserted_notice = false;
        for (idx, msg) in to_summarize.iter().enumerate() {
            if seen_indices.contains(&idx) {
                sampled.push(msg.clone());
            } else if !inserted_notice {
                sampled.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text(format!(
                        "[... {} messages omitted, see retrieved_context above ...]",
                        to_summarize.len() - seen_indices.len()
                    ))),
                    tool_calls: None,
                    tool_call_id: None,
                });
                inserted_notice = true;
            }
        }

        // If no truncation happened, use the full to_summarize
        if sampled.is_empty() {
            sampled = to_summarize.to_vec();
        }

        let user_prompt = format!(
            "Summarize the following conversation (sampled from {} messages):",
            to_summarize.len()
        );
        compact_msgs.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(user_prompt)),
            tool_calls: None,
            tool_call_id: None,
        });
        compact_msgs.extend(sampled);

        let summary_response = match llm.chat(&compact_msgs, &[]).await {
            Ok(c) => c,
            Err(e) => anyhow::bail!("{} LLM call failed: {}", error_label, e),
        };

        let summary_text = summary_response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();

        if summary_text.is_empty() {
            anyhow::bail!("{} returned empty summary", error_label);
        }

        let boundary = build_compact_boundary_marker(to_summarize.len(), 1);
        let summary_msg = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!(
                "{}\n\n{}",
                summary_label, summary_text
            ))),
            tool_calls: None,
            tool_call_id: None,
        };

        let mut result: Vec<ChatMessage> = Vec::with_capacity(3 + preserved.len());
        result.push(boundary);
        result.push(summary_msg);
        result.extend(preserved.iter().cloned());

        let nudge = recovery_nudge_for(&result);
        result.push(nudge);

        Ok(result)
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
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_skill_file".to_string(),
                    description: concat!(
                        "Read a file from a skill directory. The system prompt lists instruction skills by name and description only; ",
                        "use this tool to load a skill's full instructions when relevant (call with relative_path='SKILL.md'), then follow the loaded content. ",
                        "Also use for supporting files (style guides, templates, reference docs)."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": {
                                "type": "string",
                                "description": "Skill directory name (e.g. 'thread-writer')"
                            },
                            "relative_path": {
                                "type": "string",
                                "description": "Path within the skill directory, e.g. 'SKILL.md', 'reference.md', 'scripts/helper.py'"
                            }
                        },
                        "required": ["skill_name", "relative_path"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "invoke_agent".to_string(),
                    description: concat!(
                        "Delegate a task to a named agent running as an isolated agentic loop. ",
                        "Agents are listed under 'Available Agents' in the system prompt. ",
                        "The agent uses its own model and tool whitelist declared in its frontmatter. ",
                        "Looks up in the agents/ directory first, then falls back to the skills/ directory."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "description": "Name of the agent to invoke (e.g. 'soul-keeper', 'thread-writer')"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "The task content to pass to the agent"
                            },
                            "model": {
                                "type": "string",
                                "description": "Optional: override the agent's declared model for this invocation"
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional: override the agent's declared tool whitelist"
                            }
                        },
                        "required": ["agent", "prompt"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "read_agent_file".to_string(),
                    description: concat!(
                        "Read a file from an agent directory under the configured agents folder. ",
                        "Use this to load an agent's full instructions (call with relative_path='AGENT.md'), ",
                        "or to read supporting files (reference docs, templates, scripts)."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "Agent directory name (e.g. 'soul-keeper')"
                            },
                            "relative_path": {
                                "type": "string",
                                "description": "Path within the agent directory, e.g. 'AGENT.md', 'reference.md'"
                            }
                        },
                        "required": ["agent_name", "relative_path"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "write_agent_file".to_string(),
                    description: concat!(
                        "Write a file into an agent directory under the configured agents folder. ",
                        "Use this to create AGENT.md and any supporting files. ",
                        "Call reload_agents after ALL files for the agent are written."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "Agent directory name: lowercase letters, numbers, hyphens only, max 64 chars (e.g. 'news-fetcher')"
                            },
                            "relative_path": {
                                "type": "string",
                                "description": "Path within the agent directory, e.g. 'AGENT.md', 'reference.md'"
                            },
                            "content": {
                                "type": "string",
                                "description": "Full file content to write"
                            }
                        },
                        "required": ["agent_name", "relative_path", "content"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "reload_agents".to_string(),
                    description: concat!(
                        "Reload all agents from the agents directory into memory. ",
                        "Call this after writing agent files to make the new agent immediately active ",
                        "without restarting the bot."
                    ).to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "spawn_agents".to_string(),
                    description: concat!(
                        "Spawn one or more isolated subagents. ",
                        "Each gets its own agentic loop with system context (date/time) auto-injected. ",
                        "When multiple tasks are provided via the 'tasks' array, they run concurrently. ",
                        "For a single subagent, use shorthand fields (system_prompt+prompt). ",
                        "For multiple subagents, use the tasks array."
                    ).to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "tasks": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "system_prompt": {
                                            "type": "string",
                                            "description": "Instructions for this subagent — its role, constraints, and behavior"
                                        },
                                        "prompt": {
                                            "type": "string",
                                            "description": "The task to execute"
                                        },
                                        "model": {
                                            "type": "string",
                                            "description": "Optional model override (e.g. 'google/gemini-flash-2.0' for cheap tasks)"
                                        },
                                        "tools": {
                                            "type": "array",
                                            "items": { "type": "string" },
                                            "description": "Optional tool whitelist. Default: built-in tools only."
                                        }
                                    },
                                    "required": ["system_prompt", "prompt"]
                                }
                            },
                            "system_prompt": {
                                "type": "string",
                                "description": "Shorthand: system prompt for a single subagent (use instead of tasks for one)"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "Shorthand: task for a single subagent"
                            },
                            "model": {
                                "type": "string",
                                "description": "Shorthand: model override for a single subagent"
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Shorthand: tool whitelist for a single subagent"
                            }
                        }
                    }),
                },
            },
        ]
    }

    /// Run a named skill/agent as an isolated subagent mini-loop.
    /// `kind` controls which registry to look up and which read tool to use in the bootstrap.
    /// Returns the subagent's final text response (or an error string).
    ///
    /// Ad-hoc mode (skill_name = None): use the provided system_prompt + user_prompt
    /// directly with a default sandbox tool whitelist. The system_prompt is augmented
    /// with ambient system context (timestamp, user model, location) via
    /// `build_subagent_system_prompt`.
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
                let mut t = tools::builtin_tool_definitions();
                t.extend(self.mcp.tool_definitions());
                t.extend(self.skill_tool_definitions());
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
            let mut t = tools::builtin_tool_definitions();
            t.extend(self.mcp.tool_definitions());
            t.extend(self.skill_tool_definitions()); // includes read_skill_file + read_agent_file
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

        for _iteration in 0..max_iter {
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

    /// Ask a parallel question while the main agent is processing.
    /// Spawns an isolated ad-hoc subagent with timestamp/location context.
    /// Returns the subagent's answer or an error message.
    pub async fn ask_parallel(&self, question: &str) -> Result<String> {
        let answer = self
            .run_subagent(
                None,
                "Answer the user's follow-up question concisely and accurately using your knowledge.",
                question,
                None,
                None,
            )
            .await;
        // Detect error patterns from run_subagent/run_subagent_loop:
        // - "Subagent '...' error: ..." (API error)
        // - "Subagent '...' reached the maximum number of iterations" (max iterations)
        // - "Subagent '...' returned an empty response after ... attempts" (empty response)
        if answer.starts_with("Subagent '")
            && (answer.contains("error")
                || answer.contains("reached the maximum")
                || answer.contains("empty response"))
        {
            Err(anyhow::anyhow!("{}", answer))
        } else {
            Ok(answer)
        }
    }

    /// Get the path for a soul file by name.
    fn soul_file_path(&self, file_name: &str) -> anyhow::Result<PathBuf> {
        let home = self
            .config
            .resolved_home()
            .context("Home directory not resolved")?;
        match file_name {
            "SOUL.md" => Ok(home.join("SOUL.md")),
            "AGENTS.md" => Ok(home.join("AGENTS.md")),
            "USER.md" => Ok(home.join("USER.md")),
            _ => anyhow::bail!("Invalid soul file name: {}", file_name),
        }
    }

    /// Validate that a soul file path is within the home directory.
    async fn validate_soul_file_path(&self, file_name: &str) -> anyhow::Result<PathBuf> {
        let home = self
            .config
            .resolved_home()
            .context("Home directory not resolved")?;
        let path = self.soul_file_path(file_name)?;
        tools::validate_home_path(home, &path.to_string_lossy())
    }

    async fn execute_command_interactive(
        &self,
        arguments: &serde_json::Value,
        _user_id: &str,
        chat_id: ChatId,
    ) -> String {
        use std::time::Instant;
        use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
        use tokio::io::AsyncReadExt;

        let command = match arguments["command"].as_str() {
            Some(c) => c,
            None => return "Error: Missing 'command' argument".to_string(),
        };

        let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());
        let sandbox_dir = &self.config.sandbox.allowed_directory;

        let mut child = match TokioCommand::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(sandbox_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return format!("Error: Failed to spawn command: {}", e),
        };

        let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

        // Send initial message with cancel button
        let keyboard = InlineKeyboardMarkup::new([[InlineKeyboardButton::callback(
            "Cancel",
            format!("cancel_cmd:{}", cmd_id),
        )]]);

        let msg = match self
            .bot
            .send_message(
                chat_id,
                format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd),
            )
            .reply_markup(keyboard)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                let _ = child.kill().await;
                return format!("Error: Failed to send command message: {}", e);
            }
        };

        // Set up cancel channel
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        // Register in running_commands
        {
            let mut map = self.running_commands.lock().await;
            map.insert(cmd_id.clone(), RunningCommand { cancel_tx });
        }

        // Capture Arc for cleanup
        let running_commands = self.running_commands.clone();
        let cmd_id_clone = cmd_id.clone();

        // Output streaming
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
        let output_tx2 = output_tx.clone();
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        let stdout_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stdout.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if output_tx
                            .send(String::from_utf8_lossy(&buf[..n]).to_string())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stderr.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if output_tx2
                            .send(String::from_utf8_lossy(&buf[..n]).to_string())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        // Cap accumulated output to prevent unbounded memory growth
        const MAX_BUFFER_CHARS: usize = 100_000;

        let mut output_buffer = String::new();
        let mut last_edit = Instant::now();

        // Main select loop — only determines exit reason
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
                        let body = format!("```\n{}\n```", capped);
                        let text = format!("💻 Running: `{}`\n\n{}", escaped_cmd, body);
                        if let Err(e) = self.bot.edit_message_text(chat_id, msg.id, &text).await {
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
                    // Kill child + its process group so sh -c grandchildren are stopped
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

        // Post-loop: wait for readers to finish, drain remaining output.
        // Readers finish promptly after pipe EOF (child has exited), so this
        // join resolves within microseconds. No timeout needed — it would
        // reintroduce a race window where try_recv could miss late chunks.
        let _ = tokio::join!(stdout_handle, stderr_handle);
        while let Ok(chunk) = output_rx.try_recv() {
            output_buffer.push_str(&chunk);
        }
        // Re-cap buffer after drain (defensive — drain may push past limit)
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

        // Build the final result with complete output
        let result = if cancelled {
            let body = format_body(&output_buffer, "");
            let text = match body {
                None => format!("❌ Cancelled: `{}`", escaped_cmd),
                Some(b) => format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, b),
            };
            if let Err(e) = self.bot.edit_message_text(chat_id, msg.id, &text).await {
                warn!("Failed to update cancelled message: {e}");
            }
            "⚠️ User cancelled the command".to_string()
        } else if let Some(code) = exit_code {
            let (icon, label) = if code == 0 { ("✅", "Completed") } else { ("❌", "Failed") };
            let body = format_body(&output_buffer, "Command completed with no output.");
            let text = format!("{} {}: `{}`\n\n{}", icon, label, escaped_cmd, body.unwrap_or_default());
            if let Err(e) = self.bot.edit_message_text(chat_id, msg.id, &text).await {
                warn!("Failed to update completed message: {e}");
            }

            let mut result = String::new();
            if !output_buffer.is_empty() {
                result.push_str(output_buffer.trim_end());
                result.push('\n');
            }
            result.push_str(&format!("Exit code: {}", code));
            result
        } else {
            unreachable!("select loop always sets either cancelled or exit_code")
        };

        // Cleanup registry
        let mut map = running_commands.lock().await;
        map.remove(&cmd_id_clone);

        result
    }

    /// Execute a tool call by routing to the right handler
    async fn execute_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        user_id: &str,
        chat_id: ChatId,
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

                let mut results = Vec::new();

                // Search conversations (hybrid vector + FTS5)
                if let Ok(msgs) = self.memory.search_messages(query, limit).await {
                    for msg in msgs {
                        if let Some(content) = &msg.content {
                            results.push(format!("[{}]: {}", msg.role, content.as_text()));
                        }
                    }
                }

                // Search knowledge (hybrid vector + FTS5)
                if let Ok(entries) = self.memory.search_knowledge(query, limit).await {
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
            "read_skill_file" => {
                let skill_name = match arguments["skill_name"].as_str() {
                    Some(n) => n.to_string(),
                    None => return "Missing skill_name".to_string(),
                };
                let relative_path = match arguments["relative_path"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing relative_path".to_string(),
                };

                if let Err(e) = validate_skill_name(&skill_name) {
                    return format!("Invalid skill_name: {}", e);
                }
                if let Err(e) = validate_skill_path(&relative_path) {
                    return format!("Invalid path: {}", e);
                }

                // Resolve via registry (instance shadows bundled)
                let skills_lock = self.skills.read().await;
                let base_dir = self.resolve_skill_base_dir(
                    &skill_name,
                    &self.config.skills.directory,
                    &skills_lock,
                );
                let target = base_dir.join(&skill_name).join(&relative_path);

                // Canonicalize check against the resolved base dir
                if let Ok(base_canonical) = base_dir.canonicalize() {
                    if let Ok(target_canonical) = target.canonicalize() {
                        if !target_canonical.starts_with(&base_canonical) {
                            return format!(
                                "Access denied: path '{}' resolves outside the skills directory",
                                target.display()
                            );
                        }
                    }
                }
                // Drop the read lock before awaiting the file read
                drop(skills_lock);

                match tokio::fs::read_to_string(&target).await {
                    Ok(content) => content,
                    Err(e) => format!(
                        "Failed to read skill file '{}/{}': {}",
                        skill_name, relative_path, e
                    ),
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

                        // After writing, reload skills
                        let instance_dir = self.config.skills.directory.clone();
                        use crate::skills::loader::load_skills_from_dir;
                        if let Ok(new_reg) =
                            load_skills_from_dir(&instance_dir, instance_dir.clone()).await
                        {
                            let mut skills = self.skills.write().await;
                            *skills = new_reg;
                        }

                        format!("Written: {}", target.display())
                    }
                    Err(e) => format!("Failed to write skill file: {}", e),
                }
            }
            "reload_skills" => {
                use crate::skills::loader::load_skills_from_dir;

                let skills_dir = self.config.skills.directory.clone();
                match load_skills_from_dir(&skills_dir, skills_dir.clone()).await {
                    Ok(new_reg) => {
                        let count = new_reg.len();
                        let mut skills = self.skills.write().await;
                        *skills = new_reg;
                        info!("Skills reloaded: {} skill(s) active", count);
                        format!("Skills reloaded. {} skill(s) now active.", count)
                    }
                    Err(e) => format!("Failed to reload skills: {}", e),
                }
            }
            "invoke_agent" => {
                // Accepts `agent` parameter; falls back to `skill` for compat
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
                    Some(&agent), // skill_name: predefined agent from registry
                    "",           // system_prompt: empty (read from AGENT.md for predefined)
                    &prompt,      // user_prompt
                    model_override.as_deref(),
                    tools_override,
                ))
                .await
            }
            "spawn_agents" => {
                // --- Validate tasks first, before creating any futures ---
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
                    // Single ad-hoc subagent (shorthand fields)
                    let system_prompt = match arguments["system_prompt"].as_str() {
                        Some(s) => s.to_string(),
                        None => {
                            return "Missing tasks: provide either 'tasks' array or system_prompt+prompt"
                                .to_string()
                        }
                    };
                    if system_prompt.is_empty() {
                        return "system_prompt cannot be empty".to_string();
                    }
                    let prompt = match arguments["prompt"].as_str() {
                        Some(p) => p.to_string(),
                        None => return "Missing prompt".to_string(),
                    };
                    if prompt.is_empty() {
                        return "prompt cannot be empty".to_string();
                    }
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

                // All validation passed — now build and run futures
                let futs: Vec<_> = parsed_tasks
                    .into_iter()
                    .map(|task| {
                        let sp = task.system_prompt;
                        let pr = task.prompt;
                        let mo = task.model;
                        let to = task.tools;
                        Box::pin(async move {
                            self.run_subagent(None, &sp, &pr, mo.as_deref(), to).await
                        })
                    })
                    .collect();

                let results = futures::future::join_all(futs).await;
                serde_json::json!({ "results": results }).to_string()
            }
            "read_agent_file" => {
                let agent_name = match arguments["agent_name"].as_str() {
                    Some(n) => n.to_string(),
                    None => return "Missing agent_name".to_string(),
                };
                let relative_path = match arguments["relative_path"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing relative_path".to_string(),
                };

                if let Err(e) = validate_skill_name(&agent_name) {
                    return format!("Invalid agent_name: {}", e);
                }
                if let Err(e) = validate_skill_path(&relative_path) {
                    return format!("Invalid path: {}", e);
                }

                // Resolve via agents registry (instance shadows bundled)
                let agents_lock = self.agents.read().await;
                let base_dir = self.resolve_skill_base_dir(
                    &agent_name,
                    &self.config.agents.directory,
                    &agents_lock,
                );
                let target = base_dir.join(&agent_name).join(&relative_path);

                // Canonicalize check against the resolved base dir
                if let Ok(base_canonical) = base_dir.canonicalize() {
                    if let Ok(target_canonical) = target.canonicalize() {
                        if !target_canonical.starts_with(&base_canonical) {
                            return format!(
                                "Access denied: path '{}' resolves outside the agents directory",
                                target.display()
                            );
                        }
                    }
                }
                // Drop the read lock before awaiting the file read
                drop(agents_lock);

                match tokio::fs::read_to_string(&target).await {
                    Ok(content) => content,
                    Err(e) => format!(
                        "Failed to read agent file '{}/{}': {}",
                        agent_name, relative_path, e
                    ),
                }
            }
            "write_agent_file" => {
                let agent_name = match arguments["agent_name"].as_str() {
                    Some(n) => n.to_string(),
                    None => return "Missing agent_name".to_string(),
                };
                let relative_path = match arguments["relative_path"].as_str() {
                    Some(p) => p.to_string(),
                    None => return "Missing relative_path".to_string(),
                };
                let content = arguments["content"].as_str().unwrap_or("").to_string();

                if let Err(e) = validate_skill_name(&agent_name) {
                    return format!("Invalid agent_name: {}", e);
                }
                if let Err(e) = validate_skill_path(&relative_path) {
                    return format!("Invalid relative_path: {}", e);
                }

                let target = self
                    .config
                    .agents
                    .directory
                    .join(&agent_name)
                    .join(&relative_path);

                if let Some(parent) = target.parent() {
                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                        return format!("Failed to create directories: {}", e);
                    }
                }

                match tokio::fs::write(&target, &content).await {
                    Ok(()) => {
                        info!("Agent file written: {}", target.display());

                        // After writing, reload agents
                        let instance_dir = self.config.agents.directory.clone();
                        use crate::skills::loader::load_skills_from_dir;
                        if let Ok(new_reg) =
                            load_skills_from_dir(&instance_dir, instance_dir.clone()).await
                        {
                            let mut agents = self.agents.write().await;
                            *agents = new_reg;
                        }

                        format!("Written: {}", target.display())
                    }
                    Err(e) => format!("Failed to write agent file: {}", e),
                }
            }
            "reload_agents" => {
                use crate::skills::loader::load_skills_from_dir;

                let agents_dir = self.config.agents.directory.clone();
                match load_skills_from_dir(&agents_dir, agents_dir.clone()).await {
                    Ok(new_reg) => {
                        let count = new_reg.len();
                        let mut agents = self.agents.write().await;
                        *agents = new_reg;
                        info!("Agents reloaded: {} agent(s) active", count);
                        format!("Agents reloaded. {} agent(s) now active.", count)
                    }
                    Err(e) => format!("Failed to reload agents: {}", e),
                }
            }
            "try_new_tech" => {
                let technology = match arguments["technology"].as_str() {
                    Some(t) => t.to_string(),
                    None => return "Missing technology".to_string(),
                };
                let experiment_code = match arguments["experiment_code"].as_str() {
                    Some(c) => c.to_string(),
                    None => return "Missing experiment_code".to_string(),
                };
                let language = arguments["language"].as_str().unwrap_or("rust").to_string();

                let sandbox = &self.config.sandbox.allowed_directory;
                let exp_id = uuid::Uuid::new_v4().to_string();
                let exp_dir = sandbox.join("experiments").join(&exp_id);

                if let Err(e) = tokio::fs::create_dir_all(&exp_dir).await {
                    return format!("Failed to create experiment dir: {}", e);
                }

                let (filename, check_cmd, check_args) = match language.as_str() {
                    "javascript" => ("experiment.js", "node", vec!["experiment.js".to_string()]),
                    _ => {
                        // Rust: create a minimal Cargo project structure
                        let cargo_toml = "[package]\nname = \"experiment\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_string();
                        let src_dir = exp_dir.join("src");
                        if let Err(e) = tokio::fs::create_dir_all(&src_dir).await {
                            return format!("Failed to create src dir: {}", e);
                        }
                        if let Err(e) =
                            tokio::fs::write(exp_dir.join("Cargo.toml"), cargo_toml).await
                        {
                            return format!("Failed to write Cargo.toml: {}", e);
                        }
                        if let Err(e) =
                            tokio::fs::write(src_dir.join("main.rs"), &experiment_code).await
                        {
                            return format!("Failed to write main.rs: {}", e);
                        }
                        ("src/main.rs", "cargo", vec!["check".to_string()])
                    }
                };

                // Write experiment code for JS (Rust already written above)
                if language == "javascript" {
                    if let Err(e) = tokio::fs::write(exp_dir.join(filename), &experiment_code).await
                    {
                        return format!("Failed to write experiment file: {}", e);
                    }
                }

                info!(
                    "Running experiment '{}' in {}",
                    technology,
                    exp_dir.display()
                );

                let output = match tokio::process::Command::new(check_cmd)
                    .args(&check_args)
                    .current_dir(&exp_dir)
                    .output()
                    .await
                {
                    Ok(o) => o,
                    Err(e) => return format!("Failed to run experiment: {}", e),
                };

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);
                let success = output.status.success();

                let mut result = format!("Experiment: {}\nLanguage: {}\n", technology, language);
                if !stdout.is_empty() {
                    result.push_str(&format!("STDOUT:\n{}\n", stdout));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("STDERR:\n{}\n", stderr));
                }
                result.push_str(&format!(
                    "Exit code: {}\nResult: {}\n",
                    exit_code,
                    if success { "SUCCESS" } else { "FAILED" }
                ));

                // Cleanup: remove the experiment directory so temporary projects
                // (including Rust `target/` dirs) don't accumulate on disk.
                if let Err(e) = tokio::fs::remove_dir_all(&exp_dir).await {
                    warn!(
                        "Failed to clean up experiment dir '{}': {}",
                        exp_dir.display(),
                        e
                    );
                }

                result
            }
            "self_upgrade" => {
                let branch = arguments["branch"].as_str().unwrap_or("main").to_string();
                let mode = arguments["mode"].as_str().unwrap_or("auto").to_string();

                // Validate branch name to prevent git flag injection and path traversal.
                // A single chars() pass checks both the allowlist and the blocklist.
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
                    return format!("Self-upgrade failed: invalid branch name '{}'", branch);
                }

                info!(
                    "Self-upgrade requested: branch '{}', mode '{}'",
                    branch, mode
                );

                match crate::learning::self_upgrade(&branch, &mode, None).await {
                    Ok(log) => {
                        self.restart_pending.store(true, Ordering::Release);
                        log
                    }
                    Err(e) => format!("Self-upgrade failed: {:#}", e),
                }
            }
            "patch_skill" => {
                let skill_name = match arguments["skill_name"].as_str() {
                    Some(n) => n.to_string(),
                    None => return "Missing skill_name".to_string(),
                };
                let patch_content = match arguments["patch_content"].as_str() {
                    Some(c) => c.to_string(),
                    None => return "Missing patch_content".to_string(),
                };

                match crate::learning::self_patch_skill(
                    &self.config.skills.directory,
                    &skill_name,
                    &patch_content,
                    &self.skills,
                )
                .await
                {
                    Ok(msg) => msg,
                    Err(e) => format!("Patch failed: {:#}", e),
                }
            }
            "send_file" => {
                match async {
                    let path = arguments["path"]
                        .as_str()
                        .context("Missing 'path' argument")?;
                    let caption = arguments
                        .get("caption")
                        .and_then(|v| v.as_str())
                        .filter(|c| !c.is_empty());

                    let full_path =
                        tools::validate_sandbox_path(&self.config.sandbox.allowed_directory, path)?;

                    let metadata = tokio::fs::metadata(&full_path)
                        .await
                        .with_context(|| format!("File not found: {}", full_path.display()))?;
                    const TG_FILE_LIMIT: u64 = 50 * 1024 * 1024;
                    if metadata.len() > TG_FILE_LIMIT {
                        anyhow::bail!(
                            "File is {} MB — exceeds Telegram's 50 MB limit",
                            metadata.len() / 1024 / 1024
                        );
                    }

                    let bytes = tokio::fs::read(&full_path)
                        .await
                        .with_context(|| format!("Failed to read file: {}", full_path.display()))?;

                    let file_name = full_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();

                    let input_file = InputFile::memory(bytes).file_name(file_name.clone());
                    let mut req = self.bot.send_document(chat_id, input_file);
                    if let Some(c) = caption {
                        req = req.caption(c);
                    }
                    req.await
                        .with_context(|| "Telegram API failed to send document")?;

                    Ok(format!("File '{}' sent successfully.", file_name))
                }
                .await
                {
                    Ok(msg) => msg,
                    Err(e) => format!("Error sending file: {:#}", e),
                }
            }
            "read_soul_file" => {
                let file_name = match arguments["file_name"].as_str() {
                    Some(n) => n,
                    None => return "Missing 'file_name'".to_string(),
                };
                let path = match self.validate_soul_file_path(file_name).await {
                    Ok(p) => p,
                    Err(e) => return format!("Invalid soul file path: {}", e),
                };
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => content,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        format!(
                            "Soul file '{}' does not exist yet. It will be created on first update.",
                            file_name
                        )
                    }
                    Err(e) => format!("Error reading soul file: {}", e),
                }
            }
            "update_soul_file" => {
                let file_name = match arguments["file_name"].as_str() {
                    Some(n) => n,
                    None => return "Missing 'file_name'".to_string(),
                };
                let content = match arguments["content"].as_str() {
                    Some(c) => c,
                    None => return "Missing 'content'".to_string(),
                };
                let mode = arguments["mode"].as_str().unwrap_or("append");

                // Null byte check
                if content.contains('\0') {
                    return "Content contains null bytes and was rejected.".to_string();
                }

                // Hard size limit (check first for unambiguous rejection)
                if content.len() > 100_000 {
                    return "Content too large (max 100KB). Please consolidate the file first."
                        .to_string();
                }
                // Soft size warning
                let size_warning = if content.len() > 50_000 {
                    format!(
                        "\n\n(Warning: content is {} bytes >50KB. Consider consolidating if it keeps growing.)",
                        content.len()
                    )
                } else {
                    String::new()
                };

                let path = match self.validate_soul_file_path(file_name).await {
                    Ok(p) => p,
                    Err(e) => return format!("Invalid soul file path: {}", e),
                };

                let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();

                let new_content = match mode {
                    "append" => {
                        if existing.trim().is_empty() {
                            // New file — create with frontmatter if not provided
                            if content.starts_with("---") {
                                content.to_string()
                            } else {
                                format!(
                                    "---\nname: {}\nversion: 1\n---\n\n{}",
                                    file_name.trim_end_matches(".md"),
                                    content
                                )
                            }
                        } else {
                            if !existing.trim().starts_with("---") {
                                return "Existing soul file has invalid format (missing frontmatter). Rejected.".to_string();
                            }
                            format!("{}\n{}", existing.trim_end(), content)
                        }
                    }
                    "replace" => {
                        if !content.trim().starts_with("---") {
                            return "Replace mode requires content with YAML frontmatter"
                                .to_string();
                        }
                        content.to_string()
                    }
                    _ => return "Invalid mode. Use 'append' or 'replace'.".to_string(),
                };

                // Validate frontmatter
                if !crate::learning::has_valid_frontmatter(&new_content) {
                    return "Update would produce invalid soul file (missing frontmatter). Rejected."
                        .to_string();
                }
                // Verify name and version fields in frontmatter
                if !new_content.contains("name:") || !new_content.contains("version:") {
                    return "Update rejected: frontmatter must contain 'name' and 'version' fields."
                        .to_string();
                }

                // Helper to append suffix to path (not with_extension, which replaces .md)
                fn bak_path(p: &Path, suffix: &str) -> PathBuf {
                    let mut s = p.to_string_lossy().to_string();
                    s.push_str(suffix);
                    PathBuf::from(s)
                }

                // Rotate backups: .bak.2→.bak.3, .bak.1→.bak.2, .bak→.bak.1, current→.bak
                let bak_2 = bak_path(&path, ".bak.2");
                let bak_3 = bak_path(&path, ".bak.3");
                if bak_2.exists() {
                    let _ = tokio::fs::rename(&bak_2, &bak_3).await;
                }
                let bak_1 = bak_path(&path, ".bak.1");
                let bak_2_new = bak_path(&path, ".bak.2");
                if bak_1.exists() {
                    let _ = tokio::fs::rename(&bak_1, &bak_2_new).await;
                }
                let bak_current = bak_path(&path, ".bak");
                let bak_1_new = bak_path(&path, ".bak.1");
                if bak_current.exists() {
                    let _ = tokio::fs::rename(&bak_current, &bak_1_new).await;
                }
                if path.exists() {
                    let _ = tokio::fs::copy(&path, &bak_current).await;
                }

                // Write with post-write validation
                if let Err(e) = tokio::fs::write(&path, &new_content).await {
                    if bak_current.exists() {
                        let _ = tokio::fs::copy(&bak_current, &path).await;
                    }
                    return format!("Failed to write soul file (restored from backup): {}", e);
                }

                // Read back and verify
                match tokio::fs::read_to_string(&path).await {
                    Ok(read_back) if read_back == new_content => {
                        self.soul_updated
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        format!(
                            "{} updated successfully. Backup at {}{}",
                            file_name,
                            bak_current.display(),
                            size_warning
                        )
                    }
                    Ok(_) => {
                        if bak_current.exists() {
                            let _ = tokio::fs::copy(&bak_current, &path).await;
                        }
                        "Write verification failed (content mismatch). Restored from backup."
                            .to_string()
                    }
                    Err(e) => {
                        if bak_current.exists() {
                            let _ = tokio::fs::copy(&bak_current, &path).await;
                        }
                        format!("Write verification error (restored from backup): {}", e)
                    }
                }
            }
            "revert_soul_file" => {
                let file_name = match arguments["file_name"].as_str() {
                    Some(n) => n,
                    None => return "Missing 'file_name'".to_string(),
                };
                let path = match self.validate_soul_file_path(file_name).await {
                    Ok(p) => p,
                    Err(e) => return format!("Invalid soul file path: {}", e),
                };
                let bak = {
                    let mut s = path.to_string_lossy().to_string();
                    s.push_str(".bak");
                    PathBuf::from(s)
                };
                if !bak.exists() {
                    return format!("No backup found for {}", file_name);
                }
                match tokio::fs::copy(&bak, &path).await {
                    Ok(_) => format!("{} restored from backup.", file_name),
                    Err(e) => format!("Failed to restore backup: {}", e),
                }
            }
            "execute_command" => {
                self.execute_command_interactive(arguments, user_id, chat_id)
                    .await
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

/// Build the effective tool whitelist for a subagent/agent.
/// Always includes `read_skill_file` and `read_agent_file`; deduplicates.
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
fn missing_subagent_tools(declared: &[String], available_names: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|t| !available_names.contains(t))
        .cloned()
        .collect()
}

/// Error message returned when the main agent or a subagent produces a tool call
/// whose arguments are a regurgitated compaction marker rather than real JSON.
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
}
