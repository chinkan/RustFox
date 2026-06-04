# Empty LLM Response Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent RustFox from silently completing Telegram requests when OpenRouter returns an assistant message with no content and no tool calls.

**Architecture:** Add an explicit chat-completion wrapper that preserves `finish_reason`, classify empty assistant responses after Kimi tool-call parsing, retry invalid empty responses with a configurable budget, and compact large tool-heavy prompts only in process memory before LLM calls. Persistent memory and Telegram streaming stay unchanged except that exhausted empty-response recovery returns an error through the existing visible error path.

**Tech Stack:** Rust 2021, Tokio, reqwest, serde, anyhow, tracing, LangSmith HTTP tracing, existing RustFox memory and Telegram platform modules.

---

## File Map

- Modify: `src/config.rs` - add `empty_response_retry_limit` config, default value, accessor, tests.
- Modify: `config.example.toml` - document `[agent].empty_response_retry_limit = 3`.
- Modify: `src/llm.rs` - add `ChatCompletion`, preserve `finish_reason`, expose completion-returning methods, add empty-response classifier tests.
- Create: `src/agent_prompt.rs` - prompt-size estimation, in-memory prompt compaction, retry nudge helper, tests.
- Modify: `src/lib.rs` - export `agent_prompt` module.
- Modify: `src/agent.rs` - use prompt preparation, completion metadata, empty-response retry logic, LangSmith diagnostics, and subagent empty-response handling.

No database migration is needed. Prompt compaction must not write compacted messages back to SQLite.

---

### Task 1: Configurable Empty Response Retry Limit

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

- [x] **Step 1: Add failing config tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `src/config.rs`:

```rust
    #[test]
    fn test_agent_empty_response_retry_limit_defaults_to_three() {
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
        assert_eq!(cfg.agent.empty_response_retry_limit, 3);
        assert_eq!(cfg.empty_response_retry_limit(), 3);
    }

    #[test]
    fn test_agent_empty_response_retry_limit_can_be_configured_to_zero() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [agent]
            empty_response_retry_limit = 0
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.agent.empty_response_retry_limit, 0);
        assert_eq!(cfg.empty_response_retry_limit(), 0);
    }
```

- [x] **Step 2: Run tests and verify they fail**

Run:

```bash
cargo test config::tests::test_agent_empty_response_retry_limit -- --nocapture
```

Expected: fail because `AgentConfig` has no `empty_response_retry_limit` field and `Config::empty_response_retry_limit()` does not exist.

- [x] **Step 3: Implement config field and default**

In `src/config.rs`, replace `AgentConfig` with:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_empty_response_retry_limit")]
    pub empty_response_retry_limit: u32,
}
```

Add this default next to `default_max_iterations()`:

```rust
fn default_empty_response_retry_limit() -> u32 {
    3
}
```

Update `default_agent_config()`:

```rust
fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_iterations: default_max_iterations(),
        empty_response_retry_limit: default_empty_response_retry_limit(),
    }
}
```

Add this accessor near `Config::max_iterations()`:

```rust
    /// Maximum retry attempts for invalid empty LLM responses.
    pub fn empty_response_retry_limit(&self) -> u32 {
        self.agent.empty_response_retry_limit
    }
```

- [x] **Step 4: Document config example**

In `config.example.toml`, update the agent block comment to:

```toml
# Agent loop (optional; defaults apply if section omitted)
# [agent]
# max_iterations = 25              # Agent loop cap (default 25)
# empty_response_retry_limit = 3   # Recovery attempts for empty model responses (default 3; 0 = fail immediately)
```

- [x] **Step 5: Run config tests**

Run:

```bash
cargo test config::tests::test_agent_empty_response_retry_limit -- --nocapture
```

Expected: both new tests pass.

- [x] **Step 6: Checkpoint**

Run:

```bash
git diff -- src/config.rs config.example.toml
```

Expected: diff only includes the new config field, default, accessor, tests, and example comment. Do not commit unless the user explicitly asks.

---

### Task 2: Preserve Completion Metadata And Classify Empty Responses

**Files:**
- Modify: `src/llm.rs`

- [x] **Step 1: Add failing LLM tests**

Add these tests inside `#[cfg(test)] mod tests` in `src/llm.rs`:

