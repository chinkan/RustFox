# RustFox Guide

- [Configuration](#configuration)
- [Multi-bot](#multi-bot)
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
| | `api_base_url` | Bot API root override (teloxide + rich_sender) | `https://api.telegram.org` |
| | `allowed_user_ids` | Comma-separated whitelist of user IDs | — |
| `[openrouter]` | `api_key` | OpenRouter API key; sealed to `secret:openrouter.api_key` (see below) | — |
| | `model` | LLM model ID (`provider/model`; wizard list is a shortcut, see [openrouter.ai/models](https://openrouter.ai/models)) | `moonshotai/kimi-k2.6` |
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
> **Secrets:** macOS and Windows use the OS keyring (Keychain / Credential Manager). Linux always uses the encrypted vault, because Linux keyutils is in-memory and per-session (a `systemctl --user` service cannot see it and a reboot clears it); secrets left in keyutils by older builds are copied into the vault on first read. If the keyring is unavailable, RustFox uses an AES-GCM vault at `~/.rustfox/secrets/vault` whose master key is a **plaintext file** `~/.rustfox/secrets/vault.key` (mode `0600`). Anyone who can read `vault.key` can decrypt the vault — treat home-directory permissions as the trust boundary. Pending secret entry uses the portal masked form + Telegram notify/link (never paste values in chat). Required secrets for MCP env use the `secret:NAME` value form; `[sandbox].secret_env` injects named secrets into `execute_command` child processes via env only (never chat/LLM/tool args/logs).
>
> **Bot tokens:** `[[bots]].bot_token` / legacy `[telegram].bot_token` must be a `secret:NAME` reference (canonical name `bot.<id>.token`). Wizard initial save, `/agents` bind, and wizard add-bot write the BotFather token into SecretStore **before** disk write — `config.toml` never receives plaintext. Startup migrate+scrub remains a safety net for any leftover BotFather-shaped values (`.bak` via the shared config write path). `/config`, `/agents`, and restart replies never echo token values (`bot_token=***`). Manual path: store set `bot.<id>.token` then set `bot_token = "secret:bot.<id>.token"` in config.
>
> **OpenRouter key (ADR 0016):** `[openrouter].api_key` is stored in SecretStore as `openrouter.api_key`; config keeps only `api_key = "secret:openrouter.api_key"`. The wizard seals it before writing `config.toml`, and startup migrate+scrub moves any leftover plaintext key into the store (`.bak` via the shared config write path). The ref is resolved at startup, including the legacy `[openrouter]` provider. `[embedding].api_key` and other `[[provider]].api_key` values are deliberately not sealed yet and may stay plaintext. The `config.toml.bak` left by migrate still holds the old plaintext and is written owner-only (`chmod 600`). Once `config.toml` shows `secret:` refs, delete `config.toml.bak` by hand; RustFox never deletes it for you, so you can still roll back.

---

## Multi-bot

One RustFox process can host **N Telegram bots** (N BotFather tokens) sharing one sandbox, skills, MCP, and memory DB. Personas / allowlists are per bot; peer help is tool-mediated (not Telegram bot↔bot).

### Adding a second bot (wizard)

1. Run `rustfox --setup` (web) or `rustfox --setup --cli`.
2. Configure the primary bot as usual (`[telegram]` on first save).
3. Click **Add another bot** (or answer `y` in CLI) — enter bot id, BotFather token, and allowed user id.
4. On save, each extra bot is appended via **validate → `config.toml.bak` → atomic write** (same path as `/config` / `/agents`). The **first** multi-bot add materializes legacy `[telegram]` into `[[bots]]` (design §6 / PO §8b).
5. Restart (or let the service manager bring the process back) so both dispatchers start.

You can also add a bot from Telegram with [`/agents create`](#agents-create-persona--bind-token) (token is one-shot, never echoed).

### Config shape

```toml
[[bots]]
id = "main"
bot_token = "secret:bot.main.token"   # never plaintext after bind/migrate
allowed_user_ids = [123456789]
persona = "main"

[[bots]]
id = "researcher"
bot_token = "secret:bot.researcher.token"
allowed_user_ids = [123456789]
persona = "researcher"
```

Shared sections (`[sandbox]`, `[skills]`, `[agents]`, MCP, providers) stay install-wide. See [`config.example.toml`](../config.example.toml) for the commented researcher example. Conversation history is keyed by `(platform, bot_id, user_id)`.

### Fully silent tool calls

The default is unchanged. While a tool runs, that bot may still show a `Working…` bubble or a `Running: …` line. When the tool finishes, the completed tool message is removed. The final assistant reply still posts.

`fully_silent` is an opt-in on the **bot that is speaking** (`[[bots]]`). It is not a setup-wizard question and not a per-chat `/verbose` setting. When it is `true`, that bot's Telegram turn posts no tool-call UI: no in-progress bubble, no completed tool bubble, and no raw `Running:` line. The final assistant reply still posts. Single-bot and multi-bot both follow the speaking bot. Another bot does not inherit the flag.

```toml
[[bots]]
id = "main"
bot_token = "secret:bot.main.token"
allowed_user_ids = [123456789]
persona = "main"
fully_silent = true

[[bots]]
id = "researcher"
bot_token = "secret:bot.researcher.token"
allowed_user_ids = [123456789]
persona = "researcher"
# fully_silent stays off here. Researcher still shows Working / Running.
```

### Per-bot schedules

Every schedule row has a required `bot_id` (the `[[bots]]` id). There is no global cron and no schedule shared between bots. A run uses that bot's prompt and tools. It writes the prompt and the result into the owning bot's conversation with the owner, and every written segment includes the schedule id. Failure, cancel, and max-iterations write a result turn, not only the prompt. A task opened from the portal is stored as user `web` and platform `portal`; that segment is also written into the owning bot's Telegram conversation (the allowlisted Telegram user id kept on the row as `chat_id`, opened with the same legacy-claim rule as a Telegram reply) so the next reply in that chat can see the prompt and the result. There is no approval gate before the run. Portal and `list_scheduled_tasks` show only the current bot. Disable and delete affect only that bot. An approval reply is not sent on a bot that did not own the run. Rows saved before `bot_id` existed are assigned to `default` and logged, not dropped.

```toml
[[bots]]
id = "researcher"
bot_token = "secret:bot.researcher.token"
allowed_user_ids = [123456789]
persona = "researcher"
# A cron created in this bot's chat is stored as bot_id = "researcher".
# It does not appear in the portal (the default bot) or in another bot's list.
```

### Peer invoke + slash commands

- **`invoke_agent` / `spawn_agents`** — resolve `agents/`, subagent skills, or a `[[bots]]` persona id (`bot=` synonym). Nested peer depth max **2**; cycles rejected; user-visible peer summaries prepend `via <persona>:`. Reply stays in the **caller’s** chat. Details: [Agent Tools](#agent-tools).
- **`/agents`** — list / show / create + bind token (`bot_token=***`). Details: [`/agents`](#agents-create-persona--bind-token).
- **`/config`** — allowlisted `config.toml` edits with the same bak path (secrets denied). Details: [`/config` slash map](#config-slash-map-allowlist).

Live E2E checklist (≥2 BotFather test bots): [`docs/multi-bot-e2e.md`](multi-bot-e2e.md). Automated gate: `cargo test --test multi_bot_e2e_gate`. Telegram Update injector (no Desktop): [`docs/telegram-update-injector.md`](telegram-update-injector.md) — `cargo test --test telegram_update_injector`.

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
| `schedule_task` | Schedule a recurring (cron) or one-shot task for the current bot |
| `list_scheduled_tasks` | List the current bot's active scheduled tasks |
| `cancel_scheduled_task` | Cancel one of the current bot's scheduled tasks by ID |

### Skill Tools

| Tool | Description |
|------|-------------|
| `read_skill_file` | Read a file from a skill's directory |
| `write_skill_file` | Write new or update existing skill files |
| `patch_skill` | Patch an existing skill's SKILL.md (`mode`: `append` default, or `replace`; append strips patch frontmatter) |
| `reload_skills` | Hot-reload the skill registry without restarting |

### Agent Tools

| Tool | Description |
|------|-------------|
| `spawn_agents` | Spawn ad-hoc subagents with inline system prompts (supports parallel batch) |
| `invoke_agent` | Run a predefined agent from `agents/`, a subagent skill, or a `[[bots]]` persona id (optional `bot=` synonym). Nested peer depth max 2; cycles rejected; results prepend `via <persona>:` |
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

Release-binary `/selfupgrade` asks GitHub for the latest release. Anonymous calls share a low rate limit; if Telegram shows HTTP 403 / rate limit, wait for the quota to reset or export `GITHUB_TOKEN` / `GH_TOKEN` (classic PAT with `public_repo`, or fine-grained Releases read) and retry. See ADR 0018.

---

## Bot Commands

| Command | Description | Status |
|---------|-------------|--------|
| `/start` | Show welcome message with command list | Active |
| `/clear` | Clear conversation history | Active |
| `/tools` | List all available tools | Active |
| `/skills` | List all loaded skills | Active |
| `/verbose` | Toggle live tool call progress display | Active |
| `/queryrewrite` | Toggle RAG query rewriting for memory search | Active |
| `/update-skills` | Re-sync bundled skills/agents (backs up local edits) | Active |
| `/models` | Browse and change the LLM model | Active |
| `/format` | Switch message format: rich, markdown, or auto | Active |
| `/portal` | Portal URLs (web UI) for this network | Active |
| `/config` | Show / set allowlisted `config.toml` keys (secrets redacted) | Active |
| `/config show` | Redacted config summary (sandbox path read-only) | Active |
| `/config keys` | Slash map: editable keys vs restart-required vs denied | Active |
| `/config set <key> <value>` | Validate → `config.toml.bak` → atomic write | Active |
| `/restart` | Ack, then clean process exit (systemd/launchd/shell brings it back) | Active |
| `/agents` | List bot ids + personas (`bot_token=***`) | Active |
| `/agents show <id>` | One bot (token redacted) | Active |
| `/agents create <id>` | Create `agents/<id>/` then one-shot BotFather token → `[[bots]]` + restart | Active |
| `/agents cancel` | Abort pending token bind | Active |
| `/supervise <text>` | Submit a new supervisor task | Planned |
| `/tasks` | List active / recent supervisor tasks | Planned |
| `/resume <id>` | Resume a paused supervisor task | Planned |
| `/cancel <id>` | Cancel a supervisor task | Planned |
| `/approve <id>` | Approve a supervisor task | Planned |
| `/clarify <id> <text>` | Reply to a clarification prompt | Planned |

### `/config` slash map (allowlist)

Editable via Telegram (only users already on the bot allowlist):

| Key | Needs `/restart`? | Notes |
|-----|-------------------|-------|
| `openrouter.model` (alias `model`) | No (also live via `/models`) | Non-empty model id |
| `memory.query_rewriter_enabled` | Yes | Default for new sessions |
| `agent.max_iterations` | Yes | ≥ 1 |
| `agent.loop_detection.enabled` | Yes | Bool |
| `learning.skill_extraction_enabled` | Yes | Bool |
| `general.location` | Yes | Prompt location string |
| `supervisor.default_autonomy_mode` | Yes | `fast` / `standard` / `rigorous` |
| `portal.enabled` / `portal.port` / `portal.bind` / `portal.user_name` | Yes | Portal knobs (not tokens) |
| `mcp.<server>.enabled` | Yes | Toggle a named `[[mcp_servers]]` entry |

**Read-only display:** `sandbox.allowed_directory`.

**Denied (never editable via chat):** `bot_token` / API keys / provider secrets; portal `token` / `token_sha256`; MCP `command` / `args` / `env`; `allowed_user_ids` (empty allowlist is a hard error); `database_path`; `general.home`.

**Write path:** validate → copy `config.toml` → `config.toml.bak` → atomic write → ack. On parse failure after write, restore from `.bak` and abort restart. Secrets are never echoed in Telegram replies, logs, or `/config show`.

**`/restart`:** after a successful write (or alone) reply OK, then clean process exit so the service manager or user shell brings the bot back. v1 does **not** hot-reload Telegram dispatchers in-process.

### `/agents` (create persona + bind token)

1. `/agents` — list configured bots (ids + personas; **never** raw tokens).
2. `/agents create <id>` — writes `agents/<id>/AGENT.md` + default `SOUL.md`, then arms a **one-shot** token capture for the caller.
3. Send the BotFather token as the next message — message is **deleted** best-effort; logs/replies show `bot_token=***` only; token is never stored in conversation memory.
4. Stores the token in SecretStore as `bot.<id>.token`, appends `[[bots]] { id, bot_token="secret:bot.<id>.token", persona=<id>, allowed_user_ids=[caller] }` via validate → `config.toml.bak` → atomic write (same path as `/config`), rejects duplicate `id` or resolved token, then **restarts**.
5. Delete / soft-disable and hot-add without restart are **not** in v1.

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
  researcher/
    AGENT.md       # Research specialist (read-heavier + memory/plan/invoke)
    SOUL.md        # Citation-first overlay
```

`[[bots]]` entries also appear under Available Agents (list **`id`**; description from `agents/<persona>`). Per-bot `tools` / `model` apply on the main Telegram loop (bots fields → AGENT.md → install defaults).

### Update Engine

`/update-skills` re-syncs bundled skills/agents from embedded data, backing up locally modified files (`.bak` suffix) before overwriting.

---

## Advanced Features

### File & Image Processing

Photos and documents (PDF, DOCX, images) are processed via vision API or OCR, then injected as multi-modal content or text into the conversation.

When the active model supports vision, a photo is sent as an image and the OCR chain does not run. Otherwise the first non-empty result wins, in this order: RapidOCR ONNX pinned to PP-OCRv4 mobile (`chinese_cht`, rec file `chinese_cht_PP-OCRv3_rec_mobile.onnx`, dict `chinese_cht_dict.txt`, no conversion to simplified Chinese), then Tesseract if the `tesseract` binary is installed (`chi_tra+eng`), then `ocrs` last. `ocrs` is Latin-only and is not the Cantonese path. This is not RapidOCR's PP-OCRv6 default. The ONNX weights are not in git. `scripts/fetch-rapidocr-ppocrv4-chinese-cht.sh` downloads them, checks SHA256, and writes `$HOME/.cache/ocrs/rapidocr` (or `[ocr].model_dir/rapidocr`). Unit tests do not run that script and do not download models. If the pinned files or `python3` with RapidOCR are missing, that stage is skipped.

Native PDFs are not sent to the model as a file. Text at or under 6000 characters is injected as-is. Longer native PDFs stay on the knowledge text-RAG path: 1000-character chunks with 100-character overlap, then the existing hybrid search (top 5). The prompt cites the page number of each hit. When the active model supports vision, only those retrieved pages are rasterized (long edge 1568px, at most 8 images). Scanned-page OCR and ColPali are later slices, not this path.

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
- [x] Multi-bot (`[[bots]]`, wizard Add another bot, peer invoke)

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
| [keyring](https://github.com/hwchen/keyring-rs) | OS credential store |
| [aes-gcm](https://github.com/RustCrypto/AEADs) | Encrypted-file secret vault |
| [serde](https://github.com/serde-rs/serde) | Serialization |

> **Thanks:** Markdown-to-entities conversion inspired by [telegramify-markdown](https://github.com/sudoskys/telegramify-markdown).
