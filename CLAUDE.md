# CLAUDE.md - RustFox Development Guide

## Project Overview

RustFox is a Telegram AI assistant written in Rust. It connects to Telegram as a bot, uses OpenRouter LLM for inference (default model: `qwen/qwen3-235b-a22b`), provides built-in sandboxed tools (file I/O, command execution), and supports MCP (Model Context Protocol) servers for extensible tool integration. It implements an agentic loop that iterates tool calls until a final text response is produced (max iterations configurable, default 25).

## Build & Run

```bash
# Build (debug)
cargo build

# Build (release)
cargo build --release

# Run (uses ./config.toml by default)
cargo run

# Run with custom config path
cargo run -- /path/to/config.toml

# Check without building
cargo check

# Format code
cargo fmt

# Lint
cargo clippy
```

### Configuration

Copy `config.example.toml` to `config.toml` and fill in credentials. The `config.toml` file is gitignored and must never be committed. Required fields:

- `telegram.bot_token` - Telegram Bot API token
- `telegram.allowed_user_ids` - Whitelist of Telegram user IDs
- `openrouter.api_key` - OpenRouter API key
- `sandbox.allowed_directory` - Directory for sandboxed file/command operations

### Home directory

RustFox stores all state under a single home directory (default `~/.rustfox`),
resolved as: `RUSTFOX_HOME` env (absolute) → `[general].home` config → `~/.rustfox`.
Layout: `config.toml`, `rustfox.db`, `skills/`, `agents/`, `workspace/` (the
sandbox), `artifacts/`, `user_model.md`. Each path can be pinned to an absolute
location in `config.toml`; unset paths fall back to the home default. Run
isolated instances with `RUSTFOX_HOME=...`. See
`docs/persistent-home-directory.md`. Path resolution lives in `src/home.rs`
(`Config::resolve` writes the resolved absolute paths back into the config).
Bundled skills/agents are seed-copied on first run; `/update-skills` re-syncs
them using `<home>/skills-lock.json`.

## Architecture

```
src/
├── main.rs      # Entry point: logging init, config loading, MCP setup, bot launch
├── config.rs    # TOML config parsing (Config, TelegramConfig, OpenRouterConfig, SandboxConfig, McpServerConfig)
├── llm.rs       # OpenRouter API client (ChatMessage, ToolCall, ToolDefinition, LlmClient)
├── tools.rs     # Built-in tool definitions and execution with sandbox path validation
├── mcp.rs       # MCP client manager (McpManager, McpConnection) for external tool servers
└── bot.rs       # Telegram bot handler: message routing, agentic loop, conversation state
```

### Data Flow

1. User sends a Telegram message
2. `bot.rs` filters by `allowed_user_ids`, routes commands (`/start`, `/clear`, `/tools`)
3. Non-command messages enter `process_with_llm()` which runs the agentic loop
4. `llm.rs` sends conversation history + tool definitions to OpenRouter
5. If LLM returns tool calls, `execute_tool()` dispatches to built-in tools or MCP tools
6. Tool results are appended to conversation and the loop repeats (up to 10 iterations)
7. Final text response is split into <=4000 char chunks and sent back via Telegram

### Key Components

- **AppState** (`bot.rs`): Shared state holding `LlmClient`, `Config`, `McpManager`, and per-user `Conversation` map behind a `Mutex`
- **LlmClient** (`llm.rs`): Stateless HTTP client for OpenRouter's `/chat/completions` endpoint with tool-calling support
- **McpManager** (`mcp.rs`): Manages stdio-based MCP server child processes. Tools are namespaced as `mcp_{server_name}_{tool_name}`
- **Sandbox validation** (`tools.rs`): All file/command operations are restricted to the configured sandbox directory via path canonicalization

## Code Conventions

### Rust Patterns

- **Edition**: 2021
- **Async runtime**: Tokio with `full` features
- **Error handling**: `anyhow::Result` throughout, with `.context()` / `.with_context()` for error messages
- **Logging**: `tracing` crate with `tracing-subscriber` (env filter: `RUST_LOG`, default `info,rustfox=debug`)
- **Serialization**: `serde` derive macros with `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields
- **Shared state**: `Arc<AppState>` passed via teloxide's dependency injection (`dptree::deps!`)
- **Concurrency**: `tokio::sync::Mutex` for per-user conversation map (not `std::sync::Mutex`)

### Naming

- Module names are single words (`bot`, `config`, `llm`, `mcp`, `tools`)
- Struct fields use `snake_case`
- JSON field renames use `#[serde(rename = "type")]` where the Rust field name differs from the API field

