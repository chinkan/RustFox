use anyhow::{Context, Result};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use serde_json::Value;
use teloxide::types::ChatId;
use teloxide::Bot;

use crate::agent_prompt::PreparedPrompt;
use crate::cancel_registry::CancelRegistry;
use crate::config::Config;
use crate::langsmith::LangSmithClient;
use crate::llm::{ChatMessage, ContentPart, LlmClient, MessageContent, ToolCall, ToolDefinition};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::platform::sender::PlatformSender;
use crate::platform::IncomingMessage;
use crate::scheduler::reminders::{ScheduledTask, ScheduledTaskStore};
use crate::scheduler::Scheduler;
use crate::skills::{format_listed_section, SkillRegistry};
use crate::tool_registry::{ToolContext, ToolRegistry};
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

/// Composite key for cancel / injection / loop / mid-run maps so the same
/// human on two bots does not cross-cancel. Format: `{bot_id}:{user_id}`.
pub fn session_key(bot_id: &str, user_id: &str) -> String {
    let bot_id = crate::platform::normalize_bot_id(bot_id);
    format!("{bot_id}:{user_id}")
}

/// A request dispatched from a fire closure to the background job runner.
pub struct ScheduledJobRequest {
    pub incoming: IncomingMessage,
    pub bot: Arc<Bot>,
    pub task_id: String,
    pub is_recurring: bool,
    pub task_store: ScheduledTaskStore,
    /// Some(id) when this dispatch is a dead-letter re-fire (ADR-0013):
    /// `id` is the `pending_reruns` row driving it. The runner must not
    /// re-queue a rerun (two-strike rule); it resolves the row instead.
    pub rerun_id: Option<String>,
}

/// Why an agent run stopped short of a clean final answer.
///
/// A *typed* stop reason (rather than an error string) is what lets the job
/// runner decide policy: `MaxIterations` means the loop ran out of budget
/// mid-task — possibly after side effects — so it is notified, never
/// auto-replayed. `Llm` is a hard failure (see `provider::LlmHttpError` for
/// transient classification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStop {
    FinalResponse,
    Cancelled,
    MaxIterations,
    Llm,
}

/// Outcome of one agent run: the text to deliver *and* how the run ended.
///
/// `process_message` collapses this to the text for ordinary callers (chat,
/// portal, subagents); the scheduled-task runner inspects `stop` so a
/// budget-exhausted run is surfaced to the human instead of vanishing.
#[derive(Debug)]
pub struct RunOutcome {
    pub text: String,
    pub stop: RunStop,
}

impl RunOutcome {
    /// True when the run stopped because the agent loop exhausted its tool-call
    /// budget. Such a run may have performed side effects already (files
    /// written, posts published), so it must never be silently replayed.
    pub fn is_max_iterations(&self) -> bool {
        self.stop == RunStop::MaxIterations
    }
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
    pub self_weak: Weak<Agent>,
    /// Sender for dispatching scheduled job work to the background runner.
    pub job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
    pub langsmith: Arc<LangSmithClient>,
    pub restart_pending: Arc<AtomicBool>,
    pub soul_updated: Arc<AtomicBool>,
    pub current_model: tokio::sync::RwLock<String>,
    pub config_path: PathBuf,
    pub cancel_registry: Arc<CancelRegistry>,
    pub tool_registry: ToolRegistry,
    pub sender: Arc<dyn PlatformSender>,
    /// Telegram bot handle — captured by scheduled-task fire closures
    /// (`build_fire_closure`). Cloned from main's Arc so every arm path
    /// dispatches to the same bot without threading it through handlers.
    pub bot: Arc<Bot>,
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
    /// Optional supervisor for session-scoped cancel of in-flight CLI jobs.
    /// Attached after construction (ReasoningBackend needs the Agent Arc first).
    pub supervisor: Arc<tokio::sync::RwLock<Option<Arc<crate::supervisor::Supervisor>>>>,
}

/// A task parsed from the spawn_agents tool arguments, after validation.
#[allow(dead_code)]
struct AdHocTask {
    system_prompt: String,
    prompt: String,
    model: Option<String>,
    tools: Option<Vec<String>>,
}

/// Build the unified `# Available Agents` section from line sources
/// (subagent-style skills, agents directory, and `[[bots]]` ids). Returns
/// `None` when all inputs are empty so the caller can skip the section.
/// The returned string includes the leading `\n\n` separator so it can be
/// appended directly to a prompt that already ends with content.
fn format_available_agents_section(
    subagent_lines: &str,
    agent_lines: &str,
    bot_lines: &str,
) -> Option<String> {
    if subagent_lines.is_empty() && agent_lines.is_empty() && bot_lines.is_empty() {
        return None;
    }

    let mut section = String::from("\n\n# Available Agents\n\n");
    section.push_str(&format_listed_section(
        "agent",
        "Delegate these tasks to specialized agents using `invoke_agent`:",
    ));

    let mut parts: Vec<&str> = Vec::new();
    if !subagent_lines.is_empty() {
        parts.push(subagent_lines);
    }
    if !agent_lines.is_empty() {
        parts.push(agent_lines);
    }
    if !bot_lines.is_empty() {
        parts.push(bot_lines);
    }
    section.push_str(&parts.join("\n"));
    section.push('\n');
    Some(section)
}

