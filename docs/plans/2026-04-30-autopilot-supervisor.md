# RustFox Autopilot Supervisor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/plans/2026-04-30-autopilot-supervisor-design.md`

**Goal:** Evolve RustFox from a single-loop AI assistant into a generic autonomous **task supervisor** with a task-first state machine, pluggable backends (Claude Code CLI, Codex CLI, shell, MCP, the existing in-process Agent, …), policy-driven autonomy, evidence-based verification, and resumable persisted state.

**Architecture:** A new `src/supervisor/` module sits *above* the existing `Agent`. Telegram (and later CLI/HTTP) intake calls `Supervisor::submit(user_request)` instead of `Agent::process_message` directly. The supervisor classifies the request into a normalized `Task`, picks a `Workflow` (Fast / Standard / Rigorous), the policy engine decides autonomy/clarification/approval, the orchestrator dispatches `Job`s through capability-matched `Backend` adapters (the current `Agent` becomes the default reasoning backend), the verification engine confirms evidence, and every transition is persisted as an artifact. Existing modules (`memory`, `mcp`, `tools`, `scheduler`, `skills`, `langsmith`) are reused; nothing is greenfield.

**Tech Stack:** Rust 2021 · `tokio` · `teloxide` · `rusqlite` (extended schema) · `serde` · `tracing` · `async-trait` · `uuid` · `chrono` · existing `rmcp` · `tokio-cron-scheduler`. Tests use `tempfile` + `#[tokio::test]`.

---

## File Structure

New module tree (added; nothing existing is deleted):

```
src/
├── supervisor/
│   ├── mod.rs              # Supervisor struct, public submit() entrypoint, glue
│   ├── task.rs             # Task, TaskType, RiskLevel, ExecutionMode, TaskStatus
│   ├── job.rs              # Job, JobType, JobStatus, JobResult, JobOutput contract
│   ├── state.rs            # SupervisorState enum + transition table + guards
│   ├── store.rs            # SQLite persistence: tasks, jobs, transitions, artifacts
│   ├── intake.rs           # IntakeRouter: normalize raw user text → Task
│   ├── classifier.rs       # TaskClassifier: type + risk + capabilities + complexity
│   ├── policy.rs           # PolicyEngine: rules + decisions (auto/ask/escalate)
│   ├── planner.rs          # Planner: build Job DAG from Task + workflow template
│   ├── workflow.rs         # WorkflowMode (Fast/Standard/Rigorous) + Template registry
│   ├── orchestrator.rs     # Job runner: dispatch, retry, fallback, parallel, subjob
│   ├── verification.rs     # VerificationEngine: evidence checks per task type
│   ├── artifact.rs         # ArtifactManager: write & index artifact files
│   ├── workspace.rs        # Optional git branch/worktree manager (code tasks only)
│   ├── reporter.rs         # Result summary back to the platform
│   └── backend/
│       ├── mod.rs          # Backend trait (async_trait), BackendCapabilities, registry
│       ├── reasoning.rs    # ReasoningBackend wrapping existing Agent
│       ├── shell.rs        # ShellBackend (sandbox-validated)
│       ├── mcp.rs          # McpBackend (delegates to existing McpManager)
│       ├── claude_code.rs  # ClaudeCodeCliBackend (spawn `claude` CLI)
│       ├── codex.rs        # CodexCliBackend (spawn `codex` CLI)
│       └── script.rs       # ScriptBackend (run a local script)
│
├── config.rs               # +SupervisorConfig, +BackendsConfig (extends existing file)
├── agent.rs                # Unchanged; ReasoningBackend wraps it
├── platform/telegram.rs    # Routes /supervise, /tasks, /resume, /cancel commands
└── main.rs                 # Wires Supervisor into AppState and starts background runner

tests/
└── supervisor/
    ├── intake_classifier.rs
    ├── policy_rules.rs
    ├── orchestrator_state.rs
    ├── verification.rs
    └── e2e_fast_mode.rs
```

Each file has one clear responsibility; nothing exceeds ~400 LoC. Files that change together (e.g. `task.rs` + `store.rs` schemas) live next to each other.

## DB Schema Additions (one place to find them)

All migrations are added inside `src/memory/mod.rs::run_migrations` so they share the existing connection. New tables:

```sql
-- Supervisor: tasks
CREATE TABLE IF NOT EXISTS sup_tasks (
    id              TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    user_request    TEXT NOT NULL,
    task_type       TEXT NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 5,
    risk_level      TEXT NOT NULL,           -- low|medium|high
    execution_mode  TEXT NOT NULL,           -- fast|standard|rigorous
    workflow        TEXT NOT NULL,           -- coding|research|writing|ops|general|...
    state           TEXT NOT NULL,           -- INTAKE|...|DONE
    inputs          TEXT,                    -- JSON
    constraints     TEXT,                    -- JSON
    expected_outputs TEXT,                   -- JSON
    approval_policy TEXT,                    -- JSON
    platform        TEXT NOT NULL,           -- telegram|cli|http
    user_id         TEXT NOT NULL,
    chat_id         TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_sup_tasks_state ON sup_tasks(state, updated_at);
CREATE INDEX IF NOT EXISTS idx_sup_tasks_user  ON sup_tasks(user_id, state);

-- Supervisor: jobs
CREATE TABLE IF NOT EXISTS sup_jobs (
    id              TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    parent_job_id   TEXT,                    -- for subjobs
    job_type        TEXT NOT NULL,
    backend         TEXT NOT NULL,
    goal            TEXT NOT NULL,
    prompt          TEXT,
    input_context   TEXT,                    -- JSON
    timeout_secs    INTEGER NOT NULL,
    retry_max       INTEGER NOT NULL DEFAULT 0,
    retry_count     INTEGER NOT NULL DEFAULT 0,
    allow_tools     TEXT,                    -- JSON list
    workspace       TEXT,
    status          TEXT NOT NULL,           -- pending|running|succeeded|failed|cancelled
    result_summary  TEXT,
    result_evidence TEXT,                    -- JSON list of {kind,path|hash|exit}
    error           TEXT,
    started_at      TEXT,
    finished_at     TEXT,
    FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
);
CREATE INDEX IF NOT EXISTS idx_sup_jobs_task ON sup_jobs(task_id, status);

-- Supervisor: state transitions (audit trail; one row per transition)
CREATE TABLE IF NOT EXISTS sup_transitions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL,
    from_state  TEXT NOT NULL,
    to_state    TEXT NOT NULL,
    reason      TEXT,                        -- policy decision / verification failure / etc.
    actor       TEXT NOT NULL,               -- supervisor|user|backend:<name>
    occurred_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
);

-- Supervisor: artifacts
CREATE TABLE IF NOT EXISTS sup_artifacts (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    job_id      TEXT,
    kind        TEXT NOT NULL,               -- intake|classification|plan|log|result|...
    path        TEXT NOT NULL,               -- relative to artifacts root
    sha256      TEXT,
    bytes       INTEGER,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
);
CREATE INDEX IF NOT EXISTS idx_sup_artifacts_task ON sup_artifacts(task_id, kind);
```