```rust
    #[test]
    fn test_empty_assistant_response_detects_null_content_no_tools() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        assert!(is_empty_assistant_response(&message));
    }

    #[test]
    fn test_empty_assistant_response_detects_whitespace_content_no_tools() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: Some("  \n\t  ".to_string()),
            tool_calls: Some(vec![]),
            tool_call_id: None,
        };
        assert!(is_empty_assistant_response(&message));
    }

    #[test]
    fn test_empty_assistant_response_false_when_tool_calls_present() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "plan_view".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
        };
        assert!(!is_empty_assistant_response(&message));
    }

    #[test]
    fn test_empty_assistant_response_false_when_content_present() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: Some("Done".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        assert!(!is_empty_assistant_response(&message));
    }

    #[test]
    fn test_chat_completion_preserves_finish_reason() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        let choice = resp.choices.into_iter().next().unwrap();
        let completion = ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: "test-model".to_string(),
        };
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.message.content.as_deref(), Some("hello"));
    }
```

- [x] **Step 2: Run tests and verify they fail**

Run:

```bash
cargo test llm::tests -- --nocapture
```

Expected: fail because `ChatCompletion` and `is_empty_assistant_response()` do not exist.

- [x] **Step 3: Add response wrapper and classifier**

In `src/llm.rs`, after `ToolDefinition`, add:

```rust
#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    pub model: String,
}

pub fn is_empty_assistant_response(message: &ChatMessage) -> bool {
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    let has_content = message
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());

    !has_tool_calls && !has_content
}
```

- [x] **Step 4: Split completion-returning methods from compatibility methods**

In `impl LlmClient`, rename the current `chat_with_model()` body to `chat_completion_with_model()` and change its return type to `Result<ChatCompletion>`:

```rust
    pub async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatCompletion> {
```

At the end of that method, replace:

```rust
        Ok(choice.message)
```

with:

```rust
        Ok(ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: model.to_string(),
        })
```

Then add compatibility wrappers below it:

```rust
    pub async fn chat_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatMessage> {
        Ok(self
            .chat_completion_with_model(messages, tools, model)
            .await?
            .message)
    }

    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatCompletion> {
        self.chat_completion_with_model(messages, tools, &self.config.model)
            .await
    }
```

Keep the existing `chat()` method, but make it delegate to `chat_completion()`:

```rust
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatMessage> {
        Ok(self.chat_completion(messages, tools).await?.message)
    }
```

The Kimi parsing block stays inside `chat_completion_with_model()` before constructing `ChatCompletion`.

- [x] **Step 5: Run LLM tests**

Run:

```bash
cargo test llm::tests -- --nocapture
```

Expected: all `llm` tests pass.

- [x] **Step 6: Checkpoint**

Run:

```bash
git diff -- src/llm.rs
```

Expected: diff shows the wrapper, classifier, method split, and tests. Existing callers still compile because `chat()` and `chat_with_model()` remain available.

---

### Task 3: Add In-Memory Prompt Preparation And Compaction

**Files:**
- Create: `src/agent_prompt.rs`
- Modify: `src/lib.rs`

- [x] **Step 1: Add the new module export**

In `src/lib.rs`, add this line after `pub mod agent;`:

```rust
pub mod agent_prompt;
```

- [x] **Step 2: Create failing prompt helper tests and implementation skeleton**

Create `src/agent_prompt.rs` with this complete starting content. The tests describe the required behavior and will fail until the helper logic is filled in during Step 4.

