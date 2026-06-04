# Empty LLM Response Recovery Design

## Goal

Prevent RustFox from silently finishing a Telegram request when the LLM provider returns an assistant message with no content and no tool calls, especially during long tool-heavy tasks.

## Evidence

The production trace `20732c50-bcdb-4233-9133-5c1d89111d62` shows the failure clearly:

- Root run `rustfox_request` ended with `error = null` and `outputs.response = ""`.
- Final child run `510d2452-2dc0-4c36-bdb5-62fd7b06fa05` returned an assistant message with `content = null` and `tool_calls = null`.
- Console logging for that same call reported `finish_reason = None`.
- The failing request had 67 messages: 33 assistant messages, 32 tool messages, and 2 user messages.
- The failing prompt included roughly 12.6k content characters and 19k tool-call argument characters. The largest arguments were full README writes embedded in `write_file` and `execute_command` tool calls.

Local code currently turns this provider-null response into a successful empty answer:

- `src/llm.rs` logs `LLM returned no content and no tool calls`, then returns the empty assistant message.
- `src/agent.rs` treats any response without tool calls as final, converts missing content to `""`, saves it, ends LangSmith as success, and returns `Ok("")`.
- `src/platform/telegram.rs` sees a successful result, waits for an empty stream, deletes the thinking placeholder, and sends no error message.

## Root Cause

RustFox has no invalid-response state between the LLM client and the agent loop. A malformed or provider-empty assistant message is indistinguishable from a legitimate final assistant message whose content is an empty string.

Prompt growth from many tool calls is a contributing trigger. The agent keeps full assistant tool-call messages and full tool results in the in-flight prompt. Large file-write or shell-command arguments can dominate the prompt even after the tool result is short. This makes the provider-null response more likely during multi-step tasks.

## Design Overview

Add a response validity boundary and a bounded recovery path.

The LLM client should preserve provider metadata, including `finish_reason`, and make it possible for the agent to distinguish:

- final text response
- tool-call response
- invalid empty response
- transport or API error

The agent should retry invalid empty responses briefly, then fail visibly if recovery does not produce text or tool calls. The agent must never save an empty invalid assistant response as if it were successful user-visible output.

A prompt compaction pass should reduce older tool-heavy history before each LLM call. This reduces recurrence without changing the live tool protocol for the most recent tool call exchange.

## API Compatibility

OpenRouter follows the OpenAI chat-completions shape for `choices[0].finish_reason`, and the existing Rust code already deserializes that field into `Choice.finish_reason`. The observed trace still recorded only the app-shaped message in LangSmith, so the implementation must preserve `finish_reason` in the new response wrapper before returning from the LLM client.

If `finish_reason` is absent or `null`, validity classification must fall back to message shape: after Kimi parsing, a response with no non-whitespace content and no tool calls is invalid regardless of the missing finish reason.

## Components

### LLM Response Wrapper

Introduce a small response wrapper in `src/llm.rs`, for example:

```rust
pub struct ChatCompletion {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    pub model: String,
}
```

`chat_with_model()` should return this wrapper or an equivalent type. Existing callers that only need content can use the `message` field.

Add a helper on the wrapper or LLM module:

```rust
pub fn is_empty_assistant_response(message: &ChatMessage) -> bool
```

It should return true when:

- `message.tool_calls` is missing or empty
- `message.content` is missing or whitespace only

Kimi native tool-call parsing remains before final empty-response classification. If content contains Kimi tool-call markers and parsing succeeds, the response is a tool-call response, not invalid.

### Kimi Parsing Integration

The response flow should be:

1. Deserialize the OpenRouter response into the internal response wrapper.
2. Apply the existing Kimi native tool-call parser to `completion.message.content` if there are no standard tool calls.
3. If parsing succeeds, populate `completion.message.tool_calls`, clear content, and set `completion.finish_reason` to `Some("tool_calls")`.
4. Apply empty-response classification only after the Kimi parsing step.

