# Architecture Deepening — Phase 1

## Overview

Deepen the RustFox codebase by extracting four shallow modules into deeper ones,
improving locality, testability, and AI-navigability. All changes are pure
refactors — no new features, no behavioral change.

Split into 4 milestones (M1–M4), each independently mergeable.
M1 and M2 are independent; M3 depends on both; M4 can be done any time.

## Motivation

`src/agent.rs` (~4965 lines) mixes tool dispatch, conversation management,
platform interaction, system prompt building, compaction, subagent orchestration,
scheduling, and soul file management. This raises the cost of every change:
a new tool touches Agent, a new platform would require duplicating the loop,
and compaction fixes must avoid the subagent loop's divergent copy.

## Glossary

Terms introduced or sharpened by this design (see also `CONTEXT.md`):

| Term | Definition |
|------|------------|
| ToolHandler | A module that defines one or more tool definitions and can execute them. Registered into a ToolRegistry. |
| ToolRegistry | An index of ToolHandlers that provides tool definitions to the LLM and dispatches `execute(name, args, ctx)`. |
| ToolContext | The ambient context passed to every tool execution: sandbox path, platform sender, cancel registry, and all core services (memory, scheduler, skills, llm, mcp). |
| PlatformSender | A trait for sending messages, files, and interactive UI (keyboards, buttons) to a chat. One adapter per platform (Telegram, Discord, etc.). |
| PlatformMessageId | An opaque string encoding a platform-specific message identifier. For Telegram it is `chat_id:message_id` (`"12345:678"`). Parsed by each adapter. |
| CancelRegistry | A shared map of `oneshot::Sender<()>` channels keyed by opaque ID. Any tool can register; the Telegram callback handler cancels by ID. Distinct from `CancellationToken` (used for `/stop`). |
| ConversationManager | Owns the message history and compaction state. Prepares prompts, injects RAG context, processes attachments, applies steer messages. |
| LoopRunner | Configurable iterator over LLM calls and tool results. Supports main-agent and subagent modes via LoopConfig. |

---

## M1: ToolRegistry + PlatformSender + CancelRegistry

### Files changed/created

| File | Status | Purpose |
|------|--------|---------|
| `src/tool_registry.rs` | New | `ToolHandler` trait + `ToolRegistry` |
| `src/cancel_registry.rs` | New | `CancelRegistry` struct |
| `src/platform/sender.rs` | New | `PlatformSender` trait + `PlatformMessageId` |
| `src/platform/telegram.rs` | Modify | Implements `PlatformSender`, add `show_cancel_button`, `edit_message` |
| `src/platform/mod.rs` | Modify | Re-export sender module |
| `src/tools.rs` | Modify | Refactor into `BuiltinTools` handler |
| `src/llm.rs` | No change | Shared types stay |
| `src/agent.rs` | Modify | Remove `Arc<Bot>`, `running_commands`, inline tool definitions; hold `ToolRegistry`, `Arc<dyn PlatformSender>`, `Arc<CancelRegistry>` |
| `src/mcp.rs` | No change | Stays as-is; MCP tools called directly by Agent via prefix check |
| `src/main.rs` | Modify | Construct ToolRegistry + CancelRegistry; wire into Agent |

### ToolHandler trait

```rust
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn define(&self) -> Vec<ToolDefinition>;
    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult;
}
```

`ToolResult = anyhow::Result<String>`.

### ToolContext

```rust
pub struct ToolContext {
    // Per-execution ambient data
    pub sandbox_dir: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub sender: Arc<dyn PlatformSender>,
    pub cancel_registry: Arc<CancelRegistry>,
    pub user_id: String,
    pub chat_id: String,
}
```

Each handler holds its own long-lived dependencies (e.g. `MemoryTools` holds `MemoryStore`, `SchedulingTools` holds `Scheduler` + `ScheduledTaskStore` + `job_tx`, `SkillTools` holds skill/agent directories + `LlmClient`, `CommandTool` holds `PlatformSender` + `CancelRegistry`). `ToolContext` is for per-call data only.

### ToolRegistry

```rust
pub struct ToolRegistry {
    handlers: Vec<Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, handler: Box<dyn ToolHandler>);
    pub fn all_definitions(&self) -> Vec<ToolDefinition>;
    pub async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult;
}
```