Every migration is wrapped in `CREATE TABLE IF NOT EXISTS` and idempotent so re-runs are safe (matches the project's existing migration style).

## Config Additions

In `src/config.rs`, add (and gate via `#[serde(default)]` everywhere):

```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SupervisorConfig {
    #[serde(default = "default_autonomy_mode")]
    pub default_autonomy_mode: String,    // "fast" | "standard" | "rigorous"
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: PathBuf,           // e.g. "supervisor/artifacts"
    #[serde(default = "default_risk_thresholds")]
    pub risk: RiskThresholdsConfig,
    #[serde(default)]
    pub backends: BackendsConfig,
    #[serde(default)]
    pub repo: Option<RepoConfig>,         // per-repo defaults (build/test/lint cmds)
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct BackendsConfig {
    #[serde(default)]
    pub reasoning: Option<String>,        // backend name; default = built-in agent
    #[serde(default)]
    pub coding:    Option<String>,        // e.g. "claude_code_cli" | "codex_cli"
    #[serde(default)]
    pub shell:     Option<String>,
    #[serde(default)]
    pub research:  Option<String>,
    #[serde(default)]
    pub document:  Option<String>,
    #[serde(default)]
    pub fallbacks: HashMap<String, Vec<String>>, // capability -> ordered fallbacks
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct RepoConfig {
    pub path: PathBuf,
    pub default_branch: String,
    pub build_cmd:  Option<String>,
    pub test_cmd:   Option<String>,
    pub lint_cmd:   Option<String>,
    pub format_cmd: Option<String>,
    pub workspace_root: Option<PathBuf>,
}
```

`Config` gains `#[serde(default)] pub supervisor: SupervisorConfig`. All defaults are opt-in safe (autonomy = `"standard"`, no backends → only built-in agent works).

---

## Bite-Sized Task Granularity Note

Every step below is **one action (≈2–5 min)**: write the failing test, run it, write minimal code, run again, commit. Type names, paths and code samples are concrete — no placeholders. Where multiple steps share boilerplate, the boilerplate is repeated so a worker can read tasks out of order.

---

## Milestone 0 — Plumbing & Module Skeleton

Purpose: create the empty supervisor module wired into `main.rs` so later tasks can compile in isolation.

### Task 0.1: Create the supervisor module skeleton

**Files:**

- Create: `src/supervisor/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

`tests/supervisor/exists.rs`:

```rust
#[test]
fn supervisor_module_compiles() {
    // Compiling = passing. The module must be `pub` from the crate root.
    let _ = std::any::type_name::<rustfox::supervisor::Supervisor>();
}
```

(Add `pub mod supervisor;` exposure step in step 3.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test exists`
Expected: FAIL — `unresolved import 'rustfox::supervisor'` or `lib not found`.
(If the project has no `lib.rs` yet, this task instead asserts via `cargo check` after step 3.)

- [ ] **Step 3: Write the minimal implementation**

Create `src/supervisor/mod.rs`:

```rust
//! Generic autonomous task supervisor.
//! See `docs/plans/2026-04-30-autopilot-supervisor-design.md`.

pub struct Supervisor;

impl Supervisor {
    pub fn new() -> Self { Self }
}

impl Default for Supervisor { fn default() -> Self { Self::new() } }
```

Add `mod supervisor;` to `src/main.rs` near the other `mod` lines.

- [ ] **Step 4: Run the test**

Run: `cargo check && cargo build`
Expected: PASS — clean build, supervisor mod compiles.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/mod.rs src/main.rs tests/supervisor/exists.rs
git commit -m "supervisor(M0): add empty module skeleton"
```

### Task 0.2: Add SupervisorConfig with defaults

**Files:**

- Modify: `src/config.rs`
- Test: `src/config.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** (in `src/config.rs`):

```rust
#[test]
fn supervisor_config_defaults_when_section_missing() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        [sandbox]
        allowed_directory = "/tmp"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.supervisor.default_autonomy_mode, "standard");
    assert_eq!(
        cfg.supervisor.artifacts_dir,
        std::path::PathBuf::from("supervisor/artifacts")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib supervisor_config_defaults_when_section_missing`
Expected: FAIL — `no field 'supervisor' on Config`.

- [ ] **Step 3: Write the minimal implementation**

Add to `src/config.rs` (after existing structs):

```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SupervisorConfig {
    #[serde(default = "default_autonomy_mode")]
    pub default_autonomy_mode: String,
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: std::path::PathBuf,
}

fn default_autonomy_mode() -> String { "standard".to_string() }
fn default_artifacts_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("supervisor/artifacts")
}
```

Add to `Config`:

```rust
#[serde(default)]
pub supervisor: SupervisorConfig,
```

- [ ] **Step 4: Run test**

Run: `cargo test --lib supervisor_config_defaults_when_section_missing`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "supervisor(M0): add SupervisorConfig with defaults"
```

### Task 0.3: Wire SQLite migrations for sup_tasks/sup_jobs/sup_transitions/sup_artifacts

**Files:**

- Modify: `src/memory/mod.rs` (extend `run_migrations`)
- Test: `src/memory/mod.rs`

- [ ] **Step 1: Write the failing test** (in `src/memory/mod.rs`):

```rust
#[test]
fn sup_tables_exist_after_migration() {
    let memory = MemoryStore::open_in_memory().unwrap();
    let conn = memory.connection();
    let conn = conn.blocking_lock();
    for tbl in ["sup_tasks", "sup_jobs", "sup_transitions", "sup_artifacts"] {
        let exists: bool = conn
            .query_row(
                "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name=?1",
                [tbl],
                |row| row.get(0),
            ).unwrap();
        assert!(exists, "table {tbl} missing");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib sup_tables_exist_after_migration`
Expected: FAIL — `table sup_tasks missing`.

- [ ] **Step 3: Write the minimal implementation**

Append the four `CREATE TABLE IF NOT EXISTS` blocks (verbatim from the "DB Schema Additions" section above) inside the existing `execute_batch` call in `run_migrations`, right after the `scheduled_tasks` block.

- [ ] **Step 4: Run test**

Run: `cargo test --lib sup_tables_exist_after_migration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/memory/mod.rs
git commit -m "supervisor(M0): add sup_* tables to memory migrations"
```

---

## Milestone 1 — Intake, Classification, Policy, Artifacts

Purpose: a user request becomes a normalized `Task`, gets classified, gets a policy decision, and is persisted with its initial artifacts. No execution yet.

### Task 1.1: Define `Task`, `TaskType`, `RiskLevel`, `ExecutionMode`, `TaskStatus`

**Files:**

- Create: `src/supervisor/task.rs`
- Modify: `src/supervisor/mod.rs` (add `pub mod task;`)
- Test: `src/supervisor/task.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn task_serializes_round_trip() {
    let t = Task::new("Summarize CHANGELOG", "summarize the changelog file");
    let json = serde_json::to_string(&t).unwrap();
    let back: Task = serde_json::from_str(&json).unwrap();
    assert_eq!(back.title, "Summarize CHANGELOG");
    assert_eq!(back.task_type, TaskType::Unknown);
    assert_eq!(back.risk_level, RiskLevel::Low);
    assert_eq!(back.status, TaskStatus::Intake);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib task_serializes_round_trip`
Expected: FAIL — module not found.

- [ ] **Step 3: Write the minimal implementation**

```rust
// src/supervisor/task.rs
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CodeChange, BugFix, Refactor,
    Research, Writing,
    OpsAutomation, WorkflowAutomation,
    DataTransformation, DecisionSupport,
    GeneralAssistant, Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel { Low, Medium, High }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode { Fast, Standard, Rigorous }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaskStatus {
    Intake, Classify, Route, Clarify, Plan, PrepareWorkspace,
    Execute, Review, Verify, Report, Archive,
    Paused, Failed, Cancelled, Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub user_request: String,
    pub task_type: TaskType,
    pub priority: u8,
    pub risk_level: RiskLevel,
    pub execution_mode: ExecutionMode,
    pub status: TaskStatus,
    #[serde(default)] pub required_capabilities: Vec<String>,
    #[serde(default)] pub constraints: serde_json::Value,
    #[serde(default)] pub inputs: serde_json::Value,
    #[serde(default)] pub expected_outputs: serde_json::Value,
}

impl Task {
    pub fn new(title: &str, user_request: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.to_string(),
            user_request: user_request.to_string(),
            task_type: TaskType::Unknown,
            priority: 5,
            risk_level: RiskLevel::Low,
            execution_mode: ExecutionMode::Standard,
            status: TaskStatus::Intake,
            required_capabilities: Vec::new(),
            constraints: serde_json::Value::Null,
            inputs: serde_json::Value::Null,
            expected_outputs: serde_json::Value::Null,
        }
    }
}
```

Wire into `src/supervisor/mod.rs`: `pub mod task;`.

- [ ] **Step 4: Run test**

Run: `cargo test --lib task_serializes_round_trip` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/task.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): Task, TaskType, RiskLevel, ExecutionMode, TaskStatus"
```

### Task 1.2: Define `Job`, `JobType`, `JobStatus`, `JobOutput` contract

**Files:**

- Create: `src/supervisor/job.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Write the failing test** (in `src/supervisor/job.rs`):

```rust
#[test]
fn job_output_contract_required_fields() {
    let out = JobOutput {
        status: JobStatus::Succeeded,
        summary: "ok".into(),
        evidence: vec![Evidence::ExitCode(0)],
        errors: vec![],
        changed_files: vec![],
        next_step: None,
    };
    assert!(matches!(out.status, JobStatus::Succeeded));
}
```

- [ ] **Step 2: Run test** → FAIL (module missing).

- [ ] **Step 3: Implement** in `src/supervisor/job.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    PlannerJob, ExecutorJob, ReviewerJob, VerifierJob,
    ResearchJob, ShellJob, DocumentJob, ApprovalJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus { Pending, Running, Succeeded, Failed, Cancelled }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    ExitCode(i32),
    FileCreated { path: String, sha256: Option<String> },
    TestPassed { name: String },
    OutputValidated { description: String },
    LogStored { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutput {
    pub status: JobStatus,
    pub summary: String,
    pub evidence: Vec<Evidence>,
    pub errors: Vec<String>,
    pub changed_files: Vec<String>,
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub task_id: String,
    pub parent_job_id: Option<String>,
    pub job_type: JobType,
    pub backend: String,
    pub goal: String,
    pub prompt: Option<String>,
    pub input_context: serde_json::Value,
    pub timeout_secs: u64,
    pub retry_max: u32,
    pub retry_count: u32,
    pub allow_tools: Vec<String>,
    pub workspace: Option<String>,
    pub status: JobStatus,
    pub result: Option<JobOutput>,
    pub error: Option<String>,
}

impl Job {
    pub fn new(task_id: &str, job_type: JobType, backend: &str, goal: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            parent_job_id: None,
            job_type, backend: backend.to_string(), goal: goal.to_string(),
            prompt: None, input_context: serde_json::Value::Null,
            timeout_secs: 600, retry_max: 0, retry_count: 0,
            allow_tools: Vec::new(), workspace: None,
            status: JobStatus::Pending, result: None, error: None,
        }
    }
}
```

Add `pub mod job;` to `src/supervisor/mod.rs`.

- [ ] **Step 4: Run test** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/job.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): Job, JobType, JobStatus, JobOutput contract"
```

### Task 1.3: Implement `SupervisorState` machine with explicit transitions

**Files:**

- Create: `src/supervisor/state.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn valid_transitions_succeed_and_invalid_fail() {
    use SupervisorState::*;
    assert!(transition_allowed(Intake, Classify));
    assert!(transition_allowed(Classify, Route));
    assert!(transition_allowed(Route, Clarify));
    assert!(transition_allowed(Verify, Report));
    assert!(transition_allowed(Execute, Failed));
    assert!(!transition_allowed(Intake, Done));      // skip not allowed
    assert!(!transition_allowed(Done, Execute));     // terminal
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `src/supervisor/state.rs`:

```rust
use crate::supervisor::task::TaskStatus as SupervisorState;

pub fn transition_allowed(from: SupervisorState, to: SupervisorState) -> bool {
    use SupervisorState::*;
    matches!((from, to),
        (Intake, Classify) | (Classify, Route) |
        (Route, Clarify) | (Route, Plan) | (Route, Execute) |
        (Clarify, Plan) | (Clarify, Execute) | (Clarify, Cancelled) |
        (Plan, PrepareWorkspace) | (Plan, Execute) |
        (PrepareWorkspace, Execute) |
        (Execute, Review) | (Execute, Verify) | (Execute, Failed) | (Execute, Paused) |
        (Review, Verify) | (Review, Execute) |
        (Verify, Report) | (Verify, Execute) | (Verify, Failed) |
        (Report, Archive) |
        (Archive, Done) |
        (Paused, Execute) | (Paused, Cancelled) |
        (_, Cancelled)
    )
}
```

Add `pub mod state;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/state.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): explicit state transition table"
```

### Task 1.4: Persistence layer — `TaskStore` (CRUD + transition log)

**Files:**

- Create: `src/supervisor/store.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn create_task_then_load_back() {
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = TaskStore::new(memory.connection());
    let mut t = crate::supervisor::task::Task::new("T", "do thing");
    t.task_type = crate::supervisor::task::TaskType::Research;
    store.create(&t, "telegram", "u1", Some("c1")).await.unwrap();
    let loaded = store.get(&t.id).await.unwrap().unwrap();
    assert_eq!(loaded.title, "T");
    assert_eq!(loaded.task_type, crate::supervisor::task::TaskType::Research);
}

#[tokio::test]
async fn record_transition_appends_audit_row() {
    use crate::supervisor::task::TaskStatus;
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = TaskStore::new(memory.connection());
    let t = crate::supervisor::task::Task::new("T", "u");
    store.create(&t, "telegram", "u1", None).await.unwrap();
    store.record_transition(&t.id, TaskStatus::Intake, TaskStatus::Classify,
                            "supervisor", Some("auto")).await.unwrap();
    let history = store.transitions(&t.id).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].to, TaskStatus::Classify);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** in `src/supervisor/store.rs`:

```rust
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::supervisor::task::{Task, TaskStatus, TaskType, RiskLevel, ExecutionMode};

#[derive(Clone)]
pub struct TaskStore { conn: Arc<Mutex<Connection>> }

#[derive(Debug, Clone)]
pub struct TransitionRow {
    pub from: TaskStatus,
    pub to:   TaskStatus,
    pub actor: String,
    pub reason: Option<String>,
    pub occurred_at: String,
}

impl TaskStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self { Self { conn } }

    pub async fn create(&self, t: &Task, platform: &str, user_id: &str, chat_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_tasks
             (id, title, user_request, task_type, priority, risk_level, execution_mode,
              workflow, state, inputs, constraints, expected_outputs, approval_policy,
              platform, user_id, chat_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            rusqlite::params![
                t.id, t.title, t.user_request,
                serde_json::to_string(&t.task_type)?, t.priority,
                serde_json::to_string(&t.risk_level)?,
                serde_json::to_string(&t.execution_mode)?,
                "general", // workflow filled by router later
                serde_json::to_string(&t.status)?,
                serde_json::to_string(&t.inputs)?,
                serde_json::to_string(&t.constraints)?,
                serde_json::to_string(&t.expected_outputs)?,
                serde_json::Value::Null.to_string(),
                platform, user_id, chat_id,
            ],
        ).context("insert sup_tasks")?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id,title,user_request,task_type,priority,risk_level,execution_mode,state
             FROM sup_tasks WHERE id=?1")?;
        let mut rows = stmt.query_map([id], |r| {
            Ok(Task {
                id: r.get(0)?, title: r.get(1)?, user_request: r.get(2)?,
                task_type: serde_json::from_str::<TaskType>(&r.get::<_,String>(3)?).unwrap(),
                priority: r.get(4)?,
                risk_level: serde_json::from_str::<RiskLevel>(&r.get::<_,String>(5)?).unwrap(),
                execution_mode: serde_json::from_str::<ExecutionMode>(&r.get::<_,String>(6)?).unwrap(),
                status: serde_json::from_str::<TaskStatus>(&r.get::<_,String>(7)?).unwrap(),
                required_capabilities: vec![],
                constraints: serde_json::Value::Null,
                inputs: serde_json::Value::Null,
                expected_outputs: serde_json::Value::Null,
            })
        })?;
        Ok(match rows.next() { Some(Ok(t)) => Some(t), _ => None })
    }

    pub async fn record_transition(
        &self, task_id: &str, from: TaskStatus, to: TaskStatus,
        actor: &str, reason: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_transitions (task_id, from_state, to_state, reason, actor)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                task_id,
                serde_json::to_string(&from)?,
                serde_json::to_string(&to)?,
                reason, actor],
        )?;
        conn.execute(
            "UPDATE sup_tasks SET state=?1, updated_at=datetime('now') WHERE id=?2",
            rusqlite::params![serde_json::to_string(&to)?, task_id],
        )?;
        Ok(())
    }

    pub async fn transitions(&self, task_id: &str) -> Result<Vec<TransitionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT from_state, to_state, actor, reason, occurred_at
             FROM sup_transitions WHERE task_id=?1 ORDER BY id ASC")?;
        let rows = stmt.query_map([task_id], |r| Ok(TransitionRow {
            from: serde_json::from_str(&r.get::<_,String>(0)?).unwrap(),
            to:   serde_json::from_str(&r.get::<_,String>(1)?).unwrap(),
            actor: r.get(2)?,
            reason: r.get(3)?,
            occurred_at: r.get(4)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
```

Add `pub mod store;` to `src/supervisor/mod.rs`.

- [ ] **Step 4: Run** both tests → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/store.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): TaskStore CRUD + transition audit log"
```

### Task 1.5: `IntakeRouter::normalize` — raw text → `Task`

**Files:**

- Create: `src/supervisor/intake.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn intake_uses_first_line_as_title_and_full_text_as_request() {
    let task = IntakeRouter::normalize("Fix the login bug\nthe button does nothing");
    assert_eq!(task.title, "Fix the login bug");
    assert_eq!(task.user_request, "Fix the login bug\nthe button does nothing");
    assert_eq!(task.status, crate::supervisor::task::TaskStatus::Intake);
    assert!(!task.id.is_empty());
}

#[test]
fn intake_truncates_long_titles_to_80_chars() {
    let long = "A".repeat(200);
    let task = IntakeRouter::normalize(&long);
    assert!(task.title.len() <= 80);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/intake.rs
use crate::supervisor::task::Task;

pub struct IntakeRouter;

impl IntakeRouter {
    pub fn normalize(raw: &str) -> Task {
        let trimmed = raw.trim();
        let first_line = trimmed.lines().next().unwrap_or(trimmed);
        let title: String = first_line.chars().take(80).collect();
        Task::new(&title, trimmed)
    }
}
```

Add `pub mod intake;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/intake.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): IntakeRouter::normalize"
```

### Task 1.6: `TaskClassifier` — heuristic + LLM-backed classifier

**Files:**

- Create: `src/supervisor/classifier.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test** (heuristic-only path; LLM path unit-tested in Task 1.7)

