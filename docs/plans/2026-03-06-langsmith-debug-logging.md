# LangSmith Debug Logging Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add opt-in LangSmith tracing (chain → llm → tool runs) and fix the silent "no Telegram response" bug.

**Architecture:** New `src/langsmith.rs` module with a fire-and-forget `LangSmithClient` that posts to the LangSmith REST API via tokio::spawn. Agent holds `Arc<LangSmithClient>`. Five debug-logging fixes in telegram/llm/agent.

**Tech Stack:** reqwest (existing), serde_json (existing), uuid (existing), chrono (existing), tokio::spawn (existing). Zero new crates.

---

## Task 1: Add `LangSmithConfig` to config

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

**Step 1: Write a failing test for config deserialization**

Add to the bottom of `src/config.rs`, inside a new `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_langsmith_config_optional() {
        // Config without [langsmith] must succeed — it's fully optional
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.langsmith.is_none());
    }

    #[test]
    fn test_langsmith_config_parses() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [langsmith]
            api_key = "ls__test"
            project = "my-project"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let ls = cfg.langsmith.unwrap();
        assert_eq!(ls.api_key, "ls__test");
        assert_eq!(ls.project, "my-project");
    }

    #[test]
    fn test_langsmith_config_default_project() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [langsmith]
            api_key = "ls__test"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let ls = cfg.langsmith.unwrap();
        assert_eq!(ls.project, "default");
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test test_langsmith_config --lib 2>&1 | head -30
```

Expected: compile error — `Config` has no `langsmith` field yet.

**Step 3: Add `LangSmithConfig` struct and field to `src/config.rs`**

After the `AgentConfig` struct (around line 91), add:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LangSmithConfig {
    pub api_key: String,
    #[serde(default = "default_langsmith_project")]
    pub project: String,
    #[serde(default = "default_langsmith_base_url")]
    pub base_url: String,
}
```

Add the default functions after `default_agent_config`:

```rust
fn default_langsmith_project() -> String {
    "default".to_string()
}

fn default_langsmith_base_url() -> String {
    "https://api.smith.langchain.com".to_string()
}
```

Add the field to `Config` struct (after `pub agent: AgentConfig,`):

```rust
#[serde(default)]
pub langsmith: Option<LangSmithConfig>,
```

**Step 4: Run tests to verify they pass**

```bash
cargo test test_langsmith_config --lib 2>&1
```

Expected: all 3 tests PASS.

**Step 5: Update `config.example.toml`**

Append after the `# [agent]` block comment:

```toml
# LangSmith observability (optional)
# Traces every LLM call and tool execution for debugging in the LangSmith UI.
# Get your API key at https://smith.langchain.com → Settings → API Keys
# [langsmith]
# api_key = "ls__..."
# project = "rustfox"   # LangSmith project name (default: "default")
```

**Step 6: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

**Step 7: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat(config): add optional LangSmithConfig"
```

---

## Task 2: Create `src/langsmith.rs`

**Files:**
- Create: `src/langsmith.rs`
- Modify: `src/main.rs` (add `mod langsmith;`)

**Step 1: Write failing tests first**

Create `src/langsmith.rs` with the test module only:

```rust
use serde::Serialize;
use std::sync::Arc;
use tracing::warn;

use crate::config::LangSmithConfig;

pub struct LangSmithClient {
    inner: Option<LangSmithInner>,
}

struct LangSmithInner {
    client: reqwest::Client,
    api_key: String,
    project: String,
    base_url: String,
}

#[derive(Debug, Clone)]
pub struct RunParams {
    pub id: String,
    pub name: String,
    pub run_type: RunType,
    pub parent_run_id: Option<String>,
    pub inputs: serde_json::Value,
    pub session_name: String,
    pub start_time: String,
}