`execute` iterates handlers, calls first match. Unknown tool returns `anyhow::bail!("Unknown tool: {name}")`.

### PlatformSender trait

```rust
/// Opaque message identifier returned by the platform after sending.
/// Telegram encoding: `"{chat_id_int}:{message_id_int}"`.
/// Each adapter documents its own encoding.
pub type PlatformMessageId = String;

#[async_trait]
pub trait PlatformSender: Send + Sync {
    async fn send_message(&self, chat_id: &str, text: &str, format: MessageFormat) -> Result<PlatformMessageId>;
    async fn send_file(&self, chat_id: &str, path: &Path, caption: Option<&str>) -> Result<PlatformMessageId>;
    async fn show_cancel_button(&self, chat_id: &str, text: &str, cancel_id: &str) -> Result<PlatformMessageId>;
    async fn edit_message(&self, chat_id: &str, message_id: &PlatformMessageId, text: &str) -> Result<()>;
    async fn notify_shutdown(&self, chat_id: &str) -> Result<()>;
}
```

`MessageFormat` stays in `platform/telegram.rs` for now (revisit when a second platform exists).

### CancelRegistry

```rust
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

impl CancelRegistry {
    pub fn new() -> Self;
    pub fn register(&self, id: String, tx: oneshot::Sender<()>);
    pub fn cancel(&self, id: &str) -> bool;
    pub fn unregister(&self, id: &str);
}
```

**Two cancellation mechanisms coexist:**

| Mechanism | Scope | Trigger | Used by |
|-----------|-------|---------|---------|
| `CancellationToken` per user | Entire agentic loop | `/stop` command | `process_message`, `run_subagent_loop` |
| `CancelRegistry` per command ID | Single command execution | Cancel button callback | `CommandTool` |

`CancelRegistry` is for fine-grained command cancellation; `CancellationToken` is for aborting the entire processing session.

### Handler module breakdown

Each handler module is a struct that implements `ToolHandler`:

```rust
pub struct BuiltinTools {
    sandbox_dir: PathBuf,
    home_dir: Option<PathBuf>,
}

impl BuiltinTools {
    pub fn new(sandbox_dir: PathBuf, home_dir: Option<PathBuf>) -> Self;
}
// Defines: read_file, write_file, list_files, send_file, plan_create/update/view,
//          try_new_tech, self_upgrade, patch_skill, soul file tools
```

```rust
pub struct MemoryTools {
    memory: MemoryStore,
}

impl MemoryTools {
    pub fn new(memory: MemoryStore) -> Self;
}
// Defines: remember, recall, search_memory
```

```rust
pub struct SchedulingTools {
    task_store: ScheduledTaskStore,
    scheduler: Arc<Scheduler>,
    job_tx: UnboundedSender<ScheduledJobRequest>,
    bot: Arc<Bot>,           // kept for ScheduledJobRequest (fire closures)
}

impl SchedulingTools {
    pub fn new(
        task_store: ScheduledTaskStore,
        scheduler: Arc<Scheduler>,
        job_tx: UnboundedSender<ScheduledJobRequest>,
        bot: Arc<Bot>,
    ) -> Self;
}
// Defines: schedule_task, list_scheduled_tasks, cancel_scheduled_task,
//          get_scheduled_task_history, rerun_scheduled_task
```

**`Arc<Bot>` ownership:** Agent no longer holds `Arc<Bot>` directly. `SchedulingTools` holds one because `ScheduledJobRequest` carries a `bot: Arc<Bot>` field used by the background runner to send scheduled responses. The background runner (in `main.rs`) will continue to use `req.bot` — this code path is not moved into the platform seam (it bypasses the interactive sender).

```rust
pub struct SkillTools {
    llm: LlmClient,
    skills_dir: PathBuf,
    agents_dir: PathBuf,
}

impl SkillTools {
    pub fn new(llm: LlmClient, skills_dir: PathBuf, agents_dir: PathBuf) -> Self;
}
// Defines: write_skill_file, read_skill_file, reload_skills, invoke_agent,
//          spawn_agents, write_agent_file, read_agent_file, reload_agents
```

```rust
pub struct CommandTool {
    sandbox_dir: PathBuf,
    cancel_registry: Arc<CancelRegistry>,
    sender: Arc<dyn PlatformSender>,
}
// Defines: execute_command
```

