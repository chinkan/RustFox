# RustFox Guide

- [Configuration](#configuration)
- [MCP Server Integration](#mcp-server-integration)
- [Built-in Tools](#built-in-tools)
- [Bot Commands](#bot-commands)
- [Skills & Agents System](#skills--agents-system)
- [Advanced Features](#advanced-features)
- [Roadmap](#roadmap)
- [Dependencies](#dependencies)

---

## Configuration

RustFox reads `config.toml` on startup. Copy [`config.example.toml`](../config.example.toml) to get started, or use `rustfox --setup` for the guided wizard.

### All Settings

| Section | Setting | Description | Default |
|---------|---------|-------------|---------|
| `[telegram]` | `bot_token` | Telegram Bot API token | — |
| | `allowed_user_ids` | Comma-separated whitelist of user IDs | — |
| `[openrouter]` | `api_key` | OpenRouter API key | — |
| | `model` | LLM model ID | `moonshotai/kimi-k2.6` |
| | `base_url` | API base URL override | `https://openrouter.ai/api/v1` |
| `[sandbox]` | `allowed_directory` | Directory for sandboxed file/command ops | `<home>/workspace` |
| `[memory]` | `database_path` | SQLite database path | `<home>/rustfox.db` |
| | `user_model_path` | User model file path | `<home>/user_model.md` |
| | `query_rewriter_enabled` | Enable RAG query rewriting | `false` |
| `[embedding]` | `model` | Embedding model for vector search | `qwen/qwen3-embedding-8b` |
| | `dimensions` | Vector dimensions | — |
| | `base_url` | Embedding API base URL | — |
| | `api_key` | Embedding API key | — |
| `[ocr]` | `enabled` | Enable OCR for image processing | `true` |
| `[skills]` | `directory` | Instance skill files directory | `<home>/skills/` |
| `[agents]` | `directory` | Instance agent files directory | `<home>/agents/` |
| `[subagents]` | `default_tools` | Default tool list for subagents | — |
| `[[mcp_servers]]` | *(see below)* | MCP server definitions | — |
| `[general]` | `home` | Absolute path overriding `~/.rustfox` | — |
| | `location` | Your location (injected into system prompt) | — |
| `[agent]` | `max_iterations` | Max agentic loop iterations | `25` |
| `[langsmith]` | `api_key` | LangSmith API key for LLM observability | — |
| `[learning]` | `skill_extraction_enabled` | Post-task skill extraction | `false` |
| `[supervisor]` | `default_autonomy_mode` | Workflow mode: `fast`, `standard`, `rigorous` | `standard` |

> Persistent home: All paths resolve relative to `~/.rustfox` by default.
> Override with `RUSTFOX_HOME` env or `[general].home`.
> See [docs/persistent-home-directory.md](persistent-home-directory.md).

---

## MCP Server Integration

RustFox supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) — an open standard for connecting AI assistants to external tools and data sources.

### Prerequisites

| Runtime | Install |
|---------|---------|
| `uvx` (Python) | [Install uv](https://docs.astral.sh/uv/getting-started/installation/) |
| `npx` (Node.js) | [Install Node.js](https://nodejs.org/) |

### Config Syntax

```toml
# Stdio transport
[[mcp_servers]]
name    = "server-name"
command = "uvx"           # or "npx", or any executable on PATH
args    = ["package-name"]

# Optional: pass environment variables
[mcp_servers.env]
API_KEY = "your-key-here"

# HTTP transport (omit command)
# [[mcp_servers]]
# name = "api-server"
# url  = "https://api.example.com/mcp"

# OAuth 2.0 refresh flow
#   token_endpoint   = "https://api.example.com/oauth/token"
#   refresh_token    = "your-refresh-token"
#   token_expires_at = <unix-timestamp>
```

### Popular MCP Servers

| Server | Package | Runtime | Notes |
|--------|---------|---------|-------|
| [Git](https://github.com/modelcontextprotocol/servers/tree/main/src/git) | `mcp-server-git` | `uvx` | Read/search git repos |
| [Filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) | `@modelcontextprotocol/server-filesystem` | `npx` | File access outside sandbox |
| [Brave Search](https://github.com/brave/brave-search-mcp-server) | `@brave/brave-search-mcp-server` | `npx` | Web search (needs [API key](https://brave.com/search/api/)) |
| [GitHub](https://github.com/modelcontextprotocol/servers/tree/main/src/github) | `@modelcontextprotocol/server-github` | `npx` | Issues, PRs, repos |
| [Fetch](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch) | `mcp-server-fetch` | `uvx` | HTTP fetch / web scraping |
| [SQLite](https://github.com/modelcontextprotocol/servers/tree/main/src/sqlite) | `mcp-server-sqlite` | `uvx` | Query local SQLite databases |
| [Puppeteer](https://github.com/modelcontextprotocol/servers/tree/main/src/puppeteer) | `@modelcontextprotocol/server-puppeteer` | `npx` | Browser automation |
| [Threads](https://github.com/baguskto/threads-mcp) | `threads-mcp-server` | `npx` | Publish/manage Meta Threads posts |

> Find more at [modelcontextprotocol/servers](https://github.com/modelcontextprotocol/servers) and [mcp.so](https://mcp.so/).

### Examples

```toml
# Git
[[mcp_servers]]
name    = "git"
command = "uvx"
args    = ["mcp-server-git"]

# Brave Search (requires API key)
[[mcp_servers]]
name    = "brave-search"
command = "npx"
args    = ["-y", "@brave/brave-search-mcp-server"]
[mcp_servers.env]
BRAVE_API_KEY = "your-brave-api-key"
```

### Tool Naming

MCP tools are namespaced as `mcp_<server-name>_<tool-name>` (e.g. `mcp_git_git_log`). Run `/tools` in the bot to see all registered tools.

---

## Built-in Tools

### Core Tools

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents within sandbox |
| `write_file` | Write/create files within sandbox |
| `list_files` | List directory contents within sandbox |
| `send_file` | Send a file from the sandbox to the current chat |
| `execute_command` | Run shell commands within sandbox directory |

### Memory Tools

| Tool | Description |
|------|-------------|
| `remember` | Store information in the user's long-term memory |
| `recall` | Query the user's long-term memory (RAG + keyword search) |
| `search_memory` | Search across all conversations with vector similarity |

### Scheduling Tools

| Tool | Description |
|------|-------------|
| `schedule_task` | Schedule a recurring (cron) or one-shot task |
| `list_scheduled_tasks` | List all active scheduled tasks |
| `cancel_scheduled_task` | Cancel a scheduled task by ID |

### Skill Tools

| Tool | Description |
|------|-------------|
| `read_skill_file` | Read a file from a skill's directory |
| `write_skill_file` | Write new or update existing skill files |
| `patch_skill` | Patch an existing skill's SKILL.md (append/replace content) |
| `reload_skills` | Hot-reload the skill registry without restarting |

### Agent Tools

| Tool | Description |
|------|-------------|
| `spawn_agents` | Spawn ad-hoc subagents with inline system prompts (supports parallel batch) |
| `invoke_agent` | Run a predefined agent from `agents/` in an isolated agentic loop |
| `read_agent_file` | Read a file from within an agent's directory |
| `write_agent_file` | Write a file into an agent's directory |
| `reload_agents` | Hot-reload the agent registry |
| `reload_skills_and_agents` | Reload both registries in one call |

### Plan Tools

| Tool | Description |
|------|-------------|
| `plan_create` | Create a structured execution plan (`.rustfox_plan.json` in sandbox) |
| `plan_update` | Update a step's status or notes |
| `plan_view` | View the current plan and step statuses |

### Utility Tools

| Tool | Description |
|------|-------------|
| `try_new_tech` | Run a sandboxed experiment with a new technology (Rust/JS) |
| `self_upgrade` | Upgrade the bot — auto-detects source code (git + cargo build) or release binary (downloads from GitHub). Re-registers systemd/launchd service if installed. Restarts after success. |

---

## Bot Commands

| Command | Description | Status |
|---------|-------------|--------|
| `/start` | Show welcome message with command list | Active |
| `/clear` | Clear conversation history | Active |
| `/tools` | List all available tools | Active |
| `/skills` | List all loaded skills | Active |
| `/verbose` | Toggle live tool call progress display | Active |
| `/query-rewrite` | Toggle RAG query rewriting for memory search | Active |
| `/update-skills` | Re-sync bundled skills/agents (backs up local edits) | Active |
| `/supervise <text>` | Submit a new supervisor task | Planned |
| `/tasks` | List active / recent supervisor tasks | Planned |
| `/resume <id>` | Resume a paused supervisor task | Planned |
| `/cancel <id>` | Cancel a supervisor task | Planned |
| `/approve <id>` | Approve a supervisor task | Planned |
| `/clarify <id> <text>` | Reply to a clarification prompt | Planned |

---

## Skills & Agents System

### Skills

Skills are folder-based natural-language instructions loaded at startup and injected into the LLM's system prompt. Each skill has its own folder with a `SKILL.md` file containing YAML frontmatter and instruction body.

- **Instruction skills** (no `model` in frontmatter): loaded by the agent via `read_skill_file` when relevant
- **Subagent skills** (`model` set): invoked via `invoke_agent` with their own model and tool whitelist

```
skills/
  code-interpreter/
    SKILL.md
  problem-solver/
    SKILL.md
  news-fetcher/
    SKILL.md
  ...
```

### Agents

The `agents/` directory contains isolated agentic mini-loops with their own model, tool whitelist, and `AGENT.md` instructions. Invoked via `invoke_agent`.

```
agents/
  verifier/
    AGENT.md       # Zero-trust verifier (read-only sandbox)
```

### Update Engine

`/update-skills` re-syncs bundled skills/agents from embedded data, backing up locally modified files (`.bak` suffix) before overwriting.

---

## Advanced Features

### File & Image Processing

Photos and documents (PDF, DOCX, images) are processed via vision API or OCR (`ocrs` pure Rust OCR engine), then injected as multi-modal content or text into the conversation.

### RAG & Vector Search

- Hybrid vector + FTS5 search using `qwen/qwen3-embedding-8b`
- Chat history RAG: semantically relevant past messages are auto-injected each turn
- RAG query rewriting: ambiguous follow-ups are rewritten before vector search
- Long-context RAG: large documents are chunked, embedded, and retrieved per query

### Nightly Summarization

LLM-based cron job summarizes long conversations overnight to keep memory efficient.

### Long-Term Memory

- Conversations can be soft-archived (searchable but excluded from active context)
- Startup and shutdown notifications
- `remember` / `recall` / `search_memory` tools for persistent user knowledge

### Streaming Responses

LLM tokens are streamed progressively; Telegram message is live-edited as the response arrives.

### Post-Task Learning

Auto-extracts reusable skill patterns from completed agentic loops and persists a user model (`user_model.md`).

### Autopilot Supervisor

Generic autonomous task runner with classification, planning, multi-backend execution, verification, and approval gates. Submit tasks via `/supervise` (backend dispatch incoming).

### LangSmith Tracing

Optional observability via LangSmith for LLM calls, tool runs, and chain traces. Configure `[langsmith]` section in `config.toml`.

---

## Roadmap

### Done

- [x] Telegram bot with user allowlist
- [x] OpenRouter LLM integration with tool calling (agentic loop)
- [x] Built-in sandboxed tools (file I/O, command execution, file sending, scheduling)
- [x] MCP server integration for extensible tooling
- [x] Per-user conversation history with persistent SQLite
- [x] Vector embedding search + FTS5 hybrid search
- [x] Bot skills (folder-based, auto-loaded)
- [x] Setup wizard (web UI + CLI) for guided config creation
- [x] Agents layer (`invoke_agent`, subagents, zero-trust verifier)
- [x] Plan tools (`plan_create`, `plan_update`, `plan_view`)
- [x] LLM streaming (SSE token-by-token, live Telegram edits)
- [x] Chat history RAG + RAG query rewriting
- [x] Nightly conversation summarization
- [x] Verbose tool UI (`/verbose`)
- [x] File & image upload support (vision API + OCR + document extraction)
- [x] Persistent home directory (`~/.rustfox` with env/config override)
- [x] Autopilot v2 supervisor (classification, planning, multi-backend execution)
- [x] LangSmith observability (LLM/tool/chain tracing)
- [x] Post-task skill extraction + user model persistence
- [x] Multi-platform service setup (`--setup` wizard, `--service` install)
- [x] Build scripts & CI release workflow (`.tar.gz`, `.zip`, `.deb`)
- [x] Ad-hoc parallel subagents (`spawn_agents`)

### Planned

- [ ] Event trigger framework (e.g., on email receive)
- [ ] WhatsApp support
- [ ] Webhook mode (in addition to polling)

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| [teloxide](https://github.com/teloxide/teloxide) | Telegram bot framework |
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk) | MCP Rust SDK |
| [reqwest](https://github.com/seanmonstar/reqwest) | HTTP client for OpenRouter |
| [tokio](https://tokio.rs/) | Async runtime |
| [tokio-cron-scheduler](https://github.com/mvniekerk/tokio-cron-scheduler) | Task scheduling |
| [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) | Markdown parsing |
| [rusqlite](https://github.com/rusqlite/rusqlite) | SQLite with FTS5 + `sqlite-vec` |
| [axum](https://github.com/tokio-rs/axum) | Web server for setup wizard |
| [dirs](https://github.com/soc/dirs-rs) | OS home directory resolution |
| [sha2](https://github.com/RustCrypto/hashes) | SHA-256 hashing |
| [regex](https://github.com/rust-lang/regex) | Secret redaction |
| [serde](https://github.com/serde-rs/serde) | Serialization |

> **Thanks:** Markdown-to-entities conversion inspired by [telegramify-markdown](https://github.com/sudoskys/telegramify-markdown).