### Error Handling Style

- Use `anyhow::bail!()` for early returns with error messages
- Use `.context("message")` on `Result` chains for context propagation
- MCP connection failures are logged but do not abort startup (`connect_all` catches errors)
- Tool execution errors return error strings to the LLM rather than crashing

### Security

- All file and command operations go through `validate_sandbox_path()` which canonicalizes both the sandbox root and the requested path, then verifies the requested path starts with the sandbox root
- The bot only responds to user IDs in `allowed_user_ids`
- `config.toml` (containing secrets) is gitignored

## Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `teloxide` | Telegram bot framework |
| `reqwest` | HTTP client for OpenRouter API |
| `serde` / `serde_json` | Serialization |
| `toml` | Config file parsing |
| `rmcp` | Official MCP Rust SDK (stdio transport) |
| `tracing` / `tracing-subscriber` | Structured logging |
| `anyhow` | Error handling |
| `futures` | Async utilities |

## CI (GitHub Actions)

CI runs on every push to `main` and on pull requests targeting `main`. The pipeline is defined in `.github/workflows/ci.yml` and runs five parallel jobs:

| Job | Command | Purpose |
|-----|---------|---------|
| **Check** | `cargo check` | Fast compilation check |
| **Format** | `cargo fmt --all -- --check` | Enforces consistent formatting |
| **Clippy** | `cargo clippy -- -D warnings` | Lint — all warnings are errors |
| **Test** | `cargo test` | Runs all unit and integration tests |
| **Build** | `cargo build --release` | Release build (runs after all other jobs pass) |

All jobs use `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2` for caching. Before opening a PR, ensure `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` pass locally.

## Testing

No automated tests exist yet. When adding tests:

- Place unit tests in `#[cfg(test)] mod tests` blocks within each source file
- Integration tests go in a top-level `tests/` directory
- The sandbox path validation logic in `tools.rs` and message splitting in `bot.rs` are good candidates for unit tests

## Common Tasks

### Adding a new built-in tool

1. Add a `ToolDefinition` entry in `builtin_tool_definitions()` in `src/tools.rs`
2. Add a match arm in `execute_builtin_tool()` in `src/tools.rs`
3. Use `validate_sandbox_path()` if the tool accesses the filesystem

### Adding a new bot command

1. Add a new `if text == "/command"` block in `handle_message()` in `src/bot.rs` (before the LLM processing section)

### Changing the default LLM model

Update `default_model()` in `src/config.rs`. Users can also override this in their `config.toml`.

### Adding a new MCP server

Add a `[[mcp_servers]]` block to `config.toml` with `name`, `command`, `args`, and optional `env` fields. See `config.example.toml` for examples.

### Adding a new bot skill

Bot skills are natural-language instructions loaded at startup and injected into the LLM's system prompt. Each skill must be in its own folder following the Claude agent skills format:

```
skills/
  skill-name/
    SKILL.md           # Required: YAML frontmatter + instruction body
    supporting-file.*  # Optional: templates, examples, reference docs
```

**SKILL.md frontmatter:**
```yaml
---
name: skill-name       # lowercase letters, numbers, hyphens only
description: Brief description of what this skill does
tags: [tag1, tag2]     # optional: for organization
---
```

1. Create `skills/<skill-name>/SKILL.md` with frontmatter and instruction body
2. The skill is auto-loaded at startup — no code changes needed
3. Configure the skills directory in `config.toml`: `[skills] directory = "skills"`

All skills are represented in the system prompt by **metadata only** (name + description). **Instruction skills** (no `model` in frontmatter) have their full content loaded by the agent via `read_skill_file(skill_name="...", relative_path="SKILL.md")` when relevant. **Subagent skills** (`model` set) are invoked via `invoke_agent(agent="name", prompt="...")`. The orchestration skill teaches the agent when to call which subagent and when to override the model (e.g. `model="anthropic/claude-sonnet-4-6"` for thread-writer-hk).