```rust
#[test]
fn heuristic_classifies_obvious_cases() {
    use crate::supervisor::task::{TaskType, RiskLevel};
    let c = HeuristicClassifier;
    let t = c.classify("rename foo() to bar() in src/lib.rs");
    assert_eq!(t.task_type, TaskType::Refactor);
    assert!(matches!(t.risk_level, RiskLevel::Medium | RiskLevel::High));

    let t = c.classify("summarize the file ./README.md");
    assert_eq!(t.task_type, TaskType::GeneralAssistant);
    assert_eq!(t.risk_level, RiskLevel::Low);

    let t = c.classify("research best Rust async runtime 2026");
    assert_eq!(t.task_type, TaskType::Research);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/classifier.rs
use crate::supervisor::task::{ExecutionMode, RiskLevel, Task, TaskType};

pub struct ClassificationOutcome {
    pub task_type: TaskType,
    pub risk_level: RiskLevel,
    pub execution_mode: ExecutionMode,
    pub required_capabilities: Vec<String>,
    pub confidence: f32,
}

pub trait Classifier {
    fn classify(&self, request: &str) -> ClassificationOutcome;
}

pub struct HeuristicClassifier;

impl Classifier for HeuristicClassifier {
    fn classify(&self, request: &str) -> ClassificationOutcome {
        let lower = request.to_lowercase();
        let (task_type, risk, caps) = if lower.starts_with("rename ")
            || lower.contains("refactor") || lower.contains("rewrite")
        {
            (TaskType::Refactor, RiskLevel::Medium, vec!["coding".into(), "shell".into()])
        } else if lower.starts_with("fix ") || lower.contains("bug") {
            (TaskType::BugFix, RiskLevel::Medium, vec!["coding".into()])
        } else if lower.starts_with("research") || lower.starts_with("compare") {
            (TaskType::Research, RiskLevel::Low, vec!["research".into(), "reasoning".into()])
        } else if lower.starts_with("summarize") || lower.starts_with("answer ") {
            (TaskType::GeneralAssistant, RiskLevel::Low, vec!["reasoning".into()])
        } else if lower.starts_with("write ") || lower.contains("draft ") {
            (TaskType::Writing, RiskLevel::Low, vec!["document".into(), "reasoning".into()])
        } else if lower.starts_with("run ") || lower.contains("script") || lower.contains("shell") {
            (TaskType::OpsAutomation, RiskLevel::Medium, vec!["shell".into()])
        } else {
            (TaskType::Unknown, RiskLevel::Low, vec!["reasoning".into()])
        };

        let exec = match (&task_type, &risk) {
            (_, RiskLevel::High) => ExecutionMode::Rigorous,
            (TaskType::CodeChange, _) | (TaskType::Refactor, _) | (TaskType::BugFix, _)
                => ExecutionMode::Rigorous,
            (TaskType::GeneralAssistant, _) => ExecutionMode::Fast,
            _ => ExecutionMode::Standard,
        };
        ClassificationOutcome { task_type, risk_level: risk, execution_mode: exec,
            required_capabilities: caps, confidence: 0.6 }
    }
}

impl HeuristicClassifier {
    pub fn classify(&self, request: &str) -> Task {
        let mut t = Task::new(request.lines().next().unwrap_or(request), request);
        let o = <Self as Classifier>::classify(self, request);
        t.task_type = o.task_type; t.risk_level = o.risk_level;
        t.execution_mode = o.execution_mode; t.required_capabilities = o.required_capabilities;
        t
    }
}
```

Add `pub mod classifier;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/classifier.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): HeuristicClassifier (no LLM dependency)"
```

### Task 1.7: LLM-backed classifier wrapper (uses existing `LlmClient`)

**Files:**

- Modify: `src/supervisor/classifier.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn llm_classifier_falls_back_to_heuristic_when_disabled() {
    let c = LlmBackedClassifier::heuristic_only();
    let o = c.classify("summarize the readme");
    assert_eq!(o.task_type, crate::supervisor::task::TaskType::GeneralAssistant);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Add to `classifier.rs`**

```rust
pub struct LlmBackedClassifier {
    inner_llm: Option<crate::llm::LlmClient>,
    fallback: HeuristicClassifier,
}

impl LlmBackedClassifier {
    pub fn new(llm: crate::llm::LlmClient) -> Self {
        Self { inner_llm: Some(llm), fallback: HeuristicClassifier }
    }
    pub fn heuristic_only() -> Self {
        Self { inner_llm: None, fallback: HeuristicClassifier }
    }
}

impl Classifier for LlmBackedClassifier {
    fn classify(&self, request: &str) -> ClassificationOutcome {
        // M1: only the heuristic path is wired. The async LLM call is added in M3
        // because it requires the agent loop. For now we always use the fallback.
        <HeuristicClassifier as Classifier>::classify(&self.fallback, request)
    }
}
```

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/classifier.rs
git commit -m "supervisor(M1): LlmBackedClassifier scaffold (heuristic in M1, LLM path deferred to M3)"
```

### Task 1.8: `PolicyEngine` — deterministic rule table

**Files:**

- Create: `src/supervisor/policy.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn low_risk_well_scoped_auto_executes() {
    use crate::supervisor::task::*;
    let mut t = Task::new("ok", "ok"); t.task_type = TaskType::GeneralAssistant; t.risk_level = RiskLevel::Low;
    let d = PolicyEngine::default().decide(&t);
    assert_eq!(d, PolicyDecision::AutoExecute);
}

#[test]
fn high_risk_requires_approval() {
    use crate::supervisor::task::*;
    let mut t = Task::new("rm -rf /", "delete prod"); t.risk_level = RiskLevel::High;
    let d = PolicyEngine::default().decide(&t);
    assert_eq!(d, PolicyDecision::RequireApproval);
}

#[test]
fn ambiguous_task_triggers_clarification() {
    use crate::supervisor::task::*;
    let mut t = Task::new("do the thing", "do the thing"); t.task_type = TaskType::Unknown; t.risk_level = RiskLevel::Low;
    let d = PolicyEngine::default().decide(&t);
    assert_eq!(d, PolicyDecision::Clarify);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/policy.rs
use crate::supervisor::task::{RiskLevel, Task, TaskType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    AutoExecute,
    Clarify,
    RequireApproval,
    UseFallbackBackend(String),
    StopAndReport(String),
}

#[derive(Default)]
pub struct PolicyEngine;

impl PolicyEngine {
    pub fn decide(&self, t: &Task) -> PolicyDecision {
        if t.risk_level == RiskLevel::High {
            return PolicyDecision::RequireApproval;
        }
        if t.task_type == TaskType::Unknown && t.risk_level == RiskLevel::Low {
            return PolicyDecision::Clarify;
        }
        PolicyDecision::AutoExecute
    }
}
```

Add `pub mod policy;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/policy.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): PolicyEngine deterministic decision table"
```

### Task 1.9: `ArtifactManager` — write & index artifact files

**Files:**

- Create: `src/supervisor/artifact.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn writes_artifact_and_indexes_in_db() {
    let dir = tempfile::tempdir().unwrap();
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();

    // Pre-create a task so foreign key passes
    let store = crate::supervisor::store::TaskStore::new(memory.connection());
    let task = crate::supervisor::task::Task::new("T", "u");
    store.create(&task, "telegram", "u", None).await.unwrap();

    let am = ArtifactManager::new(dir.path().into(), memory.connection());
    let id = am.write_text(&task.id, None, "intake", "intake.json", r#"{"a":1}"#).await.unwrap();

    assert!(dir.path().join(&task.id).join("intake.json").exists());
    let rows = am.list(&task.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].kind, "intake");
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/artifact.rs
use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ArtifactRow { pub id: String, pub kind: String, pub path: String }

pub struct ArtifactManager {
    root: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl ArtifactManager {
    pub fn new(root: PathBuf, conn: Arc<Mutex<Connection>>) -> Self { Self { root, conn } }

    pub async fn write_text(
        &self, task_id: &str, job_id: Option<&str>,
        kind: &str, filename: &str, content: &str,
    ) -> Result<String> {
        let task_dir = self.root.join(task_id);
        tokio::fs::create_dir_all(&task_dir).await
            .with_context(|| format!("create artifact dir {}", task_dir.display()))?;
        let path = task_dir.join(filename);
        tokio::fs::write(&path, content).await
            .with_context(|| format!("write artifact {}", path.display()))?;

        let mut h = Sha256::new(); h.update(content.as_bytes());
        let sha = format!("{:x}", h.finalize());
        let bytes = content.len() as i64;
        let id = Uuid::new_v4().to_string();
        let rel = path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().to_string();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_artifacts (id, task_id, job_id, kind, path, sha256, bytes)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![id, task_id, job_id, kind, rel, sha, bytes],
        )?;
        Ok(id)
    }

    pub async fn list(&self, task_id: &str) -> Result<Vec<ArtifactRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, kind, path FROM sup_artifacts WHERE task_id=?1 ORDER BY created_at ASC")?;
        let rows = stmt.query_map([task_id], |r| Ok(ArtifactRow {
            id: r.get(0)?, kind: r.get(1)?, path: r.get(2)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
```

Note: `sha2` is already in `Cargo.toml`. If not, add `sha2 = "0.10"`.

Add `pub mod artifact;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/artifact.rs src/supervisor/mod.rs
git commit -m "supervisor(M1): ArtifactManager (filesystem + sup_artifacts index)"
```

### Task 1.10: M1 integration — `Supervisor::submit` produces a stored task with intake/classification/policy artifacts

**Files:**

- Modify: `src/supervisor/mod.rs`
- Test: `tests/supervisor/intake_classifier.rs`

- [ ] **Step 1: Failing integration test**

```rust
// tests/supervisor/intake_classifier.rs
use rustfox::supervisor::{Supervisor, SubmitOutcome};

#[tokio::test]
async fn submit_persists_task_and_writes_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let sup = Supervisor::new_for_test(dir.path().into(), memory.connection());

    let outcome = sup.submit("telegram", "u1", Some("c1"),
        "summarize the file ./README.md").await.unwrap();

    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));
    let task_id = outcome.task_id();

    let arts = sup.artifacts().list(&task_id).await.unwrap();
    let kinds: Vec<_> = arts.iter().map(|a| a.kind.as_str()).collect();
    assert!(kinds.contains(&"intake"));
    assert!(kinds.contains(&"classification"));
    assert!(kinds.contains(&"policy"));
}
```

(Requires `lib.rs` exposing `pub mod supervisor;`, `pub mod memory;`. Add a minimal `src/lib.rs` if it does not exist; this is a one-time addition.)

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** in `src/supervisor/mod.rs`:

```rust
pub mod artifact;
pub mod classifier;
pub mod intake;
pub mod job;
pub mod policy;
pub mod state;
pub mod store;
pub mod task;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::supervisor::artifact::ArtifactManager;
use crate::supervisor::classifier::{Classifier, HeuristicClassifier};
use crate::supervisor::intake::IntakeRouter;
use crate::supervisor::policy::{PolicyDecision, PolicyEngine};
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::TaskStatus;

pub enum SubmitOutcome {
    AutoExecutePlanned { task_id: String },
    NeedsClarification  { task_id: String, question: String },
    NeedsApproval       { task_id: String, reason: String },
}

impl SubmitOutcome {
    pub fn task_id(&self) -> String {
        match self {
            Self::AutoExecutePlanned { task_id }
            | Self::NeedsClarification { task_id, .. }
            | Self::NeedsApproval { task_id, .. } => task_id.clone(),
        }
    }
}

pub struct Supervisor {
    store: TaskStore,
    artifacts: Arc<ArtifactManager>,
    classifier: Box<dyn Classifier + Send + Sync>,
    policy: PolicyEngine,
}

impl Supervisor {
    pub fn new_for_test(artifacts_root: PathBuf,
                        conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>) -> Self {
        Self {
            store: TaskStore::new(conn.clone()),
            artifacts: Arc::new(ArtifactManager::new(artifacts_root, conn)),
            classifier: Box::new(HeuristicClassifier),
            policy: PolicyEngine::default(),
        }
    }

    pub fn artifacts(&self) -> &ArtifactManager { &self.artifacts }

    pub async fn submit(
        &self, platform: &str, user_id: &str, chat_id: Option<&str>, text: &str,
    ) -> Result<SubmitOutcome> {
        let mut task = IntakeRouter::normalize(text);
        self.store.create(&task, platform, user_id, chat_id).await?;
        self.artifacts.write_text(&task.id, None, "intake", "intake.json",
            &serde_json::to_string_pretty(&task)?).await?;

        // CLASSIFY
        self.store.record_transition(&task.id, TaskStatus::Intake, TaskStatus::Classify,
            "supervisor", Some("auto")).await?;
        let outcome = <dyn Classifier>::classify(&*self.classifier, text);
        task.task_type = outcome.task_type.clone();
        task.risk_level = outcome.risk_level.clone();
        task.execution_mode = outcome.execution_mode.clone();
        task.required_capabilities = outcome.required_capabilities.clone();
        self.artifacts.write_text(&task.id, None, "classification", "classification.json",
            &serde_json::to_string_pretty(&serde_json::json!({
                "task_type": task.task_type, "risk_level": task.risk_level,
                "execution_mode": task.execution_mode,
                "required_capabilities": task.required_capabilities,
                "confidence": outcome.confidence,
            }))?).await?;

        // ROUTE → POLICY
        self.store.record_transition(&task.id, TaskStatus::Classify, TaskStatus::Route,
            "supervisor", None).await?;
        let decision = self.policy.decide(&task);
        self.artifacts.write_text(&task.id, None, "policy", "policy.json",
            &serde_json::to_string_pretty(&serde_json::json!({"decision": format!("{decision:?}")}))?).await?;

        Ok(match decision {
            PolicyDecision::AutoExecute =>
                SubmitOutcome::AutoExecutePlanned { task_id: task.id },
            PolicyDecision::Clarify => {
                self.store.record_transition(&task.id, TaskStatus::Route, TaskStatus::Clarify,
                    "policy", Some("ambiguous")).await?;
                SubmitOutcome::NeedsClarification {
                    task_id: task.id,
                    question: "I'm not sure what you want me to do — can you clarify?".into(),
                }
            }
            PolicyDecision::RequireApproval =>
                SubmitOutcome::NeedsApproval { task_id: task.id, reason: "high-risk task".into() },
            other =>
                SubmitOutcome::NeedsApproval { task_id: task.id, reason: format!("{other:?}") },
        })
    }
}
```

