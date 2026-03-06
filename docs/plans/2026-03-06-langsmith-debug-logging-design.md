# Design: LangSmith Tracing + Debug Logging Improvements

**Date:** 2026-03-06
**Branch:** `claude/langsmith-debug-logging-OqC3H`
**Status:** Approved

---

## Problem Statement

1. **No LLM observability** — there is no way to inspect what messages are sent to OpenRouter, what tool calls are made, or how long each step takes.
2. **Silent "no response" bugs** — the bot sometimes processes a request without sending a Telegram reply. Root cause identified: `bot.send_message().await.ok()` in `telegram.rs:167` silently discards all Telegram send errors. Additional gaps: empty LLM responses are not logged, and LLM request/response bodies are not logged at debug level.

---

## Goals

- Add **LangSmith tracing** so every user request, LLM call, and tool execution is visible in the LangSmith UI.
- Fix silent failure points so any "no response" situation is logged clearly.
- Zero impact on users without a LangSmith account (opt-in via config).
- No new crate dependencies.

---

## Approach

Use the **LangSmith REST API directly** via `reqwest` (already a dependency). No official Rust SDK exists. Traces are sent **fire-and-forget** via `tokio::spawn` — network errors are logged as `warn!` but never propagate to bot response latency or correctness.

---

## Architecture

### New module: `src/langsmith.rs`

```rust
pub struct LangSmithClient {
    client: reqwest::Client,
    api_key: String,
    project: String,
    base_url: String,
    enabled: bool,   // false when no api_key configured
}

impl LangSmithClient {
    pub fn new(config: Option<&LangSmithConfig>) -> Self { ... }

    /// POST /runs — fire-and-forget
    pub fn start_run(&self, params: StartRunParams) { ... }

    /// PATCH /runs/{id} — fire-and-forget
    pub fn end_run(&self, params: EndRunParams) { ... }
}
```

`Agent` holds `Arc<LangSmithClient>` constructed at startup.

### Trace hierarchy per user message

```
[chain] "rustfox_request"            ← root: full user message → final response
  ├── [llm]  "llm_call"              ← iteration 0: messages + LLM response
  ├── [tool] "read_file"             ← tool executed
  ├── [tool] "run_command"           ← another tool
  ├── [llm]  "llm_call"              ← iteration 1
  └── ...final assistant message
```

Parent-child linking via `parent_run_id` field in the POST body.

### HTTP protocol

```
POST  https://api.smith.langchain.com/runs
PATCH https://api.smith.langchain.com/runs/{run_id}

Headers:
  x-api-key: <LANGSMITH_API_KEY>
  Content-Type: application/json
```

**Chain run start body:**
```json
{
  "id": "<uuid>",
  "name": "rustfox_request",
  "run_type": "chain",
  "inputs": { "message": "<user text>" },
  "start_time": "<ISO8601>",
  "session_name": "<project name>",
  "extra": { "user_id": "...", "platform": "telegram" }
}
```

**LLM run start body:**
```json
{
  "id": "<uuid>",
  "name": "llm_call",
  "run_type": "llm",
  "inputs": { "messages": [...chat history...] },
  "start_time": "<ISO8601>",
  "session_name": "<project>",
  "parent_run_id": "<chain run id>"
}
```

**LLM run end body (PATCH):**
```json
{
  "outputs": {
    "choices": [{"message": {"role": "assistant", "content": "...", "tool_calls": [...]}}]
  },
  "end_time": "<ISO8601>"
}
```

**Tool run start body:**
```json
{
  "id": "<uuid>",
  "name": "<tool name>",
  "run_type": "tool",
  "inputs": { "arguments": {...} },
  "start_time": "<ISO8601>",
  "session_name": "<project>",
  "parent_run_id": "<chain run id>"
}
```

**Tool run end body (PATCH):**
```json
{
  "outputs": { "result": "..." },
  "end_time": "<ISO8601>"
}
```

---

## Config Changes

### `src/config.rs` — new optional section

```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct LangSmithConfig {
    pub api_key: String,
    #[serde(default = "default_langsmith_project")]
    pub project: String,
    #[serde(default = "default_langsmith_base_url")]
    pub base_url: String,
}
```