```rust
use std::collections::HashSet;

use crate::llm::{ChatMessage, FunctionCall, ToolCall};

const COMPACTION_MESSAGE_COUNT_THRESHOLD: usize = 10;
const COMPACTION_PROMPT_CHAR_THRESHOLD: usize = 20_000;
const TOOL_ARGUMENT_COMPACT_THRESHOLD: usize = 1_000;
const TOOL_RESULT_COMPACT_THRESHOLD: usize = 2_000;
const TOOL_RESULT_PREVIEW_CHARS: usize = 1_000;
const PRESERVED_TOOL_GROUPS: usize = 2;

#[derive(Debug, Clone)]
pub struct PreparedPrompt {
    pub messages: Vec<ChatMessage>,
    pub stats: PromptStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptStats {
    pub original_message_count: usize,
    pub prepared_message_count: usize,
    pub original_prompt_chars: usize,
    pub prepared_prompt_chars: usize,
    pub compaction_applied: bool,
}

pub fn prepare_messages_for_llm(messages: &[ChatMessage]) -> PreparedPrompt {
    let original_prompt_chars = estimate_prompt_chars(messages);
    let should_compact = messages.len() > COMPACTION_MESSAGE_COUNT_THRESHOLD
        && original_prompt_chars > COMPACTION_PROMPT_CHAR_THRESHOLD;

    let prepared = if should_compact {
        compact_tool_heavy_history(messages)
    } else {
        messages.to_vec()
    };

    PreparedPrompt {
        stats: PromptStats {
            original_message_count: messages.len(),
            prepared_message_count: prepared.len(),
            original_prompt_chars,
            prepared_prompt_chars: estimate_prompt_chars(&prepared),
            compaction_applied: should_compact,
        },
        messages: prepared,
    }
}

pub fn estimate_prompt_chars(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            let content_len = message.content.as_deref().map_or(0, |content| content.chars().count());
            let tool_arg_len = message
                .tool_calls
                .as_ref()
                .map(|calls| {
                    calls
                        .iter()
                        .map(|call| call.function.arguments.chars().count())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            content_len + tool_arg_len
        })
        .sum()
}

pub fn recovery_nudge_for(messages: &[ChatMessage]) -> ChatMessage {
    let previous_role = messages.last().map(|message| message.role.as_str()).unwrap_or("");
    let content = if previous_role == "tool" {
        "The previous model response was empty: no content and no tool calls. Continue from the tool result above. Either call the next required tool or provide a concise user-visible final answer."
    } else {
        "The previous model response was empty: no content and no tool calls. Provide a concise user-visible response to the user's request above."
    };

    ChatMessage {
        role: "system".to_string(),
        content: Some(content.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

pub fn compact_tool_heavy_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let preserved = preserved_tool_group_indices(messages);
    messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            if preserved.contains(&idx) || message.role == "system" || message.role == "user" {
                message.clone()
            } else {
                compact_message(message)
            }
        })
        .collect()
}

fn preserved_tool_group_indices(messages: &[ChatMessage]) -> HashSet<usize> {
    let mut preserved = HashSet::new();
    let mut groups_found = 0;
    let mut idx = messages.len();

    while idx > 0 && groups_found < PRESERVED_TOOL_GROUPS {
        idx -= 1;
        let message = &messages[idx];
        let has_tool_calls = message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty());
        if message.role != "assistant" || !has_tool_calls {
            continue;
        }

        preserved.insert(idx);
        let mut tool_idx = idx + 1;
        while tool_idx < messages.len() && messages[tool_idx].role == "tool" {
            preserved.insert(tool_idx);
            tool_idx += 1;
        }
        groups_found += 1;
    }

    preserved
}

fn compact_message(message: &ChatMessage) -> ChatMessage {
    let mut compacted = message.clone();

    if let Some(tool_calls) = compacted.tool_calls.as_mut() {
        for call in tool_calls {
            let arg_chars = call.function.arguments.chars().count();
            if arg_chars > TOOL_ARGUMENT_COMPACT_THRESHOLD {
                call.function.arguments = serde_json::json!({
                    "_rustfox_compacted_arguments": true,
                    "tool_name": call.function.name,
                    "original_char_count": arg_chars,
                    "preview": truncate_chars(&call.function.arguments, 240)
                })
                .to_string();
            }
        }
    }

    if compacted.role == "tool" {
        if let Some(content) = compacted.content.as_deref() {
            let content_chars = content.chars().count();
            if content_chars > TOOL_RESULT_COMPACT_THRESHOLD {
                compacted.content = Some(format!(
                    "[rustfox compacted tool result: original_char_count={}]\n{}",
                    content_chars,
                    truncate_chars(content, TOOL_RESULT_PREVIEW_CHARS)
                ));
            }
        }
    }

    compacted
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn system(content: &str) -> ChatMessage {
        ChatMessage {
            role: "system".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn assistant_tool(id: &str, args: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "write_file".to_string(),
                    arguments: args.to_string(),
                },
            }]),
            tool_call_id: None,
        }
    }

    fn tool(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
        }
    }

    #[test]
    fn estimate_prompt_chars_counts_content_and_tool_arguments() {
        let messages = vec![user("hello"), assistant_tool("call_1", "{\"x\":1}")];
        assert_eq!(estimate_prompt_chars(&messages), 12);
    }

    #[test]
    fn recovery_nudge_mentions_tool_result_when_previous_message_is_tool() {
        let messages = vec![tool("call_1", "result")];
        let nudge = recovery_nudge_for(&messages);
        assert!(nudge.content.unwrap().contains("tool result above"));
    }

    #[test]
    fn recovery_nudge_mentions_user_request_when_previous_message_is_user() {
        let messages = vec![user("hello")];
        let nudge = recovery_nudge_for(&messages);
        assert!(nudge.content.unwrap().contains("user's request above"));
    }

    #[test]
    fn prepare_messages_skips_compaction_for_short_prompts() {
        let messages = vec![system("sys"), user("hello")];
        let prepared = prepare_messages_for_llm(&messages);
        assert!(!prepared.stats.compaction_applied);
        assert_eq!(prepared.messages.len(), messages.len());
    }

    #[test]
    fn compaction_preserves_newest_two_tool_groups_and_compacts_older_group() {
        let large_args = "x".repeat(5_000);
        let large_tool_result = "r".repeat(5_000);
        let messages = vec![
            system("sys"),
            user(&"u".repeat(20_001)),
            assistant_tool("old_call", &large_args),
            tool("old_call", &large_tool_result),
            assistant_tool("middle_call", &large_args),
            tool("middle_call", "middle result"),
            assistant_tool("new_call", &large_args),
            tool("new_call", "new result"),
            user("continue"),
            user("extra1"),
            user("extra2"),
        ];

        let prepared = prepare_messages_for_llm(&messages);
        assert!(prepared.stats.compaction_applied);
        assert_eq!(prepared.messages.len(), messages.len());

        let old_args = &prepared.messages[2].tool_calls.as_ref().unwrap()[0].function.arguments;
        assert!(old_args.contains("_rustfox_compacted_arguments"));
        assert!(prepared.messages[3]
            .content
            .as_deref()
            .unwrap()
            .contains("rustfox compacted tool result"));

        let middle_args = &prepared.messages[4].tool_calls.as_ref().unwrap()[0].function.arguments;
        let new_args = &prepared.messages[6].tool_calls.as_ref().unwrap()[0].function.arguments;
        assert_eq!(middle_args, &large_args);
        assert_eq!(new_args, &large_args);
    }

    #[test]
    fn compacted_message_order_is_unchanged() {
        let large_args = "x".repeat(5_000);
        let messages = vec![
            system("sys"),
            user(&"u".repeat(20_001)),
            assistant_tool("old_call", &large_args),
            tool("old_call", "old"),
            assistant_tool("new_call", &large_args),
            tool("new_call", "new"),
            user("extra1"),
            user("extra2"),
            user("extra3"),
            user("extra4"),
            user("extra5"),
        ];
        let prepared = prepare_messages_for_llm(&messages);
        let original_roles: Vec<_> = messages.iter().map(|message| message.role.as_str()).collect();
        let prepared_roles: Vec<_> = prepared.messages.iter().map(|message| message.role.as_str()).collect();
        assert_eq!(prepared_roles, original_roles);
    }
}
```

