# Self-Update Restart & `/models` Command

**Date:** 2026-06-16
**Branch:** `feat/self-update-models-command`

## Overview

Two features to improve RustFox's runtime management:

1. **Self-update restart** — after `self_update_to_branch` rebuilds the binary, automatically restart the process so the new binary takes effect.
2. **`/models` command** — browse and change the OpenRouter model at runtime, persisted to config.toml and hot-reloaded without restart.

Both features require no new dependencies.

---

## Feature 1: Self-Update Restart

### Current State

`self_update_to_branch` tool (`src/learning.rs:431-472`) correctly:
- `git fetch --all`
- `git checkout <branch>`
- `git pull origin <branch>`
- `cargo build --release`
- Returns a multi-line status log

But the running process is never replaced. The caller docstring says "the caller should arrange restart" — but no restart mechanism exists.

### Design

**Add `restart_pending: AtomicBool` to Agent** (`src/agent.rs`):

```rust
// New field on Agent
pub restart_pending: AtomicBool,
```

**In `execute_tool` (`src/agent.rs`, `self_update_to_branch` arm):** after successful build (line ~2537), set `restart_pending = true`:

```rust
Ok(log) => {
    self.restart_pending.store(true, Ordering::Release);
    log
}
```

**In `src/platform/telegram.rs`:** after `process_message` returns, check and act:

```rust
let response = agent.process_message(&msg, ...).await?;
// ... send response to user ...

if agent.restart_pending.load(Ordering::Acquire) {
    agent.restart_pending.store(false, Ordering::Release);
    // Spawn new binary as independent child process
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new(exe)
        .args(&args)
        .spawn()?;
    // Brief delay for Telegram delivery
    tokio::time::sleep(Duration::from_secs(1)).await;
    std::process::exit(0);
}
```

**In `scripts/services/rustfox.service.template`:** update systemd service template from `Restart=on-failure` to `Restart=always` so that if systemd is tracking the original PID, it re-launches. (The spawned child is independent, so this is a safety net, not the primary mechanism.)

### Why Not Detect systemd?

The simplest universal approach: spawn the new binary as a child process before exiting. This works identically under systemd, foreground terminal, or container. No need to detect `INVOCATION_ID` or `NOTIFY_SOCKET`.

### Edge Cases

| Case | Behavior |
|------|----------|
| Build fails | `restart_pending` not set, normal error returned |
| User sends another message quickly | Restart happens after first response only |
| New binary fails to start | Old process has already exited — user starts the binary manually (or systemd restart) |
| Spawn of new binary fails | Error propagates to caller, old process continues running, restart aborted |
| Multiple rapid updates | Each tool call sets `restart_pending`, only the last triggers restart |

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

**Add fields to Agent:**

```rust
pub current_model: tokio::sync::RwLock<String>,
pub config_path: PathBuf,        // for persisting config changes
```

Initialize in `Agent::new()`:
```rust
current_model: tokio::sync::RwLock::new(config.openrouter.model.clone()),
config_path,
```

**Change chat_completion call** (line ~558) to read from RwLock:
```rust
let model = self.current_model.read().await.clone();
let completion_result =
    self.llm.chat_completion_with_model(&prompt.messages, &all_tools, &model).await;
```

**Add `set_model` method:**
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

**Update `Agent::new()` call in `main.rs`** to pass `config_path.clone()`.
**Update `Agent::new()` signature** to accept `config_path: PathBuf`.

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

**No state tracking needed** — each `/models` invocation is self-contained. The model list is fetched fresh each time.

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

---

### Command Registration

Register `/models` in `supported_commands()` (`src/platform/telegram.rs`) so Telegram shows it in the "/" autocomplete menu alongside `/clear`, `/tools`, `/skills`, etc.

### TOML Round-Trip

Config persistence uses `toml::from_str` / `toml::to_string_pretty` on the raw TOML table. Comments and formatting in `config.toml` are not preserved across round-trips. This is acceptable since `config.toml` is primarily machine-generated and the model field is a simple string value.

### Background Tasks

Internal LLM calls (query rewriter, summarizer, learning) use `self.llm.chat()` which reads `self.llm.config.model` directly — these are unaffected by the `/models` command and always use the original config model. This is intentional: only the main agent's interactive model is user-switchable.

---

## Files Changed Summary

| File | Changes |
|------|---------|
| `src/agent.rs` | Add `restart_pending: AtomicBool` + `current_model: RwLock<String>` + `config_path: PathBuf` + `set_model()` method + update `chat_completion` call site |
| `src/llm.rs` | Add `ModelInfo` struct + `fetch_models()` method on `LlmClient` |
| `src/platform/telegram.rs` | Add `/models` command handler with smart matching + restart check after `process_message` |
| `src/main.rs` | Pass `config_path` to `Agent::new()` |
| `scripts/services/rustfox.service.template` | Update systemd template: `Restart=on-failure` → `Restart=always` |

## No New Dependencies

Both features use only:
- `std::sync::atomic::AtomicBool` (already in std)
- `tokio::sync::RwLock` (already a dependency)
- `toml::from_str` / `toml::to_string_pretty` (already in Cargo.toml)
- `reqwest` (already a dependency, used by LlmClient)