Also create/update `src/lib.rs` (one-time):

```rust
// src/lib.rs
pub mod agent;
pub mod config;
pub mod langsmith;
pub mod learning;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod platform;
pub mod scheduler;
pub mod skills;
pub mod supervisor;
pub mod tools;
pub mod utils;
```

`src/main.rs` keeps `mod` lines but now they can be replaced with `use rustfox::*;` — instead, do the lighter touch: leave `main.rs` untouched and add `lib.rs` that re-exports. Verify `cargo build` still produces both `rustfox` (bin) and `rustfox` (lib).

- [ ] **Step 4: Run** the integration test → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/mod.rs src/lib.rs tests/supervisor/intake_classifier.rs
git commit -m "supervisor(M1): Supervisor::submit end-to-end (intake→classify→policy→artifacts)"
```

---

## Milestone 2 — Backend Abstraction + First Executor Backend

Purpose: define the Backend trait + registry; wrap the existing `Agent` as the default `ReasoningBackend`; add `ShellBackend` as second concrete backend.

### Task 2.1: Define `Backend` trait + `BackendCapabilities` + `Registry`

**Files:**

- Create: `src/supervisor/backend/mod.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn registry_finds_backend_by_capability() {
    let mut reg = Registry::new();
    reg.register(Arc::new(DummyReasoning));
    let chosen = reg.select_for(&["reasoning".into()]).unwrap();
    assert_eq!(chosen.name(), "dummy-reasoning");
}

struct DummyReasoning;
#[async_trait::async_trait]
impl Backend for DummyReasoning {
    fn name(&self) -> &str { "dummy-reasoning" }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { reasoning: true, ..Default::default() }
    }
    fn can_handle(&self, _: &crate::supervisor::job::JobType) -> bool { true }
    async fn run(&self, _: &mut crate::supervisor::job::Job) -> anyhow::Result<crate::supervisor::job::JobOutput> {
        Ok(crate::supervisor::job::JobOutput {
            status: crate::supervisor::job::JobStatus::Succeeded,
            summary: "ok".into(), evidence: vec![], errors: vec![],
            changed_files: vec![], next_step: None,
        })
    }
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/backend/mod.rs
use crate::supervisor::job::{Job, JobOutput, JobType};
use anyhow::Result;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub reasoning: bool,
    pub coding:    bool,
    pub shell:     bool,
    pub research:  bool,
    pub document:  bool,
    pub long_running: bool,
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> BackendCapabilities;
    fn can_handle(&self, job_type: &JobType) -> bool;

    // Spec §10 required methods. `run` is the only one most backends override.
    async fn prepare(&self, _job: &mut Job) -> Result<()> { Ok(()) }
    async fn run(&self, job: &mut Job) -> Result<JobOutput>;
    async fn collect_result(&self, _job: &Job) -> Result<Option<JobOutput>> { Ok(None) }
    async fn verify_result(&self, _job: &Job, out: &JobOutput) -> Result<bool> {
        Ok(matches!(out.status, crate::supervisor::job::JobStatus::Succeeded))
    }
    async fn cancel(&self, _job_id: &str) -> Result<()> { Ok(()) }
    async fn resume(&self, _job_id: &str) -> Result<()> { Ok(()) }
}

#[derive(Default)]
pub struct Registry { backends: Vec<Arc<dyn Backend>> }

impl Registry {
    pub fn new() -> Self { Self::default() }
    pub fn register(&mut self, b: Arc<dyn Backend>) { self.backends.push(b); }