This preserves the existing Kimi fallback behavior while making empty-response handling explicit.

### Agent Recovery Loop

In `src/agent.rs`, after each LLM call and after Kimi parsing:

1. If tool calls are present, continue with existing tool execution behavior.
2. If final content is non-empty after trimming, stream/save/end as success.
3. If content is empty and there are no tool calls, classify as invalid.
4. For invalid responses, end the current LangSmith LLM child run with an error field and diagnostic outputs. Include `finish_reason`, iteration, message count, tool count, approximate prompt size, and retry count. A retry must create a fresh LLM child run rather than reusing the ended child run.
5. Retry at most the configured empty-response retry limit. The default should be 3.
6. Before retrying, append a context-appropriate internal recovery nudge to the in-memory request only, not persistent memory.

If the previous message is a tool result:

```text
The previous model response was empty: no content and no tool calls. Continue from the tool result above. Either call the next required tool or provide a concise user-visible final answer.
```

If the previous message is a user message:

```text
The previous model response was empty: no content and no tool calls. Provide a concise user-visible response to the user's request above.
```

7. If retries are exhausted, end the root LangSmith run with an error and return `Err(anyhow!(...))` so Telegram sends the existing visible error reply.

The invalid empty assistant message should not be saved to memory.

Retry counter scope: the counter resets after any successful non-empty LLM response, including a tool-call response. Each contiguous sequence of empty responses gets an independent retry budget.

### Prompt Compaction

Add a prompt preparation helper in `src/agent.rs` or a focused helper module, for example:

```rust
fn compact_tool_heavy_history(messages: &[ChatMessage]) -> Vec<ChatMessage>
```

Rules:

- Preserve the system message and recent user messages.
- Preserve the latest assistant tool-call groups exactly, so provider tool-call protocol stays valid.
- For older assistant messages with tool calls, replace large `function.arguments` strings with compact placeholders that retain tool name, call id, and argument length.
- For older tool messages, truncate content to a bounded preview plus original length and exit/status summary when obvious.
- Do not compact the message currently required to answer a pending tool call.
- Keep message order unchanged.

Apply compaction before an LLM call when both conditions are true:

- message count is greater than 10
- estimated prompt size is greater than 20,000 characters

This applies to normal LLM calls and retry calls. Short conversations bypass compaction.

Define preserved tool groups by walking backward from the end of the message list:

1. Preserve the most recent assistant message with tool calls, if present.
2. Preserve all tool messages immediately following that assistant message, until the next assistant or user message.
3. Preserve the previous assistant-with-tool-calls group and its immediate tool results.
4. Compact eligible tool-heavy messages before those two preserved groups.
5. Never compact system messages or user messages.

Initial thresholds:

- Compact tool-call arguments over 1,000 characters.
- Compact tool results over 2,000 characters.
- Always keep the most recent two assistant-with-tool-calls groups and their immediate tool results unmodified.

This pass should affect only the request sent to the LLM. Persistent memory should retain the original message records.

Prompt compaction stays entirely in process memory. It should not write compacted messages back to SQLite, update historical message rows, or change RAG source records.

### Configuration

Add an agent configuration field for empty-response retries:

```toml
[agent]
max_iterations = 25
empty_response_retry_limit = 3
```

The Rust config should expose this as a `u32` with default `3`. Existing configs that omit the field should keep working. `config.example.toml` should document the new option next to `max_iterations`.

Implementation shape in `src/config.rs`:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    #[serde(default = "default_empty_response_retry_limit")]
    pub empty_response_retry_limit: u32,
}