**`execute_command` full flow:**
1. `CommandTool::execute("execute_command", args, ctx)` is called
2. Spawns `sh -c <command>` in sandbox_dir
3. Calls `ctx.sender.show_cancel_button(chat_id, text, cmd_id)` → gets `PlatformMessageId`
4. Selects on stdout/stderr vs cancel channel (`ctx.cancel_registry`):
   - Cancel received: kills child, `ctx.sender.edit_message(chat_id, &msg_id, "Cancelled")`
   - Output received: `ctx.sender.edit_message(chat_id, &msg_id, updated_text)`
   - Command exits: returns output text

### Agent changes

```rust
pub struct Agent {
    // Kept
    pub llm: LlmClient,
    pub registry: Arc<ProviderRegistry>,
    pub config: Config,
    pub mcp: McpManager,
    pub memory: MemoryStore,
    pub skills: RwLock<SkillRegistry>,
    pub agents: RwLock<SkillRegistry>,
    pub task_store: ScheduledTaskStore,
    pub scheduler: Arc<Scheduler>,
    pub self_weak: Weak<Agent>,
    pub job_tx: UnboundedSender<ScheduledJobRequest>,
    pub langsmith: Arc<LangSmithClient>,
    pub restart_pending: AtomicBool,
    pub soul_updated: AtomicBool,
    pub current_model: RwLock<String>,
    pub config_path: PathBuf,
    pub cancel_token_registry: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub pending_injections: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub pending_loop_callbacks: Arc<Mutex<HashMap<String, oneshot::Sender<LoopCallbackChoice>>>>,

    // New
    pub tool_registry: ToolRegistry,
    pub sender: Arc<dyn PlatformSender>,
    pub cancel_registry: Arc<CancelRegistry>,

    // Removed
    // pub bot: Arc<Bot>,
    // pub running_commands: Arc<Mutex<HashMap<String, RunningCommand>>>,
}
```

- `execute_tool(name, args, user_id, chat_id)` → checks `mcp_` prefix, calls `self.mcp.call_tool(name, args)` or `self.tool_registry.execute(name, args, ctx).await`
- `all_tool_definitions()` → `self.mcp.tool_definitions()` + `self.tool_registry.all_definitions()`
- `memory_tool_definitions()`, `scheduling_tool_definitions()`, `skill_tool_definitions()` — removed, handlers register themselves

### Main.rs changes

```rust
// Construct CancelRegistry
let cancel_registry = Arc::new(CancelRegistry::new());

// Construct PlatformSender
let sender: Arc<dyn PlatformSender> = Arc::new(TelegramAdapter::new(bot.clone()));

// Construct ToolRegistry
let mut tool_registry = ToolRegistry::new();
tool_registry.register(Box::new(BuiltinTools::new(
    config.sandbox.allowed_directory.clone(),
    config.resolved_home.clone(),
)));
tool_registry.register(Box::new(MemoryTools::new(memory.clone())));
tool_registry.register(Box::new(SchedulingTools::new(
    task_store.clone(),
    scheduler.clone(),
    job_tx.clone(),
    bot.clone(),
)));
tool_registry.register(Box::new(SkillTools::new(
    llm.clone(),
    config.skills.directory.clone(),
    config.agents.directory.clone(),
)));
tool_registry.register(Box::new(CommandTool::new(
    config.sandbox.allowed_directory.clone(),
    cancel_registry.clone(),
    sender.clone(),
)));

// Pass to Agent (instead of bot directly)
let agent = Arc::new_cyclic(|weak| {
    Agent::new(
        config.clone(), registry.clone(), mcp_manager, memory.clone(),
        skills, agents, task_store.clone(), scheduler.clone(), weak.clone(),
        job_tx, langsmith.clone(), config_path.clone(),
        tool_registry, sender, cancel_registry,
    )
});
```

`ScheduledJobRequest` and background runner stay unchanged (they use `req.bot` directly).

### MCP dispatch

MCP tools remain unregistered. `Agent::execute_tool` becomes:

```rust
async fn execute_tool(&self, name: &str, args: &Value, user_id: &str, chat_id: ChatId) -> String {
    if name.starts_with("mcp_") {
        return self.mcp.call_tool(name, args).await.unwrap_or_else(|e| format!("Error: {e}"));
    }
    let ctx = ToolContext {
        sandbox_dir: self.config.sandbox.allowed_directory.clone(),
        home_dir: self.config.resolved_home.clone(),
        sender: self.sender.clone(),
        cancel_registry: self.cancel_registry.clone(),
        user_id: user_id.to_string(),
        chat_id: chat_id.to_string(),
    };
    self.tool_registry.execute(name, args, ctx).await
        .unwrap_or_else(|e| format!("Error: {e}"))
}
```

The MCP prefix check is unavoidable unless we register MCP tools dynamically (which adds complexity around reconnection). The 3-line prefix check is an acceptable cost.

---

## M2: ConversationManager

### Files changed/created

| File | Status | Purpose |
|------|--------|---------|
| `src/conversation.rs` | New | `ConversationManager` struct |
| `src/agent.rs` | Modify | `process_message` uses `ConversationManager` |
| `src/agent_prompt.rs` | No change | `PreparedPrompt`, `find_tool_groups` stay |
| `src/memory/rag.rs` | No change | RAG functions stay |
| `src/file_processor/` | No change | Attachment processing stays |

### ConversationManager

```rust
pub struct ConversationManager {
    messages: Vec<ChatMessage>,
    meta: ConversationMeta,
    memory: MemoryStore,       // owns a clone (MemoryStore: Clone)
    conversation_id: String,
}

impl ConversationManager {
    /// Construct from existing conversation, loading history + building system prompt.
    pub async fn new(
        memory: &MemoryStore,
        platform: &str,
        user_id: &str,
        system_prompt: String,
        skills: &SkillRegistry,
        config: &Config,
    ) -> Result<Self>;

    /// Process an incoming message: extract attachments, run RAG, save user message.
    /// Returns the ContentParts for multi-modal messages.
    pub async fn add_incoming(
        &mut self,
        incoming: &IncomingMessage,
        llm: &LlmClient,
        config: &Config,
    ) -> Result<Vec<ContentPart>>;

    pub fn add_user_turn(&mut self, msg: ChatMessage);
    pub fn add_assistant_turn(&mut self, msg: ChatMessage);
    pub fn add_tool_result(&mut self, msg: ChatMessage);
    pub fn system_prompt_mut(&mut self) -> &mut String;
    pub fn inject_rag_context(&mut self, rag_block: &str);
    pub fn apply_steer(&mut self, text: &str, mode: MidRunMode);
    pub async fn compact_tier3(&mut self, llm: &LlmClient, context_window: usize);
    pub async fn compact_tier4(&mut self, llm: &LlmClient, context_window: usize) -> Result<bool>;
    pub fn prepare(&self, context_window: usize) -> PreparedPrompt;
    pub fn messages(&self) -> &[ChatMessage];
    pub fn into_messages(self) -> Vec<ChatMessage>;
}
```

- `new` loads history from MemoryStore, builds the system prompt (via `Agent::build_system_prompt`), optionally injects current system context (soul files, timestamp)
- `add_incoming` runs attachment processing (`file_processor::process_attachments`), RAG auto-retrieval (`memory::rag::auto_retrieve_context`), saves the user message to DB, and returns any image ContentParts
- `compact_tier3` / `compact_tier4` move the static methods from Agent (they take `llm` as parameter because LlmClient is not owned by the manager)
- `prepare` delegates to `prepare_messages_for_llm` (in agent_prompt.rs)
- `apply_steer` drains pending injections and pushes steer/queue messages

### process_message refactor

The outer shape becomes:

```rust
let mut cmgr = ConversationManager::new(
    &self.memory, platform, user_id,
    current_system_prompt, &skills, &self.config,
).await?;

let image_parts = cmgr.add_incoming(incoming, &self.llm, &self.config).await?;

// ... decide image_parts handling, build user_msg_content ...

cmgr.add_user_turn(user_msg);

let mut loop_outcome = AgenticLoop {
    llm: &self.llm,
    tools: &self.tool_registry,
    config: &loop_config,
    cancel: Some(cancel_token),
    chain_run_id: Some(chain_run_id.clone()),
    langsmith: Some(&self.langsmith),
    platform_sender: self.sender.as_ref(),
}.run(&mut cmgr).await?;
// ... handle loop_outcome ...
```