/// Personas whose `agents/<name>` registry line should be omitted because a
/// `[[bots]]` entry with `id == persona` already lists that id (§7.6: bots id
/// always listed; agents/ description wins for copy — no duplicate line).
fn bot_ids_covering_persona(
    bots: &[crate::config::BotConfig],
) -> std::collections::HashSet<String> {
    bots.iter()
        .filter(|b| b.id.trim() == b.persona.trim())
        .map(|b| b.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Format `# Available Agents` lines for every `[[bots]]` entry.
///
/// Primary invoke name is `bots[].id`. Description is copied from
/// `agents/<persona>` when present. When `id != persona`, the persona is shown
/// as an alias.
///
/// `exclude_bot_id` omits the calling bot so the model cannot self-invoke via
/// its own Telegram bot id (peer-cycle / stuck Working).
fn format_bots_available_lines(
    bots: &[crate::config::BotConfig],
    agents: &crate::skills::SkillRegistry,
    exclude_bot_id: Option<&str>,
) -> String {
    let exclude = exclude_bot_id.map(str::trim).filter(|s| !s.is_empty());
    let mut lines = Vec::new();
    for bot in bots {
        let id = bot.id.trim();
        let persona = bot.persona.trim();
        if id.is_empty() {
            continue;
        }
        if exclude.is_some_and(|ex| ex == id) {
            continue;
        }
        let desc = agents
            .get(persona)
            .map(|s| s.description.as_str())
            .filter(|d| !d.is_empty())
            .unwrap_or("Telegram bot persona");
        if id == persona {
            lines.push(format!(
                "- **{id}**: {desc}\n  Invoke via: `invoke_agent(agent=\"{id}\", prompt=\"<task>\")`"
            ));
        } else {
            lines.push(format!(
                "- **{id}** (persona: {persona}) — {desc}\n  Invoke via: `invoke_agent(agent=\"{id}\", prompt=\"<task>\")` (persona alias: `{persona}`)"
            ));
        }
    }
    lines.join("\n")
}

/// Agent-dir lines excluding personas already covered by a bots id (== persona).
fn format_agent_lines_excluding(
    agents: &crate::skills::SkillRegistry,
    exclude: &std::collections::HashSet<String>,
) -> String {
    let mut lines = Vec::new();
    for agent in agents.list() {
        if exclude.contains(&agent.name) {
            continue;
        }
        lines.push(format!(
            "- **{}**: {}\n  Invoke via: `invoke_agent(agent=\"{}\", prompt=\"<task>\")`",
            agent.name, agent.description, agent.name
        ));
    }
    lines.join("\n")
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
        self_weak: Weak<Agent>,
        job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
        langsmith: Arc<LangSmithClient>,
        config_path: PathBuf,
        cancel_registry: Arc<CancelRegistry>,
        tool_registry: ToolRegistry,
        sender: Arc<dyn PlatformSender>,
        bot: Arc<Bot>,
        restart_pending: Arc<AtomicBool>,
        soul_updated: Arc<AtomicBool>,
    ) -> Self {
        let llm = LlmClient::new(registry.clone()).with_fallback_chain(config.fallback_chain());
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
            self_weak,
            job_tx,
            langsmith,
            restart_pending,
            soul_updated,
            current_model: tokio::sync::RwLock::new(initial_model),
            config_path,
            cancel_registry,
            tool_registry,
            sender,
            bot,
            cancel_token_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_injections: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pending_loop_callbacks: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            supervisor: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    /// Attach the process-wide [`Supervisor`] so `/stop`, `cancel_cmd:`, and
    /// portal cancel can reach [`Supervisor::cancel_for_session`].
    pub async fn attach_supervisor(&self, supervisor: Arc<crate::supervisor::Supervisor>) {
        *self.supervisor.write().await = Some(supervisor);
    }

    /// Build the system prompt for a bot identity, incorporating loaded skills
    /// and agents. Base prompt + SOUL overlay are persona-scoped (§7.4);
    /// USER.md stays install-wide.
    async fn build_system_prompt(&self, bot_id: &str) -> String {
        let bot = crate::persona_prompt::bot_for_prompt(&self.config, bot_id);
        let persona = bot.persona.clone();
        // §7.4: bot.system_prompt_file > agents/<persona>/AGENT.md > global
        // openrouter resolve (ADR 0011 R7). Re-read every turn so portal edits
        // and persona file changes apply without restart.
        let mut prompt = crate::persona_prompt::resolve_bot_base_prompt(&self.config, bot).0;

        let skills = self.skills.read().await;
        let skill_context = skills.build_context();
        if !skill_context.is_empty() {
            prompt.push_str("\n\n# Available Skills\n\n");
            prompt.push_str(&skill_context);
        }

        // Build unified "Available Agents" section: subagent skills + agents/
        // + [[bots]] ids (§7.6). When bots id == persona, agents/ line for that
        // name is dropped (bots id listed; agents description used for copy).
        let subagent_skills = skills.build_subagent_lines();
        drop(skills);
        let agents = self.agents.read().await;
        let covered = bot_ids_covering_persona(&self.config.bots);
        let agent_lines = format_agent_lines_excluding(&agents, &covered);
        let bot_lines = format_bots_available_lines(&self.config.bots, &agents, Some(bot_id));
        drop(agents);

        if let Some(section) =
            format_available_agents_section(&subagent_skills, &agent_lines, &bot_lines)
        {
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

        // Append ambient system context (persona SOUL overlay, shared USER.md,
        // timestamp, location). `build_system_context` already includes the
        // leading `\n\n` separators.
        prompt.push_str(&self.build_system_context(Some(&persona)).await);

        // Warn if system prompt is very large (tight on context window)
        if prompt.len() > 50_000 {
            warn!(
                "System prompt is large: {} bytes — consider reducing skill/agent descriptions",
                prompt.len()
            );
        }

        prompt
    }

    /// Build ambient system context (soul files, timestamp, location).
    ///
    /// When `persona` is `Some`, SOUL prefers `agents/<persona>/SOUL.md`
    /// (§7.4; persona bind = AGENT.md + SOUL). AGENTS.md and USER.md are
    /// always the shared home copies (PO/TL lock). Subagents pass `None` to
    /// keep home-only soul files. Unlike build_system_prompt, this does NOT
    /// include skills/agents listings.
    async fn build_system_context(&self, persona: Option<&str>) -> String {
        let mut ctx = String::new();

        if let Some(home) = &self.config.resolved_home {
            let files = match persona {
                Some(p) => crate::persona_prompt::resolve_persona_soul_files(
                    home,
                    &self.config.agents.directory,
                    p,
                ),
                None => crate::persona_prompt::PersonaSoulFiles::home_only(home),
            };

            // Inject SOUL.md (persona overlay or home)
            let soul_content = crate::learning::read_soul_file(&files.soul).await;
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

            // Inject AGENTS.md — always home learned memory (PO/TL lock)
            let agents_content = crate::learning::read_soul_file(&files.agents_md).await;
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

            // Inject USER.md — always shared home (PO lock: do not split)
            let user_content = crate::learning::read_soul_file(&files.user).await;
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
        let mut prompt = self.build_system_context(None).await;
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

    /// Change the active model and persist to config.toml via the shared
    /// validate → `.bak` → atomic write → restore-on-fail path
    /// ([`crate::config_edit::persist_model_edit`]).
    pub async fn set_model(&self, model_id: &str) -> anyhow::Result<()> {
        let (provider_name, actual_model) = self.validate_model_id(model_id)?;

        // Disk write on blocking thread — same sync bak helpers as `/config set`.
        let path = self.config_path.clone();
        let provider_name_owned = provider_name.clone();
        let actual_owned = actual_model.to_string();
        tokio::task::spawn_blocking(move || {
            crate::config_edit::persist_model_edit(&path, &provider_name_owned, &actual_owned)
        })
        .await
        .map_err(|e| anyhow::anyhow!("set_model persist task join error: {e}"))??;

        self.apply_model_in_memory(model_id).await;
        tracing::info!(model = %model_id, provider = %provider_name, "Model changed and persisted");
        Ok(())
    }

    /// Apply a model id live in memory only (no disk write).
    ///
    /// Used after `/config set openrouter.model` already wrote `config.toml`
    /// through the bak path, so we do not perform a second (redundant) write.
    pub async fn set_model_live(&self, model_id: &str) -> anyhow::Result<()> {
        let (provider_name, _) = self.validate_model_id(model_id)?;
        self.apply_model_in_memory(model_id).await;
        tracing::info!(
            model = %model_id,
            provider = %provider_name,
            "Model changed live (memory only; disk already updated)"
        );
        Ok(())
    }

    fn validate_model_id<'a>(&'a self, model_id: &'a str) -> anyhow::Result<(String, &'a str)> {
        if model_id.is_empty() {
            anyhow::bail!("Model ID cannot be empty");
        }

        // Validate: resolve succeeds for any string
        let (provider, actual_model) = self.registry.resolve_model(model_id);
        if let Some((prefix, _)) = model_id.split_once('/') {
            if self.registry.get_provider(prefix).is_none() {
                tracing::warn!(
                    "Model '{}': prefix '{}' does not match any known provider                      (falling through to default '{}')",
                    model_id,
                    prefix,
                    self.registry.default_provider_name()
                );
            }
        }

        Ok((provider.name().to_string(), actual_model))
    }

    async fn apply_model_in_memory(&self, model_id: &str) {
        let mut current = self.current_model.write().await;
        *current = model_id.to_string();
    }

    /// Register a CancellationToken for `(bot_id, user_id)` before processing starts.
    /// Called at the start of process_message. Returns the token for cancellation checks.
    pub async fn register_cancel_token(&self, bot_id: &str, user_id: &str) -> CancellationToken {
        let key = session_key(bot_id, user_id);
        let token = CancellationToken::new();
        self.cancel_token_registry
            .lock()
            .await
            .insert(key, token.clone());
        token
    }

    /// Cancel in-flight supervisor tasks for `{bot_id}:{user_id}`.
    /// Returns how many tasks were cancelled (0 if no supervisor attached).
    pub async fn cancel_supervisor_session(&self, bot_id: &str, user_id: &str) -> usize {
        let key = session_key(bot_id, user_id);
        let guard = self.supervisor.read().await;
        let Some(sup) = guard.as_ref() else {
            return 0;
        };
        match sup.cancel_for_session(&key).await {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, session = %key, "supervisor cancel_for_session failed");
                0
            }
        }
    }

    /// Cancel processing for a bot+user session.
    ///
    /// Cancels the agent-loop [`CancellationToken`] (if any) **and** any
    /// cancellable supervisor tasks for the same session key. Returns true if
    /// either the loop token or at least one supervisor task was cancelled.
    pub async fn cancel_processing(&self, bot_id: &str, user_id: &str) -> bool {
        let key = session_key(bot_id, user_id);
        let loop_cancelled = {
            let mut map = self.cancel_token_registry.lock().await;
            if let Some(token) = map.remove(&key) {
                token.cancel();
                true
            } else {
                false
            }
        };
        let sup_n = self.cancel_supervisor_session(bot_id, user_id).await;
        loop_cancelled || sup_n > 0
    }

    /// Check if a bot+user session has active processing.
    pub async fn is_processing(&self, bot_id: &str, user_id: &str) -> bool {
        let key = session_key(bot_id, user_id);
        self.cancel_token_registry.lock().await.contains_key(&key)
    }

    /// Queue an injection message for a bot+user session. Returns false if queue is full (max 10).
    pub async fn queue_injection(&self, bot_id: &str, user_id: &str, text: &str) -> bool {
        const MAX_INJECTIONS: usize = 10;
        let key = session_key(bot_id, user_id);
        let mut map = self.pending_injections.lock().await;
        let queue = map.entry(key).or_default();
        if queue.len() >= MAX_INJECTIONS {
            false
        } else {
            queue.push(text.to_string());
            true
        }
    }

    /// Drain all pending injection messages for a bot+user session.
    pub async fn drain_injections(&self, bot_id: &str, user_id: &str) -> Vec<String> {
        let key = session_key(bot_id, user_id);
        let mut map = self.pending_injections.lock().await;
        map.remove(&key).unwrap_or_default()
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
        bot_id: &str,
        user_id: &str,
        conversation_id: &str,
        messages: &mut Vec<ChatMessage>,
    ) {
        let inject_mode = self.get_mid_run_mode(bot_id, user_id).await;
        let injections = self.drain_injections(bot_id, user_id).await;
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
        bot_id: &str,
        user_id: &str,
        sender: tokio::sync::oneshot::Sender<LoopCallbackChoice>,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let key = session_key(bot_id, user_id);
        let mut map = self.pending_loop_callbacks.lock().await;
        map.insert(key, sender)
    }

    /// Take the loop callback sender for a bot+user session, if any.
    pub async fn take_loop_callback(
        &self,
        bot_id: &str,
        user_id: &str,
    ) -> Option<tokio::sync::oneshot::Sender<LoopCallbackChoice>> {
        let key = session_key(bot_id, user_id);
        let mut map = self.pending_loop_callbacks.lock().await;
        map.remove(&key)
    }

    /// Get the current MidRunMode for a bot+user session. Defaults to Steer.
    pub async fn get_mid_run_mode(&self, bot_id: &str, user_id: &str) -> MidRunMode {
        let key = format!("mid_run_mode_{}", session_key(bot_id, user_id));
        self.memory
            .recall("settings", &key)
            .await
            .ok()
            .flatten()
            .and_then(|v| MidRunMode::from_mode_str(&v))
            .unwrap_or(MidRunMode::Steer)
    }

    /// Set the MidRunMode for a bot+user session.
    pub async fn set_mid_run_mode(&self, bot_id: &str, user_id: &str, mode: MidRunMode) {
        let key = format!("mid_run_mode_{}", session_key(bot_id, user_id));
        self.memory
            .remember("settings", &key, mode.as_str(), None)
            .await
            .ok();
    }

    /// Delete the MidRunMode for a user (resets to default).
    pub async fn delete_mid_run_mode(&self, bot_id: &str, user_id: &str) {
        let key = format!("mid_run_mode_{}", session_key(bot_id, user_id));
        self.memory.forget("settings", &key).await.ok();
    }

    /// Remove cancel token for a user (called on process_message exit).
    pub async fn clear_cancel_token(&self, bot_id: &str, user_id: &str) {
        let key = session_key(bot_id, user_id);
        self.cancel_token_registry.lock().await.remove(&key);
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

    /// Run the agent for one turn, returning only the delivered text.
    ///
    /// Thin wrapper over [`Self::process_message_outcome`] for callers that
    /// don't care *why* a run ended (chat, portal, subagents). The
    /// scheduled-task runner uses the outcome form to distinguish a
    /// budget-exhausted run from a clean one.
    pub async fn process_message(
        &self,
        incoming: &IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        tool_ui_mode: crate::tool_registry::ToolUiMode,
    ) -> Result<String> {
        Ok(self
            .process_message_outcome(incoming, tool_event_tx, stream_token_tx, tool_ui_mode)
            .await?
            .text)
    }

    /// Run the agent for one turn, reporting *how* it ended alongside the text.
    ///
    /// Stop reasons matter downstream: a `MaxIterations` finish is a run that
    /// ran out of budget mid-task, which the dead-letter queue records as a
    /// human-gated failure rather than a success (ADR-0013).
    pub async fn process_message_outcome(
        &self,
        incoming: &IncomingMessage,
        tool_event_tx: Option<tokio::sync::mpsc::Sender<crate::platform::tool_notifier::ToolEvent>>,
        stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        tool_ui_mode: crate::tool_registry::ToolUiMode,
    ) -> Result<RunOutcome> {
        let platform = &incoming.platform;
        let bot_id = crate::platform::normalize_bot_id(&incoming.bot_id);
        let user_id = &incoming.user_id;
        let _parsed_chat_id: ChatId = incoming
            .chat_id
            .parse::<i64>()
            .map(ChatId)
            .unwrap_or(ChatId(0));

        // Get or create persistent conversation (isolated by bot_id).
        // Sole-custom-id installs claim legacy default rows; multi-bot §7.3.
        let claim_legacy =
            crate::config::Config::bot_claims_legacy_default(&self.config.bots, bot_id);
        let conversation_id = self
            .memory
            .get_or_create_conversation_with_claim(platform, bot_id, user_id, claim_legacy)
            .await?;

        // Always build the system prompt from the live registry, scoped to
        // this bot's persona (§7.4).
        let current_system_prompt = self.build_system_prompt(bot_id).await;

        // Use ConversationManager for message construction and management
        let skills = self.skills.read().await;
        let mut cmgr = crate::conversation::ConversationManager::new(
            &self.memory,
            platform,
            bot_id,
            user_id,
            current_system_prompt.clone(),
            &skills,
            &self.config,
        )
        .await?;
        drop(skills);

        // RAG: auto-retrieve relevant past messages and inject into system prompt
        if !incoming.text.is_empty() {
            let filtered_msgs: Vec<_> = cmgr
                .messages()
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

        let image_parts = cmgr
            .add_incoming(incoming, &self.config, supports_vision)
            .await?;

        // Build user message content
        let user_msg_content = if image_parts.is_empty() {
            MessageContent::from_text(incoming.text.clone())
        } else {
            let mut parts: Vec<ContentPart> = Vec::new();
            if !incoming.text.is_empty() {
                parts.push(ContentPart::Text {
                    text: incoming.text.clone(),
                });
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

        // Per-turn compaction (ADR 0003 Q1): routine compaction runs once per
        // user turn, before the agentic loop, at 85% of the real provider window.
        let current_model = self.current_model.read().await.clone();
        let context_window = self.registry.effective_context_window(&current_model);
        let compaction_model = self.config.learning.compaction_model.clone();
        let user_model_path = self
            .config
            .resolved_home
            .as_ref()
            .map(|h| h.join("USER.md"));
        let compact_ctx = crate::conversation::CompactionContext {
            llm: &self.llm,
            context_window,
            compaction_model: compaction_model.as_deref(),
            user_model_path: user_model_path.as_deref(),
        };
        if let Err(e) = cmgr.compact_messages(&compact_ctx).await {
            warn!(
                user_id = %user_id,
                error = %format!("{e:#}"),
                "Per-turn compaction failed"
            );
        }

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
        let cancel_token = self.register_cancel_token(bot_id, user_id).await;

        // Build make_ctx closure for ToolContext construction
        let make_ctx = {
            let sandbox_dir = self.config.sandbox.allowed_directory.clone();
            let home_dir = self.config.resolved_home.clone();
            let sender = self.sender.clone();
            let cancel_registry = self.cancel_registry.clone();
            let mode = tool_ui_mode;
            move |_user_id: &str, _chat_id: &str| ToolContext {
                sandbox_dir: sandbox_dir.clone(),
                home_dir: home_dir.clone(),
                sender: sender.clone(),
                cancel_registry: cancel_registry.clone(),
                user_id: _user_id.to_string(),
                chat_id: _chat_id.to_string(),
                tool_ui_mode: mode,
            }
        };

        // §7.6: main loop resolves bots[].tools/model → agents/<persona> → defaults
        let bot_cfg = crate::persona_prompt::bot_for_prompt(&self.config, bot_id);
        let (loop_model, loop_tools) = {
            let agents = self.agents.read().await;
            let persona_skill = agents.get(bot_cfg.persona.trim());
            let persona_model = persona_skill.and_then(|s| s.model.clone());
            let persona_tools = persona_skill.map(|s| s.tools.clone()).unwrap_or_default();
            crate::peer_invoke::resolve_bot_loop_overrides(
                bot_cfg,
                persona_model.as_deref(),
                &persona_tools,
            )
        };

        let loop_config = crate::loop_runner::LoopConfig {
            max_iterations: self.config.max_iterations(),
            empty_response_retry_limit: self.config.empty_response_retry_limit(),
            context_window,
            loop_detection_enabled: true,
            interactive_loop_callback: true,
            allowed_tools: loop_tools,
            langsmith_project: Some(ls_project.clone()),
            model: loop_model,
            tool_event_tx,
            stream_token_tx,
            recovery_nudge: None,
        };

        // §7.5: main loop can peer-invoke (depth/cycle + via attribution).
        // Seed with bot_id (canonical); persona aliases resolve to this key.
        let root_stack = vec![bot_id.to_string()];
        let special_handler = {
            let self_weak = self.self_weak.clone();
            let parent_stack = root_stack.clone();
            move |name: &str, args: &Value, _user_id: &str, _chat_id: &str| {
                let name_owned = name.to_string();
                let args_owned = args.clone();
                let self_weak = self_weak.clone();
                let parent_stack = parent_stack.clone();
                Box::pin(async move {
                    match name_owned.as_str() {
                        "invoke_agent" => {
                            let agent = match self_weak.upgrade() {
                                Some(a) => a,
                                None => return Some("Agent is shutting down".to_string()),
                            };
                            Some(
                                agent
                                    .handle_invoke_agent_tool(&args_owned, parent_stack)
                                    .await,
                            )
                        }
                        "spawn_agents" => {
                            // Parse tasks inline (same shape as subagent handler).
                            let parsed_tasks: Vec<AdHocTask> = if let Some(tasks) =
                                args_owned["tasks"].as_array()
                            {
                                if tasks.is_empty() {
                                    return Some("tasks array is empty".to_string());
                                }
                                let mut parsed = Vec::with_capacity(tasks.len());
                                for (i, task) in tasks.iter().enumerate() {
                                    let system_prompt = match task["system_prompt"].as_str() {
                                        Some(s) => s.to_string(),
                                        None => {
                                            return Some(format!(
                                                "Task at index {}: missing system_prompt",
                                                i
                                            ))
                                        }
                                    };
                                    let prompt = match task["prompt"].as_str() {
                                        Some(p) => p.to_string(),
                                        None => {
                                            return Some(format!(
                                                "Task at index {}: missing prompt",
                                                i
                                            ))
                                        }
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
                                let system_prompt = match args_owned["system_prompt"].as_str() {
                                    Some(s) => s.to_string(),
                                    None => {
                                        return Some("Missing system_prompt or tasks".to_string())
                                    }
                                };
                                let prompt = match args_owned["prompt"].as_str() {
                                    Some(p) => p.to_string(),
                                    None => return Some("Missing prompt".to_string()),
                                };
                                vec![AdHocTask {
                                    system_prompt,
                                    prompt,
                                    model: args_owned["model"].as_str().map(str::to_string),
                                    tools: args_owned["tools"].as_array().map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(str::to_string))
                                            .collect()
                                    }),
                                }]
                            };
                            let agent = match self_weak.upgrade() {
                                Some(a) => a,
                                None => return Some("Agent is shutting down".to_string()),
                            };
                            let futures: Vec<_> = parsed_tasks
                                .into_iter()
                                .map(|task| {
                                    let sp = task.system_prompt.clone();
                                    let p = task.prompt.clone();
                                    let m = task.model.clone();
                                    let t = task.tools.clone();
                                    let a = agent.clone();
                                    let stack = parent_stack.clone();
                                    Box::pin(async move {
                                        a.run_subagent(None, &sp, &p, m.as_deref(), t, stack).await
                                    })
                                })
                                .collect();
                            let results = futures::future::join_all(futures).await;
                            let mut output = String::from(
                                "Spawned agents results:

",
                            );
                            for (i, result) in results.iter().enumerate() {
                                output.push_str(&format!(
                                    "--- Agent {} ---
{}

",
                                    i + 1,
                                    result
                                ));
                            }
                            Some(output)
                        }
                        _ => None,
                    }
                }) as Pin<Box<dyn Future<Output = Option<String>> + Send + 'static>>
            }
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
            Some(Box::new(special_handler)),
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

                self.clear_cancel_token(bot_id, user_id).await;

                // Post-loop soul reflection: if the agent didn't update SOUL.md during the
                // conversation but the soul_updated flag was set by a tool, fire a reflection
                // update to capture session-end insights.
                if self.soul_updated.load(std::sync::atomic::Ordering::Relaxed) {
                    // The soul was already updated by update_soul_file tool during the conversation.
                    // No need to fire a second reflection.
                }

                Ok(RunOutcome {
                    text: final_content,
                    stop: RunStop::FinalResponse,
                })
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
                self.clear_cancel_token(bot_id, user_id).await;
                Ok(RunOutcome {
                    text: "Processing was cancelled.".to_string(),
                    stop: RunStop::Cancelled,
                })
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
                    error: Some(format!(
                        "Reached max iterations ({})",
                        self.config.max_iterations()
                    )),
                    end_time: Self::now_iso8601_static(),
                });
                self.clear_cancel_token(bot_id, user_id).await;
                Ok(RunOutcome {
                    text: "I've reached the maximum number of tool call iterations. Please try rephrasing your request.".to_string(),
                    stop: RunStop::MaxIterations,
                })
            }
            Err(e) => {
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: chain_run_id,
                    outputs: None,
                    error: Some(format!("{:#}", e)),
                    end_time: Self::now_iso8601_static(),
                });
                self.clear_cancel_token(bot_id, user_id).await;
                Err(e)
            }
        }
    }

    /// Build the fire closure shared by every scheduled-task arming path
    /// (startup restore, Telegram tool, portal CRUD). Dispatches a synthetic
    /// agent turn to the background job runner; the response delivery is the
    /// runner's `reply_to` choice, so no platform-specific delivery logic
    /// lives here. `bot` is passed in (rather than read from self) because
    /// the closure must own an Arc and callers like `restore_scheduled_tasks`
    /// run against `&self`.
    pub(crate) fn build_fire_closure(
        job_tx: tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
        store: ScheduledTaskStore,
        task: &ScheduledTask,
    ) -> impl Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static {
        let tid = task.id.clone();
        let uid = task.user_id.clone();
        let cid = task.chat_id.clone();
        let prompt = task.prompt.clone();
        let is_recurring = task.trigger_type == "recurring";
        move || {
            let tx = job_tx.clone();
            let bot = bot.clone();
            let store = store.clone();
            let tid = tid.clone();
            let uid = uid.clone();
            let cid = cid.clone();
            let prompt = prompt.clone();
            let recurring = is_recurring;
            Box::pin(async move {
                let incoming = crate::platform::IncomingMessage {
                    platform: "scheduled_task".to_string(),
                    bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
                    user_id: format!("{uid}:{tid}"),
                    chat_id: cid,
                    user_name: String::new(),
                    text: prompt,
                    attachments: vec![],
                };
                let req = ScheduledJobRequest {
                    incoming,
                    bot,
                    is_recurring: recurring,
                    task_store: store,
                    task_id: tid,
                    rerun_id: None,
                };
                if let Err(e) = tx.send(req) {
                    tracing::error!("Failed to dispatch scheduled job: {}", e);
                }
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        }
    }

    /// Build the dispatch payload for a dead-letter re-fire (ADR-0013).
    /// Mirrors [`Self::build_fire_closure`] exactly (same synthetic
    /// IncomingMessage shape) so the agent processes a re-run identically to
    /// a cron-fired run — the only difference is `rerun_id`, which tells the
    /// runner to resolve the queue row on outcome. Public so main's watchdog
    /// can dispatch without reaching into internals.
    pub fn build_rerun_request(
        job_tx: &tokio::sync::mpsc::UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
        store: ScheduledTaskStore,
        task: &ScheduledTask,
        rerun_id: &str,
    ) -> Result<()> {
        let incoming = crate::platform::IncomingMessage {
            platform: "scheduled_task".to_string(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            user_id: format!("{}:{}", task.user_id, task.id),
            chat_id: task.chat_id.clone(),
            user_name: String::new(),
            text: task.prompt.clone(),
            attachments: vec![],
        };
        let req = ScheduledJobRequest {
            incoming,
            bot,
            is_recurring: task.trigger_type == "recurring",
            task_store: store,
            task_id: task.id.clone(),
            rerun_id: Some(rerun_id.to_string()),
        };
        job_tx.send(req).context("Failed to dispatch rerun")?;
        Ok(())
    }

    /// Arm (schedule) a task with the live JobScheduler and persist the job
    /// id back onto the DB row. One-shot triggers that already passed return
    /// Err (caller decides: restore marks them `completed`, portal 400s).
    pub async fn arm_task(&self, task: &ScheduledTask) -> Result<uuid::Uuid> {
        // NOTE (issue #109, Bug 2): rows created through the Telegram
        // `schedule_task` tool historically never persisted their live job id,
        // so a pre-existing job registered by such a row cannot be looked up
        // here (there is no id to remove by). Disarming such orphans must be
        // done by the *creator* persisting the id up-front — see
        // `schedule_task` routing through this method.
        let fire = Self::build_fire_closure(
            self.job_tx.clone(),
            Arc::clone(&self.bot),
            self.task_store.clone(),
            task,
        );
        let job_id = if task.trigger_type == "one_shot" {
            let delay = parse_one_shot_delay(&task.trigger_value)?;
            self.scheduler
                .add_one_shot_job(delay, &task.description, fire)
                .await?
        } else {
            self.scheduler
                .add_cron_job(&task.trigger_value, &task.description, fire)
                .await?
        };
        // Persist the new job id so disable/delete can find it again. This is
        // the single write-back every arm path (restore / portal / tool) must
        // go through — see issue #109, Bug 2.
        self.task_store
            .update_scheduler_job_id(&task.id, &job_id.to_string())
            .await?;
        Ok(job_id)
    }

    /// Remove a task's live job (if any). Idempotent: an unparseable or
    /// missing scheduler_job_id (already-fired one-shot, pre-restart row)
    /// counts as success — there is simply nothing left to disarm.
    pub async fn disarm_task(&self, task: &ScheduledTask) -> bool {
        match task
            .scheduler_job_id
            .as_deref()
            .map(|j| j.parse::<uuid::Uuid>())
        {
            Some(Ok(job_id)) => self.scheduler.remove_job(job_id).await.is_ok(),
            _ => true,
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
            match self.arm_task(&task).await {
                Ok(sched_id) => {
                    tracing::info!(
                        "Restored scheduled task: {} ({}, job {})",
                        task.id,
                        task.description,
                        sched_id
                    );
                }
                Err(e) => {
                    if task.trigger_type == "one_shot" {
                        // Trigger time passed while the bot was down — the
                        // fire can never happen; retire it like before.
                        tracing::warn!(
                            "Skipping restore of one-shot task {} (trigger has passed or invalid: {})",
                            task.id,
                            e
                        );
                        let _ = self.task_store.set_status(&task.id, "completed").await;
                    } else {
                        tracing::error!(
                            "Failed to restore scheduled task {} ({}): {}",
                            task.id,
                            task.description,
                            e
                        );
                    }
                }
            }
        }

        if count > 0 {
            tracing::info!("Restored {} scheduled task(s) from DB", count);
        }
    }

    /// Clear conversation history for a bot+user session
    pub async fn clear_conversation(
        &self,
        platform: &str,
        bot_id: &str,
        user_id: &str,
    ) -> Result<()> {
        self.memory
            .clear_conversation(platform, bot_id, user_id)
            .await?;
        // Reset mid-run mode to default (Steer)
        self.delete_mid_run_mode(bot_id, user_id).await;
        Ok(())
    }

    /// Get all tool definitions for display
    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut all = self.tool_registry.all_definitions();
        all.extend(self.mcp.tool_definitions());
        all
    }

    /// Handle `invoke_agent` tool args with §7.5 peer depth/cycle guards and
    /// `via <persona>:` attribution. Shared by the main loop and nested subagent loops.
    pub(crate) async fn handle_invoke_agent_tool(
        &self,
        args: &Value,
        invoke_stack: Vec<String>,
    ) -> String {
        let agent_name = match args["bot"]
            .as_str()
            .or_else(|| args["agent"].as_str())
            .or_else(|| args["skill"].as_str())
        {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => return "Missing agent (or bot) name".to_string(),
        };
        let prompt = match args["prompt"].as_str() {
            Some(p) => p.to_string(),
            None => return "Missing prompt".to_string(),
        };
        let model_override = args["model"].as_str().map(str::to_string);
        let tools_override = args["tools"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });

        // Classify first (agents → skills → bots), then guard/push a *canonical*
        // stack key: BotPersona → bot_id (persona is alias); agent/skill keep name
        // unless that name is the caller's persona/bot_id on the stack (self).
        // bot_id ≠ agent pack without explicit map: a [[bots]].id that differs
        // from its persona must not be looked up as agents/<bot_id> / skills.
        let skip_pack =
            crate::peer_invoke::bot_id_is_unmapped_pack_name(&self.config.bots, &agent_name);
        let in_agents = if skip_pack {
            false
        } else {
            self.agents.read().await.get(&agent_name).is_some()
        };
        let in_skills = if skip_pack || in_agents {
            false
        } else {
            self.skills.read().await.get(&agent_name).is_some()
        };
        let source = crate::peer_invoke::resolve_invoke_source(
            &agent_name,
            in_agents,
            in_skills,
            &self.config.bots,
        );
        let stack_key = crate::peer_invoke::stack_key_for_invoke(&agent_name, &source);
        // AgentRegistry/Skill pack named like the caller's persona (or bot_id)
        // must canonicalize to the stacked bot_id and hard-reject as self.
        let stack_key = crate::peer_invoke::canonicalize_self_stack_key(
            &invoke_stack,
            &agent_name,
            &stack_key,
            &self.config.bots,
        );

        if let Err(e) = crate::peer_invoke::guard_peer_invoke(&invoke_stack, &stack_key) {
            warn!(
                peer_target = %agent_name,
                stack_key = %stack_key,
                stack = ?invoke_stack,
                "{e}"
            );
            return e;
        }

        let via_label = match &source {
            Some(crate::peer_invoke::InvokeSource::BotPersona { persona, .. }) => persona.clone(),
            _ => agent_name.clone(),
        };

        info!(
            "Invoking agent '{}' (source: {:?}, stack_key: {}, model_override: {:?}, stack: {:?})",
            agent_name, source, stack_key, model_override, invoke_stack
        );

        let child_stack = crate::peer_invoke::push_invoke_stack(&invoke_stack, &stack_key);
        let result = self
            .run_subagent(
                Some(&agent_name),
                "",
                &prompt,
                model_override.as_deref(),
                tools_override,
                child_stack,
            )
            .await;
        crate::peer_invoke::format_via_attribution(&via_label, &result)
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
        invoke_stack: Vec<String>,
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
                    invoke_stack,
                )
                .await;
        }

        // --- Predefined agent path ---
        let skill_name = skill_name.unwrap(); // safe: we handled None above

        // Resolve model and tool list from registry metadata (or overrides).
        // Order (§7.5): agents registry → skills registry → [[bots]] persona.
        let (resolved_model, declared_tools, max_iter, bot_persona_fallback) = {
            let default_model = self.config.openrouter.model.clone();

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

            let bot_peer = if skill_opt.is_none() {
                crate::peer_invoke::bot_config_for_peer(&self.config.bots, skill_name)
            } else {
                None
            };

            let model = model_override
                .map(str::to_string)
                .or_else(|| skill_opt.as_ref().and_then(|s| s.model.clone()))
                .or_else(|| bot_peer.and_then(|b| b.model.clone()))
                .unwrap_or_else(|| default_model.clone());
            if model == default_model && skill_opt.is_none() && bot_peer.is_none() {
                warn!(
                    "Agent/skill/bot persona '{}' not found; using default model.",
                    skill_name
                );
            }
            let tools = tools_override
                .or_else(|| skill_opt.as_ref().map(|s| s.tools.clone()))
                .or_else(|| bot_peer.and_then(|b| b.tools.clone()))
                .unwrap_or_default();
            let max_i = skill_opt
                .as_ref()
                .and_then(|s| s.max_iterations)
                .unwrap_or_else(|| self.config.max_iterations())
                .min(self.config.max_iterations());
            let bot_fallback = bot_peer.map(|b| b.persona.trim().to_string());
            (model, tools, max_i, bot_fallback)
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

        // Strip YAML frontmatter from content if present
        let body = skill_opt
            .as_ref()
            .map(|s| crate::persona_prompt::strip_md_frontmatter(&s.content));

        let system_content = if let Some(ref persona) = bot_persona_fallback {
            // §7.5 path (3): bot persona with no agents/skills pack — use resolved
            // base prompt (system_prompt_file / AGENT.md / global) as the body.
            let bot = crate::peer_invoke::bot_config_for_peer(&self.config.bots, skill_name)
                .expect("bot_persona_fallback implies bot exists");
            let (base, _) = crate::persona_prompt::resolve_bot_base_prompt(&self.config, bot);
            format!(
                "You are the '{persona}' bot persona (peer invoke).

{base}"
            )
        } else if skip_bootstrap {
            let agent_body = body.as_deref().unwrap_or("");
            format!(
                "You are the '{skill_name}' agent.

{agent_body}"
            )
        } else {
            format!(
                "You are the '{skill_name}' agent. Your first action MUST be to call                  read_agent_file with agent_name='{skill_name}' and relative_path='AGENT.md' to load your instructions."
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
            invoke_stack,
        )
        .await
    }

    /// Shared mini-agentic loop used by both ad-hoc and predefined subagents.
    /// Runs LLM calls, executes whitelisted tools, and returns the final text response.
    /// Returns a boxed future to break type-level cycles with the special_tool_handler.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn run_subagent_loop<'a>(
        &'a self,
        messages: &'a mut Vec<ChatMessage>,
        _subagent_tools: &'a [ToolDefinition],
        allowed_tools: &'a [String],
        model: &'a str,
        max_iter: u32,
        label: &'a str,
        cancel_token: Option<CancellationToken>,
        invoke_stack: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            // Build special_tool_handler for invoke_agent/spawn_agents (circular
            // dependency with run_subagent). Depth/cycle + via attribution: §7.5.
            let special_handler = {
                let self_weak = self.self_weak.clone();
                let parent_stack = invoke_stack.clone();
                move |name: &str, args: &Value, _user_id: &str, _chat_id: &str| {
                    let name_owned = name.to_string();
                    let args_owned = args.clone();
                    let self_weak = self_weak.clone();
                    let parent_stack = parent_stack.clone();
                    Box::pin(async move {
                        match name_owned.as_str() {
                            "invoke_agent" => {
                                let agent = match self_weak.upgrade() {
                                    Some(a) => a,
                                    None => return Some("Agent is shutting down".to_string()),
                                };
                                Some(
                                    agent
                                        .handle_invoke_agent_tool(&args_owned, parent_stack)
                                        .await,
                                )
                            }
                            "spawn_agents" => {
                                let parsed_tasks: Vec<AdHocTask> = if let Some(tasks) =
                                    args_owned["tasks"].as_array()
                                {
                                    if tasks.is_empty() {
                                        return Some("tasks array is empty".to_string());
                                    }
                                    let mut parsed = Vec::with_capacity(tasks.len());
                                    for (i, task) in tasks.iter().enumerate() {
                                        let system_prompt = match task["system_prompt"].as_str() {
                                            Some(s) => s.to_string(),
                                            None => {
                                                return Some(format!(
                                                    "Task at index {}: missing system_prompt",
                                                    i
                                                ))
                                            }
                                        };
                                        let prompt = match task["prompt"].as_str() {
                                            Some(p) => p.to_string(),
                                            None => {
                                                return Some(format!(
                                                    "Task at index {}: missing prompt",
                                                    i
                                                ))
                                            }
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
                                    let system_prompt = match args_owned["system_prompt"].as_str() {
                                        Some(s) => s.to_string(),
                                        None => {
                                            return Some(
                                                "Missing system_prompt or tasks".to_string(),
                                            )
                                        }
                                    };
                                    let prompt = match args_owned["prompt"].as_str() {
                                        Some(p) => p.to_string(),
                                        None => return Some("Missing prompt".to_string()),
                                    };
                                    vec![AdHocTask {
                                        system_prompt,
                                        prompt,
                                        model: args_owned["model"].as_str().map(str::to_string),
                                        tools: args_owned["tools"].as_array().map(|arr| {
                                            arr.iter()
                                                .filter_map(|v| v.as_str().map(str::to_string))
                                                .collect()
                                        }),
                                    }]
                                };
                                let agent = match self_weak.upgrade() {
                                    Some(a) => a,
                                    None => return Some("Agent is shutting down".to_string()),
                                };
                                let futures: Vec<_> = parsed_tasks
                                    .into_iter()
                                    .map(|task| {
                                        let sp = task.system_prompt.clone();
                                        let p = task.prompt.clone();
                                        let m = task.model.clone();
                                        let t = task.tools.clone();
                                        let a = agent.clone();
                                        let stack = parent_stack.clone();
                                        Box::pin(async move {
                                            a.run_subagent(None, &sp, &p, m.as_deref(), t, stack)
                                                .await
                                        })
                                    })
                                    .collect();
                                let results = futures::future::join_all(futures).await;
                                let mut output = String::from("Spawned agents results:\n\n");
                                for (i, result) in results.iter().enumerate() {
                                    output.push_str(&format!(
                                        "--- Agent {} ---\n{}\n\n",
                                        i + 1,
                                        result
                                    ));
                                }
                                Some(output)
                            }
                            _ => None,
                        }
                    })
                        as Pin<Box<dyn Future<Output = Option<String>> + Send + 'static>>
                }
            };

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
                    user_id: String::new(),
                    chat_id: String::new(),
                    tool_ui_mode: crate::tool_registry::ToolUiMode::Minimal,
                }
            };

            let allowed_tools_vec: Vec<String> = allowed_tools.to_vec();
            let subagent_window = self.registry.effective_context_window(model);
            let loop_config = crate::loop_runner::LoopConfig {
                max_iterations: max_iter,
                empty_response_retry_limit: self.config.empty_response_retry_limit(),
                context_window: subagent_window,
                loop_detection_enabled: true,
                interactive_loop_callback: false,
                allowed_tools: Some(allowed_tools_vec),
                langsmith_project: None,
                model: Some(model.to_string()),
                tool_event_tx: None,
                stream_token_tx: None,
                recovery_nudge: None,
            };

            let outcome = crate::loop_runner::AgenticLoop::new(
                &self.llm,
                &self.tool_registry,
                &self.mcp,
                &loop_config,
                cancel_token,
                None,
                None,
                self.sender.as_ref() as &dyn PlatformSender,
                Box::new(make_ctx),
                Some(Box::new(special_handler)),
            )
            .run(
                &mut crate::loop_runner::MessageContainer::Plain(std::mem::take(messages)),
                "",
                "",
            )
            .await;

            match outcome {
                Ok(crate::loop_runner::LoopOutcome::FinalResponse(text)) => text,
                Ok(crate::loop_runner::LoopOutcome::Cancelled) => {
                    format!("Subagent '{}' cancelled by user.", label)
                }
                Ok(crate::loop_runner::LoopOutcome::MaxIterations) => {
                    format!(
                        "Subagent '{}' reached the maximum number of iterations ({}).",
                        label, max_iter
                    )
                }
                Err(e) => format!("Subagent '{}' error: {}", label, e),
            }
        })
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
///
/// Uses the SAME parser configuration tokio-cron-scheduler uses internally
/// (`croner` with `with_seconds_required()` + `with_dom_and_dow()`), so
/// anything that passes here is guaranteed to be accepted by `Job::new_async`.
/// The previous implementation only counted whitespace-separated fields — a
/// gate that let "not a cron at all here" through (six words, zero meaning)
/// and surfaced the real failure later as an opaque scheduler error.
pub(crate) fn validate_cron_expr(expr: &str) -> anyhow::Result<()> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 6 {
        anyhow::bail!(
            "Cron expression must have 6 fields (sec min hour day month weekday), got {}: '{}'",
            fields.len(),
            expr
        );
    }
    croner::Cron::new(expr)
        .with_seconds_required()
        .with_dom_and_dow()
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid cron expression '{}': {}", expr, e))?;
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
    fn test_validate_cron_expr_rejects_garbage_with_six_words() {
        // Regression: the old gate only counted fields, so six random words
        // sailed through and the failure surfaced later as an opaque
        // arm_failed from the scheduler.
        assert!(validate_cron_expr("not a cron at all here").is_err());
        assert!(validate_cron_expr("0 30 9 * * BADDAY").is_err());
        assert!(validate_cron_expr("99 99 99 99 99 99").is_err());
    }

    #[test]
    fn test_validate_cron_expr_accepts_what_scheduler_accepts() {
        for ok in [
            "0 30 7 * * *",      // every day 07:30:00 (weather-report shape)
            "0 0 9 * * MON-FRI", // weekday mornings
            "15 30 12 1,15 * *", // 12:30:15 on the 1st and 15th
        ] {
            assert!(validate_cron_expr(ok).is_ok(), "{ok} must validate");
        }
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
        let section = format_available_agents_section("", "", "");
        assert!(
            section.is_none(),
            "expected None when both inputs are empty"
        );
    }

    #[test]
    fn test_format_available_agents_section_only_subagent_nonempty() {
        let section = format_available_agents_section("- sub line", "", "").expect("expected Some");
        assert!(section.contains("# Available Agents"));
        assert!(section.contains("- sub line"));
        assert!(section.contains("All available agents are listed below"));
        assert!(section.contains("invoke_agent"));
        // No separator needed when only one source is present
        assert!(!section.contains("- sub line\n\n"));
    }

    #[test]
    fn test_format_available_agents_section_only_agents_nonempty() {
        let section =
            format_available_agents_section("", "- agent line", "").expect("expected Some");
        assert!(section.contains("# Available Agents"));
        assert!(section.contains("- agent line"));
        assert!(section.contains("All available agents are listed below"));
    }

    #[test]
    fn test_format_available_agents_section_both_nonempty_merged() {
        let section = format_available_agents_section("- sub line", "- agent line", "")
            .expect("expected Some");

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
        let section = format_available_agents_section("- sub", "- ag", "").expect("expected Some");
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
    fn test_format_available_agents_section_includes_bot_lines() {
        let section = format_available_agents_section(
            "",
            "- agent line",
            "- **r1** (persona: researcher) — Research specialist",
        )
        .expect("expected Some");
        assert!(section.contains("- **r1** (persona: researcher)"));
        assert!(section.contains("- agent line"));
        let agent_idx = section.find("- agent line").unwrap();
        let bot_idx = section.find("- **r1**").unwrap();
        assert!(agent_idx < bot_idx, "bot lines follow agent-dir lines");
    }

    fn test_bot_cfg(id: &str, persona: &str) -> crate::config::BotConfig {
        crate::config::BotConfig {
            id: id.to_string(),
            bot_token: format!("tok-{id}"),
            allowed_user_ids: vec![1],
            persona: persona.to_string(),
            system_prompt_file: None,
            model: None,
            tools: None,
        }
    }

    fn test_agent_skill(name: &str, description: &str, tools: Vec<&str>) -> crate::skills::Skill {
        use crate::skills::Skill;

        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: String::new(),
            tags: vec![],
            model: None,
            tools: tools.into_iter().map(str::to_string).collect(),
            max_iterations: None,
            skip_bootstrap: true,
            supervisor_workflow: None,
            supervisor_required_caps: vec![],
        }
    }

    #[test]
    fn test_format_bots_available_lines_excludes_caller_bot_id() {
        let agents = SkillRegistry::new();
        let bots = vec![
            test_bot_cfg("qa", "main"),
            test_bot_cfg("qa2", "researcher"),
        ];
        let lines = format_bots_available_lines(&bots, &agents, Some("qa2"));
        assert!(
            !lines.contains("**qa2**"),
            "caller bot_id must be omitted: {lines}"
        );
        assert!(
            lines.contains("**qa**"),
            "peer bots must remain listed: {lines}"
        );
    }

    #[test]
    fn test_format_bots_available_lines_lists_id_and_copies_description() {
        let mut agents = SkillRegistry::new();
        agents.register(
            test_agent_skill(
                "researcher",
                "Research specialist. Web/docs digests with citations.",
                vec!["read_file"],
            ),
            std::path::PathBuf::from("/tmp/researcher"),
        );
        let bots = vec![
            test_bot_cfg("main", "main"),
            test_bot_cfg("r1", "researcher"),
        ];
        let lines = format_bots_available_lines(&bots, &agents, None);
        assert!(lines.contains("- **main**:"));
        assert!(lines.contains("- **r1** (persona: researcher) — Research specialist"));
        assert!(lines.contains("invoke_agent(agent=\"r1\""));
        assert!(lines.contains("persona alias: `researcher`"));
    }

    #[test]
    fn test_bot_id_equals_persona_skips_duplicate_agent_line() {
        let mut agents = SkillRegistry::new();
        agents.register(
            test_agent_skill("researcher", "Research specialist from agents/", vec![]),
            std::path::PathBuf::from("/tmp/researcher"),
        );
        agents.register(
            test_agent_skill("verifier", "Zero-trust verifier", vec![]),
            std::path::PathBuf::from("/tmp/verifier"),
        );
        let bots = vec![test_bot_cfg("researcher", "researcher")];
        let covered = bot_ids_covering_persona(&bots);
        assert!(covered.contains("researcher"));
        let agent_lines = format_agent_lines_excluding(&agents, &covered);
        assert!(
            !agent_lines.contains("**researcher**"),
            "agents/ line for id==persona must be dropped: {agent_lines}"
        );
        assert!(agent_lines.contains("**verifier**"));
        let bot_lines = format_bots_available_lines(&bots, &agents, None);
        assert!(bot_lines.contains("- **researcher**: Research specialist from agents/"));
        let section = format_available_agents_section("", &agent_lines, &bot_lines).expect("Some");
        assert_eq!(
            section.matches("**researcher**").count(),
            1,
            "researcher must appear exactly once"
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

    // ---------------------------------------------------------------
    // ADR-0013: run-stop classification (max-iterations notify policy)
    // ---------------------------------------------------------------

    #[test]
    fn run_outcome_flags_max_iterations_for_the_runner() {
        let stalled = RunOutcome {
            text: "I've reached the maximum number of tool call iterations.".to_string(),
            stop: RunStop::MaxIterations,
        };
        assert!(
            stalled.is_max_iterations(),
            "the runner must be able to tell a budget-exhausted run from a clean one"
        );
    }

    #[test]
    fn clean_runs_are_never_treated_as_stalled() {
        for stop in [RunStop::FinalResponse, RunStop::Cancelled, RunStop::Llm] {
            let o = RunOutcome {
                text: "x".to_string(),
                stop,
            };
            assert!(
                !o.is_max_iterations(),
                "stop reason {stop:?} must not trigger the human gate"
            );
        }
    }

    /// The two-strike policy hinges on this: a max-iterations run is recorded
    /// as a human-gated row, never an auto-fire one. Guard the distinction at
    /// the type level so a future refactor can't quietly swap them.
    #[tokio::test]
    async fn manual_enqueue_is_awaiting_user_and_never_due() {
        use crate::scheduler::reruns::{RerunQueue, RerunState};
        use rusqlite::Connection;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let conn: Arc<Mutex<Connection>> =
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        {
            let c = conn.lock().await;
            c.execute_batch(
                "CREATE TABLE pending_reruns (
                    id TEXT PRIMARY KEY, task_id TEXT NOT NULL, original_run_id TEXT NOT NULL,
                    fail_reason TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL DEFAULT 'queued', next_eligible_at TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')));",
            )
            .unwrap();
        }
        let q = RerunQueue::new(conn.clone());
        let id = q
            .enqueue_manual("t1", "run-1", "Reached max iterations (25)")
            .await
            .unwrap();
        let row = q.get(&id).await.unwrap().unwrap();
        assert_eq!(row.state, RerunState::AwaitingUser);
        assert_eq!(
            row.attempts, 1,
            "pre-burned attempt: no auto re-fire budget"
        );
        {
            let c = conn.lock().await;
            c.execute(
                "UPDATE pending_reruns SET next_eligible_at = datetime('now','-1 hour') WHERE id=?1",
                rusqlite::params![id],
            )
            .unwrap();
        }
        assert!(
            q.due().await.unwrap().is_empty(),
            "a stalled run must never be auto-dispatched, even when overdue"
        );
    }

    #[test]
    fn test_session_key_isolates_bots_for_same_user() {
        assert_eq!(session_key("main", "42"), "main:42");
        assert_eq!(
            session_key("", "42"),
            format!("{}:42", crate::platform::DEFAULT_BOT_ID)
        );
        assert_ne!(
            session_key("bot_a", "user1"),
            session_key("bot_b", "user1"),
            "/stop on bot A must not share a key with bot B"
        );
    }

    /// Cancel-registry keying: cancelling bot_a must not cancel bot_b for the
    /// same Telegram user. Mirrors Agent::{register_cancel_token,cancel_processing}.
    #[tokio::test]
    async fn test_cancel_token_for_bot_a_does_not_cancel_bot_b() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        let registry: Arc<Mutex<HashMap<String, CancellationToken>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let register =
            |bot_id: &'static str,
             user_id: &'static str,
             reg: Arc<Mutex<HashMap<String, CancellationToken>>>| async move {
                let token = CancellationToken::new();
                let key = session_key(bot_id, user_id);
                reg.lock().await.insert(key, token.clone());
                token
            };
        let cancel = |bot_id: &'static str,
                      user_id: &'static str,
                      reg: Arc<Mutex<HashMap<String, CancellationToken>>>| async move {
            let key = session_key(bot_id, user_id);
            let mut map = reg.lock().await;
            if let Some(token) = map.remove(&key) {
                token.cancel();
                true
            } else {
                false
            }
        };

        let token_a = register("bot_a", "user1", registry.clone()).await;
        let token_b = register("bot_b", "user1", registry.clone()).await;

        assert!(cancel("bot_a", "user1", registry.clone()).await);
        assert!(token_a.is_cancelled());
        assert!(
            !token_b.is_cancelled(),
            "cancelling bot_a must leave bot_b's token intact"
        );
        assert!(
            registry
                .lock()
                .await
                .contains_key(&session_key("bot_b", "user1")),
            "bot_b session must still be registered"
        );
    }
}
