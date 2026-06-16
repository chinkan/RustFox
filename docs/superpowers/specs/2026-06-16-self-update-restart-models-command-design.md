# Self-Upgrade, `/self-upgrade` Command & `/models` Command

**Date:** 2026-06-16
**Branch:** `feat/self-update-models-command`

## Overview

Two features to improve RustFox's runtime management:

1. **Unified self-upgrade** — a single `self_upgrade()` function that auto-detects deployment mode (source code, release binary, system service), upgrades the binary, re-registers the service if needed, and restarts. Exposed as both a `/self-upgrade` command (bypasses LLM, shows inline progress) and a `self_upgrade` tool (for LLM use).
2. **`/models` command** — browse and change the OpenRouter model at runtime, persisted to config.toml and hot-reloaded without restart.

New dependency: `self_update = { version = "0.44", features = ["archive-tar", "compression-flate2", "archive-zip"] }`

---

## Feature 1: Unified Self-Upgrade

### Current State

`self_update_to_branch` tool (`src/learning.rs:431-472`) correctly does `git fetch → checkout → pull → cargo build --release`, but:
- Does NOT restart the process after build
- Only works for source code mode
- Tool name is too narrow for the unified design

The project has a full release pipeline (`.github/workflows/release.yml`) that builds archives for:
- `x86_64-unknown-linux-gnu` (`.tar.gz` + `.deb`)
- `aarch64-apple-darwin` (`.tar.gz`)
- `x86_64-pc-windows-msvc` (`.zip`)

Archive naming: `rustfox-<tag>-<target>.tar.gz` / `.zip`, containing the binary, `config.example.toml`, `install.sh`, and service scripts.

### Design: Unified `self_upgrade()` Function

A single function in `src/learning.rs` that handles all deployment modes:

```
self_upgrade(branch: "main", mode: "auto")
  │
  ├─ Step 1: Detect mode
  │   ├─ Cargo.toml found walking up from binary? → SOURCE MODE
  │   └─ No Cargo.toml? → RELEASE BINARY MODE
  │
  ├─ Step 2a: SOURCE MODE
  │   ├─ git fetch --all
  │   ├─ git checkout <branch>
  │   ├─ git pull origin <branch>
  │   └─ cargo build --release
  │
  ├─ Step 2b: RELEASE BINARY MODE
  │   └─ Wrap in tokio::task::spawn_blocking (self_update is sync)
  │   └─ self_update::backends::github::Update::configure()
  │        .repo_owner("chinkan")
  │        .repo_name("RustFox")
  │        .bin_name("rustfox")
  │        .current_version(cargo_crate_version!())  // "1.0.1"
  │        .build()?.update()?
  │
  ├─ Step 3: Service detection (both modes)
  │   ├─ systemd unit: ~/.config/systemd/user/rustfox.service exists?
  │   ├─ LaunchAgent: ~/Library/LaunchAgents/com.rustfox.bot.plist exists?
  │   └─ No service file → FOREGROUND MODE
  │
  ├─ Step 4a: SERVICE MODE
  │   ├─ SOURCE only: cargo install --path . --force
  │   ├─ rustfox --service install (re-renders service file)
  │   └─ systemctl --user restart rustfox.service (or launchctl equivalent)
  │
  └─ Step 4b: FOREGROUND MODE
      ├─ Spawn new binary as child process
      ├─ Brief delay (1s) for message delivery
      └─ std::process::exit(0)
```

**Detection methods:**
- **Source mode**: Walk up from `current_exe()`, look for `Cargo.toml`, max depth 10
- **Release binary mode**: No `Cargo.toml` found — use `self_update` crate
- **Service mode**: Check `~/.config/systemd/user/rustfox.service` (Linux), `~/Library/LaunchAgents/com.rustfox.bot.plist` (macOS), or `sc query RustFox` (Windows)

**`mode` parameter** allows the caller (LLM or user) to force a specific mode: `"auto"`, `"source"`, `"release"`.

### `/self-upgrade` Command (`src/platform/telegram.rs`)

A direct Telegram command that bypasses the LLM entirely. Flow:

1. User sends `/self-upgrade [branch]`
2. Bot immediately responds with "🔄 Starting self-upgrade..."
3. Runs steps sequentially, updating the message via `edit_message_text`:
   ```
   ✓ git fetch --all (0.3s)
   ✓ git checkout main (0.1s)  
   ✓ git pull origin main (0.5s)
   🔨 Building release binary... (120s elapsed)
   ✓ Build successful
   ✓ Service re-registered (if applicable)
   ✅ Self-upgrade complete. Restarting in 3s...
   ```
4. After completion: spawn + exit (foreground) or `systemctl restart` (service)

Progress is reported via a `tokio::sync::mpsc::UnboundedSender<String>` channel connected to `edit_message_text`. Each step sends a status update.

Edge case: user sends `/self-upgrade feature-branch` — only relevant in source mode; release mode ignores the branch param.

### `self_upgrade` Tool (replaces `self_update_to_branch`)

Renamed and improved for LLM readability:

```rust
ToolDefinition {
    function: FunctionDefinition {
        name: "self_upgrade".to_string(),
        description: "Upgrade the bot to the latest version. \
Auto-detects whether running from source code (git pull + cargo build --release) \
or from a pre-built release binary (downloads latest from GitHub). If running as \
a systemd/launchd service, re-registers the service unit. Restarts the bot \
after successful upgrade. Use this when the user asks to update/upgrade the bot.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Git branch to build from (source mode only, default: 'main')"
                },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "source", "release"],
                    "description": "Force a specific upgrade mode (default: 'auto')"
                }
            },
            "required": []
        }),
    },
}
```

The tool returns a multi-line status log (same as now, but richer). After the tool returns, `restart_pending` is set and the restart happens after `process_message` completes.

### `restart_pending` Flow

**In `Agent` (`src/agent.rs`):**
```rust
pub restart_pending: AtomicBool,
```

Set in `execute_tool` after successful upgrade:
```rust
Ok(log) => {
    self.restart_pending.store(true, Ordering::Release);
    log
}
```

**In `src/platform/telegram.rs`:** after `process_message` returns:
```rust
if agent.restart_pending.load(Ordering::Acquire) {
    agent.restart_pending.store(false, Ordering::Release);
    restart_bot().await?; // shared restart logic
}
```

**Shared restart function:**
```rust
async fn restart_bot() -> Result<()> {
    if is_service_installed() {
        // Service mode: systemd/launchd handles restart
        restart_service().await
    } else {
        // Foreground mode: spawn child + exit
        let exe = std::env::current_exe()?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::Command::new(exe).args(&args).spawn()?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        std::process::exit(0);
    }
}
```

### Service Template Update

`scripts/services/rustfox.service.template`: change `Restart=on-failure` to `Restart=always` so that if systemd was tracking the original PID, it re-launches after the parent exits. (With the service restart path, systemctl stop/start is used directly, so this is more of a safety net.)

### Edge Cases

| Case | Behavior |
|------|----------|
| Build fails (source mode) | Error returned, no restart |
| No internet / API unreachable (release mode) | Error returned, no restart |
| No newer GitHub release | "Already up to date" message, no restart |
| Service restart fails | Error logged, old binary continues |
| User sends `/self-upgrade` while LLM tool is running | Command runs independently — two concurrent upgrades, last one wins |
| Spawn of new binary fails | Error returned, old process continues |

---

## Feature 2: `/models` Command

### OpenRouter Models API

`GET https://openrouter.ai/api/v1/models` returns 337 models. Key fields:

| Field | Type | Example |
|-------|------|---------|
| `id` | string | `"moonshotai/kimi-k2.6"` |
| `name` | string | `"MoonshotAI: Kimi K2.6"` |
| `pricing.prompt` | string (USD) | `"0.00000075"` |
| `pricing.completion` | string (USD) | `"0.0000035"` |
| `context_length` | int | `262144` |
| `architecture.modality` | string | `"text+image->text"` |

### Agent Changes (`src/agent.rs`)

Add fields:
```rust
pub current_model: tokio::sync::RwLock<String>,
pub config_path: PathBuf,        // for persisting config changes
```

Initialize in `Agent::new()`:
```rust
current_model: tokio::sync::RwLock::new(config.openrouter.model.clone()),
config_path,
```

Change `chat_completion` call (line ~558) to read from RwLock:
```rust
let model = self.current_model.read().await.clone();
let completion_result =
    self.llm.chat_completion_with_model(&prompt.messages, &all_tools, &model).await;
```