- [x] **Step 3: Run prompt helper tests**

Run:

```bash
cargo test agent_prompt::tests -- --nocapture
```

Expected: tests compile and pass once the file above is in place. If a formatting or borrow-check issue appears, fix only the helper file and rerun this command.

- [x] **Step 4: Checkpoint**

Run:

```bash
git diff -- src/lib.rs src/agent_prompt.rs
```

Expected: diff shows only the new module export and prompt helper module.

---

### Task 4: Add Empty Response Recovery To Main Agent Loop

**Files:**
- Modify: `src/agent.rs`

- [x] **Step 1: Add imports and constants**

At the top of `src/agent.rs`, extend the existing LLM import to include the empty classifier:

```rust
use crate::llm::{is_empty_assistant_response, ChatMessage, LlmClient, ToolDefinition};
```

Also import prompt helpers:

```rust
use crate::agent_prompt::{prepare_messages_for_llm, recovery_nudge_for, PreparedPrompt};
```

If `src/agent.rs` currently imports those types in a different grouped `use`, adjust the existing line instead of adding duplicates.

- [x] **Step 2: Add a small LangSmith helper inside `impl Agent`**

Add this private helper near `now_iso8601_static()`:

```rust
    fn llm_run_outputs(
        completion: Option<&crate::llm::ChatCompletion>,
        prompt: &PreparedPrompt,
        retry_count: u32,
    ) -> serde_json::Value {
        let finish_reason = completion.and_then(|c| c.finish_reason.clone());
        let model = completion
            .map(|c| c.model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let message = completion.map(|c| &c.message);

        serde_json::json!({
            "choices": [{
                "finish_reason": finish_reason,
                "message": message.map(|message| serde_json::json!({
                    "role": message.role,
                    "content": message.content,
                    "tool_calls": message.tool_calls,
                }))
            }],
            "metadata": {
                "model": model,
                "message_count": prompt.stats.prepared_message_count,
                "original_message_count": prompt.stats.original_message_count,
                "prompt_chars": prompt.stats.prepared_prompt_chars,
                "original_prompt_chars": prompt.stats.original_prompt_chars,
                "prompt_compaction_applied": prompt.stats.compaction_applied,
                "empty_response_retry_count": retry_count,
            }
        })
    }
```