**Subagent tool whitelist:** For subagent skills, the frontmatter `tools:` list must use the **exact** tool names as seen by the agent. MCP tools are named `mcp_{server_name}_{tool_name}` (e.g. `mcp_google-workspace_query_gmail_emails`). These names are logged at startup when MCP servers connect (`MCP server 'X' provides N tools`). A mismatch (e.g. declaring `search_gmail_messages` when the server exposes `query_gmail_emails`) causes the subagent to have no access to that tool.

**Daily News to Threads flow:** The `daily-news-to-threads` orchestration skill (instruction) directs the main agent to: (1) call the `news-fetcher` subagent (default model) to get AI news from Gmail Google 快訊, (2) call the `thread-writer-hk` subagent with model override to write a HK-style Threads thread with verified links, (3) post the thread via Threads MCP and report success. Requires Gmail (google-workspace), fetch, and threads MCP servers in config.

## Files Not to Commit

- `config.toml` - Contains API keys and tokens
- `.env` - Environment variables
- `/target/` - Build artifacts

## Supervisor (Autopilot v2)

The supervisor is a generic autonomous task runner that lives alongside the
existing chat agent. It accepts a free-form request, classifies it, picks a
plan, dispatches work to one or more **backends** (reasoning, shell, MCP,
Claude Code CLI, Codex CLI, scripts), verifies the result, and persists
artifacts + audit transitions to SQLite.

### Module tree (`src/supervisor/`)

```
src/supervisor/
 mod.rs              — Supervisor facade: submit / execute_now / pause / resume / state / artifacts
 task.rs             — Task, TaskType, RiskLevel, ExecutionMode, TaskStatus enums
 job.rs              — Job, JobType, JobStatus, JobOutput, Evidence
 state.rs            — transition_allowed() — single source of truth for the state machine
 store.rs            — TaskStore: CRUD over sup_tasks / sup_jobs / sup_transitions
 intake.rs           — IntakeRouter::normalize() → Task from raw text
 classifier.rs       — Classifier trait + HeuristicClassifier / LlmBackedClassifier / SkillAwareClassifier
 policy.rs           — PolicyEngine: AutoExecute | Clarify | RequireApproval | UseFallbackBackend | StopAndReport
 planner.rs          — Planner: Task → Plan { jobs, parallel_groups }
 workflow.rs         — Fast / Standard / Rigorous workflow stage templates
 orchestrator.rs     — Orchestrator: executes Plan with fallback + parallel groups + subjob spawning
 verification.rs     — VerificationEngine: ≥1 evidence per job gate
 artifact.rs         — ArtifactManager: write_text() (redacts) + list()
 workspace.rs        — WorkspaceManager: per-task git branch / optional worktree
 reporter.rs         — Human-readable per-job summary
 redact.rs           — Secret scrubber for api_key / password / secret / token / bearer values
 backend/
  mod.rs            — Backend trait + BackendCapabilities + Registry + RunContext
  reasoning.rs      — Wraps the chat Agent
  shell.rs          — Sandboxed shell commands
  mcp.rs            — Calls tools on a connected MCP server
  claude_code.rs    — Spawns the `claude` CLI as a backend
  codex.rs          — Spawns the `codex` CLI as a backend
  script.rs         — Runs a script file from the sandbox
```

### Lifecycle

```
INTAKE → CLASSIFY → ROUTE
              ↓
       (CLARIFY) | (PREPARE_WORKSPACE)? → PLAN → EXECUTE
              ↓                                    ↓
              (Paused ⇄ Execute)         REVIEW (rigorous mode)
                                                   ↓
                                              VERIFY
                                                   ↓
                              REPORT → ARCHIVE → DONE
                                  ↘ Failed   ↘ Cancelled
```

`state.rs::transition_allowed(from, to)` enumerates every legal edge. Add a
new arm there before introducing a new state — the rest of the supervisor
treats unknown transitions as bugs.

### Backend trait + adding a new backend

