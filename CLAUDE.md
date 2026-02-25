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

Skills can be **instruction skills** (no `model` in frontmatter; full body injected into the system prompt) or **subagent skills** (`model` set; only name + description shown; invoke via `invoke_subagent(skill="name", prompt="...")`). The orchestration skill teaches the agent when to call which subagent and when to override the model (e.g. `model="anthropic/claude-sonnet-4-6"` for thread-writer-hk).

**Daily News to Threads flow:** The `daily-news-to-threads` orchestration skill (instruction) directs the main agent to: (1) call the `news-fetcher` subagent (default model) to get AI news from Gmail Google 快訊, (2) call the `thread-writer-hk` subagent with model override to write a HK-style Threads thread with verified links, (3) post the thread via Threads MCP and report success. Requires Gmail (google-workspace), fetch, and threads MCP servers in config.

## Files Not to Commit

- `config.toml` - Contains API keys and tokens
- `.env` - Environment variables
- `/target/` - Build artifacts