- [x] **Step 3: Replace the single LLM call with a retry loop**

Inside `process_message()`, before the `for iteration in 0..max_iterations` loop, add:

```rust
        let empty_response_retry_limit = self.config.empty_response_retry_limit();
```

Inside the `for iteration` loop, replace the current block from LLM child run creation through `let response = match response { ... };` and the immediate successful `end_run` call with this structure:

```rust
            let mut empty_response_retry_count = 0u32;
            let completion = loop {
                let mut prompt = prepare_messages_for_llm(&messages);
                if empty_response_retry_count > 0 {
                    prompt.messages.push(recovery_nudge_for(&messages));
                    prompt.stats.prepared_message_count = prompt.messages.len();
                    prompt.stats.prepared_prompt_chars =
                        crate::agent_prompt::estimate_prompt_chars(&prompt.messages);
                }

                debug!(
                    iteration = iteration,
                    retry = empty_response_retry_count,
                    message_count = prompt.stats.prepared_message_count,
                    original_message_count = prompt.stats.original_message_count,
                    prompt_chars = prompt.stats.prepared_prompt_chars,
                    compaction = prompt.stats.compaction_applied,
                    "Trying LLM iteration"
                );

                let llm_run_id = uuid::Uuid::new_v4().to_string();
                let llm_start = Self::now_iso8601_static();
                self.langsmith.start_run(crate::langsmith::RunParams {
                    id: llm_run_id.clone(),
                    name: "llm_call".to_string(),
                    run_type: crate::langsmith::RunType::Llm,
                    parent_run_id: Some(chain_run_id.clone()),
                    inputs: serde_json::json!({
                        "messages": prompt.messages,
                        "metadata": {
                            "message_count": prompt.stats.prepared_message_count,
                            "original_message_count": prompt.stats.original_message_count,
                            "prompt_chars": prompt.stats.prepared_prompt_chars,
                            "original_prompt_chars": prompt.stats.original_prompt_chars,
                            "prompt_compaction_applied": prompt.stats.compaction_applied,
                            "empty_response_retry_count": empty_response_retry_count,
                        }
                    }),
                    session_name: ls_project.clone(),
                    start_time: llm_start,
                });

                let completion_result = self.llm.chat_completion(&prompt.messages, &all_tools).await;
                let completion = match completion_result {
                    Ok(completion) => completion,
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

                if is_empty_assistant_response(&completion.message) {
                    let error = format!(
                        "Empty LLM response: no content and no tool calls (finish_reason={:?}, retry={}/{})",
                        completion.finish_reason,
                        empty_response_retry_count,
                        empty_response_retry_limit
                    );
                    warn!(
                        user_id = %user_id,
                        iteration = iteration,
                        retry = empty_response_retry_count,
                        finish_reason = ?completion.finish_reason,
                        "LLM returned invalid empty response"
                    );
                    self.langsmith.end_run(crate::langsmith::EndRunParams {
                        id: llm_run_id,
                        outputs: Some(Self::llm_run_outputs(Some(&completion), &prompt, empty_response_retry_count)),
                        error: Some(error.clone()),
                        end_time: Self::now_iso8601_static(),
                    });

                    if empty_response_retry_count >= empty_response_retry_limit {
                        let user_error = format!(
                            "Unable to get a valid response from the AI model after {} attempts. Your conversation history has been saved. Please try rephrasing your request or continue from where we left off.",
                            empty_response_retry_limit
                        );
                        self.langsmith.end_run(crate::langsmith::EndRunParams {
                            id: chain_run_id,
                            outputs: None,
                            error: Some(user_error.clone()),
                            end_time: Self::now_iso8601_static(),
                        });
                        anyhow::bail!(user_error);
                    }

                    empty_response_retry_count += 1;
                    continue;
                }

                self.langsmith.end_run(crate::langsmith::EndRunParams {
                    id: llm_run_id,
                    outputs: Some(Self::llm_run_outputs(Some(&completion), &prompt, empty_response_retry_count)),
                    error: None,
                    end_time: Self::now_iso8601_static(),
                });

                break completion;
            };

            let response = completion.message;
```