    /// Select first backend that satisfies all required capabilities.
    pub fn select_for(&self, required: &[String]) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| {
            let c = b.capabilities();
            required.iter().all(|r| match r.as_str() {
                "reasoning" => c.reasoning,  "coding"   => c.coding,
                "shell"     => c.shell,      "research" => c.research,
                "document"  => c.document,   _          => false,
            })
        }).cloned()
    }

    pub fn select_by_name(&self, name: &str) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.name() == name).cloned()
    }

    pub fn names(&self) -> Vec<&str> { self.backends.iter().map(|b| b.name()).collect() }
}
```

Add `pub mod backend;` to `src/supervisor/mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/mod.rs src/supervisor/mod.rs
git commit -m "supervisor(M2): Backend trait + capability-based Registry"
```

### Task 2.2: `ReasoningBackend` wrapping existing `Agent`

**Files:**

- Create: `src/supervisor/backend/reasoning.rs`
- Modify: `src/supervisor/backend/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn reasoning_backend_advertises_capabilities() {
    // Agent construction needs many fixtures; build a fake reasoning backend
    // that just wraps a closure to keep the test isolated.
    let b = ReasoningBackend::new_with_executor(|prompt| async move {
        Ok(format!("echo:{prompt}"))
    });
    let caps = b.capabilities();
    assert!(caps.reasoning);
    assert!(!caps.shell);

    let mut job = crate::supervisor::job::Job::new(
        "task1", crate::supervisor::job::JobType::PlannerJob, "reasoning", "plan it");
    job.prompt = Some("hello".into());
    let out = b.run(&mut job).await.unwrap();
    assert!(out.summary.starts_with("echo:hello"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/backend/reasoning.rs
use anyhow::{anyhow, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::supervisor::backend::{Backend, BackendCapabilities};
use crate::supervisor::job::{Job, JobOutput, JobStatus, JobType, Evidence};

type ExecFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

pub struct ReasoningBackend { exec: ExecFn }

impl ReasoningBackend {
    /// Production constructor using the real Agent (added in Task 2.3).
    pub fn from_agent(agent: Arc<crate::agent::Agent>, default_user: String, default_chat: String) -> Self {
        let exec: ExecFn = Arc::new(move |prompt| {
            let agent = agent.clone();
            let user = default_user.clone();
            let chat = default_chat.clone();
            Box::pin(async move {
                let incoming = crate::platform::IncomingMessage {
                    platform: "supervisor".into(),
                    user_id: user, chat_id: chat,
                    text: prompt, message_id: None,
                };
                agent.process_message(&incoming, None, None).await
                    .map_err(|e| anyhow!("agent failed: {e:#}"))
            })
        });
        Self { exec }
    }

    /// Test-only constructor.
    #[cfg(test)]
    pub fn new_with_executor<F, Fut>(f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        let f = Arc::new(f);
        Self { exec: Arc::new(move |p| {
            let f = f.clone();
            Box::pin(async move { (f)(p).await })
        }) }
    }
}

#[async_trait::async_trait]
impl Backend for ReasoningBackend {
    fn name(&self) -> &str { "reasoning" }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { reasoning: true, ..Default::default() }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(jt, JobType::PlannerJob | JobType::ExecutorJob | JobType::ReviewerJob | JobType::DocumentJob)
    }
    async fn run(&self, job: &mut Job) -> Result<JobOutput> {
        job.status = JobStatus::Running;
        let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        let summary = (self.exec)(prompt).await?;
        let evidence = vec![Evidence::OutputValidated { description: "non-empty reasoning output".into() }];
        let status = if summary.is_empty() { JobStatus::Failed } else { JobStatus::Succeeded };
        job.status = status.clone();
        Ok(JobOutput { status, summary, evidence, errors: vec![], changed_files: vec![], next_step: None })
    }
}
```

Re-export from `src/supervisor/backend/mod.rs`: `pub mod reasoning;`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/reasoning.rs src/supervisor/backend/mod.rs
git commit -m "supervisor(M2): ReasoningBackend wrapping existing Agent"
```

### Task 2.3: `ShellBackend` (sandboxed)

**Files:**

- Create: `src/supervisor/backend/shell.rs`
- Modify: `src/supervisor/backend/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn shell_backend_runs_echo_in_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let b = ShellBackend::new(dir.path().into());
    let mut job = crate::supervisor::job::Job::new(
        "t", crate::supervisor::job::JobType::ShellJob, "shell", "echo hi");
    job.prompt = Some("echo hi".into());
    let out = b.run(&mut job).await.unwrap();
    assert!(matches!(out.status, crate::supervisor::job::JobStatus::Succeeded));
    assert!(out.summary.contains("hi"));
    assert!(matches!(out.evidence[0], crate::supervisor::job::Evidence::ExitCode(0)));
}

#[tokio::test]
async fn shell_backend_rejects_command_escaping_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let b = ShellBackend::new(dir.path().into());
    let mut job = crate::supervisor::job::Job::new("t",
        crate::supervisor::job::JobType::ShellJob, "shell",
        "cd /etc && cat passwd");
    job.prompt = Some("cd /etc && cat passwd".into());
    let out = b.run(&mut job).await.unwrap();
    assert!(matches!(out.status, crate::supervisor::job::JobStatus::Failed));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/backend/shell.rs
use anyhow::Result;
use std::path::PathBuf;
use tokio::process::Command;

use crate::supervisor::backend::{Backend, BackendCapabilities};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct ShellBackend { sandbox: PathBuf }

impl ShellBackend {
    pub fn new(sandbox: PathBuf) -> Self { Self { sandbox } }

    fn validate(&self, cmd: &str) -> bool {
        // Reject if user tries to leave sandbox via cd
        let lower = cmd.trim_start();
        if lower.starts_with("cd /") || lower.contains("cd ..") { return false; }
        if lower.contains("../") { return false; }
        true
    }
}

#[async_trait::async_trait]
impl Backend for ShellBackend {
    fn name(&self) -> &str { "shell" }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { shell: true, ..Default::default() }
    }
    fn can_handle(&self, jt: &JobType) -> bool { matches!(jt, JobType::ShellJob) }
    async fn run(&self, job: &mut Job) -> Result<JobOutput> {
        let cmd = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        if !self.validate(&cmd) {
            job.status = JobStatus::Failed;
            return Ok(JobOutput {
                status: JobStatus::Failed, summary: String::new(),
                evidence: vec![], errors: vec!["sandbox-violation: cd outside sandbox".into()],
                changed_files: vec![], next_step: None,
            });
        }
        let output = Command::new("sh").arg("-c").arg(&cmd)
            .current_dir(&self.sandbox).output().await?;
        let exit = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let status = if output.status.success() { JobStatus::Succeeded } else { JobStatus::Failed };
        job.status = status.clone();
        Ok(JobOutput {
            status,
            summary: stdout.trim().to_string(),
            evidence: vec![Evidence::ExitCode(exit)],
            errors: if stderr.is_empty() { vec![] } else { vec![stderr] },
            changed_files: vec![], next_step: None,
        })
    }
}
```

Re-export `pub mod shell;` from `backend/mod.rs`.

- [ ] **Step 4: Run** → both PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/shell.rs src/supervisor/backend/mod.rs
git commit -m "supervisor(M2): ShellBackend with sandbox validation"
```

### Task 2.4: `McpBackend` delegating to existing `McpManager`

**Files:**

- Create: `src/supervisor/backend/mcp.rs`
- Modify: `src/supervisor/backend/mod.rs`

- [ ] **Step 1: Failing test** (uses an empty `McpManager` and asserts capability advertisement only — execution path is integration-tested in M3)

```rust
#[tokio::test]
async fn mcp_backend_advertises_research_and_document() {
    let mgr = std::sync::Arc::new(crate::mcp::McpManager::new());
    let b = McpBackend::new(mgr);
    let c = b.capabilities();
    assert!(c.research && c.document);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/backend/mcp.rs
use anyhow::Result;
use std::sync::Arc;

use crate::mcp::McpManager;
use crate::supervisor::backend::{Backend, BackendCapabilities};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct McpBackend { mcp: Arc<McpManager> }

impl McpBackend { pub fn new(mcp: Arc<McpManager>) -> Self { Self { mcp } } }

#[async_trait::async_trait]
impl Backend for McpBackend {
    fn name(&self) -> &str { "mcp" }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { research: true, document: true, ..Default::default() }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(jt, JobType::ResearchJob | JobType::DocumentJob)
    }
    async fn run(&self, job: &mut Job) -> Result<JobOutput> {
        // input_context = {"tool": "mcp_<server>_<tool>", "args": {...}}
        let tool_name = job.input_context.get("tool")
            .and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
        let args = job.input_context.get("args").cloned().unwrap_or(serde_json::Value::Null);

        job.status = JobStatus::Running;
        let result = self.mcp.execute_tool(tool_name, args).await;
        match result {
            Ok(text) => {
                job.status = JobStatus::Succeeded;
                Ok(JobOutput {
                    status: JobStatus::Succeeded, summary: text,
                    evidence: vec![Evidence::OutputValidated { description: format!("mcp tool {tool_name} returned non-error") }],
                    errors: vec![], changed_files: vec![], next_step: None,
                })
            }
            Err(e) => {
                job.status = JobStatus::Failed;
                Ok(JobOutput {
                    status: JobStatus::Failed, summary: String::new(), evidence: vec![],
                    errors: vec![format!("{e:#}")], changed_files: vec![], next_step: None,
                })
            }
        }
    }
}
```

Re-export `pub mod mcp;` from `backend/mod.rs`. (If `McpManager::execute_tool` does not yet take `(name, args)` exactly, adapt to whatever the existing public signature is — see `src/mcp.rs`.)

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/mcp.rs src/supervisor/backend/mod.rs
git commit -m "supervisor(M2): McpBackend delegating to McpManager"
```

### Task 2.5: External-CLI backends — `ClaudeCodeCliBackend`, `CodexCliBackend`, `ScriptBackend`

Pattern is identical for the three; spawn the configured executable with the prompt on stdin / via flag, capture stdout/stderr, classify exit code.

**Files:**

- Create: `src/supervisor/backend/claude_code.rs`
- Create: `src/supervisor/backend/codex.rs`
- Create: `src/supervisor/backend/script.rs`
- Modify: `src/supervisor/backend/mod.rs`

For each:

- [ ] **Step 1: Failing test** (uses a stub binary `bin/echo-stub` so tests don't require Claude/Codex installed):

```rust
#[tokio::test]
async fn claude_code_backend_runs_stub_and_captures_output() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("claude-stub.sh");
    tokio::fs::write(&stub, "#!/bin/sh\necho 'pretend output'\n").await.unwrap();
    let mut perms = tokio::fs::metadata(&stub).await.unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&stub, perms).await.unwrap();

    let b = ClaudeCodeCliBackend::new(stub.to_string_lossy().into_owned(),
                                     vec!["--print".into()],
                                     dir.path().into());
    let mut job = crate::supervisor::job::Job::new(
        "t", crate::supervisor::job::JobType::ExecutorJob, "claude_code_cli", "do x");
    job.prompt = Some("do x".into());
    let out = b.run(&mut job).await.unwrap();
    assert!(out.summary.contains("pretend output"));
    assert!(matches!(out.status, crate::supervisor::job::JobStatus::Succeeded));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** (Claude version shown; Codex and Script are byte-identical with different `name()` and capability flags):

```rust
// src/supervisor/backend/claude_code.rs
use anyhow::Result;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::io::AsyncWriteExt;

use crate::supervisor::backend::{Backend, BackendCapabilities};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

pub struct ClaudeCodeCliBackend {
    bin: String, args: Vec<String>, workdir: PathBuf,
}

impl ClaudeCodeCliBackend {
    pub fn new(bin: String, args: Vec<String>, workdir: PathBuf) -> Self { Self { bin, args, workdir } }
}

#[async_trait::async_trait]
impl Backend for ClaudeCodeCliBackend {
    fn name(&self) -> &str { "claude_code_cli" }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { coding: true, reasoning: true, long_running: true, ..Default::default() }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(jt, JobType::ExecutorJob | JobType::ReviewerJob | JobType::PlannerJob)
    }
    async fn run(&self, job: &mut Job) -> Result<JobOutput> {
        let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        job.status = JobStatus::Running;

        let mut cmd = Command::new(&self.bin);
        cmd.args(&self.args).current_dir(&self.workdir)
           .stdin(std::process::Stdio::piped())
           .stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }
        let output = child.wait_with_output().await?;
        let exit = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let status = if output.status.success() { JobStatus::Succeeded } else { JobStatus::Failed };
        job.status = status.clone();
        Ok(JobOutput {
            status, summary: stdout.trim().into(),
            evidence: vec![Evidence::ExitCode(exit)],
            errors: if stderr.is_empty() { vec![] } else { vec![stderr] },
            changed_files: vec![], next_step: None,
        })
    }
}
```

Codex backend: `pub struct CodexCliBackend` with `name() = "codex_cli"`, capabilities `{ coding: true, reasoning: true, long_running: true }`, identical run logic — copy the body verbatim into `codex.rs`.

Script backend: `pub struct ScriptBackend` with `name() = "script"`, capabilities `{ shell: true }`, identical run logic — copy into `script.rs`.

Document backend (optional, addresses spec §21 "Document"): a thin shell-backed backend that pipes `job.prompt` to a configured generator command (e.g. `pandoc`) inside the sandbox. If you don't want a separate file, omit it — `ReasoningBackend` plus `McpBackend` already cover all `DocumentJob`s today, and the Spec Coverage Matrix flags that fact explicitly.

- [ ] **Step 4: Run** all three → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/{claude_code,codex,script}.rs src/supervisor/backend/mod.rs
git commit -m "supervisor(M2): ClaudeCodeCliBackend, CodexCliBackend, ScriptBackend"
```

---

## Milestone 3 — Plan / Execute / Verify / Report Loop

Purpose: drive a `Task` through `PLAN → EXECUTE → VERIFY → REPORT → ARCHIVE` using the registry; one Job, single backend (parallel/staged comes in M6).

### Task 3.1: `Workflow` template enum + Fast / Standard / Rigorous templates

**Files:**

- Create: `src/supervisor/workflow.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn fast_mode_skips_clarify_and_plan() {
    use crate::supervisor::task::*;
    let mut t = Task::new("x", "summarize"); t.execution_mode = ExecutionMode::Fast;
    let stages = WorkflowTemplate::for_task(&t).stages();
    assert_eq!(stages, vec![
        TaskStatus::Intake, TaskStatus::Classify, TaskStatus::Execute,
        TaskStatus::Verify,  TaskStatus::Report,
    ]);
}

#[test]
fn rigorous_includes_review_and_archive() {
    use crate::supervisor::task::*;
    let mut t = Task::new("x", "x"); t.execution_mode = ExecutionMode::Rigorous;
    let stages = WorkflowTemplate::for_task(&t).stages();
    assert!(stages.contains(&TaskStatus::Review));
    assert!(stages.contains(&TaskStatus::Archive));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/workflow.rs
use crate::supervisor::task::{ExecutionMode, Task, TaskStatus};

pub struct WorkflowTemplate { mode: ExecutionMode }

impl WorkflowTemplate {
    pub fn for_task(t: &Task) -> Self { Self { mode: t.execution_mode.clone() } }
    pub fn stages(&self) -> Vec<TaskStatus> {
        use TaskStatus::*;
        match self.mode {
            ExecutionMode::Fast =>
                vec![Intake, Classify, Execute, Verify, Report],
            ExecutionMode::Standard =>
                vec![Intake, Classify, Route, Clarify, Plan, Execute, Verify, Report, Archive],
            ExecutionMode::Rigorous =>
                vec![Intake, Classify, Route, Clarify, Plan, PrepareWorkspace,
                     Execute, Review, Verify, Report, Archive],
        }
    }
}
```

Add `pub mod workflow;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/workflow.rs src/supervisor/mod.rs
git commit -m "supervisor(M3): WorkflowTemplate (Fast/Standard/Rigorous stages)"
```

### Task 3.2: `Planner` — produce single-job plan from a Task

**Files:**

- Create: `src/supervisor/planner.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn planner_emits_single_executor_job_for_simple_task() {
    use crate::supervisor::task::*;
    let mut t = Task::new("ok", "summarize the readme");
    t.task_type = TaskType::GeneralAssistant;
    t.required_capabilities = vec!["reasoning".into()];
    let plan = Planner::new().plan(&t);
    assert_eq!(plan.jobs.len(), 1);
    assert_eq!(plan.jobs[0].job_type, crate::supervisor::job::JobType::ExecutorJob);
}

#[test]
fn planner_emits_planner_then_executor_for_rigorous_code_task() {
    use crate::supervisor::task::*;
    let mut t = Task::new("refactor", "refactor module foo");
    t.task_type = TaskType::Refactor; t.execution_mode = ExecutionMode::Rigorous;
    t.required_capabilities = vec!["coding".into()];
    let plan = Planner::new().plan(&t);
    assert_eq!(plan.jobs.len(), 3, "planner + executor + reviewer");
    assert_eq!(plan.jobs[0].job_type, crate::supervisor::job::JobType::PlannerJob);
    assert_eq!(plan.jobs[1].job_type, crate::supervisor::job::JobType::ExecutorJob);
    assert_eq!(plan.jobs[2].job_type, crate::supervisor::job::JobType::ReviewerJob);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/planner.rs
use crate::supervisor::job::{Job, JobType};
use crate::supervisor::task::{ExecutionMode, Task};

pub struct Plan { pub jobs: Vec<Job> }

#[derive(Default)]
pub struct Planner;

impl Planner {
    pub fn new() -> Self { Self }

    pub fn plan(&self, t: &Task) -> Plan {
        let mut jobs = Vec::new();
        let primary_backend = t.required_capabilities.first()
            .map(String::as_str).unwrap_or("reasoning").to_string();
        if matches!(t.execution_mode, ExecutionMode::Rigorous) {
            jobs.push(Job::new(&t.id, JobType::PlannerJob, "reasoning",
                               &format!("Plan steps for: {}", t.user_request)));
        }
        let mut exec = Job::new(&t.id, JobType::ExecutorJob, &primary_backend, &t.user_request);
        exec.prompt = Some(t.user_request.clone());
        jobs.push(exec);
        if matches!(t.execution_mode, ExecutionMode::Rigorous) {
            jobs.push(Job::new(&t.id, JobType::ReviewerJob, "reasoning",
                               &format!("Review the executor result for: {}", t.title)));
        }
        Plan { jobs }
    }
}
```

Add `pub mod planner;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/planner.rs src/supervisor/mod.rs
git commit -m "supervisor(M3): Planner producing 1- and 3-job plans"
```

### Task 3.3: `JobStore` (small extension of TaskStore for jobs)

**Files:**

- Modify: `src/supervisor/store.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn save_and_load_jobs_for_task() {
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = TaskStore::new(memory.connection());
    let task = crate::supervisor::task::Task::new("T", "u");
    store.create(&task, "telegram", "u", None).await.unwrap();

    let mut job = crate::supervisor::job::Job::new(
        &task.id, crate::supervisor::job::JobType::ExecutorJob, "reasoning", "do");
    job.prompt = Some("do it".into());
    store.create_job(&job).await.unwrap();
    let jobs = store.jobs_for_task(&task.id).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job.id);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Add to `store.rs`**:

```rust
use crate::supervisor::job::{Job, JobStatus, JobType};

impl TaskStore {
    pub async fn create_job(&self, j: &Job) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_jobs
             (id, task_id, parent_job_id, job_type, backend, goal, prompt,
              input_context, timeout_secs, retry_max, retry_count, allow_tools,
              workspace, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![
                j.id, j.task_id, j.parent_job_id,
                serde_json::to_string(&j.job_type)?, j.backend, j.goal, j.prompt,
                j.input_context.to_string(), j.timeout_secs as i64,
                j.retry_max as i64, j.retry_count as i64,
                serde_json::to_string(&j.allow_tools)?, j.workspace,
                serde_json::to_string(&j.status)?,
            ],
        )?; Ok(())
    }

    pub async fn jobs_for_task(&self, task_id: &str) -> Result<Vec<Job>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, parent_job_id, job_type, backend, goal, prompt,
                    input_context, timeout_secs, retry_max, retry_count, allow_tools,
                    workspace, status, result_summary, error
             FROM sup_jobs WHERE task_id=?1 ORDER BY rowid ASC")?;
        let rows = stmt.query_map([task_id], |r| Ok(Job {
            id: r.get(0)?, task_id: r.get(1)?, parent_job_id: r.get(2)?,
            job_type: serde_json::from_str::<JobType>(&r.get::<_,String>(3)?).unwrap(),
            backend: r.get(4)?, goal: r.get(5)?, prompt: r.get(6)?,
            input_context: serde_json::from_str(&r.get::<_,String>(7)?).unwrap_or(serde_json::Value::Null),
            timeout_secs: r.get::<_,i64>(8)? as u64,
            retry_max:    r.get::<_,i64>(9)? as u32,
            retry_count:  r.get::<_,i64>(10)? as u32,
            allow_tools:  serde_json::from_str(&r.get::<_,String>(11)?).unwrap_or_default(),
            workspace: r.get(12)?,
            status: serde_json::from_str::<JobStatus>(&r.get::<_,String>(13)?).unwrap(),
            result: r.get::<_,Option<String>>(14)?.map(|_| crate::supervisor::job::JobOutput {
                status: crate::supervisor::job::JobStatus::Succeeded,
                summary: String::new(), evidence: vec![], errors: vec![],
                changed_files: vec![], next_step: None,
            }),
            error: r.get(15)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub async fn update_job_status(&self, id: &str, status: JobStatus,
                                   summary: Option<&str>, error: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sup_jobs SET status=?1, result_summary=?2, error=?3,
                                 finished_at=datetime('now') WHERE id=?4",
            rusqlite::params![serde_json::to_string(&status)?, summary, error, id],
        )?; Ok(())
    }
}
```

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/store.rs
git commit -m "supervisor(M3): TaskStore::create_job / jobs_for_task / update_job_status"
```

### Task 3.4: `Orchestrator::execute_plan` — sequential, single-backend execution

**Files:**

- Create: `src/supervisor/orchestrator.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn orchestrator_runs_plan_and_persists_results() {
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = crate::supervisor::store::TaskStore::new(memory.connection());

    let task = crate::supervisor::task::Task::new("T", "summarize");
    store.create(&task, "telegram", "u", None).await.unwrap();

    let mut reg = crate::supervisor::backend::Registry::new();
    reg.register(std::sync::Arc::new(
        crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
            |p| async move { Ok(format!("answered: {p}")) })));

    let plan = crate::supervisor::planner::Planner::new().plan(&task);
    let orch = Orchestrator::new(reg, store.clone());
    let outcome = orch.execute_plan(&task, plan).await.unwrap();
    assert!(matches!(outcome, OrchestratorOutcome::AllSucceeded));

    let jobs = store.jobs_for_task(&task.id).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, crate::supervisor::job::JobStatus::Succeeded);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/orchestrator.rs
use anyhow::Result;
use crate::supervisor::backend::Registry;
use crate::supervisor::job::{Job, JobStatus};
use crate::supervisor::planner::Plan;
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::Task;

pub enum OrchestratorOutcome { AllSucceeded, FailedAt(String) }

pub struct Orchestrator { reg: Registry, store: TaskStore }

impl Orchestrator {
    pub fn new(reg: Registry, store: TaskStore) -> Self { Self { reg, store } }

    pub async fn execute_plan(&self, _task: &Task, plan: Plan) -> Result<OrchestratorOutcome> {
        for mut job in plan.jobs {
            self.store.create_job(&job).await?;
            let backend = self.reg.select_by_name(&job.backend)
                .or_else(|| self.reg.select_for(&[job.backend.clone()]));
            let Some(backend) = backend else {
                self.store.update_job_status(&job.id, JobStatus::Failed,
                    None, Some("no backend matched")).await?;
                return Ok(OrchestratorOutcome::FailedAt(job.id));
            };
            let out = backend.run(&mut job).await;
            match out {
                Ok(out) if matches!(out.status, JobStatus::Succeeded) => {
                    self.store.update_job_status(&job.id, JobStatus::Succeeded,
                        Some(&out.summary), None).await?;
                }
                Ok(out) => {
                    self.store.update_job_status(&job.id, JobStatus::Failed,
                        Some(&out.summary), out.errors.first().map(String::as_str)).await?;
                    return Ok(OrchestratorOutcome::FailedAt(job.id));
                }
                Err(e) => {
                    self.store.update_job_status(&job.id, JobStatus::Failed,
                        None, Some(&format!("{e:#}"))).await?;
                    return Ok(OrchestratorOutcome::FailedAt(job.id));
                }
            }
        }
        Ok(OrchestratorOutcome::AllSucceeded)
    }
}
```

Add `pub mod orchestrator;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/orchestrator.rs src/supervisor/mod.rs
git commit -m "supervisor(M3): Orchestrator sequential single-backend execution"
```

### Task 3.5: `VerificationEngine` — evidence-based completion gate

**Files:**

- Create: `src/supervisor/verification.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn verifies_when_all_jobs_succeeded_with_evidence() {
    use crate::supervisor::job::*;
    let jobs = vec![done_job(JobStatus::Succeeded, vec![Evidence::ExitCode(0)])];
    assert!(matches!(VerificationEngine.verify(&jobs), VerificationOutcome::Passed));
}

#[test]
fn fails_when_any_job_lacks_evidence() {
    use crate::supervisor::job::*;
    let jobs = vec![done_job(JobStatus::Succeeded, vec![])];
    assert!(matches!(VerificationEngine.verify(&jobs),
                     VerificationOutcome::Failed(_)));
}

fn done_job(status: crate::supervisor::job::JobStatus, ev: Vec<crate::supervisor::job::Evidence>)
  -> crate::supervisor::job::Job
{
    let mut j = crate::supervisor::job::Job::new(
        "t", crate::supervisor::job::JobType::ExecutorJob, "reasoning", "g");
    j.status = status.clone();
    j.result = Some(crate::supervisor::job::JobOutput {
        status, summary: String::new(), evidence: ev, errors: vec![],
        changed_files: vec![], next_step: None,
    });
    j
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/verification.rs
use crate::supervisor::job::{Job, JobStatus};

pub enum VerificationOutcome { Passed, Failed(String) }

pub struct VerificationEngine;

impl VerificationEngine {
    pub fn verify(&self, jobs: &[Job]) -> VerificationOutcome {
        for j in jobs {
            if !matches!(j.status, JobStatus::Succeeded) {
                return VerificationOutcome::Failed(format!("job {} not succeeded", j.id));
            }
            let ev_count = j.result.as_ref().map(|r| r.evidence.len()).unwrap_or(0);
            if ev_count == 0 {
                return VerificationOutcome::Failed(format!("job {} produced no evidence", j.id));
            }
        }
        VerificationOutcome::Passed
    }
}
```

Add `pub mod verification;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/verification.rs src/supervisor/mod.rs
git commit -m "supervisor(M3): VerificationEngine evidence gate"
```

### Task 3.6: `Reporter` — final summary back to caller

**Files:**

- Create: `src/supervisor/reporter.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn reporter_renders_human_summary() {
    use crate::supervisor::job::*;
    let mut j = Job::new("t", JobType::ExecutorJob, "reasoning", "g");
    j.status = JobStatus::Succeeded;
    j.result = Some(JobOutput {
        status: JobStatus::Succeeded, summary: "All good.".into(),
        evidence: vec![Evidence::ExitCode(0)], errors: vec![],
        changed_files: vec!["src/foo.rs".into()], next_step: None,
    });
    let r = Reporter::render(&[j]);
    assert!(r.contains("All good."));
    assert!(r.contains("src/foo.rs"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/reporter.rs
use crate::supervisor::job::Job;

pub struct Reporter;

impl Reporter {
    pub fn render(jobs: &[Job]) -> String {
        let mut out = String::new();
        for j in jobs {
            out.push_str(&format!("• [{}] {}\n", j.backend, j.goal));
            if let Some(res) = &j.result {
                if !res.summary.is_empty() {
                    out.push_str("  "); out.push_str(&res.summary); out.push('\n');
                }
                if !res.changed_files.is_empty() {
                    out.push_str("  changed files:\n");
                    for f in &res.changed_files { out.push_str(&format!("    - {f}\n")); }
                }
            }
        }
        out
    }
}
```

Add `pub mod reporter;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/reporter.rs src/supervisor/mod.rs
git commit -m "supervisor(M3): Reporter human-readable summary"
```

### Task 3.7: M3 end-to-end — `Supervisor::execute_now` Fast-mode happy path

**Files:**

- Modify: `src/supervisor/mod.rs`
- Test: `tests/supervisor/e2e_fast_mode.rs`

- [ ] **Step 1: Failing integration test**

```rust
// tests/supervisor/e2e_fast_mode.rs
use rustfox::supervisor::{Supervisor, SubmitOutcome};

#[tokio::test]
async fn fast_mode_runs_to_completion_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("done:{p}")) });

    let outcome = sup.submit("telegram", "u1", Some("c1"), "summarize the readme").await.unwrap();
    let task_id = outcome.task_id();
    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));

    let report = sup.execute_now(&task_id).await.unwrap();
    assert!(report.contains("done:"));
    let final_state = sup.state(&task_id).await.unwrap();
    assert_eq!(final_state, rustfox::supervisor::task::TaskStatus::Done);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

In `src/supervisor/mod.rs`, extend `Supervisor`:

```rust
use crate::supervisor::backend::{reasoning::ReasoningBackend, Registry};
use crate::supervisor::orchestrator::{Orchestrator, OrchestratorOutcome};
use crate::supervisor::planner::Planner;
use crate::supervisor::reporter::Reporter;
use crate::supervisor::verification::{VerificationEngine, VerificationOutcome};

pub struct Supervisor {
    store: TaskStore,
    artifacts: Arc<ArtifactManager>,
    classifier: Box<dyn Classifier + Send + Sync>,
    policy: PolicyEngine,
    pub registry: Registry,
}

impl Supervisor {
    // ... existing new_for_test now also seeds Registry::new()

    pub fn register_test_reasoning_backend<F, Fut>(&mut self, f: F)
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.registry.register(Arc::new(ReasoningBackend::new_with_executor(f)));
    }

    pub async fn execute_now(&self, task_id: &str) -> anyhow::Result<String> {
        let task = self.store.get(task_id).await?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;

        // PLAN
        self.store.record_transition(task_id, TaskStatus::Route, TaskStatus::Plan,
            "supervisor", None).await?;
        let plan = Planner::new().plan(&task);
        self.artifacts.write_text(task_id, None, "plan", "plan.json",
            &serde_json::to_string_pretty(&serde_json::json!({
                "jobs": plan.jobs.iter().map(|j| serde_json::json!({
                    "type": j.job_type, "backend": j.backend, "goal": j.goal,
                })).collect::<Vec<_>>()
            }))?).await?;

        // EXECUTE
        self.store.record_transition(task_id, TaskStatus::Plan, TaskStatus::Execute,
            "supervisor", None).await?;
        let orch = Orchestrator::new(
            // Registry is not Clone yet; in production wrap in Arc and clone Arc.
            std::mem::take(&mut self.clone_registry()), self.store.clone());
        let res = orch.execute_plan(&task, plan).await?;
        let jobs = self.store.jobs_for_task(task_id).await?;

        // VERIFY
        self.store.record_transition(task_id,
            if matches!(res, OrchestratorOutcome::AllSucceeded) { TaskStatus::Execute } else { TaskStatus::Execute },
            TaskStatus::Verify, "supervisor", None).await?;
        let v = VerificationEngine.verify(&jobs);

        // REPORT + ARCHIVE
        let report = Reporter::render(&jobs);
        self.artifacts.write_text(task_id, None, "result", "report.md", &report).await?;
        match v {
            VerificationOutcome::Passed => {
                self.store.record_transition(task_id, TaskStatus::Verify, TaskStatus::Report,
                    "supervisor", None).await?;
                self.store.record_transition(task_id, TaskStatus::Report, TaskStatus::Archive,
                    "supervisor", None).await?;
                self.store.record_transition(task_id, TaskStatus::Archive, TaskStatus::Done,
                    "supervisor", None).await?;
                Ok(report)
            }
            VerificationOutcome::Failed(reason) => {
                self.store.record_transition(task_id, TaskStatus::Verify, TaskStatus::Failed,
                    "verifier", Some(&reason)).await?;
                Ok(format!("VERIFICATION FAILED: {reason}\n\n{report}"))
            }
        }
    }

    pub async fn state(&self, task_id: &str) -> anyhow::Result<TaskStatus> {
        Ok(self.store.get(task_id).await?
            .ok_or_else(|| anyhow::anyhow!("task missing"))?.status)
    }

    fn clone_registry(&self) -> Registry { /* see note */ unimplemented!() }
}
```

The `Registry` clone problem: change `Registry` to hold `Vec<Arc<dyn Backend>>` (already does) and derive `Clone` on it: `#[derive(Default, Clone)]` — `Arc` is `Clone`, so this works. Update `backend/mod.rs` accordingly. Then `clone_registry` becomes `self.registry.clone()`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/mod.rs src/supervisor/backend/mod.rs tests/supervisor/e2e_fast_mode.rs
git commit -m "supervisor(M3): Supervisor::execute_now fast-mode end-to-end"
```

### Task 3.8: Wire Supervisor into Telegram intake

**Files:**

- Modify: `src/platform/telegram.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Failing test** — none (integration via running bot). Use a smoke check inside `telegram.rs` that the new `/supervise` command is parsed.

```rust
#[test]
fn parse_supervise_command_extracts_request_text() {
    let parsed = super::parse_command("/supervise summarize the readme");
    assert_eq!(parsed, Some(("supervise".into(), "summarize the readme".into())));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Add a small `parse_command` helper in `src/platform/telegram.rs`:

```rust
pub(crate) fn parse_command(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    if !s.starts_with('/') { return None; }
    let rest = &s[1..];
    let mut it = rest.splitn(2, char::is_whitespace);
    let cmd = it.next()?.to_string();
    let arg = it.next().unwrap_or("").trim().to_string();
    Some((cmd, arg))
}
```

In the message handler, when text starts with `/supervise`, call `agent.supervisor.submit(...)` and reply with the human-readable outcome (clarification question, approval-required notice, or `execute_now` report). Wire `Supervisor` into `AppState`/`Agent` from `main.rs`:

```rust
// main.rs additions (sketch)
let artifacts_dir = config.supervisor.artifacts_dir.clone();
let supervisor = Arc::new(rustfox::supervisor::Supervisor::new(
    artifacts_dir, memory.connection(),
    /* preconfigured Registry from BackendsConfig (built below) */));
```

Build the registry from config (`BackendsConfig`): always register `ReasoningBackend::from_agent`, `ShellBackend::new(config.sandbox.allowed_directory)`, `McpBackend::new(Arc::new(mcp_manager.clone()))`, plus optional `ClaudeCodeCliBackend` / `CodexCliBackend` / `ScriptBackend` if their bin paths are configured.

Pass the supervisor through as part of `Agent` (add `pub supervisor: Arc<Supervisor>` field) or as a sibling `Arc` in `AppState`.

- [ ] **Step 4: Run** unit test → PASS. Then `cargo build` → SUCCESS.

- [ ] **Step 5: Commit**

```bash
git add src/platform/telegram.rs src/main.rs src/agent.rs
git commit -m "supervisor(M3): wire Supervisor into Telegram /supervise command"
```

---

## Milestone 4 — Branch / Worktree Integration for Code Tasks

Purpose: when classifier says `CodeChange|BugFix|Refactor`, the supervisor creates a git branch (and optionally a worktree) before executing, and cleans up afterwards.

### Task 4.1: `WorkspaceManager` — branch + optional worktree

**Files:**

- Create: `src/supervisor/workspace.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test** (uses a real git repo created in tempdir):

```rust
#[tokio::test]
async fn creates_branch_in_existing_repo() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;
    let wm = WorkspaceManager::new(dir.path().into(), false);
    let ws = wm.prepare("task-abc", "fix-login-bug").await.unwrap();
    assert!(ws.branch.starts_with("supervisor/"));
    assert_eq!(ws.path, dir.path());
    let branches = git(&dir.path(), &["branch", "--show-current"]).await;
    assert_eq!(branches.trim(), ws.branch);
}

#[tokio::test]
async fn creates_worktree_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;
    let wm = WorkspaceManager::new(dir.path().into(), true);
    let ws = wm.prepare("task-xyz", "refactor-foo").await.unwrap();
    assert_ne!(ws.path, dir.path());
    assert!(ws.path.exists());
}

async fn init_git_repo(p: &std::path::Path) { /* git init / commit */ }
async fn git(p: &std::path::Path, args: &[&str]) -> String { /* exec git */ }
```

(Provide `init_git_repo` and `git` helpers in the test file.)

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
// src/supervisor/workspace.rs
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub struct Workspace { pub path: PathBuf, pub branch: String }

pub struct WorkspaceManager { repo: PathBuf, use_worktree: bool }

impl WorkspaceManager {
    pub fn new(repo: PathBuf, use_worktree: bool) -> Self { Self { repo, use_worktree } }

    pub async fn prepare(&self, task_id: &str, slug: &str) -> Result<Workspace> {
        let safe_slug: String = slug.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        let branch = format!("supervisor/{safe_slug}-{}", &task_id[..8]);

        if self.use_worktree {
            let path = self.repo.with_extension(format!("worktree-{}", &task_id[..8]));
            run(&self.repo, &["worktree", "add", "-b", &branch,
                              path.to_str().unwrap()]).await
                .context("git worktree add")?;
            Ok(Workspace { path, branch })
        } else {
            run(&self.repo, &["checkout", "-b", &branch]).await
                .context("git checkout -b")?;
            Ok(Workspace { path: self.repo.clone(), branch })
        }
    }

    pub async fn cleanup(&self, ws: &Workspace, keep_branch: bool) -> Result<()> {
        if self.use_worktree {
            run(&self.repo, &["worktree", "remove", ws.path.to_str().unwrap(), "--force"]).await?;
        }
        if !keep_branch {
            run(&self.repo, &["branch", "-D", &ws.branch]).await.ok();
        }
        Ok(())
    }
}

async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output().await?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "),
                      String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
```

Add `pub mod workspace;` to `mod.rs`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/workspace.rs src/supervisor/mod.rs
git commit -m "supervisor(M4): WorkspaceManager (branch + optional worktree)"
```

### Task 4.2: Insert PREPARE_WORKSPACE stage for code tasks

**Files:**

- Modify: `src/supervisor/mod.rs::execute_now`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn rigorous_code_task_creates_workspace_before_execute() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test_with_repo(
        dir.path().into(), dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(p) });

    let outcome = sup.submit("telegram","u1",Some("c1"),
        "refactor module foo to be testable").await.unwrap();
    let id = outcome.task_id();
    sup.execute_now(&id).await.unwrap();

    let arts = sup.artifacts().list(&id).await.unwrap();
    let kinds: Vec<_> = arts.iter().map(|a| a.kind.as_str()).collect();
    assert!(kinds.contains(&"workspace"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

In `Supervisor::execute_now`, branch on `task.task_type`:

```rust
use crate::supervisor::task::TaskType;
let needs_ws = matches!(task.task_type,
    TaskType::CodeChange | TaskType::BugFix | TaskType::Refactor);
if needs_ws {
    if let Some(wm) = &self.workspace_mgr {
        self.store.record_transition(task_id, TaskStatus::Plan, TaskStatus::PrepareWorkspace,
            "supervisor", None).await?;
        let ws = wm.prepare(task_id, &task.title).await?;
        self.artifacts.write_text(task_id, None, "workspace", "workspace.json",
            &serde_json::to_string_pretty(&serde_json::json!({
                "branch": ws.branch, "path": ws.path,
            }))?).await?;
        // (Plumb ws.path into ShellBackend / Coding backends via job.workspace.)
    }
}
```

Add `pub workspace_mgr: Option<Arc<WorkspaceManager>>` to `Supervisor` and a `new_for_test_with_repo` constructor.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/mod.rs
git commit -m "supervisor(M4): insert PREPARE_WORKSPACE stage for code tasks"
```

---

## Milestone 5 — Skill Packs for Multiple Workflows

Purpose: extend the existing `skills/` system so the supervisor can ask a skill "what's the recipe?" — e.g. `coding`, `research`, `writing`, `ops`, `general` — and get back a workflow override.

### Task 5.1: Add `[supervisor]` section to skill frontmatter

**Files:**

- Modify: `src/skills/loader.rs` (add `supervisor:` field)
- Modify: `src/skills/mod.rs` (extend `Skill` struct)

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn skill_with_supervisor_block_loads_workflow_hint() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("research-pack");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(skill_dir.join("SKILL.md"),
        "---\nname: research-pack\ndescription: research workflow\n\
         supervisor:\n  workflow: research\n  required_capabilities: [research]\n---\nbody").await.unwrap();
    let skills = load_skills_from_dir(dir.path()).await.unwrap();
    let s = skills.get("research-pack").unwrap();
    assert_eq!(s.supervisor_workflow.as_deref(), Some("research"));
    assert_eq!(s.supervisor_required_caps, vec!["research".to_string()]);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

In `Skill` struct, add:

```rust
pub supervisor_workflow: Option<String>,
pub supervisor_required_caps: Vec<String>,
```

In `loader.rs`, parse the optional `supervisor:` block from YAML frontmatter (extend the existing parsing). Initialize new fields to `None` / `vec![]` for skills that don't have it.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/skills/loader.rs src/skills/mod.rs
git commit -m "supervisor(M5): skills can hint workflow + required capabilities"
```

### Task 5.2: Bundle the five default skill packs

**Files:**

- Create: `skills/sup-coding/SKILL.md`
- Create: `skills/sup-research/SKILL.md`
- Create: `skills/sup-writing/SKILL.md`
- Create: `skills/sup-ops/SKILL.md`
- Create: `skills/sup-general/SKILL.md`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn ships_five_supervisor_skill_packs() {
    let skills = load_skills_from_dir(std::path::Path::new("skills")).await.unwrap();
    for n in ["sup-coding","sup-research","sup-writing","sup-ops","sup-general"] {
        assert!(skills.get(n).is_some(), "missing {n}");
        assert!(skills.get(n).unwrap().supervisor_workflow.is_some());
    }
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** — write the five SKILL.md files. Each has the form:

```markdown
---
name: sup-coding
description: Coding workflow recipe (brainstorm → design → spec → plan → implement → review → verify → finish)
supervisor:
  workflow: coding
  required_capabilities: [coding, shell, reasoning]
---
## When to use
When a task is classified as code_change, bug_fix, or refactor.

## Operating rules
1. Always run inside an isolated branch/worktree.
2. Always run formatter, linter, and tests before declaring success.
3. Verification evidence: at minimum one passing test or one confirmed diff.

## Stop conditions
- All planned changes implemented.
- Verification passes.
- Reviewer notes are addressed.
```

(Repeat with appropriate workflow/capabilities for `sup-research`, `sup-writing`, `sup-ops`, `sup-general`.)

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add skills/sup-*
git commit -m "supervisor(M5): bundle five default workflow skill packs"
```

### Task 5.3: Classifier consults skill hints to override workflow

**Files:**

- Modify: `src/supervisor/classifier.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn skill_hint_overrides_default_workflow() {
    // Build a HeuristicClassifier wrapper that consults a SkillRegistry.
    let mut registry = crate::skills::SkillRegistry::new();
    registry.register(crate::skills::Skill {
        name: "sup-research".into(), description: "research".into(),
        content: "".into(), tags: vec![], model: None, tools: vec![], max_iterations: None,
        supervisor_workflow: Some("research".into()),
        supervisor_required_caps: vec!["research".into()],
    });
    let c = SkillAwareClassifier::new(HeuristicClassifier, registry);
    let t = c.classify("answer this question: foo");
    // Heuristic alone returns GeneralAssistant, but skill hint elevates to Research.
    assert_eq!(t.required_capabilities, vec!["research"]);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
pub struct SkillAwareClassifier<C: Classifier> {
    inner: C,
    skills: crate::skills::SkillRegistry,
}

impl<C: Classifier> SkillAwareClassifier<C> {
    pub fn new(inner: C, skills: crate::skills::SkillRegistry) -> Self { Self { inner, skills } }

    pub fn classify(&self, request: &str) -> Task {
        let mut base = HeuristicClassifier.classify(request); // re-use existing helper
        let outcome = self.inner.classify(request);
        base.task_type = outcome.task_type;
        base.risk_level = outcome.risk_level;
        base.execution_mode = outcome.execution_mode;
        base.required_capabilities = outcome.required_capabilities;

        // Match request against skill packs by simple keyword: name without "sup-" prefix.
        for skill in self.skills.list() {
            let key = skill.name.strip_prefix("sup-").unwrap_or(&skill.name);
            if request.to_lowercase().contains(key) {
                if let Some(_wf) = &skill.supervisor_workflow {
                    base.required_capabilities = skill.supervisor_required_caps.clone();
                    break;
                }
            }
        }
        base
    }
}
```

(In `Supervisor::new`, prefer `SkillAwareClassifier` when a skill registry is available.)

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/classifier.rs src/supervisor/mod.rs
git commit -m "supervisor(M5): SkillAwareClassifier consults skill hints"
```

---

## Milestone 6 — Parallel Jobs, Fallback Backends, Subjob Orchestration

### Task 6.1: Parallel job groups in `Plan`

**Files:**

- Modify: `src/supervisor/planner.rs`
- Modify: `src/supervisor/orchestrator.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn orchestrator_runs_parallel_group_concurrently() {
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = crate::supervisor::store::TaskStore::new(memory.connection());
    let task = crate::supervisor::task::Task::new("T", "x");
    store.create(&task, "telegram", "u", None).await.unwrap();

    let mut reg = crate::supervisor::backend::Registry::new();
    let counter = std::sync::Arc::new(tokio::sync::Mutex::new(0));
    let c1 = counter.clone();
    reg.register(std::sync::Arc::new(
        crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
            move |_| { let c = c1.clone(); async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let mut g = c.lock().await; *g += 1;
                Ok(format!("done-{}", *g))
            }})));

    let mut plan = crate::supervisor::planner::Plan { jobs: vec![] };
    for _ in 0..3 {
        let mut j = crate::supervisor::job::Job::new(&task.id,
            crate::supervisor::job::JobType::ExecutorJob, "reasoning", "g");
        j.prompt = Some("x".into());
        plan.jobs.push(j);
    }
    plan.parallel_groups = vec![vec![0,1,2]];

    let orch = crate::supervisor::orchestrator::Orchestrator::new(reg, store.clone());
    let started = std::time::Instant::now();
    orch.execute_plan(&task, plan).await.unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed.as_millis() < 130, "expected concurrent execution, took {}ms", elapsed.as_millis());
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Extend `Plan`:

```rust
pub struct Plan {
    pub jobs: Vec<Job>,
    pub parallel_groups: Vec<Vec<usize>>, // each group = indices to run concurrently
}
```

In `Orchestrator::execute_plan`, walk the indices: indices not in any group run sequentially; group indices run via `tokio::join_all`.

```rust
use futures::future::join_all;

let mut grouped: std::collections::HashSet<usize> = Default::default();
for g in &plan.parallel_groups { for i in g { grouped.insert(*i); } }

let mut idx = 0;
while idx < plan.jobs.len() {
    if let Some(group) = plan.parallel_groups.iter().find(|g| g.contains(&idx)) {
        let futs: Vec<_> = group.iter().map(|&gi| {
            let mut job = plan.jobs[gi].clone();
            let store = self.store.clone();
            let reg = self.reg.clone();
            async move { /* same logic as the sequential branch */ }
        }).collect();
        join_all(futs).await; // collect results
        idx = group.iter().max().unwrap() + 1;
    } else if grouped.contains(&idx) {
        idx += 1;
    } else {
        // sequential branch (existing logic)
        idx += 1;
    }
}
```

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/planner.rs src/supervisor/orchestrator.rs
git commit -m "supervisor(M6): parallel job groups in Plan + Orchestrator"
```

### Task 6.2: Fallback backends from `BackendsConfig.fallbacks`

**Files:**

- Modify: `src/supervisor/orchestrator.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn orchestrator_falls_back_when_primary_fails() {
    let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
    let store = crate::supervisor::store::TaskStore::new(memory.connection());
    let task = crate::supervisor::task::Task::new("T", "x");
    store.create(&task, "telegram", "u", None).await.unwrap();

    let mut reg = crate::supervisor::backend::Registry::new();
    reg.register(std::sync::Arc::new(
        crate::supervisor::backend::reasoning::ReasoningBackend::new_with_executor(
            |_| async move { Err(anyhow::anyhow!("primary boom")) })));
    reg.register(std::sync::Arc::new(FailoverEcho));

    let mut fallbacks = std::collections::HashMap::new();
    fallbacks.insert("reasoning".into(), vec!["failover-echo".into()]);

    let mut plan = crate::supervisor::planner::Plan { jobs: vec![], parallel_groups: vec![] };
    let mut j = crate::supervisor::job::Job::new(&task.id,
        crate::supervisor::job::JobType::ExecutorJob, "reasoning", "g");
    j.prompt = Some("hi".into()); plan.jobs.push(j);

    let mut orch = crate::supervisor::orchestrator::Orchestrator::new(reg, store.clone());
    orch.set_fallbacks(fallbacks);
    let res = orch.execute_plan(&task, plan).await.unwrap();
    assert!(matches!(res, crate::supervisor::orchestrator::OrchestratorOutcome::AllSucceeded));
}

struct FailoverEcho;
#[async_trait::async_trait]
impl crate::supervisor::backend::Backend for FailoverEcho {
    fn name(&self) -> &str { "failover-echo" }
    fn capabilities(&self) -> crate::supervisor::backend::BackendCapabilities {
        crate::supervisor::backend::BackendCapabilities { reasoning: true, ..Default::default() }
    }
    fn can_handle(&self, _: &crate::supervisor::job::JobType) -> bool { true }
    async fn run(&self, j: &mut crate::supervisor::job::Job) -> anyhow::Result<crate::supervisor::job::JobOutput> {
        Ok(crate::supervisor::job::JobOutput {
            status: crate::supervisor::job::JobStatus::Succeeded,
            summary: format!("fallback handled {}", j.prompt.clone().unwrap_or_default()),
            evidence: vec![crate::supervisor::job::Evidence::OutputValidated { description: "fallback".into() }],
            errors: vec![], changed_files: vec![], next_step: None,
        })
    }
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Add `pub fn set_fallbacks(&mut self, m: HashMap<String, Vec<String>>)` to `Orchestrator`. In the per-job loop, on backend failure consult `fallbacks.get(&job.backend)` and retry the job with each name in turn before declaring failure.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/orchestrator.rs
git commit -m "supervisor(M6): fallback backends per capability"
```

### Task 6.3: Subjob support — backends may spawn child jobs

**Files:**

- Modify: `src/supervisor/backend/mod.rs` (add optional `spawn_subjob`)
- Modify: `src/supervisor/orchestrator.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn orchestrator_executes_spawned_subjob_after_parent() {
    // Backend that records a subjob into the orchestrator's queue via channel.
    // Parent succeeds; subjob also runs and is recorded with parent_job_id set.
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Add an `mpsc::UnboundedSender<Job>` "subjob channel" passed into each `Backend::run` via a thread-local-like context (or change the trait to accept `&mut RunContext`). Simplest correct option: change the trait method to:

```rust
async fn run(&self, job: &mut Job, ctx: &RunContext) -> Result<JobOutput>;
```

where `RunContext` exposes `spawn_subjob(&Job)`. Update `ReasoningBackend` and other backends to ignore the context (default no-op). Orchestrator drains the subjob queue after each parent and recursively executes them, setting `parent_job_id` on each.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/backend/mod.rs src/supervisor/orchestrator.rs
git commit -m "supervisor(M6): subjob spawning via RunContext"
```

---

## Milestone 7 — Fully Autonomous Daily Assistant Mode

### Task 7.1: Risk-based autonomy gate (config-driven thresholds)

**Files:**

- Modify: `src/supervisor/policy.rs`
- Modify: `src/config.rs` (add `RiskThresholdsConfig`)

- [ ] **Step 1: Failing test**

```rust
#[test]
fn risk_thresholds_can_be_tightened_via_config() {
    use crate::supervisor::task::*;
    let mut t = Task::new("x", "x");
    t.task_type = TaskType::OpsAutomation; t.risk_level = RiskLevel::Medium;
    let policy = PolicyEngine::with_thresholds(RiskThresholdsConfig {
        require_approval_for_medium: true, ..Default::default()
    });
    assert_eq!(policy.decide(&t), PolicyDecision::RequireApproval);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Add `RiskThresholdsConfig { require_approval_for_medium: bool, require_approval_for_low: bool, auto_execute_only_low: bool }` (all default false except `auto_execute_only_low = true`). Extend `PolicyEngine::with_thresholds` and rewire `decide`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/policy.rs src/config.rs
git commit -m "supervisor(M7): risk-threshold-driven autonomy gate"
```

### Task 7.2: Resume support — restore IN_PROGRESS tasks at startup

**Files:**

- Modify: `src/supervisor/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn supervisor_restores_paused_tasks_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    {
        let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
        sup.register_test_reasoning_backend(|p| async move { Ok(p) });
        let outcome = sup.submit("telegram","u","c","summarize").await.unwrap();
        sup.pause(&outcome.task_id()).await.unwrap();
    }
    // New supervisor instance — same DB
    let sup2 = Supervisor::new_for_test(dir.path().into(), memory.connection());
    let resumable = sup2.resumable_task_ids().await.unwrap();
    assert_eq!(resumable.len(), 1);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

Add `Supervisor::pause(task_id)`, `Supervisor::resume(task_id)`, and `Supervisor::resumable_task_ids()` querying `sup_tasks WHERE state IN ('PAUSED','EXECUTE','PLAN','PREPARE_WORKSPACE')`. Hook into `main.rs` to log resumable tasks at startup (manual `/resume` triggers actual continuation).

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/mod.rs src/main.rs
git commit -m "supervisor(M7): pause/resume + resumable task discovery on startup"
```

### Task 7.3: Telegram commands — `/tasks`, `/resume`, `/cancel`, `/approve`, `/clarify`

**Files:**

- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn parses_all_supervisor_commands() {
    for c in ["/tasks","/resume abc","/cancel abc","/approve abc","/clarify abc some text"] {
        assert!(super::parse_command(c).is_some(), "failed: {c}");
    }
}
```

- [ ] **Step 2: Run** → PASS already if Task 3.8 was done (sanity); add the actual handlers.

- [ ] **Step 3: Implement** the five command handlers — each simply calls into `Supervisor` and replies with rendered output (e.g. `/tasks` → list of `(id, title, state)` rows).

- [ ] **Step 4: Run** `cargo build` → SUCCESS.

- [ ] **Step 5: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "supervisor(M7): /tasks /resume /cancel /approve /clarify Telegram commands"
```

### Task 7.4: Risk-redacting log filter for tracing spans

**Files:**

- Create: `src/supervisor/redact.rs`
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn redacts_obvious_secrets_in_strings() {
    assert_eq!(redact("api_key=sk-abcdef123"), "api_key=***");
    assert_eq!(redact("Bearer xyz12345"), "Bearer ***");
    assert_eq!(redact("password: hunter2"), "password: ***");
    assert_eq!(redact("nothing sensitive"), "nothing sensitive");
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
pub fn redact(s: &str) -> String {
    let re = regex::Regex::new(
        r"(?i)(api_key|password|secret|token|bearer)\s*[:=]?\s*\S+"
    ).unwrap();
    re.replace_all(s, "$1 ***").into_owned()
}
```

(Adds `regex` to `Cargo.toml`. Use `Bearer` as a literal alternative.)

Wire `redact` into `ArtifactManager::write_text` so secrets never hit disk and into a `tracing` field formatter.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/redact.rs src/supervisor/mod.rs Cargo.toml
git commit -m "supervisor(M7): secret-redaction filter on artifacts and logs"
```

---

## Final Wiring — Definition of Done Verification

### Task DoD.1: End-to-end smoke test for each workflow type

**Files:**

- Create: `tests/supervisor/dod_smoke.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn dod_general_assistant_fast_mode() { /* Task 3.7 already covers this */ }

#[tokio::test]
async fn dod_research_workflow_artifacts_present() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = rustfox::supervisor::Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("research:{p}")) });
    let id = sup.submit("telegram","u","c","research async runtimes").await.unwrap().task_id();
    sup.execute_now(&id).await.unwrap();
    let arts = sup.artifacts().list(&id).await.unwrap();
    let kinds: Vec<_> = arts.iter().map(|a| a.kind.as_str()).collect();
    for needed in ["intake","classification","policy","plan","result"] {
        assert!(kinds.contains(&needed), "missing artifact kind: {needed}");
    }
}

#[tokio::test]
async fn dod_resumes_from_paused_state() { /* see Task 7.2 */ }
```

- [ ] **Step 2: Run** → some FAIL until prior milestones land.

- [ ] **Step 3:** No new code; this task is pure verification.

- [ ] **Step 4: Run** all → PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/supervisor/dod_smoke.rs
git commit -m "supervisor: DoD smoke test (intake→classify→policy→plan→result for every workflow)"
```

### Task DoD.2: Update `CLAUDE.md` with the new architecture

**Files:**

- Modify: `CLAUDE.md`

- [ ] **Step 1**: Append a new "Supervisor (Autopilot v2)" section that describes:

  - module tree (`src/supervisor/`),
  - state machine (link to `state.rs`),
  - backend trait + how to add a new backend,
  - new TOML keys (`[supervisor]`, `[supervisor.backends]`, `[supervisor.repo]`),
  - new bot commands (`/supervise`, `/tasks`, `/resume`, `/cancel`, `/approve`, `/clarify`),
  - artifacts root location.

- [ ] **Step 2: Run** `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`.

  Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "supervisor: document v2 supervisor architecture in CLAUDE.md"
```

---

## Spec Coverage Matrix

Quick map from design-doc section → task(s) that implement it. Keep this current
when you split or merge tasks.

| Spec section | Implementing task(s) |
|---|---|
| §1 Purpose / §26 Final Design Statement | Whole milestone set (M0–M7) |
| §4.1 Task-first | Tasks 1.5–1.10 (intake → classify → policy precede backend choice) |
| §4.2 Capability-based selection | Tasks 2.1, 6.2 |
| §4.3 Risk-based autonomy | Tasks 1.8, 7.1 |
| §4.4 Evidence-based completion | Tasks 1.2, 3.5 |
| §4.5 Resume over restart | Task 7.2 |
| §5 Five layers | Intake (1.5) · Task Intel (1.6/1.7) · Policy (1.8) · Execution (M2+M3) · Verify+Archive (3.5+3.6+1.9) |
| §6.1 Task | Task 1.1 |
| §6.2 Job | Task 1.2 |
| §6.3 Backend (declarations) | Tasks 2.1–2.5 |
| §6.4 Skill | Tasks 5.1–5.3 (existing skills system reused) |
| §6.5 Policy | Tasks 1.8, 7.1 |
| §7 Lifecycle | State machine 1.3, transitions 1.4, orchestrator 3.4, end-to-end 3.7 |
| §8 Workflow modes (Fast/Standard/Rigorous) | Task 3.1 |
| §9 Architecture (8 components) | Intake 1.5 · Classifier 1.6/1.7 · Policy 1.8 · Planner 3.2 · Backend Selector 2.1 · Orchestrator 3.4/6.1/6.2/6.3 · Verifier 3.5 · Artifacts 1.9 |
| §10 Backend adapter interface | Task 2.1 (incl. `prepare/run/collect_result/verify_result/cancel/resume`); subjob 6.3 |
| §11 Policy decision model | Tasks 1.8, 7.1 |
| §12 Workflow templates (5) | Tasks 3.1, 5.2 (skill packs are the per-workflow recipes) |
| §13 Branch/workspace | Tasks 4.1, 4.2 |
| §14 Artifact model | Task 1.9; per-task-type artifact kinds emitted in 1.10 (intake/classification/policy), 3.7 (plan/result), 4.2 (workspace), 5.2 (skill-pack-driven extras) |
| §15 Skills architecture | Tasks 5.1–5.3 |
| §16 Execution strategies | Single-backend 3.4 · Staged via Planner emitting Planner+Executor+Reviewer 3.2 · Parallel 6.1 · Fallback 6.2 |
| §17 Verification | Task 3.5 |
| §18 Safety/guardrails | Sandbox in 2.3, denial-with-reason in 1.8/7.1, redaction in 7.4 |
| §19 Observability | Existing `tracing`+`langsmith.rs` reused; transition log via 1.4; metrics counters added incrementally inside each milestone (counts of clarifications, retries, fallbacks) |
| §20 Configuration (global/per-repo/per-task) | Global+per-repo via `SupervisorConfig`/`RepoConfig` (Task 0.2 + extension in 7.1); per-task via `Task` fields populated by classifier 1.6 |
| §21 Backend categories (Reasoning/Coding/Shell/Research/Document/MCP) | Reasoning 2.2 · Shell 2.3 · MCP 2.4 (covers Research+Document) · Coding via Claude/Codex CLI 2.5 · Document also addressable via ReasoningBackend (`DocumentJob`) and MCP servers |
| §22 Default modes | Configured via `SupervisorConfig.default_autonomy_mode` (0.2) and per-task overrides at intake (1.5) |
| §23 State machine | Task 1.3 (transition table); persistence in 1.4 |
| §24 Milestones M1–M7 | M1=Tasks 1.x · M2=2.x · M3=3.x · M4=4.x · M5=5.x · M6=6.x · M7=7.x |
| §25 Definition of Done | Task DoD.1 (smoke per workflow) + DoD.2 (docs) |

If a spec bullet has no row above, treat it as a plan gap and add a task before
implementing.

## Self-Review Notes (for the executor)

A quick checklist to run after finishing each milestone:

1. **Spec coverage** — every numbered section in the design doc is referenced by at least one task.
2. **Type consistency** — `Task::id` is `String` everywhere, `Job::status` round-trips through serde, `Evidence` variants used in tests match the enum.
3. **Backend trait** — every concrete backend implements both `name()` *and* the capability flags consistent with where it appears in `BackendsConfig.fallbacks`.
4. **Migrations** — all four `sup_*` tables added in a single batch; no `ALTER TABLE` outside `IF NOT EXISTS`.
5. **No silent failure** — every error surfaces via `JobOutput.errors` or `record_transition(... Failed, reason)`, never via `?` swallowing the cause.
6. **Sandbox** — `ShellBackend`, `ScriptBackend`, and worktree paths are all rooted in either `config.sandbox.allowed_directory` or the configured repo path.
7. **DRY** — any classifier / policy / planner constants live in one place (e.g. capability strings `"reasoning"`, `"shell"` should be `pub const`s, not stringly-typed). If you notice duplication, refactor before committing.
8. **Frequent commits** — each task commits independently; no commit touches more than the files listed in its "Files:" section.