---

## M3: LoopRunner

### Files changed/created

| File | Status | Purpose |
|------|--------|---------|
| `src/loop_runner.rs` | New | `LoopConfig`, `AgenticLoop`, `LoopOutcome` |
| `src/agent.rs` | Modify | `process_message` and `run_subagent_loop` delegate to `AgenticLoop` |

### LoopConfig

```rust
pub type ToolResult = anyhow::Result<String>;

pub struct LoopConfig {
    pub max_iterations: u32,
    pub empty_response_retry_limit: u32,
    pub compaction_enabled: bool,           // tier 3+4
    pub loop_detection_enabled: bool,
    pub interactive_loop_callback: bool,    // true = main loop (inline keyboard), false = auto-nudge
    pub allowed_tools: Option<Vec<String>>, // None = all tools, Some = whitelist (subagent mode)
    pub langsmith_project: Option<String>,
    pub tool_event_tx: Option<mpsc::Sender<ToolEvent>>,
    pub stream_token_tx: Option<mpsc::Sender<String>>,
}
```

`ToolEvent` is re-exported from `platform::tool_notifier::ToolEvent`.

### AgenticLoop

```rust
pub struct AgenticLoop<'a> {
    llm: &'a LlmClient,
    tools: &'a ToolRegistry,
    mcp: &'a McpManager,
    config: &'a LoopConfig,
    cancel: Option<CancellationToken>,
    chain_run_id: Option<String>,
    langsmith: Option<&'a LangSmithClient>,
    platform_sender: &'a dyn PlatformSender,
}

impl AgenticLoop<'_> {
    pub async fn run(
        &self,
        conv_manager: &mut ConversationManager,
    ) -> Result<LoopOutcome>;
}

pub enum LoopOutcome {
    FinalResponse(String),
    Cancelled,
    MaxIterations,
}
```

**LangSmith wiring:** `AgenticLoop::run` starts a LangSmith `chain` run (if `langsmith_project` is set), wraps each LLM call as an `llm` sub-run, wraps each tool call as a `tool` sub-run, and wraps compaction as a `chain` sub-run. The `chain_run_id` stored in the struct is the root run.

**Tool event broadcasting:** When `tool_event_tx` is set, the loop sends `ToolEvent::Started` before each tool execution and `ToolEvent::Completed` after.

**Streaming:** When `stream_token_tx` is set, the final response text is pushed through the channel in small chunks after the loop completes.

**Subagent tool filtering:** When `allowed_tools` is `Some(whitelist)`, tool execution checks the whitelist before dispatching. Tools not in the whitelist return `"Tool '{name}' is not available to this agent."`. MCP tools are always filtered by the whitelist too (the `mcp_` prefix tool names are in the whitelist if allowed).

### Usage

```rust
// Main loop
let outcome = AgenticLoop {
    llm: &self.llm,
    tools: &self.tool_registry,
    mcp: &self.mcp,
    config: &LoopConfig {
        max_iterations: self.config.max_iterations(),
        empty_response_retry_limit: self.config.empty_response_retry_limit(),
        compaction_enabled: true,
        loop_detection_enabled: true,
        interactive_loop_callback: true,
        allowed_tools: None,    // all tools available
        langsmith_project: Some(ls_project),
        tool_event_tx,
        stream_token_tx,
    },
    cancel: Some(cancel_token),
    chain_run_id: Some(chain_run_id),
    langsmith: Some(&self.langsmith),
    platform_sender: self.sender.as_ref(),
}.run(&mut cmgr).await?;

// Subagent loop
let outcome = AgenticLoop {
    llm: &self.llm,
    tools: &self.tool_registry,
    mcp: &self.mcp,
    config: &LoopConfig {
        max_iterations: max_iter,
        empty_response_retry_limit: self.config.empty_response_retry_limit(),
        compaction_enabled: false,
        loop_detection_enabled: true,
        interactive_loop_callback: false,  // auto-nudge
        allowed_tools: Some(allowed_tools), // whitelist
        langsmith_project: None,
        tool_event_tx: None,
        stream_token_tx: None,
    },
    cancel,
    chain_run_id: None,
    langsmith: None,
    platform_sender: self.sender.as_ref(),
}.run(&mut messages).await;  // Note: subagent uses owned Vec<ChatMessage>, not ConversationManager
```