This preserves retry attempts as provider recovery work. It does not increment `iteration` for retries because retries happen inside one normal agent loop iteration.

- [x] **Step 4: Remove the old empty-success branch**

In the final response section, replace:

```rust
            let content = response.content.clone().unwrap_or_default();

            if content.is_empty() {
                warn!(
                    user_id = %user_id,
                    iteration = iteration,
                    "LLM returned empty content with no tool calls -- bot will send nothing"
                );
            }
```

with:

```rust
            let content = response.content.clone().unwrap_or_default();
```

The retry loop already prevents empty final content from reaching this section.

- [x] **Step 5: Run targeted compile check**

Run:

```bash
cargo check
```

Expected: the first run may expose imports, moved values inside the JSON macro, or helper visibility issues. Fix only those issues and rerun until `cargo check` passes.

- [x] **Step 6: Checkpoint**

Run:

```bash
git diff -- src/agent.rs
```

Expected: main-loop diff shows prompt preparation, retry handling, LangSmith diagnostics, and removal of the silent empty-success behavior.

---

### Task 5: Add Empty Response Recovery To Subagents

**Files:**
- Modify: `src/agent.rs`

- [x] **Step 1: Update subagent LLM call to use completion metadata**

In `run_subagent()`, before the mini agent loop, add:

```rust
        let empty_response_retry_limit = self.config.empty_response_retry_limit();
```

Replace the current direct `chat_with_model()` call inside the mini loop with a nested retry loop:

```rust
            let mut empty_response_retry_count = 0u32;
            let completion = loop {
                let mut prompt = prepare_messages_for_llm(&messages);
                if empty_response_retry_count > 0 {
                    prompt.messages.push(recovery_nudge_for(&messages));
                    prompt.stats.prepared_message_count = prompt.messages.len();
                    prompt.stats.prepared_prompt_chars =
                        crate::agent_prompt::estimate_prompt_chars(&prompt.messages);
                }

                let completion = match self
                    .llm
                    .chat_completion_with_model(&prompt.messages, &subagent_tools, &resolved_model)
                    .await
                {
                    Ok(completion) => completion,
                    Err(e) => {
                        error!(
                            "Agent '{}' API call failed (model: '{}'): {}",
                            skill_name, resolved_model, e
                        );
                        return format!("Agent '{}' error: {}", skill_name, e);
                    }
                };

                if is_empty_assistant_response(&completion.message) {
                    warn!(
                        agent = %skill_name,
                        iteration = iteration,
                        retry = empty_response_retry_count,
                        finish_reason = ?completion.finish_reason,
                        "Subagent returned invalid empty response"
                    );

                    if empty_response_retry_count >= empty_response_retry_limit {
                        return format!(
                            "Error: Subagent '{}' returned an empty response after {} attempts.",
                            skill_name, empty_response_retry_limit
                        );
                    }

                    empty_response_retry_count += 1;
                    continue;
                }

                break completion;
            };

            let response = completion.message;
```

Keep the existing tool-call execution and final response code below this block.

- [x] **Step 2: Keep subagent error as tool result**

Do not return `Err` from `run_subagent()` because it currently returns `String` and is used as a tool result by `invoke_agent` and `invoke_subagent`. The exhausted empty response string must start with `Error:` so the main agent can see it as a failed tool result.

- [x] **Step 3: Run targeted check**

Run:

```bash
cargo check
```

Expected: compile succeeds. Fix only subagent-related type or import issues.

- [x] **Step 4: Checkpoint**

Run:

```bash
git diff -- src/agent.rs
```

Expected: diff includes both main agent and subagent recovery paths.

---

### Task 6: Add Focused Tests For Helpers And Config

**Files:**
- Modify: `src/agent_prompt.rs`
- Modify: `src/llm.rs`
- Modify: `src/config.rs`

- [x] **Step 1: Run all focused test modules**

Run:

```bash
cargo test config::tests -- --nocapture
cargo test llm::tests -- --nocapture
cargo test agent_prompt::tests -- --nocapture
```

Expected: all focused helper/config tests pass.

- [x] **Step 2: Add any missing helper tests uncovered during implementation**

If implementation introduced a separate helper such as `empty_response_error_message(limit: u32)`, add this test in the same module as the helper:

```rust
    #[test]
    fn empty_response_error_message_includes_configured_attempt_count() {
        let message = empty_response_error_message(3);
        assert!(message.contains("3 attempts"));
        assert!(message.contains("Unable to get a valid response"));
    }
```

If no such helper exists, do not add this test.

- [x] **Step 3: Re-run focused tests**

Run:

```bash
cargo test config::tests -- --nocapture
cargo test llm::tests -- --nocapture
cargo test agent_prompt::tests -- --nocapture
```

Expected: all focused tests pass.

- [x] **Step 4: Checkpoint**

Run:

```bash
git diff --stat
```

Expected: changed files match the file map only.

---

### Task 7: Full Verification

**Files:**
- No new files. This task verifies the implementation.

- [x] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: command exits 0.

- [x] **Step 2: Compile**

Run:

```bash
cargo check
```

Expected: command exits 0.

- [x] **Step 3: Test**

Run:

```bash
cargo test
```

Expected: command exits 0 with all tests passing.

- [x] **Step 4: Lint**

Run:

```bash
cargo clippy -- -D warnings
```

Expected: command exits 0 with no warnings.

- [x] **Step 5: Final diff review**

Run:

```bash
git diff -- src/config.rs config.example.toml src/llm.rs src/agent_prompt.rs src/lib.rs src/agent.rs
```

Expected: diff implements only empty-response recovery, configurable retry limit, prompt compaction, tests, and documentation. No unrelated local changes are reverted or modified.

- [x] **Step 6: Report completion evidence**

Report the exact verification commands run and whether they passed. Do not claim completion unless the fresh command outputs confirm it.