Add `set_model` method:
```rust
pub async fn set_model(&self, model_id: &str) -> Result<()> {
    // 1. Validate model_id is non-empty
    // 2. Read config.toml, parse as TOML table
    // 3. Navigate to ["openrouter"]["model"], set value
    // 4. Write back to self.config_path
    // 5. Update self.current_model RwLock
    // 6. Return Ok
}
```

Update `Agent::new()` call in `main.rs` to pass `config_path.clone()`.
Update `Agent::new()` signature to accept `config_path: PathBuf`.

Config persistence reuses the existing TOML-edit approach (`src/mcp.rs:128-167`):
1. `tokio::fs::read_to_string(config_path)`
2. `toml::from_str::<toml::value::Table>(&content)` — parse as raw TOML
3. Navigate to `["openrouter"]["model"]`, set the new value
4. `tokio::fs::write(config_path, toml::to_string_pretty(&doc))`

### Platform Command (`src/platform/telegram.rs`)

Add before the LLM processing section (alongside `/clear`, `/start`, etc.):

**`/models`** — show current model + instructions
**`/models <text>`** — smart dispatch:
1. Fetch model list from OpenRouter API (public endpoint, no auth required)
2. If exact match on `id` → save and hot-reload
3. Otherwise → case-insensitive search `id` and `name` fields → return top 10
4. If single result → auto-select
5. Show results with: "To select: `/models <id>`"

No state tracking needed — each `/models` invocation is self-contained.

### Hot-Reload Guarantee

After `set_model()` returns:
- `current_model` RwLock is updated
- Next `chat_completion_with_model` call reads the new model
- No process restart needed
- Config.toml is persisted on disk for next startup

### Edge Cases

| Case | Behavior |
|------|----------|
| OpenRouter API unreachable | Show informative error, offer retry |
| Model ID not found | Show closest matches |
| Config write fails (permissions) | RwLock still updated (runtime works), report write error |
| Empty model ID | Show usage help |
| `/models` with no args | Show current model + examples |

### Command Registration

Register `/models` and `/self-upgrade` in `supported_commands()` (`src/platform/telegram.rs`) so Telegram shows them in the "/" autocomplete menu.

### TOML Round-Trip

Config persistence uses `toml::from_str` / `toml::to_string_pretty` on the raw TOML table. Comments and formatting in `config.toml` are not preserved across round-trips. This is acceptable since `config.toml` is primarily machine-generated and the model field is a simple string value.

### Background Tasks

Internal LLM calls (query rewriter, summarizer, learning) use `self.llm.chat()` which reads `self.llm.config.model` directly — these are unaffected by the `/models` command and always use the original config model. This is intentional: only the main agent's interactive model is user-switchable.

---

## Files Changed Summary

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `self_update` dependency |
| `src/learning.rs` | Rewrite `self_update()` → unified `self_upgrade()` with source + release + service modes |
| `src/agent.rs` | Rename tool dispatch `self_update_to_branch` → `self_upgrade`; add `restart_pending: AtomicBool` + `current_model: RwLock<String>` + `config_path: PathBuf` + `set_model()` method + update `chat_completion` call site |
| `src/tools.rs` | Rename tool definition `self_update_to_branch` → `self_upgrade` with richer description and `mode` param |
| `src/llm.rs` | Add `ModelInfo` struct + `fetch_models()` method on `LlmClient` |
| `src/platform/telegram.rs` | Add `/self-upgrade` command with inline progress + `/models` command with smart matching + restart check after `process_message` + restart_bot() helper + `supported_commands()` updates |
| `src/platform/tool_notifier.rs` | Update display name `self_update_to_branch` → `self_upgrade` |
| `src/main.rs` | Pass `config_path` to `Agent::new()` |
| `scripts/services/rustfox.service.template` | Update systemd template: `Restart=on-failure` → `Restart=always` |

## No Other New Dependencies

Beyond `self_update`, both features use only:
- `std::sync::atomic::AtomicBool` (already in std)
- `tokio::sync::RwLock` (already a dependency)
- `toml::from_str` / `toml::to_string_pretty` (already in Cargo.toml)
- `reqwest` (already a dependency, used by LlmClient)