#[derive(Debug, Clone)]
pub struct EndRunParams {
    pub id: String,
    pub outputs: Option<serde_json::Value>,
    pub error: Option<String>,
    pub end_time: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunType {
    Chain,
    Llm,
    Tool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_when_no_config() {
        let client = LangSmithClient::new(None);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_enabled_when_config_present() {
        let cfg = LangSmithConfig {
            api_key: "ls__test".to_string(),
            project: "test".to_string(),
            base_url: "https://api.smith.langchain.com".to_string(),
        };
        let client = LangSmithClient::new(Some(&cfg));
        assert!(client.is_enabled());
    }

    #[test]
    fn test_run_type_serializes_lowercase() {
        let json = serde_json::to_string(&RunType::Llm).unwrap();
        assert_eq!(json, r#""llm""#);
        let json = serde_json::to_string(&RunType::Chain).unwrap();
        assert_eq!(json, r#""chain""#);
        let json = serde_json::to_string(&RunType::Tool).unwrap();
        assert_eq!(json, r#""tool""#);
    }
}
```

**Step 2: Run tests to confirm they fail**

```bash
cargo test --lib langsmith 2>&1 | head -20
```

Expected: compile error — methods `new`, `is_enabled` not found.

**Step 3: Implement `LangSmithClient`**

Replace the file with the full implementation:

```rust
use serde::Serialize;
use tracing::warn;

use crate::config::LangSmithConfig;

pub struct LangSmithClient {
    inner: Option<LangSmithInner>,
}

struct LangSmithInner {
    client: reqwest::Client,
    api_key: String,
    project: String,
    base_url: String,
}

#[derive(Debug, Clone)]
pub struct RunParams {
    pub id: String,
    pub name: String,
    pub run_type: RunType,
    pub parent_run_id: Option<String>,
    pub inputs: serde_json::Value,
    pub session_name: String,
    pub start_time: String,
}

#[derive(Debug, Clone)]
pub struct EndRunParams {
    pub id: String,
    pub outputs: Option<serde_json::Value>,
    pub error: Option<String>,
    pub end_time: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunType {
    Chain,
    Llm,
    Tool,
}

impl LangSmithClient {
    pub fn new(config: Option<&LangSmithConfig>) -> Self {
        let inner = config.map(|cfg| LangSmithInner {
            client: reqwest::Client::new(),
            api_key: cfg.api_key.clone(),
            project: cfg.project.clone(),
            base_url: cfg.base_url.clone(),
        });
        Self { inner }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Fire-and-forget: POST /runs to start a run.
    pub fn start_run(&self, params: RunParams) {
        let Some(inner) = &self.inner else { return };
        let client = inner.client.clone();
        let api_key = inner.api_key.clone();
        let url = format!("{}/runs", inner.base_url);

        tokio::spawn(async move {
            let mut body = serde_json::json!({
                "id": params.id,
                "name": params.name,
                "run_type": params.run_type,
                "inputs": params.inputs,
                "start_time": params.start_time,
                "session_name": params.session_name,
            });
            if let Some(parent) = params.parent_run_id {
                body["parent_run_id"] = serde_json::Value::String(parent);
            }

            match client
                .post(&url)
                .header("x-api-key", &api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!("LangSmith POST /runs {} failed: {} — {}", params.name, status, &text[..text.len().min(200)]);
                }
                Err(e) => warn!("LangSmith POST /runs {}: {}", params.name, e),
                _ => {}
            }
        });
    }

    /// Fire-and-forget: PATCH /runs/{id} to finish a run.
    pub fn end_run(&self, params: EndRunParams) {
        let Some(inner) = &self.inner else { return };
        let client = inner.client.clone();
        let api_key = inner.api_key.clone();
        let url = format!("{}/runs/{}", inner.base_url, params.id);

        tokio::spawn(async move {
            let mut body = serde_json::json!({
                "end_time": params.end_time,
            });
            if let Some(outputs) = params.outputs {
                body["outputs"] = outputs;
            }
            if let Some(error) = params.error {
                body["error"] = serde_json::Value::String(error);
            }

            match client
                .patch(&url)
                .header("x-api-key", &api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!("LangSmith PATCH /runs/{} failed: {} — {}", params.id, status, &text[..text.len().min(200)]);
                }
                Err(e) => warn!("LangSmith PATCH /runs/{}: {}", params.id, e),
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_when_no_config() {
        let client = LangSmithClient::new(None);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_enabled_when_config_present() {
        let cfg = LangSmithConfig {
            api_key: "ls__test".to_string(),
            project: "test".to_string(),
            base_url: "https://api.smith.langchain.com".to_string(),
        };
        let client = LangSmithClient::new(Some(&cfg));
        assert!(client.is_enabled());
    }

    #[test]
    fn test_run_type_serializes_lowercase() {
        let json = serde_json::to_string(&RunType::Llm).unwrap();
        assert_eq!(json, r#""llm""#);
        let json = serde_json::to_string(&RunType::Chain).unwrap();
        assert_eq!(json, r#""chain""#);
        let json = serde_json::to_string(&RunType::Tool).unwrap();
        assert_eq!(json, r#""tool""#);
    }
}
```

**Step 4: Register the module in `src/main.rs`**

Add `mod langsmith;` after `mod llm;` (line 4):

```rust
mod langsmith;
```

**Step 5: Run tests**

```bash
cargo test --lib langsmith 2>&1
```

Expected: all 3 tests PASS.

**Step 6: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

**Step 7: Commit**

```bash
git add src/langsmith.rs src/main.rs
git commit -m "feat(langsmith): add LangSmithClient with fire-and-forget REST tracing"
```

---

## Task 3: Wire `LangSmithClient` into `Agent`

**Files:**
- Modify: `src/agent.rs`
- Modify: `src/main.rs`

**Step 1: Add `langsmith` field to `Agent`**

In `src/agent.rs`, add the import after line 15:

```rust
use crate::langsmith::LangSmithClient;
```

Add the field to the `Agent` struct (after `pub job_tx: ...`):

```rust
pub langsmith: std::sync::Arc<LangSmithClient>,
```

**Step 2: Update `Agent::new()` signature and body**

Add parameter to `Agent::new()`:

```rust
langsmith: std::sync::Arc<LangSmithClient>,
```

Add to the `Self { ... }` initializer:

```rust
langsmith,
```

**Step 3: Update `main.rs` to construct and pass `LangSmithClient`**

In `src/main.rs`, add import after `use crate::config::Config;`:

```rust
use crate::langsmith::LangSmithClient;
```

After the `info!("  MCP servers: ...")` log line, add:

```rust
let langsmith = std::sync::Arc::new(LangSmithClient::new(config.langsmith.as_ref()));
if langsmith.is_enabled() {
    info!("  LangSmith: enabled (project: {})", config.langsmith.as_ref().unwrap().project);
} else {
    info!("  LangSmith: disabled (no [langsmith] config)");
}
```

Pass `langsmith` as the last argument to `Agent::new()` inside `Arc::new_cyclic`:

```rust
Arc::new_cyclic(|weak| {
    Agent::new(
        config.clone(),
        mcp_manager,
        memory.clone(),
        skills,
        task_store.clone(),
        Arc::clone(&scheduler),
        Arc::clone(&bot),
        weak.clone(),
        job_tx,
        Arc::clone(&langsmith),   // ← add this
    )
});
```

**Step 4: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

**Step 5: Commit**

```bash
git add src/agent.rs src/main.rs
git commit -m "feat(agent): add LangSmithClient field and wire through main"
```

---

## Task 4: Instrument `process_message()` with LangSmith traces

**Files:**
- Modify: `src/agent.rs`

This is the core tracing task. All run IDs are generated at the start; parent-child linking is done via `parent_run_id`.

**Step 1: Add a helper at the top of the `impl Agent` block**

Add this private helper right before `process_message`:

```rust
fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
```

**Step 2: Write a unit test for the helper**

In the `#[cfg(test)]` block of `agent.rs` (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now_iso8601_is_valid_rfc3339() {
        let ts = Agent::now_iso8601_static();
        // Must parse without error
        chrono::DateTime::parse_from_rfc3339(&ts).unwrap();
        // Must end with Z (UTC)
        assert!(ts.ends_with('Z'), "timestamp must be UTC: {}", ts);
    }
}
```

Note: rename the private helper to a `pub(crate)` static method so tests can call it:

```rust
pub(crate) fn now_iso8601_static() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
```

And in `process_message`, use `Self::now_iso8601_static()`.

**Step 3: Run test**

```bash
cargo test test_now_iso8601 --lib 2>&1
```

Expected: PASS.

**Step 4: Instrument `process_message()` with chain + llm + tool runs**

Replace the agentic loop section in `process_message()` (from the `// Agentic loop` comment through `return Ok(...)`) with the instrumented version:

```rust
// --- LangSmith: start root chain run ---
let chain_run_id = uuid::Uuid::new_v4().to_string();
let ls_project = self
    .config
    .langsmith
    .as_ref()
    .map(|l| l.project.as_str())
    .unwrap_or("default")
    .to_string();

self.langsmith.start_run(crate::langsmith::RunParams {
    id: chain_run_id.clone(),
    name: "rustfox_request".to_string(),
    run_type: crate::langsmith::RunType::Chain,
    parent_run_id: None,
    inputs: serde_json::json!({ "message": incoming.text }),
    session_name: ls_project.clone(),
    start_time: Self::now_iso8601_static(),
});

// Agentic loop — keep calling LLM until we get a non-tool response
let max_iterations = self.config.max_iterations();
let mut iteration_count = 0u32;

for iteration in 0..max_iterations {
    debug!("Trying iteration {}: messages length: {}", iteration, messages.len());

    // --- LangSmith: start llm run (child of chain) ---
    let llm_run_id = uuid::Uuid::new_v4().to_string();
    let llm_start = Self::now_iso8601_static();
    self.langsmith.start_run(crate::langsmith::RunParams {
        id: llm_run_id.clone(),
        name: "llm_call".to_string(),
        run_type: crate::langsmith::RunType::Llm,
        parent_run_id: Some(chain_run_id.clone()),
        inputs: serde_json::json!({ "messages": messages }),
        session_name: ls_project.clone(),
        start_time: llm_start,
    });

    let response = self.llm.chat(&messages, &all_tools).await;

    // Handle LLM errors
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            self.langsmith.end_run(crate::langsmith::EndRunParams {
                id: llm_run_id,
                outputs: None,
                error: Some(format!("{:#}", e)),
                end_time: Self::now_iso8601_static(),
            });
            self.langsmith.end_run(crate::langsmith::EndRunParams {
                id: chain_run_id,
                outputs: None,
                error: Some(format!("{:#}", e)),
                end_time: Self::now_iso8601_static(),
            });
            return Err(e);
        }
    };

    // --- LangSmith: end llm run ---
    self.langsmith.end_run(crate::langsmith::EndRunParams {
        id: llm_run_id,
        outputs: Some(serde_json::json!({
            "choices": [{
                "message": {
                    "role": response.role,
                    "content": response.content,
                    "tool_calls": response.tool_calls,
                }
            }]
        })),
        error: None,
        end_time: Self::now_iso8601_static(),
    });

    if let Some(tool_calls) = &response.tool_calls {
        if !tool_calls.is_empty() {
            info!(
                "LLM requested {} tool call(s) (iteration {})",
                tool_calls.len(),
                iteration
            );

            // Save assistant message with tool calls
            self.memory
                .save_message(&conversation_id, &response)
                .await?;
            messages.push(response.clone());

            // Execute each tool call
            for tool_call in tool_calls {
                let arguments: serde_json::Value =
                    serde_json::from_str(&tool_call.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                // --- LangSmith: start tool run (child of chain) ---
                let tool_run_id = uuid::Uuid::new_v4().to_string();
                self.langsmith.start_run(crate::langsmith::RunParams {
                    id: tool_run_id.clone(),
                    name: tool_call.function.name.clone(),
                    run_type: crate::langsmith::RunType::Tool,
                    parent_run_id: Some(chain_run_id.clone()),
                    inputs: serde_json::json!({ "arguments": arguments }),
                    session_name: ls_project.clone(),
                    start_time: Self::now_iso8601_static(),
                });

                let tool_result = self
                    .execute_tool(&tool_call.function.name, &arguments, user_id, chat_id)
                    .await;

                info!(
                    "Tool '{}' result length: {} chars",
                    tool_call.function.name,
                    tool_result.len()
                );
                debug!("Tool '{}' result: {}", tool_call.function.name, tool_result);

                // --- LangSmith: end tool run ---
                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: tool_run_id,
                    outputs: Some(serde_json::json!({ "result": tool_result })),
                    error: None,
                    end_time: Self::now_iso8601_static(),
                });

                let tool_msg = ChatMessage {
                    role: "tool".to_string(),
                    content: Some(tool_result),
                    tool_calls: None,
                    tool_call_id: Some(tool_call.id.clone()),
                };
                self.memory
                    .save_message(&conversation_id, &tool_msg)
                    .await?;
                messages.push(tool_msg);
            }

            iteration_count = iteration + 1;
            continue;
        }
    }

    // Final response — no tool calls
    let content = response.content.clone().unwrap_or_default();

    if content.is_empty() {
        warn!(
            user_id = %user_id,
            iteration = iteration,
            "LLM returned empty content with no tool calls — bot will send nothing"
        );
    }

    self.memory
        .save_message(&conversation_id, &response)
        .await?;

    // --- LangSmith: end chain run (success) ---
    self.langsmith.end_run(crate::langsmith::EndRunParams {
        id: chain_run_id,
        outputs: Some(serde_json::json!({
            "response": content,
            "iterations": iteration,
        })),
        error: None,
        end_time: Self::now_iso8601_static(),
    });

    return Ok(content);
}

// Reached max iterations
warn!(
    user_id = %user_id,
    max_iterations = max_iterations,
    iteration_count = iteration_count,
    "Reached max iterations without final text response"
);

// --- LangSmith: end chain run (max iterations) ---
self.langsmith.end_run(crate::langsmith::EndRunParams {
    id: chain_run_id,
    outputs: None,
    error: Some(format!("Reached max iterations ({})", max_iterations)),
    end_time: Self::now_iso8601_static(),
});

Ok("I've reached the maximum number of tool call iterations. Please try rephrasing your request.".to_string())
```

**Step 5: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors. Fix any field name mismatches by referring to the existing struct definitions.

**Step 6: Run all tests**

```bash
cargo test 2>&1
```

Expected: all passing.

**Step 7: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): instrument process_message with LangSmith chain/llm/tool traces"
```

---

## Task 5: Fix silent Telegram send errors and add delivery logging

**Files:**
- Modify: `src/platform/telegram.rs`

**Step 1: Write a unit test for the message splitter (the function already exists)**

In `src/platform/telegram.rs`, add at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_message_empty_response_produces_no_chunks() {
        let chunks = split_message("", 4000);
        // An empty string should still produce one empty chunk or none —
        // currently it returns [""] which means send_message sends empty text.
        // This test documents the behavior so we notice if it changes.
        assert!(chunks.len() <= 1);
    }

    #[test]
    fn test_split_message_short_stays_intact() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_message_long_splits_at_boundary() {
        let text = "a ".repeat(3000); // 6000 chars
        let chunks = split_message(&text, 4000);
        assert_eq!(chunks.len(), 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 4000);
        }
    }
}
```

**Step 2: Run tests**

```bash
cargo test --lib platform 2>&1
```

Expected: all 3 PASS (documenting existing behavior).

**Step 3: Fix the silent send error in `handle_message()`**

Locate the `Ok(response)` arm in `handle_message()` (around line 164):

```rust
// Before:
Ok(response) => {
    for chunk in split_message(&response, 4000) {
        bot.send_message(msg.chat.id, chunk).await.ok();
    }
}

// After:
Ok(response) => {
    if response.is_empty() {
        warn!(
            user_id = user_id,
            "Agent returned empty response — nothing will be sent to Telegram"
        );
    }
    let chunks = split_message(&response, 4000);
    let total = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        if chunk.is_empty() {
            continue;
        }
        match bot.send_message(msg.chat.id, &chunk).await {
            Ok(_) => {
                if total > 1 {
                    info!("Sent Telegram chunk {}/{} ({} chars)", i + 1, total, chunk.len());
                }
            }
            Err(e) => {
                error!(
                    user_id = user_id,
                    chunk = i + 1,
                    total_chunks = total,
                    "Failed to send Telegram message: {:#}", e
                );
            }
        }
    }
}
```

**Step 4: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

**Step 5: Run tests**

```bash
cargo test 2>&1
```

Expected: all passing.

**Step 6: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "fix(telegram): log Telegram send failures instead of silently discarding them"
```

---

## Task 6: Add debug logging to `llm.rs`

**Files:**
- Modify: `src/llm.rs`

**Step 1: Write a test for the response parser path**

The existing tests in `llm.rs` test serialization. Add one more that verifies finish_reason detection (note: the `Choice` struct currently has no `finish_reason` — we need to add it for logging):

```rust
#[test]
fn test_chat_response_deserializes_finish_reason() {
    let json = r#"{
        "choices": [{
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop"
        }]
    }"#;
    let resp: ChatResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
}
```

**Step 2: Run test to confirm it fails**

```bash
cargo test test_chat_response_deserializes_finish_reason --lib 2>&1 | head -20
```

Expected: fail — `finish_reason` field missing on `Choice`.

**Step 3: Add `finish_reason` to `Choice` struct and log it**

Modify `Choice` in `src/llm.rs`:

```rust
#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}
```

In `chat_with_model()`, after the `debug!("Sending request...")` log, replace with more detail:

```rust
debug!(
    url = %url,
    model = %model,
    message_count = messages.len(),
    tool_count = tools.len(),
    "Sending request to OpenRouter"
);
```

After `chat_response` is parsed, add before the `into_iter()` call:

```rust
if let Some(choice) = chat_response.choices.first() {
    debug!(
        finish_reason = ?choice.finish_reason,
        has_content = choice.message.content.is_some(),
        tool_call_count = choice.message.tool_calls.as_ref().map_or(0, |t| t.len()),
        "Received LLM response"
    );
    if choice.message.content.as_deref().map_or(true, str::is_empty)
        && choice.message.tool_calls.as_ref().map_or(true, Vec::is_empty)
    {
        warn!(
            finish_reason = ?choice.finish_reason,
            "LLM returned no content and no tool calls"
        );
    }
}
```

**Step 4: Run tests**

```bash
cargo test 2>&1
```

Expected: all passing including the new `test_chat_response_deserializes_finish_reason`.

**Step 5: Verify clippy**

```bash
cargo clippy -- -D warnings 2>&1
```

Expected: no warnings.

**Step 6: Commit**

```bash
git add src/llm.rs
git commit -m "fix(llm): add finish_reason to response and improve debug logging"
```

---

## Task 7: Final verification and push

**Step 1: Run the full CI suite locally**

```bash
cargo fmt --all -- --check 2>&1
cargo clippy -- -D warnings 2>&1
cargo test 2>&1
cargo build --release 2>&1
```

All must pass. If `cargo fmt` fails, run `cargo fmt` then commit the formatting fix.

**Step 2: Fix any fmt issues**

```bash
cargo fmt
git add -p   # stage only formatting changes
git commit -m "style: cargo fmt"
```

**Step 3: Push to branch**

```bash
git push -u origin claude/langsmith-debug-logging-OqC3H
```

---

## Testing LangSmith Integration End-to-End

After deploying with `[langsmith]` configured in `config.toml`:

1. Send a message to the bot in Telegram
2. Open [https://smith.langchain.com](https://smith.langchain.com) → your project
3. You should see a `rustfox_request` chain run with child `llm_call` and `tool_*` runs
4. Each run shows inputs, outputs, start/end timestamps, and any errors

To test the debug logging fix for silent failures:
- Check logs for `warn!` messages about empty responses
- Set `RUST_LOG=debug` when running to see full LLM request/response logs:
  ```bash
  RUST_LOG=debug cargo run
  ```

---

## Summary of Changes

| File | What Changes |
|---|---|
| `src/config.rs` | Add `LangSmithConfig` + `langsmith: Option<LangSmithConfig>` in `Config` |
| `src/langsmith.rs` | New: `LangSmithClient` with fire-and-forget `start_run` / `end_run` |
| `src/agent.rs` | Add `langsmith` field; instrument `process_message()`; add empty-response warn |
| `src/main.rs` | Add `mod langsmith`; construct and pass `LangSmithClient` |
| `src/platform/telegram.rs` | Fix silent `.ok()` swallow; log chunk delivery and empty responses |
| `src/llm.rs` | Add `finish_reason` to `Choice`; improve debug log detail |
| `config.example.toml` | Document optional `[langsmith]` section |