The subagent path uses a plain `Vec<ChatMessage>` rather than `ConversationManager` (compaction is disabled, no RAG, no persistence). A future phase could make `ConversationManager` optional.

---

## M4: Supervisor ADR + seed::write_lock

### Supervisor ADR

**New file:** `docs/adr/0001-supervisor-module-structure.md`

Context: The Supervisor module has 17 source files (`task.rs`, `job.rs`, `state.rs`, `store.rs`, `intake.rs`, `classifier.rs`, `policy.rs`, `planner.rs`, `workflow.rs`, `orchestrator.rs`, `verification.rs`, `artifact.rs`, `workspace.rs`, `reporter.rs`, `redact.rs`, `mod.rs`, `backend/`) with only one caller (the Supervisor facade).

Decision: Keep the 17-file split. Each file represents one stage of the pipeline (intake, classify, plan, execute, verify, report) and is independently testable. The design anticipates future variations (multiple classifier types, workflow modes, additional backends).

Consequences:
- Higher file count but each file is small (50-150 lines) and focused
- New pipeline stages can be added without changing existing files
- If after 6 months no second variation exists, consolidate files
- The `backend/` submodule already justifies the structure (multiple backend types)

### seed::write_lock

In `src/skills/seed.rs`:

```rust
pub fn write_lock(lock_name: &str, dir: &Path, home: &Path) -> Result<()> {
    let lock_path = home.join(lock_name);
    if !lock_path.exists() {
        let lock = SkillLock {
            version: 1,
            skills: lock_map_for(dir),
        };
        let json = serde_json::to_string_pretty(&lock)?;
        std::fs::write(&lock_path, json)?;
    }
    Ok(())
}
```

`main.rs` changes from inline block to:

```rust
seed::write_lock("skills-lock.json", &config.skills.directory, home)?;
seed::write_lock("agents-lock.json", &config.agents.directory, home)?;
```

---

## Execution order

```
M1 ─── ToolRegistry + PlatformSender + CancelRegistry
├── 1. Create CancelRegistry
├── 2. Create PlatformSender trait + TelegramAdapter
├── 3. Create ToolHandler trait + ToolRegistry
├── 4. Refactor tool categories into handler modules
├── 5. Wire through Agent + main.rs, remove Arc<Bot> and running_commands

M2 ─── ConversationManager (independent of M1)
├── 6. Create ConversationManager
├── 7. Refactor process_message to use it

M3 ─── LoopRunner (depends on M1 + M2)
├── 8. Create LoopRunner
├── 9. Refactor process_message and run_subagent_loop to use it

M4 ─── ADR + seed::write_lock (independent)
├── 10. Write docs/adr/0001-supervisor-module-structure.md
├── 11. Extract seed::write_lock()
```

## Testing

| Module | Testability |
|--------|-------------|
| `CancelRegistry` | Pure unit tests (no async, no I/O). Register, cancel, unregister. |
| `PlatformSender` | Mock `PlatformSender` for tool handler tests. TelegramAdapter tested via integration. |
| `ToolRegistry` | Test registration + dispatch with mock handlers. |
| `ConversationManager` | Test compaction strategies, RAG injection, steer application with a `MemoryStore` connected to in-memory SQLite. |
| `LoopRunner` | Test with mock LLM that alternates tool calls and text responses. Test both main and subagent configs. |
| `seed::write_lock` | Test file creation, skip-if-exists, content format. |

**Source-inspection test impact:** `telegram.rs` has tests that assert specific string patterns in tool definitions and message formatting. Moving keyboard/dialog logic into `PlatformSender` implementations and `CommandTool` will change those strings. Update these tests in M1.

Existing tests (`tests::*` in tools.rs, provider.rs, llm.rs) should pass unchanged.

## Non-goals

- No new features or behaviors
- No changes to MCP tool handling internals (prefix dispatch stays in Agent)
- No changes to Provider/ProviderRegistry (already well-seamed)
- No changes to Supervisor module logic (only an ADR)
- No changes to memory/ module structure
- No changes to how scheduled task background runner dispatches (keeps `Arc<Bot>` directly)