Every backend implements `Backend` from `src/supervisor/backend/mod.rs`. The
defaults from spec §10 (`prepare`, `collect_result`, `verify_result`,
`cancel`, `resume`) are already provided; most backends only override
`name`, `capabilities`, `can_handle`, and `run`. Register an `Arc<MyBackend>`
into the `Registry` at startup.

```rust
struct EchoBackend;
#[async_trait::async_trait]
impl rustfox::supervisor::backend::Backend for EchoBackend {
    fn name(&self) -> &str { "echo" }
    fn capabilities(&self) -> rustfox::supervisor::backend::BackendCapabilities {
        rustfox::supervisor::backend::BackendCapabilities { reasoning: true, ..Default::default() }
    }
    fn can_handle(&self, _: &rustfox::supervisor::job::JobType) -> bool { true }
    async fn run(&self, job: &mut rustfox::supervisor::job::Job, _: &rustfox::supervisor::backend::RunContext)
        -> anyhow::Result<rustfox::supervisor::job::JobOutput> { /* ... */ todo!() }
}
let mut reg = rustfox::supervisor::backend::Registry::new();
reg.register(std::sync::Arc::new(EchoBackend));
```

### Adding a workflow skill pack

Drop a `skills/sup-<name>/SKILL.md` with frontmatter:

```yaml
---
name: sup-<name>
description: One-line summary
supervisor:
  workflow: research          # or: writing | refactor | research | ops | review
  required_capabilities: [research, reasoning]
---
```

Skill packs are auto-loaded by the existing `SkillRegistry` at startup; the
`SkillAwareClassifier` consults them and overrides the default
`required_capabilities` when the request keyword matches the skill name
(prefix `sup-` is stripped before matching).

### TOML config keys

```toml
[supervisor]
default_autonomy_mode = "standard"   # "fast" | "standard" | "rigorous"
artifacts_dir         = "supervisor/artifacts"

[supervisor.risk]
require_approval_for_low    = false
require_approval_for_medium = false
auto_execute_only_low       = false   # when true, Medium escalates to RequireApproval
```

Defaults preserve M1–M6 behavior (Medium-risk auto-executes). Flip individual
fields to tighten the gate.

### Bot commands

| Command | Behaviour |
|---------|-----------|
| `/supervise <text>` | Submit a new supervisor task |
| `/tasks`            | List active / recent tasks |
| `/resume <id>`      | Resume a paused task |
| `/cancel <id>`      | Cancel a task |
| `/approve <id>`     | Approve a task that hit `RequireApproval` |
| `/clarify <id> <text>` | Reply to a `Clarify` prompt |

The command **parser** is wired and emits a startup log line in `main.rs`;
routing user commands into supervisor handlers in the live Telegram dispatcher
is a minimum-viable integration (M3.8 / M7.3) and the full handler surface is
a follow-up task.

### Artifacts

Per-task artifacts are written to `<supervisor.artifacts_dir>/<task_id>/<filename>`
and indexed in `sup_artifacts` (`kind`, `path`, `sha256`, `bytes`). Every
artifact write goes through `redact::redact()`, which scrubs values that
follow `api_key`, `password`, `secret`, `token`, or `bearer` (case-insensitive)
and replaces them with `***` while preserving the key + separator so the
file stays human-readable. Standard kinds emitted by the pipeline: `intake`,
`classification`, `policy`, `plan`, `workspace` (when workspace prepared),
and `result` (Reporter Markdown summary).

### Database tables added

| Table | Purpose |
|-------|---------|
| `sup_tasks`       | One row per submitted task — title, user_request, classification (`task_type` / `risk_level` / `execution_mode`), current `state`, platform / user / chat origin |
| `sup_jobs`        | One row per job dispatched within a task — backend, goal, prompt, status, result_summary, error, optional `parent_job_id` for spawned subjobs |
| `sup_transitions` | Append-only audit log of every state change (`from_state`, `to_state`, `actor`, `reason`, `occurred_at`) |
| `sup_artifacts`   | Index of files written under `artifacts_dir` (`task_id`, `job_id`, `kind`, `path`, `sha256`, `bytes`) |

All four tables are created idempotently in `MemoryStore` at startup.

## Agent skills

### Issue tracker

GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-role vocabulary. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context. See `docs/agents/domain.md`.