fn default_empty_response_retry_limit() -> u32 {
    3
}
```

Setting `empty_response_retry_limit = 0` should fail immediately on the first invalid empty response.

The retry limit counts recovery attempts for a single invalid empty provider response position in the agent loop. It should not increase `max_iterations`; retries are provider recovery attempts, not normal agent tool iterations.

### LangSmith Observability

Record enough detail in LangSmith to diagnose future cases without relying only on console logs.

For each `llm_call`, include:

- `finish_reason`
- `model`
- `message_count`
- `tool_count`
- approximate prompt character count
- whether prompt compaction was applied
- empty-response retry number

Approximate prompt size calculation:

- sum of all `message.content` character lengths
- plus sum of all `tool_call.function.arguments` character lengths
- plus sum of all tool result content character lengths
- excluding JSON structure overhead and tool definitions

For root chain success, keep current outputs. For exhausted empty-response recovery, set `error` to a clear message instead of `outputs.response = ""`.

## Error Handling

Provider HTTP failures continue to return `Err` from the LLM client.

Provider-empty responses are treated as transient invalid responses at the agent layer because retry and context adjustment need access to iteration state and the current prompt.

If all retries fail, the user sees a visible Telegram error such as:

```text
Error: Unable to get a valid response from the AI model after 3 attempts. Your conversation history has been saved. Please try rephrasing your request or continue from where we left off.
```

Scheduled tasks should also treat this as failure, because returning `Err` already marks one-shot scheduled jobs failed in the background runner.

### Subagent Empty Response Handling

Subagents have the same silent-empty risk. The same validity helper and retry limit should be used in `run_subagent()`.

When a subagent exhausts retries:

1. The subagent runner returns an error-like result string rather than `""`.
2. The main agent records that string as the tool result for `invoke_agent` or `invoke_subagent`.
3. The main agent does not automatically retry the subagent invocation outside the LLM loop.
4. The main LLM can decide whether to call the subagent again, switch approach, or report the problem to the user.

Example subagent tool result:

```text
Error: Subagent 'news-fetcher' returned an empty response after 3 attempts.
```

## Testing

Add focused unit tests before implementation:

1. `src/llm.rs`: empty response deserializes with `finish_reason = None` and is classified as empty.
2. `src/llm.rs`: whitespace content with no tool calls is classified as empty.
3. `src/llm.rs`: Kimi native tool-call content is parsed into tool calls and not classified as empty.
4. `src/agent.rs` or helper module: prompt compaction preserves the newest tool pair and compacts older large tool arguments.
5. `src/agent.rs` or helper module: compacted message order remains unchanged.
6. Retry helper or agent-loop test: empty response, recovery nudge, then tool-call response succeeds.
7. Retry helper or agent-loop test: empty response exhausted after the configured limit returns an error.
8. Retry helper or agent-loop test: empty response, successful response, later empty response gets a reset retry budget.
9. Subagent helper test: exhausted subagent empty responses produce an error-like tool result, not `""`.
10. Scheduled-task path test or smoke test: exhausted empty responses return `Err` so one-shot scheduled jobs can be marked failed.

Integration testing for the full agent loop may require a mock LLM client abstraction. If one does not exist, keep the first implementation covered by helper-level tests and add the abstraction only if it can be done without a broad refactor.

## Monitoring

Emit structured logs for operational visibility:

- `empty_response.count`
- `empty_response.retry_success`
- `empty_response.retry_exhausted`
- `prompt_compaction.applied`

Counters can be logs first; a metrics sink can be added later if RustFox gains one.

## Non-Goals

- Do not change the default model.
- Do not remove plan tools.
- Do not alter persistent memory schema for this fix.
- Do not rewrite Telegram streaming behavior except through the existing `Err` path.
- Do not introduce broad agent architecture changes beyond the response validity boundary and prompt preparation helper.

## Final Decisions

Use a configurable retry limit with default `3`.

Keep prompt compaction in process memory rather than database storage. The original history remains available for audit and RAG.

Return an error to Telegram after exhausted retries rather than sending a friendly synthetic success. This is more honest and prevents the task from being marked complete when the model did not produce a final answer.
