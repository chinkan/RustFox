<p align="center">
  <img src="assets/logo.jpeg" alt="RustFox Logo" width="200"/>
</p>

# RustFox — Telegram AI Assistant

[![CI](https://github.com/chinkan/RustFox/actions/workflows/ci.yml/badge.svg)](https://github.com/chinkan/RustFox/actions)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Buy Me a Coffee](https://img.shields.io/badge/buy%20me%20a%20coffee-%E2%98%95-yellow)](https://buymeacoffee.com/chinkan.ai)

A self-hosted, agentic Telegram AI assistant written in Rust, powered by OpenRouter LLM (default: `moonshotai/kimi-k2.6`) with built-in sandboxed tools, scheduling, persistent memory, and MCP server integration.

## Features

- **Telegram Bot** — Responds only to configured user IDs
- **OpenRouter LLM** — Configurable model (default: `moonshotai/kimi-k2.6`)
- **Built-in Tools** — File read/write, directory listing, command execution (sandboxed)
- **Scheduling Tools** — Schedule, list, and cancel recurring or one-shot tasks
- **Persistent Memory** — SQLite-backed conversation history and knowledge base
- **Vector Embedding Search** — Hybrid vector + FTS5 search using `qwen/qwen3-embedding-8b`
- **MCP Integration** — Connect any MCP-compatible server to extend capabilities
- **Bot Skills** — Folder-based natural-language skill instructions auto-loaded at startup; orchestration and subagent skills (e.g. **daily-news-to-threads**) let the main agent delegate to specialized subagents and override models per task
- **Ad-Hoc Subagents** — `spawn_agents` tool lets the LLM spawn subagents with inline system prompts; run multiple subagents concurrently via `tokio::join_all`
- **Agents Layer** — Isolated agentic mini-loops in `agents/` with their own model, tool whitelist, and `AGENT.md` instructions; invoked via `invoke_agent`, with `read_agent_file`/`write_agent_file` for file I/O and `reload_agents` for hot-reloading
- **Zero-Trust Verifier** — Predefined verifier agent at `agents/verifier/AGENT.md` with read-only sandbox access; checks work output before the main agent finalizes; uses structured PASS/NEEDS_IMPROVEMENT/FAIL evaluation
- **Plan Tools** — `plan_create`, `plan_update`, `plan_view` built-in tools let the agent create and manage structured execution plans stored in the sandbox; power the `problem-solver` subagent skill
- **Bundled Subagent Skills** — `code-interpreter` (executes and iterates code snippets), `problem-solver` (orchestrates multi-step reasoning), and `verifier` (zero-trust output validation) ship out of the box
- **File & Image Support** — Photos and documents (PDF, DOCX, images) are processed via vision API or OCR (`ocrs` pure Rust OCR engine), then injected as multi-modal content or text into the conversation
- **Long-Context RAG** — Large document content is chunked, embedded, and retrieved via vector search per user query
- **Streaming Responses** — LLM tokens streamed progressively; Telegram message is live-edited as the response arrives
- **Chat History RAG** — Semantically relevant past messages are auto-injected into each turn's system prompt using vector search
- **RAG Query Rewriting** — Ambiguous follow-up questions are rewritten before vector search for more accurate retrieval
- **Nightly Summarization** — LLM-based cron job summarizes long conversations overnight to keep memory efficient
- **Long-Term Memory** — Conversations can be soft-archived (searchable but excluded from active context); startup and shutdown notifications
- **Verbose Tool UI** — `/verbose` command toggles a live Telegram status message showing tool calls as they run
- **Agentic Loop** — Automatic multi-step tool calling until task completion (max iterations configurable, default 25)
- **Per-user Conversations** — Independent conversation history per user
- **Persistent Home Directory** — All state under `~/.rustfox` (config, DB, skills, agents, workspace); override via `RUSTFOX_HOME` env or `[general].home` config
- **Autopilot Supervisor** — Generic autonomous task runner with classification, planning, multi-backend execution, verification, and approval gates; `/supervise` to submit tasks
- **LangSmith Tracing** — Optional observability via LangSmith for LLM calls, tool runs, and chain traces
- **Post-task Learning** — Auto-extracts reusable skill patterns from completed agentic loops; persists honcho-style user model
- **Skill/Agent Update Engine** — Content-hash diffing with lock files; `/update-skills` re-syncs bundled skills/agents with backup of local edits
- **Instance + Bundled Layering** — Skills and agents load from two directories (instance shadows bundled); bundled templates ship with the project

## Quick Start

### 1. Install

**Option A — Download a release (recommended)**

Download the latest archive from the [Releases page](https://github.com/chinkan/RustFox/releases) for your platform:

| Platform | Download |
|----------|----------|
| Linux x86_64 | `rustfox-<tag>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `rustfox-<tag>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon) | `rustfox-<tag>-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `rustfox-<tag>-x86_64-pc-windows-msvc.zip` |

Extract and run directly:

```bash
tar xzf rustfox-*.tar.gz
./rustfox --setup    # configure your bot
```

**Option B — Build from source**

```bash
cargo install --path . --locked
# or
cargo build --release
```

### 2. Configure

Run the setup wizard — it guides you through all required fields and writes `config.toml`:

```bash
# Browser-based wizard (opens http://localhost:8719)
rustfox --setup

# Terminal wizard (no browser required)
rustfox --setup --cli
```

The wizard will ask for your:
- Telegram bot token (from [@BotFather](https://t.me/BotFather))
- Allowed Telegram user IDs (from [@userinfobot](https://t.me/userinfobot))
- OpenRouter API key (from [openrouter.ai/keys](https://openrouter.ai/keys))
- Model, storage paths, and optional MCP tools

> **Manual setup:** Copy `config.example.toml` to `config.toml` and edit it directly if you prefer.

### 3. Run

```bash
rustfox
# or with a custom config path:
rustfox --config /path/to/config.toml
```

### 4. (Optional) Run as a background service

After configuring, set RustFox to run automatically in the background:

```bash
# Linux (systemd user service — no sudo needed)
rustfox --service install

# macOS (launchd agent)
rustfox --service install

# Windows (Windows Service)
rustfox --service install
```

Then check status with:

```bash
rustfox --service status
```

## Configuration

> **Persistent home:** RustFox keeps all state under `~/.rustfox` by default
> (config, database, skills, agents, and a durable `workspace/` sandbox).
> Override with the `RUSTFOX_HOME` environment variable or `[general].home`.
> See [docs/persistent-home-directory.md](docs/persistent-home-directory.md).

See [`config.example.toml`](config.example.toml) for all options.

### Key Settings

| Setting | Description |
|---------|-------------|
| `telegram.bot_token` | Telegram Bot API token |
| `telegram.allowed_user_ids` | List of user IDs allowed to use the bot |
| `openrouter.api_key` | OpenRouter API key |
| `openrouter.model` | LLM model ID (default: `moonshotai/kimi-k2.6`) |
| `sandbox.allowed_directory` | Directory for file/command operations |
| `memory.database_path` | SQLite DB path (default: `<home>/rustfox.db`) |
| `memory.user_model_path` | User model file path (default: `<home>/user_model.md`) |
| `memory.query_rewriter_enabled` | Whether RAG query rewriting is on by default |
| `embedding` (optional) | Vector search API config (default model: `qwen/qwen3-embedding-8b`) |
| `skills.directory` | Instance (writable) skill files (default: `<home>/skills/`) |
| `skills.bundled_directory` | Bundled (read-only) skill templates (default: `./skills/`) |
| `agents.directory` | Instance (writable) agent files (default: `<home>/agents/`) |
| `agents.bundled_directory` | Bundled (read-only) agent templates (default: `./agents/`) |
| `mcp_servers` | List of MCP servers to connect |
| `general.home` | Absolute path overriding `~/.rustfox` home root |
| `general.location` | Your location string (under `[general]`), injected into system prompt |
| `agent.max_iterations` | Max agentic loop iterations (default: 25) |
| `langsmith.api_key` | LangSmith API key for LLM observability |
| `learning.skill_extraction_enabled` | Post-task skill extraction on/off |
| `supervisor.default_autonomy_mode` | Supervisor workflow mode: `fast`, `standard`, `rigorous` |

### MCP Server Configuration

RustFox supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) — an open standard for connecting AI assistants to external tools and data sources. Any MCP-compatible server can be plugged in via `config.toml`.

#### Prerequisites

MCP servers are usually distributed as Python packages (run via `uvx`) or npm packages (run via `npx`).

| Runtime | Install |
|---------|---------|
| `uvx` (Python) | [Install uv](https://docs.astral.sh/uv/getting-started/installation/) — `curl -LsSf https://astral.sh/uv/install.sh \| sh` |
| `npx` (Node.js) | [Install Node.js](https://nodejs.org/) — comes bundled with npm/npx |

#### Config Syntax

Add one `[[mcp_servers]]` block per server in `config.toml`:

```toml
# Stdio-based server
[[mcp_servers]]
name   = "server-name"   # used to namespace tools: mcp_<name>_<tool>
command = "uvx"          # or "npx", or any executable on PATH
args   = ["package-name", "optional-arg"]

# Optional: pass environment variables to the server process
# [mcp_servers.env]
# API_KEY = "your-key-here"

# HTTP/Streamable HTTP server (omit command for this transport)
# [[mcp_servers]]
# name       = "api-server"
# url        = "https://api.example.com/mcp"
# auth_token = "bearer-token-here"

# OAuth 2.0 refresh flow (auto-exchanges refresh_token for new auth_token)
#   token_endpoint  = "https://api.example.com/oauth/token"
#   refresh_token   = "your-refresh-token"
#   token_expires_at = <unix-timestamp>
```

#### Popular MCP Servers

| Server | Package | Runtime | Notes |
|--------|---------|---------|-------|
| [Git](https://github.com/modelcontextprotocol/servers/tree/main/src/git) | `mcp-server-git` | `uvx` | Read/search git repos |
| [Filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) | `@modelcontextprotocol/server-filesystem` | `npx` | File access outside the sandbox |
| [Brave Search](https://github.com/brave/brave-search-mcp-server) | `@brave/brave-search-mcp-server` | `npx` | Web search (needs [Brave API key](https://brave.com/search/api/)) |
| [GitHub](https://github.com/modelcontextprotocol/servers/tree/main/src/github) | `@modelcontextprotocol/server-github` | `npx` | Issues, PRs, repos |
| [Fetch](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch) | `mcp-server-fetch` | `uvx` | HTTP fetch / web scraping |
| [SQLite](https://github.com/modelcontextprotocol/servers/tree/main/src/sqlite) | `mcp-server-sqlite` | `uvx` | Query local SQLite databases |
| [Puppeteer](https://github.com/modelcontextprotocol/servers/tree/main/src/puppeteer) | `@modelcontextprotocol/server-puppeteer` | `npx` | Browser automation |
| [Threads](https://github.com/baguskto/threads-mcp) | `threads-mcp-server` | `npx` | Publish/manage Meta Threads posts (needs access token) |

> Find more servers at the [MCP server registry](https://github.com/modelcontextprotocol/servers) and [mcp.so](https://mcp.so/).

#### Examples

```toml
# Git — inspect repositories
[[mcp_servers]]
name    = "git"
command = "uvx"
args    = ["mcp-server-git"]

# Filesystem — expose an extra directory to the bot
[[mcp_servers]]
name    = "filesystem"
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]

# Brave Search — web search (requires API key)
[[mcp_servers]]
name    = "brave-search"
command = "npx"
args    = ["-y", "@brave/brave-search-mcp-server"]
[mcp_servers.env]
BRAVE_API_KEY = "your-brave-api-key"

# Meta Threads — publish posts and read replies (requires long-lived access token)
# Token setup: Facebook Developers → Create App → add Threads API product →
#   request threads_basic / threads_content_publish / threads_manage_replies /
#   threads_read_replies → generate token under Threads API → Access Tokens
[[mcp_servers]]
name    = "threads"
command = "npx"
args    = ["-y", "threads-mcp-server"]
[mcp_servers.env]
THREADS_ACCESS_TOKEN = "your-long-lived-access-token"
```

#### Tool Naming

Tools from MCP servers are automatically namespaced as `mcp_<server-name>_<tool-name>` (e.g. `mcp_git_git_log`). Run `/tools` in the bot to see all registered tools after startup.

## Built-in Tools

### Core Tools

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents within sandbox |
| `write_file` | Write/create files within sandbox |
| `list_files` | List directory contents within sandbox |
| `execute_command` | Run shell commands within sandbox directory |

### Scheduling Tools

| Tool | Description |
|------|-------------|
| `schedule_task` | Schedule a recurring (cron) or one-shot task with a message |
| `list_scheduled_tasks` | List all active scheduled tasks |
| `cancel_scheduled_task` | Cancel a scheduled task by ID |

### Skill Tools

| Tool | Description |
|------|-------------|
| `read_skill_file` | Read a file from a skill's directory (loads skill instructions) |
| `write_skill_file` | Write new or update existing skill files |
| `reload_skills` | Hot-reload the skill registry without restarting the bot |

### Agent Tools

| Tool | Description |
|------|-------------|
| `spawn_agents` | Spawn one or more ad-hoc subagents with inline system prompts (supports parallel batch via `tasks` array) |
| `invoke_agent` | Run a predefined agent from the `agents/` directory in an isolated agentic loop |
| `read_agent_file` | Read a file from within an agent's directory |
| `write_agent_file` | Write a file into an agent's directory |
| `reload_agents` | Hot-reload the agent registry without restarting the bot |
| `reload_skills_and_agents` | Reload both registries in one call |

### Plan Tools

| Tool | Description |
|------|-------------|
| `plan_create` | Create a new structured execution plan (stored as `.rustfox_plan.json` in the sandbox) |
| `plan_update` | Update a step's status or notes in the current plan |
| `plan_view` | View the current plan and its step statuses |

## Bot Commands

| Command | Description |
|---------|-------------|
| `/start` | Show welcome message with command list |
| `/clear` | Clear conversation history |
| `/tools` | List all available tools |
| `/skills` | List all loaded skills |
| `/verbose` | Toggle live tool call progress display |
| `/query-rewrite` | Toggle RAG query rewriting for memory search |
| `/update-skills` | Re-sync bundled skills/agents (backs up local edits) |
| `/supervise <text>` | Submit a new supervisor task |
| `/tasks` | List active / recent supervisor tasks |
| `/resume <id>` | Resume a paused supervisor task |
| `/cancel <id>` | Cancel a supervisor task |
| `/approve <id>` | Approve a task that requires approval |
| `/clarify <id> <text>` | Reply to a clarification prompt |

## Architecture

```
src/
├── main.rs           # Entry point, config loading, initialization
├── config.rs         # TOML configuration parsing (Telegram, OpenRouter, sandbox, memory, skills, agents, langsmith, learning, supervisor)
├── home.rs           # Persistent home directory resolution (~/.rustfox)
├── llm.rs            # OpenRouter API client with tool calling
├── agent.rs          # Agentic loop, tool dispatch, scheduling tools; skills/agents/ layer
├── agent_prompt.rs   # Prompt preparation, compaction, recovery nudges, message assembly
├── tools.rs          # Built-in tools (file I/O, command execution, plan tools)
├── file_processor/   # File/attachment processing (image OCR/vision, PDF, DOCX)
├── mcp.rs            # MCP client manager for external tool servers
├── memory/           # SQLite persistence, vector embeddings, RAG, query rewriter, summarizer
├── scheduler/        # Cron/one-shot task scheduler with DB persistence
├── skills/           # Skill loader, registry, seeding, update engine (loader.rs, mod.rs, seed.rs, update.rs)
├── learning.rs       # Post-task skill extraction, user model persistence
├── langsmith.rs      # Optional LangSmith observability client
├── supervisor/       # Autopilot v2 generic autonomous task runner
│   ├── mod.rs        # Supervisor facade (submit, execute_now, pause, resume, state, artifacts)
│   ├── task.rs       # Task, TaskType, RiskLevel, ExecutionMode, TaskStatus enums
│   ├── job.rs        # Job, JobType, JobStatus, JobOutput, Evidence
│   ├── state.rs      # Transition-allowed state machine
│   ├── store.rs      # CRUD over sup_tasks / sup_jobs / sup_transitions
│   ├── intake.rs     # IntakeRouter — raw text → Task normalization
│   ├── classifier.rs # Heuristic / LLM-backed / Skill-aware classifiers
│   ├── policy.rs     # PolicyEngine — auto-execute, clarify, require approval
│   ├── planner.rs    # Task → Plan with jobs and parallel groups
│   ├── workflow.rs   # Fast / Standard / Rigorous workflow templates
│   ├── orchestrator.rs  # Plan executor with fallback + parallel + subjobs
│   ├── verification.rs  # Evidence-gated verification engine
│   ├── artifact.rs   # ArtifactManager with secret redaction
│   ├── workspace.rs  # Per-task git worktree management
│   ├── reporter.rs   # Human-readable job summary
│   ├── redact.rs     # Secret scrubber for api_key / password / token / bearer
│   └── backend/      # Backend implementations (reasoning, shell, MCP, claude-code, codex, script)
├── platform/         # Telegram bot handler (telegram.rs + tool_notifier.rs)
├── setup/            # Setup wizard module (mod.rs, wizard.rs, service.rs)
└── utils/            # String utilities, markdown-to-entities conversion

skills/               # Bundled skills (15+): code-interpreter, problem-solver, coding-assistant,
│                     #   soul, soul-keeper, memory-manager, creating-skills, creating-agents,
│                     #   news-fetcher, codebase-gap-analysis, sup-* workflow skills
agents/               # Agent definition files (AGENT.md per agent)
└── verifier/         #   Zero-trust verifier (read-only sandbox, structured evaluation)
setup/                # Setup wizard HTML
```

## Roadmap

### Done

- [x] Telegram bot with user allowlist
- [x] OpenRouter LLM integration with tool calling (agentic loop)
- [x] Built-in sandboxed tools (file read/write, directory listing, command execution)
- [x] MCP server integration for extensible tooling
- [x] Per-user conversation history
- [x] Persistent memory with SQLite
- [x] Vector embedding search (`qwen/qwen3-embedding-8b`)
- [x] Scheduling tools (`schedule_task`, `list_scheduled_tasks`, `cancel_scheduled_task`)
- [x] Bot skills (folder-based, auto-loaded at startup)
- [x] Setup wizard (web UI + CLI) for guided `config.toml` creation
- [x] Agent skill writer (`write_skill_file` tool — creates/updates skill files from within the agent)
- [x] Agent skill reload (`reload_skills` tool — hot-reloads skill registry without restart)
- [x] Meta Threads MCP integration (setup wizard entry, config example, token setup guide)
- [x] Agents layer (`invoke_agent`, `read_agent_file`, `write_agent_file`, `reload_agents` — isolated agentic mini-loops in `agents/` with own model and tool whitelist)
- [x] Plan tools (`plan_create`, `plan_update`, `plan_view` — structured execution plans in the sandbox)
- [x] Bundled subagent skills: `code-interpreter` and `problem-solver`
- [x] LLM streaming (SSE token-by-token, live Telegram message edits)
- [x] Chat history RAG (auto-inject relevant past context per turn)
- [x] RAG query rewriting (disambiguates follow-up questions before vector search)
- [x] Nightly conversation summarization (LLM-based cron job)
- [x] Verbose tool UI (`/verbose` command — live tool call progress in Telegram)
- [x] Google integration tools (Calendar, Email, Drive)
- [x] Persistent home directory (`~/.rustfox` with env/config override)
- [x] Autopilot v2 supervisor (classification, planning, multi-backend execution, verification)
- [x] LangSmith observability (LLM/tool/chain tracing)
- [x] Post-task skill extraction (auto-learns reusable patterns)
- [x] User model persistence (honcho-style `user_model.md`)
- [x] Skill/agent content hash engine + lock-file re-sync
- [x] Instance + bundled skills/agents layering
- [x] File & image upload support (vision API + OCR + document extraction)
- [x] Long-term memory (soft archive, startup/shutdown notifications)
- [x] Ad-hoc parallel subagents (`spawn_agents` tool)
- [x] Zero-trust verifier (read-only verification agent)
- [x] Context compaction improvements (hard cap, image preservation, retry optimization)
- [x] Multi-platform service setup (`--setup` web/CLI wizard, `--service install/remove/status`)
- [x] Build scripts & CI release workflow (`.tar.gz`, `.zip`, `.deb` per release)

### Planned

- [ ] Event trigger framework (e.g., on email receive)
- [ ] WhatsApp support
- [ ] Webhook mode (in addition to polling)
- [ ] And more…

## Contributing

This project is open source under the [MIT License](LICENSE). Contributions are very welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for how to open issues and submit pull requests.

## Support

If you find RustFox useful, consider supporting the project:

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-%E2%98%95-yellow?style=for-the-badge&logo=buy-me-a-coffee)](https://buymeacoffee.com/chinkan.ai)

[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-pink?style=for-the-badge&logo=github)](https://github.com/sponsors/chinkan)

## Dependencies

- [teloxide](https://github.com/teloxide/teloxide) — Telegram bot framework
- [rmcp](https://github.com/modelcontextprotocol/rust-sdk) — Official MCP Rust SDK
- [reqwest](https://github.com/seanmonstar/reqwest) — HTTP client for OpenRouter
- [tokio](https://tokio.rs/) — Async runtime
- [tokio-cron-scheduler](https://github.com/mvniekerk/tokio-cron-scheduler) — Task scheduling
- [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) — Markdown parser (entity-based Telegram formatting)
- [rusqlite](https://github.com/rusqlite/rusqlite) — SQLite with FTS5 and `sqlite-vec`
- [axum](https://github.com/tokio-rs/axum) — Web server for the setup wizard
- [dirs](https://github.com/soc/dirs-rs) — OS home directory resolution
- [sha2](https://github.com/RustCrypto/hashes) — SHA-256 hashing for skill/agent update engine
- [regex](https://github.com/rust-lang/regex) — Secret redaction in supervisor artifacts

> **Thanks:** Markdown-to-entities conversion approach inspired by [telegramify-markdown](https://github.com/sudoskys/telegramify-markdown) by sudoskys.
