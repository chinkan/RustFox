<p align="center">
  <img src="assets/logo.jpeg" alt="RustFox Logo" width="200"/>
</p>

# RustFox — Telegram AI Assistant

[![CI](https://github.com/chinkan/RustFox/actions/workflows/ci.yml/badge.svg)](https://github.com/chinkan/RustFox/actions)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Buy Me a Coffee](https://img.shields.io/badge/buy%20me%20a%20coffee-%E2%98%95-yellow)](https://buymeacoffee.com/chinkan.ai)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-pink?logo=github)](https://github.com/sponsors/chinkan)

A self-hosted, agentic Telegram AI assistant written in Rust, powered by OpenRouter LLM with sandboxed tools, MCP server integration, and persistent memory.

**docs:** [README.md](README.md) · [GUIDE.md](docs/GUIDE.md) · [ARCHITECTURE.md](docs/ARCHITECTURE.md)

---

## Features

| | |
|---|---|
| 🤖 **AI Agent** | OpenRouter LLM (default: `qwen/qwen3-235b-a22b`), agentic loop with tool calling, configurable max iterations |
| 🔧 **Built-in Tools** | File read/write, command execution, file sending, task scheduling — all sandboxed |
| 🧩 **MCP Servers** | Connect any MCP-compatible server (Git, Brave Search, GitHub, Filesystem, Threads…) |
| 🧠 **Persistent Memory** | SQLite-backed conversation history, vector embedding search (hybrid + FTS5), RAG |
| 🧬 **Skills & Agents** | Folder-based skill instructions auto-loaded at startup; subagent skills with own model and tool whitelist |
| 🤝 **Agent Layer** | Isolated agentic mini-loops in `agents/` with own model/tools; `invoke_agent`, `spawn_agents`, zero-trust verifier |
| 🔄 **Task Scheduling** | Cron and one-shot task scheduler with SQLite persistence |
| 📦 **Self-Hosting** | Single binary, 2-min setup wizard, background service (systemd/launchd/Windows Service) |

→ Full feature reference: [docs/GUIDE.md](docs/GUIDE.md#advanced-features)

---

## Quick Start

### 1. Install

**Option A — Download a release (recommended)**

Download from the [Releases page](https://github.com/chinkan/RustFox/releases):

```bash
tar xzf rustfox-*.tar.gz
```

**Option B — Build from source**

```bash
cargo install --path . --locked
```

### 2. Configure

```bash
# Browser wizard
./rustfox --setup

# Or terminal wizard
./rustfox --setup --cli
```

The wizard guides you through: Telegram bot token, allowed user IDs, OpenRouter API key, model, and optional MCP tools.

### 3. Run

```bash
rustfox
# or with a custom config:
rustfox --config /path/to/config.toml
```

### 4. (Optional) Background service

```bash
rustfox --service install   # Linux (systemd), macOS (launchd), or Windows
rustfox --service status
```

---

## Configuration

| Setting | Description |
|---------|-------------|
| `telegram.bot_token` | Telegram Bot API token (from [@BotFather](https://t.me/BotFather)) |
| `telegram.allowed_user_ids` | Comma-separated user IDs allowed to use the bot |
| `openrouter.api_key` | OpenRouter API key ([openrouter.ai/keys](https://openrouter.ai/keys)) |
| `openrouter.model` | LLM model ID (default: `qwen/qwen3-235b-a22b`) |
| `sandbox.allowed_directory` | Directory for sandboxed file/command operations |
| `mcp_servers` | List of MCP servers to connect (see [GUIDE.md](docs/GUIDE.md#mcp-server-integration)) |

→ Full configuration reference: [docs/GUIDE.md](docs/GUIDE.md#configuration)

---

## Quick Tool Overview

| Tool | Description |
|------|-------------|
| `read_file` / `write_file` | Read and write files within the sandbox |
| `send_file` | Send a file from the sandbox to the current chat |
| `execute_command` | Run shell commands within the sandbox |
| `schedule_task` | Schedule recurring (cron) or one-shot tasks |
| `invoke_agent` | Run a predefined agent from the `agents/` directory |

→ Full tool reference: [docs/GUIDE.md](docs/GUIDE.md#built-in-tools)

---

## Architecture

RustFox runs an agentic loop: user message → LLM (OpenRouter) → tool calls → execute → loop until final response. Tools dispatch to built-in functions, MCP servers, or skill/agent directories.

→ Full architecture with source tree and data flow: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

---

## Contributing

MIT License. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to open issues and submit PRs.

## Support

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-%E2%98%95-yellow?style=for-the-badge&logo=buy-me-a-coffee)](https://buymeacoffee.com/chinkan.ai)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-pink?style=for-the-badge&logo=github)](https://github.com/sponsors/chinkan)