Added to `Config`:
```rust
#[serde(default)]
pub langsmith: Option<LangSmithConfig>,
```

### `config.toml` / `config.example.toml` — new optional section

```toml
# Optional: LangSmith observability
# [langsmith]
# api_key = "ls__..."
# project = "rustfox"   # default: "default"
```

---

## Debug Logging Fixes

### Fix 1 — Silent Telegram send failure (`telegram.rs:167`)

**Before:**
```rust
bot.send_message(msg.chat.id, chunk).await.ok();
```

**After:**
```rust
if let Err(e) = bot.send_message(msg.chat.id, &chunk).await {
    error!("Failed to send Telegram message chunk ({} chars): {:#}", chunk.len(), e);
}
```

This is the most likely cause of "processed but no reply" symptoms.

### Fix 2 — Log empty LLM response (`agent.rs`)

```rust
let content = response.content.clone().unwrap_or_default();
if content.is_empty() && response.tool_calls.as_ref().map_or(true, |t| t.is_empty()) {
    warn!("LLM returned empty content with no tool calls on iteration {}", iteration);
}
```

### Fix 3 — Debug-log LLM request (`llm.rs`)

Add before the HTTP call:
```rust
debug!(
    model = %self.config.model,
    message_count = messages.len(),
    tool_count = tools.len(),
    "Sending chat request to OpenRouter"
);
```

Add after response parsed:
```rust
debug!(
    finish_reason = ?response.choices[0].finish_reason,
    has_content = response.choices[0].message.content.is_some(),
    tool_call_count = response.choices[0].message.tool_calls.as_ref().map_or(0, |t| t.len()),
    "Received LLM response"
);
```

### Fix 4 — Log successful Telegram chunk delivery

```rust
info!("Sent Telegram chunk {}/{} ({} chars)", i+1, total_chunks, chunk.len());
```

### Fix 5 — Log agentic loop exit reason

```rust
// At max iterations:
warn!(
    user_id = %incoming.user_id,
    iterations = max_iterations,
    "Reached max iterations without final response"
);
```

---

## Files to Change

| File | Change |
|---|---|
| `src/config.rs` | Add `LangSmithConfig` struct + `langsmith: Option<LangSmithConfig>` field |
| `src/langsmith.rs` | New file: `LangSmithClient` with `start_run` / `end_run` |
| `src/agent.rs` | Add `langsmith: Arc<LangSmithClient>` field; instrument `process_message()` |
| `src/main.rs` | Construct `LangSmithClient` from config, pass to `Agent::new()` |
| `src/platform/telegram.rs` | Fix silent send error; log chunk delivery |
| `src/llm.rs` | Add debug logs for request/response |
| `config.example.toml` | Document optional `[langsmith]` section |

---

## Error Handling

- LangSmith HTTP errors → `warn!("{} POST /runs failed: {} {:?}", name, status, body_snippet)`
- `tokio::spawn` isolates all LangSmith I/O from the main bot task
- If `enabled = false` (no config) → all methods are no-ops, zero overhead

---

## Non-Goals

- No evaluation datasets or LangSmith feedback buttons (scope creep)
- No OpenTelemetry integration
- No LangSmith SDK dependency
- No changes to MCP tracing (out of scope for now)

---

## References

- [LangSmith Trace with API](https://docs.smith.langchain.com/observability/how_to_guides/trace_with_api)
- [langsmith-cookbook REST example](https://github.com/langchain-ai/langsmith-cookbook/blob/main/tracing-examples/rest/rest.ipynb)
- [LangSmith OpenAPI spec](https://github.com/langchain-ai/langsmith-sdk/blob/main/openapi/openapi.yaml)
- [LangSmith Deep Dive — Beyond the Docs](https://medium.com/@aviadr1/langsmith-tracing-deep-dive-beyond-the-docs-75016c91f747)
- [Advanced LangSmith Tracing 2025](https://sparkco.ai/blog/advanced-langsmith-tracing-techniques-in-2025)
