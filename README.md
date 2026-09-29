<p align="center">
  <img src="assets/logo.jpeg" alt="RustFox Logo" width="200"/>
</p>

# RustFox — Telegram AI Assistant

[![CI](https://github.com/chinkan/RustFox/actions/workflows/ci.yml/badge.svg)](https://github.com/chinkan/RustFox/actions)
[![Release](https://img.shields.io/github/v/release/chinkan/RustFox?include_prereleases&sort=semver)](https://github.com/chinkan/RustFox/releases)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Buy Me a Coffee](https://img.shields.io/badge/buy%20me%20a%20coffee-%E2%98%95-yellow)](https://buymeacoffee.com/chinkan.ai)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-pink?logo=github)](https://github.com/sponsors/chinkan)

**What is RustFox?**

An open-source, self-hosted Telegram AI assistant written in Rust. It solves a simple problem: most AI assistants are locked inside proprietary chat UIs with no access to your files, tools, or schedule. RustFox lives in Telegram — your everyday messaging app — and acts as a full agentic AI teammate.

**Why RustFox?**

Drop a file, ask a question, schedule a task — RustFox handles it. It runs an agentic loop against your choice of LLM provider — **OpenRouter by default, or any local model via Ollama / LM Studio** — receive your message, call sandboxed tools (file I/O, command execution, web search via MCP), and loop until done. It remembers context via SQLite + vector RAG, runs skills and sub-agents, and even verifies its own work.

**Self-hosted, no cloud dependency.** Single binary. Setup wizard. Runs as systemd/launchd service. `cargo install` and you're running in 2 minutes.

Star the repo ⭐, fork to contribute, or open an issue for feedback.

**docs:** [README.md](README.md) · [GUIDE.md](docs/GUIDE.md) · [ARCHITECTURE.md](docs/ARCHITECTURE.md) · [CHANGELOG.md](CHANGELOG.md)

---

## Features

| | |
|---|---|
| 🤖 **AI Agent** | Multi-provider LLM (OpenRouter, local Ollama, any OpenAI-compatible endpoint), agentic loop with tool calling, configurable max iterations, request-layer model fallback chain |
| 🔧 **Built-in Tools** | File read/write, command execution, file sending, task scheduling — all sandboxed |
| 🧩 **MCP Servers** | Connect any MCP-compatible server (Git, Brave Search, GitHub, Filesystem, Threads…) |
| 🧠 **Persistent Memory** | SQLite-backed conversation history, vector embedding search (hybrid + FTS5), RAG |
| 🧬 **Skills & Agents** | Folder-based skill instructions auto-loaded at startup; subagent skills with own model and tool whitelist |
| 🤝 **Agent Layer** | Isolated agentic mini-loops in `agents/` with own model/tools; `invoke_agent`, `spawn_agents`, zero-trust verifier |
| 🪪 **Multi-Bot / Multi-Persona** | Run several Telegram bots in one process, each bound to its own persona, model, and tool whitelist via `[[bots]]` |
| 🔄 **Task Scheduling** | Cron and one-shot task scheduler with SQLite persistence, plus a dead-letter rerun queue |
| 🌐 **Web Portal** | Built-in browser UI (chat, agents, memory, tasks, settings) served from the same binary — token-auth, loopback by default |
| 📦 **Self-Hosting** | Single binary, 2-min setup wizard, background service (systemd/launchd/Windows Service) |

→ Full feature reference: [docs/GUIDE.md](docs/GUIDE.md#advanced-features)

### Agent Lifecycle & Runtime

| Capability | Description |
|------------|-------------|
| **Self-Upgrade** | Trigger an in-place upgrade: pulls from git source or downloads the latest GitHub release binary. Auto-restarts after upgrade — no SSH, no manual steps. |
| **Model Switching** | Switch models at runtime via `/models`. Interactive picker lets you choose the best model per task: fast/cheap for simple queries, powerful for complex reasoning. |
| **Model Fallback** | Ordered `[fallback] chain` of provider-prefixed model IDs. When the primary model fails (e.g. HTTP 429), RustFox retries down the chain before surfacing an error. |
| **Soul Files** | SOUL.md (persona), AGENTS.md (behaviour), USER.md (preferences) — persistent identity files auto-injected into every system prompt. Session-end self-reflection with `.bak` backups. |

---

### 🪪 Multi-Bot / Multi-Persona

Run **N Telegram bots inside a single process**, each bound to its own persona, model, and tool whitelist. Configure them with a `[[bots]]` array; the legacy single-bot `[telegram]` section is still supported (it synthesizes one bot and is treated as a deprecated migration alias when `[[bots]]` is present).

```toml
[[bots]]
id = "main"
bot_token = "..."              # written by the wizard / secret store
allowed_user_ids = [123456789]
persona = "main"               # agents/<persona>/AGENT.md (+ optional SOUL.md overlay)
# model = "moonshotai/kimi-k2.6"
# tools = ["read_file", "write_file", "execute_command", "invoke_agent"]

[[bots]]
id = "researcher"
bot_token = "..."
allowed_user_ids = [123456789]
persona = "researcher"         # read-heavier defaults; no execute_command / write_file
# tools = ["read_file", "list_files", "remember", "recall",
#          "search_memory", "invoke_agent", "spawn_agents"]
```

**Precedence** for a bot's model and tools: `[[bots]]` fields → `agents/<persona>/AGENT.md` → install defaults.

**Routing & isolation** — when more than one bot is configured, the first non-`default` bot owns Telegram claim/routing; each bot keeps its own conversation context. `invoke_agent` / `spawn_agents` can target a persona by id (`bot=` synonym), with nested peer depth capped at 2 and cycle detection. USER.md stays shared under `RUSTFOX_HOME` (not per-bot); the portal uses the default/shim persona.

> An empty `allowed_user_ids` on any bot is a **hard error at config load** — the process will not start.

See [docs/multi-bot-e2e.md](docs/multi-bot-e2e.md) and [ADR-0010 (decision layer)](docs/adr/0010-decision-layer-interface.md) for design details.

## Quick Start

### 1. Install

**Option A — Download a release (recommended)**

Download from the [Releases page](https://github.com/chinkan/RustFox/releases):

```bash
tar xzf rustfox-*.tar.gz
```

**Option B — Build from source (recommended script)**

```bash
git clone https://github.com/chinkan/RustFox && cd RustFox
./scripts/build-all.sh --install
```

`build-all.sh` builds the web portal frontend first (`npm ci` + `vite build`), *then*
the Rust binary — the portal UI is compiled into the binary via `include_dir!`, so
build order matters (see [Building & Verifying](#-building--verifying)). If you don't
need the portal, plain `cargo install --path . --locked` still works.

### 2. Configure

```bash
# Browser wizard
./rustfox --setup

# Or terminal wizard
./rustfox --setup --cli
```

The wizard guides you through: Telegram bot token, allowed user IDs, LLM provider + API key, model, and optional MCP tools.

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
| `[[bots]]` | Array of Telegram bots, each with `id`, `bot_token`, `allowed_user_ids`, `persona`, and optional `model` / `tools` overrides |
| `telegram.*` | Legacy single-bot section (deprecated alias when `[[bots]]` is present) |
| `openrouter.api_key` | OpenRouter API key ([openrouter.ai/keys](https://openrouter.ai/keys)) |
| `openrouter.model` | Default LLM model ID (default: `moonshotai/kimi-k2.6`) |
| `[[provider]]` | Additional LLM providers (Ollama, LM Studio, any OpenAI-compatible endpoint); model strings use `provider/model_id` |
| `[fallback] chain` | Ordered model fallback list, e.g. `["openrouter/…", "ollama/llama3.1"]` |
| `[portal]` | Web portal settings (`enabled`, `port`, `bind`, `token_sha256`) |
| `sandbox.allowed_directory` | Directory for sandboxed file/command operations |
| `mcp_servers` | List of MCP servers to connect (see [GUIDE.md](docs/GUIDE.md#mcp-server-integration)) |

→ Full configuration reference: [docs/GUIDE.md](docs/GUIDE.md#configuration)

---

## 🌐 Web Portal

RustFox can serve a built-in web UI from the **same binary** — no separate frontend
deploy. Open `http://127.0.0.1:8090` in your browser to chat with the agent (live
SSE token streaming), browse memory, manage scheduled tasks and sub-agents, and
inspect your config.

Enable it in `config.toml`:

```toml
[portal]
enabled = true
port = 8090
bind = "127.0.0.1"        # keep loopback/private-network only
token_sha256 = "<sha256>" # hash of your login token (preferred over plaintext)
```

Generate a token + hash:

```bash
python3 -c 'import hashlib,secrets; t=secrets.token_hex(32); print("token:", t); print("token_sha256:", hashlib.sha256(t.encode()).hexdigest())'
```

| Page | What you can do |
|------|-----------------|
| 💬 **Chat** | Talk to the agent in the browser, watch tool calls stream live; history persisted and isolated from Telegram |
| 🧬 **Agents** | Browse configured sub-agents, filter by name |
| 🧠 **Memory** | Search conversation/knowledge store (FTS5 hybrid), browse recent entries |
| 🔄 **Tasks** | List cron/one-shot tasks, view run history, pause/resume |
| ⚙️ **Settings** | Inspect active config (secrets masked) |

> 🔒 The portal is **off by default** and intended for loopback/private networks
> (e.g. a Tailscale IP). Public exposure/TLS is out of scope for now.

---

## Quick Tool Overview

| Tool | Description |
|------|-------------|
| `read_file` / `write_file` / `list_files` | Read, write, and list files within the sandbox |
| `send_file` | Send a file from the sandbox to the current chat |
| `execute_command` | Run shell commands within the sandbox |
| `self_upgrade` | Trigger self-upgrade from git source or GitHub release, auto-restart |
| `read_soul_file` / `update_soul_file` / `revert_soul_file` | Read, update (append/replace, `.bak` backup), or restore SOUL.md / AGENTS.md / USER.md |
| `remember` / `recall` / `search_memory` | Store, look up, and search long-term user knowledge |
| `plan_create` / `plan_update` / `plan_view` | Create and track a structured execution plan |
| `read_skill_file` / `write_skill_file` / `reload_skills` | Read, write, and hot-reload skills |
| `read_agent_file` / `write_agent_file` / `reload_agents` | Read, write, and hot-reload sub-agent definitions |
| `invoke_agent` | Run a predefined agent from `agents/` (or a `[[bots]]` persona via `bot=`) |
| `spawn_agents` | Spawn ad-hoc subagents with inline system prompts (parallel batch supported) |
| `try_new_tech` | Sandboxed experiment — run Rust/JS code and check results |
| `schedule_task` / `list_scheduled_tasks` / `get_scheduled_task_history` / `cancel_scheduled_task` | Create, list, inspect, and cancel cron/one-shot tasks |
| `task_reruns` | Inspect and resolve the dead-letter rerun queue |

→ Full tool reference: [docs/GUIDE.md](docs/GUIDE.md#built-in-tools)

---

## Architecture

RustFox runs an agentic loop: user message → LLM (OpenRouter or a local provider) → tool calls → execute → loop until final response. Tools dispatch to built-in functions, MCP servers, or skill/agent directories.

→ Full architecture with source tree and data flow: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

---

## 🛠 Building & Verifying

### One-command build (portal-safe)

```bash
./scripts/build-all.sh                # web (vite) → dist → cargo build --release
./scripts/build-all.sh --install      # ...then install to ~/.cargo/bin/rustfox
./scripts/build-all.sh --skip-web     # reuse existing web/dist (Rust only)
./scripts/build-all.sh --profile dev  # debug build (faster compile)
```

Why a script? The portal frontend is **embedded into the binary at compile time**
(`include_dir!("web/dist")`). Two traps this guards against:

1. **Build order** — cargo before `vite build` embeds a stale/empty dist → the
   portal renders a white screen. `build-all.sh` enforces web-first and fails
   fast if `web/dist/index.html` is missing.
2. **Stale embedding** — `include_dir!` has no cargo change-fingerprint, so
   updating dist alone won't trigger a rebuild. The script touches the embedding
   module to force re-embedding.

### Try the portal without Telegram/LLM

```bash
cargo run --release --example portal_preview   # serves the real embedded UI + seeded demo data
```

### End-to-end regression gate (38 checks)

```bash
node e2e-verify.mjs     # Playwright: login → chat SSE → agents → memory → tasks → settings
```

Covers every portal page against the **real embedded-dist binary** (same code path
as production), including a stub-dist control test that catches white-screen
regressions. Zero console errors is part of the gate. CI runs the web build before
cargo tests, and the same sequence applies to release builds.

---

## Contributing

MIT License. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to open issues and submit PRs.

## Support

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-%E2%98%95-yellow?style=for-the-badge&logo=buy-me-a-coffee)](https://buymeacoffee.com/chinkan.ai)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-pink?style=for-the-badge&logo=github)](https://github.com/sponsors/chinkan)
