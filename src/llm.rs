use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

use std::sync::{Arc, RwLock};
use std::time::Duration;

/// A single part in a multi-modal message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlContent {
    /// "data:image/jpeg;base64,..." or a URL
    pub url: String,
}

/// Either a plain text string or a list of content parts (multi-modal).
/// Serializes as a plain JSON string for text-only, or as a JSON array for multi-modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Extract all text from the content (for logging, RAG, DB storage, etc.)
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let ContentPart::Text { text } = p {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    pub fn from_text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(s) => s.is_empty(),
            Self::Parts(parts) => parts.is_empty(),
        }
    }
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Deduplicate tool definitions by exact `function.name`, keeping the **first**
/// occurrence (ADR-0019 slice ② / Wafer `duplicate_tool_name`). Later duplicates
/// are dropped; do not rename tools or change MCP prefix rules.
pub fn dedupe_tool_definitions_keep_first(tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool.function.name.clone();
        if seen.insert(name.clone()) {
            out.push(tool);
        } else {
            tracing::warn!(
                tool_name = %name,
                kept = "first",
                "Dropping duplicate tool function.name; keeping first occurrence"
            );
        }
    }
    out
}

/// Completion wrapper that preserves metadata alongside the assistant message.
#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    pub model: String,
}

/// Classifier that detects empty assistant responses (no content and no tool calls).
///
/// Returns `true` when the assistant message has neither meaningful text content
/// nor any tool calls, indicating a potentially problematic response that may
/// warrant retry logic.
pub fn is_empty_assistant_response(message: &ChatMessage) -> bool {
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    let has_content = message.content.as_ref().is_some_and(|content| {
        let text = content.as_text();
        !text.trim().is_empty()
    });

    !has_tool_calls && !has_content
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    pub max_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[doc(hidden)]
pub mod internal {
    pub use super::{ChatRequest, ChatResponse, Choice};
}

/// Sanitize a JSON Schema parameter object so it is accepted by strict providers
/// (e.g. Google Gemini via OpenRouter).
///
/// Gemini enforces that every entry in the `required` array corresponds to a key
/// that is actually defined in `properties`.  Some MCP servers return schemas where
/// `required` contains field names that do not exist in `properties` — this causes a
/// 400 INVALID_ARGUMENT from Google AI Studio.
///
/// Additional Gemini restrictions handled here:
/// - `additionalProperties`, `$schema`, `$defs`, `$ref` are not accepted.
/// - `required: []` (empty array) is rejected; the key must be omitted entirely.
/// - `anyOf`/`oneOf`/`allOf` variants with `{"type": "null"}` are stripped because
///   Gemini does not support nullable types expressed as `null` union members.
///   If stripping leaves exactly one variant, it is inlined (unwrapped) into the
///   parent object.  If stripping leaves zero variants, the key is removed entirely.
///
/// This function mutates the schema in-place and recurses into `properties`,
/// `items`, `anyOf`, `oneOf`, and `allOf` sub-schemas.
pub fn sanitize_parameters(schema: &mut serde_json::Value) {
    let obj = match schema.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    // Remove fields that Gemini rejects.
    obj.remove("additionalProperties");
    obj.remove("$schema");
    obj.remove("$defs");
    obj.remove("$ref");

    // Collect the set of property names that are actually defined.
    let known_props: std::collections::HashSet<String> = obj
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();

    // Filter `required` so it only lists names that appear in `properties`.
    // Gemini also rejects an empty `required: []`, so remove the key entirely
    // if nothing remains after filtering.
    if let Some(required) = obj.get_mut("required") {
        if let Some(arr) = required.as_array_mut() {
            arr.retain(|v| v.as_str().is_some_and(|s| known_props.contains(s)));
        }
    }
    if obj
        .get("required")
        .and_then(|r| r.as_array())
        .is_some_and(|a| a.is_empty())
    {
        obj.remove("required");
    }

    // Recurse into property sub-schemas.
    if let Some(properties) = obj.get_mut("properties") {
        if let Some(props_obj) = properties.as_object_mut() {
            for prop_schema in props_obj.values_mut() {
                sanitize_parameters(prop_schema);
            }
        }
    }

    // Recurse into array item schema.
    if let Some(items) = obj.get_mut("items") {
        sanitize_parameters(items);
    }

    // Recurse into anyOf / oneOf / allOf variant schemas and strip null variants.
    // Gemini does not support {"type": "null"} as a union member.
    for key in &["anyOf", "oneOf", "allOf"] {
        if let Some(variants) = obj.get_mut(*key) {
            if let Some(arr) = variants.as_array_mut() {
                // Recurse into each variant first.
                for v in arr.iter_mut() {
                    sanitize_parameters(v);
                }
                // Remove variants that are purely {"type": "null"}.
                arr.retain(|v| {
                    v.get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| t != "null")
                        .unwrap_or(true)
                });
            }
        }

        // If only one variant remains, unwrap it by merging into the parent.
        // If zero variants remain, remove the key entirely.
        let variant_count = obj.get(*key).and_then(|v| v.as_array()).map(|a| a.len());
        match variant_count {
            Some(0) => {
                obj.remove(*key);
            }
            Some(1) => {
                if let Some(single) = obj
                    .remove(*key)
                    .and_then(|mut v| v.as_array_mut().and_then(|a| a.pop()))
                {
                    if let Some(inner) = single.as_object() {
                        for (k, v) in inner {
                            obj.entry(k.clone()).or_insert(v.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Parse Kimi's native tool-call text format and convert it into `ToolCall` structs.
///
/// Some models (e.g. `moonshotai/kimi-k2.5`) occasionally leak their internal
/// tool-invocation syntax into the `content` field instead of populating the
/// standard `tool_calls` API field.  The leaked text looks like:
///
/// ```text
/// <|tool_calls_section_begin|> <|tool_call_begin|> functions.my_tool:0
/// <|tool_call_argument_begin|> {"arg": "value"} <|tool_call_end|>
/// <|tool_calls_section_end|>
/// ```
///
/// Returns `Some(Vec<ToolCall>)` with at least one entry when the format is
/// detected, or `None` if the content does not contain the Kimi markers.
pub fn parse_kimi_tool_calls(content: &str) -> Option<Vec<ToolCall>> {
    if !content.contains("<|tool_calls_section_begin|>") {
        return None;
    }

    let mut calls = Vec::new();

    // Split on the per-call begin marker; the first chunk is the preamble/section
    // header and is discarded.
    for block in content.split("<|tool_call_begin|>").skip(1) {
        // Strip everything from the closing marker onwards (handles trailing
        // section-end marker and whitespace).
        let block = block
            .split("<|tool_call_end|>")
            .next()
            .unwrap_or(block)
            .trim();

        // Split into function descriptor and JSON arguments.
        let (descriptor, args_raw) = if let Some(pos) = block.find("<|tool_call_argument_begin|>") {
            let d = block[..pos].trim();
            let a = block[pos + "<|tool_call_argument_begin|>".len()..].trim();
            (d, a)
        } else {
            continue;
        };

        // Descriptor format: `functions.{name}:{index}` or just `functions.{name}`.
        // Extract the plain function name.
        let func_name = descriptor
            .trim_start_matches("functions.")
            .split(':')
            .next()
            .unwrap_or(descriptor)
            .trim()
            .to_string();

        if func_name.is_empty() {
            continue;
        }

        // Use the call index (if present) as part of the synthetic tool-call ID.
        let call_index = descriptor.split(':').nth(1).unwrap_or("0").trim();
        let call_id = format!("kimi_fallback_{func_name}_{call_index}");

        // Verify the arguments are valid JSON; fall back to an empty object on
        // parse failure so the tool handler can still attempt execution.
        let arguments = if serde_json::from_str::<serde_json::Value>(args_raw).is_ok() {
            args_raw.to_string()
        } else {
            "{}".to_string()
        };

        calls.push(ToolCall {
            id: call_id,
            call_type: "function".to_string(),
            function: FunctionCall {
                name: func_name,
                arguments,
            },
        });
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

/// ADR-0029 / ADR-0032 / ADR-0034: tool-weak backends must not receive a
/// mid-tool-loop transcript unchanged — they 500 with "no user query found".
///
/// Matches `ProviderType::Ollama` and OpenAI-compatible endpoints that are
/// actually Ollama (common live misconfig: `type = "openai_compatible"` +
/// `name = "ollama"` or `base_url` on `:11434`). Soft-fail on tip `25dc563`:
/// omit-retry gated only on `ProviderType::Ollama`, so openai-compat Active
/// skipped ADR-0034 WARN and fell straight to fallback.
pub fn is_tool_weak_provider(config: &crate::provider::ProviderConfig) -> bool {
    use crate::config::ProviderType;
    match config.provider_type {
        ProviderType::Ollama => true,
        ProviderType::OpenAICompatible => is_ollama_openai_compat_endpoint(config),
        ProviderType::OpenRouter | ProviderType::OpenCode => false,
    }
}

/// True when an openai_compatible provider is almost certainly Ollama's
/// OpenAI-compatible listener (Kan live / config.example `localhost:11434/v1`).
fn is_ollama_openai_compat_endpoint(config: &crate::provider::ProviderConfig) -> bool {
    if config.name.eq_ignore_ascii_case("ollama") {
        return true;
    }
    let u = config.base_url.to_lowercase();
    u.contains(":11434")
}

/// True when the history already has assistant tool_calls and/or `tool` roles.
pub fn is_mid_tool_loop(messages: &[ChatMessage]) -> bool {
    messages
        .iter()
        .any(|m| m.role == "tool" || m.has_tool_calls())
}

/// True when at least one `role:user` turn has non-empty text that is not a
/// wrapped tool-response (Qwen 3.5+ / Ollama qwen3.8 template requirement).
pub fn has_plain_user_turn(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|m| {
        if m.role != "user" {
            return false;
        }
        let t = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
        let t = t.trim();
        !t.is_empty() && !(t.starts_with("<tool_response>") && t.ends_with("</tool_response>"))
    })
}

/// Fold system + last user + tool results into a payload Ollama can accept.
/// Tools are omitted by the caller.
pub fn rewrite_messages_for_tool_weak(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = messages
        .iter()
        .filter(|m| m.role == "system")
        .cloned()
        .collect();

    let mut parts: Vec<String> = Vec::new();
    // Prefer a plain user query (not a <tool_response>-wrapped stand-in).
    if let Some(user) = messages.iter().rev().find(|m| {
        if m.role != "user" {
            return false;
        }
        let t = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
        let t = t.trim();
        !t.is_empty() && !(t.starts_with("<tool_response>") && t.ends_with("</tool_response>"))
    }) {
        let t = user
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        parts.push(t);
    }
    let tool_bits: Vec<String> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.content.as_ref().map(|c| c.as_text()))
        .filter(|t| !t.trim().is_empty())
        .collect();
    if !tool_bits.is_empty() {
        parts.push(format!("Tool results:\n{}", tool_bits.join("\n\n")));
    }
    let body = if parts.is_empty() {
        "Please continue from the previous step.".to_string()
    } else {
        parts.join("\n\n")
    };
    out.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::from_text(body)),
        tool_calls: None,
        tool_call_id: None,
    });
    debug_assert!(
        has_plain_user_turn(&out),
        "rewrite_messages_for_tool_weak must always emit a user turn"
    );
    out
}

/// ADR-0032: OpenRouter-style `org/model` whose first `/` segment is not a
/// registered provider must not be sent to Ollama (incl. default fallthrough).
/// Bare Ollama library names (no `/`) and `ollama/...` (or any registered
/// provider prefix) are allowed.
pub fn is_foreign_id_for_ollama(registry: &crate::provider::ProviderRegistry, model: &str) -> bool {
    match model.split_once('/') {
        Some((prefix, _)) => registry.get_provider(prefix).is_none(),
        None => false,
    }
}

/// Shape messages/tools for **primary** Ollama (ADR-0032). Prefer keeping tools.
/// Mid-loop / user-less: rewrite to guarantee a plain user turn; keep tools.
/// If a valid payload still cannot be built, returns Err with
/// [`USER_FACING_LLM_UNAVAILABLE`].
pub fn shape_primary_ollama_payload(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
) -> Result<(Vec<ChatMessage>, Vec<ToolDefinition>), String> {
    let needs_rewrite = is_mid_tool_loop(messages) || !has_plain_user_turn(messages);
    let shaped = if needs_rewrite {
        rewrite_messages_for_tool_weak(messages)
    } else {
        messages.to_vec()
    };
    if !has_plain_user_turn(&shaped) {
        return Err(USER_FACING_LLM_UNAVAILABLE.to_string());
    }
    // Prefer keeping tools on primary (unlike ADR-0029 fallback omit).
    Ok((shaped, tools.to_vec()))
}

/// Same plain sentence as ADR-0029 §3 / ADR-0032 (Telegram / portal).
pub const USER_FACING_LLM_UNAVAILABLE: &str =
    "The AI service is temporarily unavailable or out of quota. Please try again in a moment.";

/// Plain sentence for Telegram / portal. Never forward Ollama 500 JSON.
pub fn user_facing_llm_error(err: &anyhow::Error) -> String {
    const PLAIN: &str = USER_FACING_LLM_UNAVAILABLE;
    let s = format!("{err:#}");
    let lower = s.to_lowercase();
    if lower.contains("no user query found")
        || lower.contains("ollama api error")
        || lower.contains("temporarily unavailable or out of quota")
        || lower.contains("not a valid ollama model id")
        || s.contains("{\"error\"")
        || s.contains("\\\"error\\\"")
    {
        return PLAIN.to_string();
    }
    // Keep short non-JSON errors readable; still strip huge bodies.
    if s.len() > 240 || s.contains('{') {
        return PLAIN.to_string();
    }
    s
}

/// True when the provider error is Ollama's userless-payload reject
/// (`no user query found` or equivalent). Used by ADR-0029 fallback skip
/// and ADR-0034 primary omit-retry.
pub fn is_ollama_no_user_query_error(err: &anyhow::Error) -> bool {
    format!("{err:#}")
        .to_lowercase()
        .contains("no user query found")
}

/// Cap on `[fallback].chain` length (ADR-0012 attach + ADR-0037 Telegram add).
/// Boot/attach may truncate oversized config; Telegram `add` must **reject**.
pub const MAX_FALLBACK_CHAIN: usize = 5;

#[derive(Clone)]
pub struct LlmClient {
    pub client: reqwest::Client,
    pub registry: Arc<crate::provider::ProviderRegistry>,
    /// Ordered fallback chain of fully-qualified `provider/model` names tried
    /// (after the primary) on almost any failure (ADR-0019; amends ADR-0012).
    /// Hard-excludes only HTTP 401/403 and pre-HTTP config errors — see
    /// [`crate::provider::should_walk_fallback`]. Empty (default) = ADR-0009
    /// behaviour exactly: primary only.
    ///
    /// Shared via `Arc` so Telegram `/fallback` (ADR-0037) can update live
    /// without restart; walk snapshots under a short read lock.
    pub fallback_chain: Arc<RwLock<Vec<String>>>,
}

impl LlmClient {
    /// Default LLM HTTP request timeout (seconds) when config omits it.
    pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 180;

    /// Untimed HTTP client (used by unit tests with `start_paused` time).
    /// Production paths must call [`Self::with_request_timeout_secs`].
    pub fn new(registry: Arc<crate::provider::ProviderRegistry>) -> Self {
        Self {
            client: reqwest::Client::new(),
            registry,
            fallback_chain: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Build a client with an explicit per-request HTTP timeout (socket safety
    /// net for #231). `secs == 0` uses [`Self::DEFAULT_REQUEST_TIMEOUT_SECS`].
    pub fn with_request_timeout_secs(
        registry: Arc<crate::provider::ProviderRegistry>,
        secs: u64,
    ) -> Self {
        let secs = if secs == 0 {
            Self::DEFAULT_REQUEST_TIMEOUT_SECS
        } else {
            secs
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(secs))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to build reqwest client with {secs}s timeout ({e}); using default client"
                );
                reqwest::Client::new()
            });
        Self {
            client,
            registry,
            fallback_chain: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Attach a fallback chain (from `[fallback] chain` in config). Entries
    /// whose provider prefix is not in the registry are dropped with a
    /// warning at call time, never attempted against the wrong provider.
    pub fn with_fallback_chain(self, chain: Vec<String>) -> Self {
        // Bound worst-case latency (primary budget + N fallback budgets).
        if chain.len() > MAX_FALLBACK_CHAIN {
            tracing::warn!(
                "[fallback] chain: {} entries exceeds cap {MAX_FALLBACK_CHAIN}, truncating",
                chain.len()
            );
        }
        let capped: Vec<String> = chain.into_iter().take(MAX_FALLBACK_CHAIN).collect();
        *self
            .fallback_chain
            .write()
            .unwrap_or_else(|p| p.into_inner()) = capped;
        self
    }

    /// Snapshot the live fallback chain (ADR-0037 list / edit).
    pub fn fallback_chain_snapshot(&self) -> Vec<String> {
        self.fallback_chain
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the live in-memory fallback chain (no disk). Caller validates
    /// and persists; length should already be ≤ [`MAX_FALLBACK_CHAIN`].
    pub fn set_fallback_chain_live(&self, chain: Vec<String>) {
        let capped: Vec<String> = chain.into_iter().take(MAX_FALLBACK_CHAIN).collect();
        *self
            .fallback_chain
            .write()
            .unwrap_or_else(|p| p.into_inner()) = capped;
    }

    /// Core chat method returning full completion metadata (message, finish_reason, model).
    ///
    /// Resolves the model string through the registry to pick the right provider,
    /// then delegates the actual HTTP call to that provider.
    pub async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatCompletion> {
        let (provider, _) = self.registry.resolve_model(model);
        let primary_tool_weak = is_tool_weak_provider(provider.config());
        let (primary_msgs, primary_tools): (Vec<ChatMessage>, Vec<ToolDefinition>) =
            if primary_tool_weak {
                match shape_primary_ollama_payload(messages, tools) {
                    Ok((msgs, tls)) => {
                        if is_mid_tool_loop(messages) || !has_plain_user_turn(messages) {
                            tracing::warn!(
                                "Shaping primary Ollama payload for {model} (ADR-0032; tools kept)"
                            );
                        }
                        (msgs, tls)
                    }
                    Err(msg) => {
                        tracing::warn!(
                            "Cannot build valid primary Ollama payload for {model}: {msg}"
                        );
                        return Err(anyhow::anyhow!(msg));
                    }
                }
            } else {
                (messages.to_vec(), tools.to_vec())
            };
        let primary_err = match self.try_model(&primary_msgs, &primary_tools, model).await {
            Ok(completion) => return Ok(completion),
            Err(e) => e,
        };
        // ADR-0034: keep-tools first pass unchanged (above). If primary Ollama
        // still returns no-user-query with tools present, rewrite+omit and
        // retry ONCE on the same primary (journal WARN). No loop.
        let mut last_err = primary_err;
        if primary_tool_weak
            && !primary_tools.is_empty()
            && is_ollama_no_user_query_error(&last_err)
        {
            tracing::warn!(
                "Primary Ollama {model}: no-user-query after keep-tools; rewrite+omit tools, retry once (ADR-0034)"
            );
            let omit_msgs = rewrite_messages_for_tool_weak(messages);
            match self.try_model(&omit_msgs, &[], model).await {
                Ok(completion) => return Ok(completion),
                Err(e) => {
                    last_err = e;
                }
            }
        }
        // ADR-0019: walk on any failure except hard exclusions (401/403,
        // pre-HTTP missing-key/config). 400/402/timeout/network/parse all walk.
        let fallback_chain = self.fallback_chain_snapshot();
        if !crate::provider::should_walk_fallback(&last_err) || fallback_chain.is_empty() {
            return Err(last_err);
        }
        for cand in &fallback_chain {
            let known = match cand.split_once('/') {
                Some((prefix, _)) => self.registry.get_provider(prefix).is_some(),
                None => true, // bare model → default provider, resolvable
            };
            if !known {
                tracing::warn!(
                    "Fallback entry '{cand}' names an unknown provider — skipping (typo in [fallback] chain?)"
                );
                continue;
            }
            let (provider, _) = self.registry.resolve_model(cand);
            let tool_weak = is_tool_weak_provider(provider.config());
            // ADR-0032: never 404-loop foreign OpenRouter-style ids on Ollama.
            if tool_weak && is_foreign_id_for_ollama(&self.registry, cand) {
                tracing::warn!("Fallback: skipping foreign id '{cand}' for Ollama (ADR-0032)");
                continue;
            }
            // ADR-0029 (+ #218): any tool-weak fallback with tools present
            // (incl. first turn + plain user), mid-loop, or user-less → rewrite + omit tools.
            // Do NOT reuse primary keep-tools shaping (ADR-0032) on the fallback walk.
            let needs_safe = tool_weak
                && (is_mid_tool_loop(messages)
                    || !has_plain_user_turn(messages)
                    || !tools.is_empty());
            let did_rewrite = needs_safe;
            let (req_messages, req_tools): (Vec<ChatMessage>, Vec<ToolDefinition>) = if needs_safe {
                tracing::warn!(
                    "Fallback: rewriting payload for tool-weak provider {cand} (ADR-0029; tools omitted)"
                );
                (rewrite_messages_for_tool_weak(messages), Vec::new())
            } else {
                (messages.to_vec(), tools.to_vec())
            };
            tracing::warn!(
                "Fallback: {} failed ({:#}) → trying {}",
                model,
                last_err,
                cand
            );
            match self.try_model(&req_messages, &req_tools, cand).await {
                Ok(mut completion) => {
                    completion.model = cand.clone();
                    tracing::info!("Fallback: {} answered after {} failed", cand, model);
                    return Ok(completion);
                }
                Err(e) => {
                    if tool_weak && is_ollama_no_user_query_error(&e) {
                        if did_rewrite {
                            tracing::warn!(
                                "Fallback: {cand} returned no-user-query after rewrite; skipping"
                            );
                        } else {
                            tracing::warn!(
                                "Fallback: {cand} returned no-user-query (no rewrite applied); skipping"
                            );
                        }
                        last_err = anyhow::anyhow!(USER_FACING_LLM_UNAVAILABLE);
                        continue;
                    }
                    if !crate::provider::should_walk_fallback(&e) {
                        // Auth hard-exclusion on a backup: surface it — do not
                        // keep hammering later chain entries with a bad key story.
                        return Err(e);
                    }
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// Single attempt against one qualified model, no chain logic.
    async fn try_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatCompletion> {
        let (provider, actual_model) = self.registry.resolve_model(model);
        // ADR-0032: do not send unknown-prefix OpenRouter-style ids to Ollama.
        if is_tool_weak_provider(provider.config())
            && is_foreign_id_for_ollama(&self.registry, model)
        {
            tracing::warn!(
                "Rejecting foreign model id '{model}' for Ollama (ADR-0032); not calling provider"
            );
            anyhow::bail!(
                "Model '{model}' is not a valid Ollama model id (unknown provider prefix). \
                 Use a bare Ollama library name or a registered provider/model."
            );
        }
        let max_tokens = provider.config().max_tokens;
        let tools_n = tools.len();
        tracing::info!(model = %model, tools = tools_n, "llm_call start");

        let mut completion = match provider
            .chat_completion(&self.client, messages, tools, actual_model, max_tokens)
            .await
        {
            Ok(c) => {
                tracing::info!(model = %model, tools = tools_n, "llm_call done");
                c
            }
            Err(e) => {
                let msg = format!("{e:#}").to_lowercase();
                if msg.contains("timed out") || msg.contains("timeout") {
                    tracing::warn!(model = %model, tools = tools_n, "llm_call timeout");
                } else {
                    tracing::debug!(model = %model, tools = tools_n, error = %e, "llm_call error");
                }
                return Err(e);
            }
        };

        completion.model = model.to_string();
        Ok(completion)
    }

    /// Convenience wrapper that uses the default provider's model.
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatMessage> {
        let default_model = self.registry.default_qualified_model();
        self.chat_completion_with_model(messages, tools, &default_model)
            .await
            .map(|c| c.message)
    }

    /// Stream an already-complete `content` string through a token channel in small chunks.
    ///
    /// This avoids a second LLM API call when the full response is already available from a
    /// preceding non-streaming call.  The caller still sees tokens arrive progressively,
    /// keeping the Telegram streaming UX intact, but there is no risk of the SSE connection
    /// being dropped mid-stream and silently returning a truncated response.
    pub async fn stream_text(
        content: String,
        token_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<()> {
        const CHUNK_SIZE: usize = 30;

        let chars: Vec<char> = content.chars().collect();
        let mut start = 0;

        while start < chars.len() {
            let end = (start + CHUNK_SIZE).min(chars.len());
            let chunk: String = chars[start..end].iter().collect();
            start = end;

            if token_tx.send(chunk).await.is_err() {
                // Receiver dropped — stop early, this is not an error
                debug!("stream_text: receiver dropped — stopping early");
                return Ok(());
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;
        }

        Ok(())
    }
}

/// Information about an OpenRouter model, deserialized from GET /api/v1/models.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_content_text_serializes_as_string() {
        let content = MessageContent::from_text("hello world");
        let json = serde_json::to_string(&content).unwrap();
        assert_eq!(json, r#""hello world""#);
    }

    #[test]
    fn test_message_content_parts_serializes_as_array() {
        let content = MessageContent::Parts(vec![ContentPart::Text {
            text: "hello".to_string(),
        }]);
        let json = serde_json::to_value(&content).unwrap();
        assert!(json.is_array());
        assert_eq!(json[0]["type"], "text");
        assert_eq!(json[0]["text"], "hello");
    }

    #[test]
    fn test_message_content_as_text_from_parts() {
        let content = MessageContent::Parts(vec![
            ContentPart::Text {
                text: "hello".to_string(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,abc".to_string(),
                },
            },
        ]);
        assert_eq!(content.as_text(), "hello");
    }

    #[test]
    fn test_chat_request_serializes_model_field() {
        // Verifies the model string will appear in the JSON POST body
        let req = ChatRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            messages: vec![],
            tools: None,
            max_tokens: 100,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn test_chat_request_default_model_is_different_from_override() {
        // Ensures chat_completion_with_model can use a different model than the config default
        let default_req = ChatRequest {
            model: "moonshotai/kimi-k2.5".to_string(),
            messages: vec![],
            tools: None,
            max_tokens: 100,
        };
        let override_req = ChatRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            messages: vec![],
            tools: None,
            max_tokens: 100,
        };
        let json_default = serde_json::to_value(&default_req).unwrap();
        let json_override = serde_json::to_value(&override_req).unwrap();
        assert_ne!(json_default["model"], json_override["model"]);
    }

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

    #[test]
    fn test_parse_kimi_tool_calls_single_call() {
        let content = " <|tool_calls_section_begin|> <|tool_call_begin|> functions.read_skill_file:5 \
            <|tool_call_argument_begin|> {\"skill_name\": \"reddit-fetcher\", \"relative_path\": \"SKILL.md\"} \
            <|tool_call_end|> <|tool_calls_section_end|>";
        let calls = parse_kimi_tool_calls(content).expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_skill_file");
        assert_eq!(calls[0].call_type, "function");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["skill_name"], "reddit-fetcher");
    }

    #[test]
    fn test_parse_kimi_tool_calls_multiple_calls() {
        let content = "<|tool_calls_section_begin|>\
            <|tool_call_begin|> functions.tool_a:0 <|tool_call_argument_begin|> {\"x\": 1} <|tool_call_end|>\
            <|tool_call_begin|> functions.tool_b:1 <|tool_call_argument_begin|> {\"y\": 2} <|tool_call_end|>\
            <|tool_calls_section_end|>";
        let calls = parse_kimi_tool_calls(content).expect("should parse");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "tool_a");
        assert_eq!(calls[1].function.name, "tool_b");
    }

    #[test]
    fn test_parse_kimi_tool_calls_no_markers_returns_none() {
        assert!(parse_kimi_tool_calls("Hello, world!").is_none());
        assert!(parse_kimi_tool_calls("").is_none());
    }

    #[test]
    fn test_parse_kimi_tool_calls_invalid_json_falls_back_to_empty_object() {
        let content = "<|tool_calls_section_begin|>\
            <|tool_call_begin|> functions.my_tool:0 \
            <|tool_call_argument_begin|> not valid json <|tool_call_end|>\
            <|tool_calls_section_end|>";
        let calls = parse_kimi_tool_calls(content).expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn test_parse_kimi_tool_calls_id_uses_index() {
        let content = "<|tool_calls_section_begin|>\
            <|tool_call_begin|> functions.do_thing:7 \
            <|tool_call_argument_begin|> {} <|tool_call_end|>\
            <|tool_calls_section_end|>";
        let calls = parse_kimi_tool_calls(content).expect("should parse");
        assert!(calls[0].id.contains("7"), "id should embed the call index");
    }

    #[tokio::test]
    async fn test_stream_text_sends_all_content() {
        let content = "Hello, world! This is a test of stream_text.".to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);

        LlmClient::stream_text(content.clone(), tx).await.unwrap();

        let mut received = String::new();
        while let Ok(chunk) = rx.try_recv() {
            received.push_str(&chunk);
        }
        assert_eq!(received, content);
    }

    #[tokio::test]
    async fn test_stream_text_stops_when_receiver_dropped() {
        let content = "A".repeat(1000);
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        // Drop the receiver immediately — stream_text should return Ok without panic
        drop(rx);

        let result = LlmClient::stream_text(content, tx).await;
        assert!(
            result.is_ok(),
            "stream_text must return Ok even when receiver is dropped"
        );
    }

    fn make_tool(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: name.to_string(),
                description: description.to_string(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        }
    }

    #[test]
    fn test_dedupe_tool_definitions_keep_first_drops_later_duplicates() {
        let tools = vec![
            make_tool("alpha", "first-alpha"),
            make_tool("beta", "only-beta"),
            make_tool("alpha", "second-alpha"),
            make_tool("gamma", "only-gamma"),
            make_tool("beta", "second-beta"),
        ];
        let deduped = dedupe_tool_definitions_keep_first(tools);
        let names: Vec<&str> = deduped.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
        assert_eq!(deduped[0].function.description, "first-alpha");
        assert_eq!(deduped[1].function.description, "only-beta");
    }

    #[test]
    fn test_dedupe_tool_definitions_wafer_style_high_index_duplicate() {
        // Regression for Wafer `duplicate_tool_name` at tools[155]-style payloads:
        // a long tool list with a late duplicate must not retain two identical names.
        let mut tools: Vec<ToolDefinition> = (0..160)
            .map(|i| make_tool(&format!("tool_{i}"), &format!("desc_{i}")))
            .collect();
        // Insert a duplicate of tools[10] at index 155.
        tools[155] = make_tool("tool_10", "late-duplicate-of-tool-10");
        let deduped = dedupe_tool_definitions_keep_first(tools);
        let names: Vec<&str> = deduped.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(names.len(), 159, "exactly one duplicate should be dropped");
        assert_eq!(
            names.iter().filter(|n| **n == "tool_10").count(),
            1,
            "tool_10 must appear exactly once"
        );
        let tool_10 = deduped
            .iter()
            .find(|t| t.function.name == "tool_10")
            .expect("tool_10 kept");
        assert_eq!(
            tool_10.function.description, "desc_10",
            "first occurrence must be kept"
        );
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "no duplicate function.name remains"
        );
    }

    #[test]
    fn test_sanitize_parameters_removes_undefined_required_entries() {
        // Google Gemini rejects required entries not present in properties.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "foo": { "type": "string" }
            },
            "required": ["foo", "bar"]  // "bar" is not in properties
        });
        sanitize_parameters(&mut schema);
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "foo");
    }

    #[test]
    fn test_sanitize_parameters_removes_additional_properties() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        });
        sanitize_parameters(&mut schema);
        assert!(schema.get("additionalProperties").is_none());
    }

    #[test]
    fn test_sanitize_parameters_removes_schema_metadata_fields() {
        let mut schema = serde_json::json!({
            "type": "object",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$defs": {},
            "$ref": "#/$defs/SomeType",
            "properties": {}
        });
        sanitize_parameters(&mut schema);
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("$defs").is_none());
        assert!(schema.get("$ref").is_none());
    }

    #[test]
    fn test_sanitize_parameters_recurses_into_properties() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "string" }
                    },
                    "required": ["x", "missing"],
                    "additionalProperties": true
                }
            }
        });
        sanitize_parameters(&mut schema);
        let nested = &schema["properties"]["nested"];
        let required = nested["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "x");
        assert!(nested.get("additionalProperties").is_none());
    }

    #[test]
    fn test_sanitize_parameters_recurses_into_array_items() {
        let mut schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "a": { "type": "number" }
                },
                "required": ["a", "b"],
                "additionalProperties": false
            }
        });
        sanitize_parameters(&mut schema);
        let items = &schema["items"];
        let required = items["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "a");
        assert!(items.get("additionalProperties").is_none());
    }

    #[test]
    fn test_sanitize_parameters_valid_schema_unchanged() {
        // A schema that is already valid should pass through unmodified.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" }
            },
            "required": ["path"]
        });
        let original = schema.clone();
        sanitize_parameters(&mut schema);
        assert_eq!(schema, original);
    }

    #[test]
    fn test_sanitize_parameters_removes_empty_required_array() {
        // Gemini rejects required: [] — it must be omitted entirely.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {},
            "required": ["a", "b"]  // neither "a" nor "b" is in properties
        });
        sanitize_parameters(&mut schema);
        // required should be gone, not left as []
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn test_sanitize_parameters_strips_null_anyof_variants() {
        // Gemini does not support {"type": "null"} as a union member.
        // When only one variant remains after stripping, it is unwrapped.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }
            }
        });
        sanitize_parameters(&mut schema);
        let name = &schema["properties"]["name"];
        // The null variant was stripped, leaving one variant which was unwrapped.
        assert!(name.get("anyOf").is_none(), "anyOf should be unwrapped");
        assert_eq!(name["type"], "string");
    }

    #[test]
    fn test_sanitize_parameters_recurses_into_anyof() {
        // Nested schemas inside anyOf/oneOf should also be sanitized.
        // Single-variant anyOf is unwrapped into the parent.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "val": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": { "x": { "type": "string" } },
                            "required": ["x", "missing"],
                            "additionalProperties": false
                        }
                    ]
                }
            }
        });
        sanitize_parameters(&mut schema);
        // Single-variant anyOf is unwrapped: inner object fields are inlined into "val".
        let val = &schema["properties"]["val"];
        assert!(val.get("anyOf").is_none(), "anyOf should be unwrapped");
        let required = val["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "x");
        assert!(val.get("additionalProperties").is_none());
    }

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
            content: Some(MessageContent::Text("  \n\t  ".to_string())),
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
            content: Some(MessageContent::Text("Done".to_string())),
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
        assert_eq!(
            completion.message.content.as_ref().map(|c| c.as_text()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_kimi_fallback_preserves_chat_completion_metadata() {
        // Simulates the Kimi fallback integration path in chat_completion_with_model.
        // When Kimi leaks native tool-call syntax into content, we parse it, clear
        // content, populate tool_calls, and set finish_reason to "tool_calls".
        let json = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<|tool_calls_section_begin|> <|tool_call_begin|> functions.read_skill_file:0 <|tool_call_argument_begin|> {\"skill_name\": \"reddit-fetcher\", \"relative_path\": \"SKILL.md\"} <|tool_call_end|> <|tool_calls_section_end|>"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        let mut choice = resp.choices.into_iter().next().unwrap();

        // Apply the same Kimi parsing logic used in chat_completion_with_model
        let has_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if !has_tool_calls {
            if let Some(ref content) = choice.message.content.clone() {
                if let Some(parsed) = parse_kimi_tool_calls(&content.as_text()) {
                    choice.message.tool_calls = Some(parsed);
                    choice.message.content = None;
                    choice.finish_reason = Some("tool_calls".to_string());
                }
            }
        }

        // Construct ChatCompletion as chat_completion_with_model does
        let completion = ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: "moonshotai/kimi-k2.5".to_string(),
        };

        // Assert metadata is correctly preserved
        assert_eq!(
            completion.finish_reason.as_deref(),
            Some("tool_calls"),
            "finish_reason must be set to 'tool_calls'"
        );
        assert!(
            completion.message.content.is_none(),
            "content must be cleared when Kimi tool calls are parsed"
        );
        assert!(
            completion.message.tool_calls.is_some(),
            "tool_calls must be populated"
        );
        let tool_calls = completion.message.tool_calls.as_ref().unwrap();
        assert!(
            !tool_calls.is_empty(),
            "tool_calls must contain at least one entry"
        );
        assert_eq!(
            tool_calls[0].function.name, "read_skill_file",
            "parsed tool name must match the Kimi content"
        );
    }

    // ---------------------------------------------------------------
    // ADR-0012 / ADR-0019: request-layer fallback chain (wiremock matrix)
    // ---------------------------------------------------------------

    use crate::config::ProviderType;
    use crate::provider::{OpenRouterProvider, ProviderConfig, ProviderRegistry};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::RwLock;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fb_provider_config(name: &str, base_url: String, model: &str) -> ProviderConfig {
        ProviderConfig {
            name: name.to_string(),
            provider_type: ProviderType::OpenRouter,
            base_url,
            api_key: Some("k".to_string()),
            default_model: model.to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 100,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0, // fail fast → deterministic request counts
        }
    }

    /// Registry with providers p1/p2/p3 all pointing at the same mock server.
    fn fb_registry(uri: String) -> Arc<ProviderRegistry> {
        let mut providers = HashMap::new();
        for (name, model) in [("p1", "m1"), ("p2", "m2"), ("p3", "m3")] {
            let provider: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
                fb_provider_config(name, uri.clone(), model),
            ));
            providers.insert(name.to_string(), provider);
        }
        Arc::new(ProviderRegistry::new(providers, "p1".to_string()))
    }

    fn fb_msgs() -> Vec<ChatMessage> {
        vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text("hi".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }]
    }

    fn ok_body() -> String {
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#
            .to_string()
    }

    async fn mount_model(server: &MockServer, model: &'static str, template: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": model })))
            .respond_with(template)
            .mount(server)
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_primary_success_never_touches_chain() {
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        // m1 ok; any m2 request would hit no mock → 404 counted via catch-all
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let h = Arc::clone(&hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |_req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(500)
            })
            .mount(&server)
            .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p1/m1");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "chain must not be consulted"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_429_primary_200_backup_rewrites_model() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p2/m2", "model must record who actually answered");
        assert_eq!(
            c.message.content.as_ref().map(|m| m.as_text()).unwrap(),
            "ok"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_5xx_first_backup_tries_second_in_order() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        mount_model(&server, "m2", ResponseTemplate::new(503)).await;
        mount_model(
            &server,
            "m3",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string(), "p3/m3".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p3/m3");
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_400_walks_to_backup_and_rewrites_model() {
        // ADR-0019: top-level 400 (incl. OpenRouter envelopes) MUST walk.
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(400).set_body_string("bad"),
        )
        .await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p2/m2");
        assert_eq!(
            c.message.content.as_ref().map(|m| m.as_text()).unwrap(),
            "ok"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_401_never_walks() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(401).set_body_string("bad key"),
        )
        .await;
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |_req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(ok_body())
            })
            .mount(&server)
            .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<crate::provider::LlmHttpError>()
                .is_some_and(|e| e.status == 401),
            "typed 401 must surface: {err}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "401 must not walk fallback");
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_403_never_walks() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(403).set_body_string("forbidden"),
        )
        .await;
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |_req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(ok_body())
            })
            .mount(&server)
            .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<crate::provider::LlmHttpError>()
                .is_some_and(|e| e.status == 403),
            "typed 403 must surface: {err}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "403 must not walk fallback");
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_402_walks_to_backup() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(402).set_body_json(serde_json::json!({
                "error": { "code": 402, "message": "Payment Required" }
            })),
        )
        .await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(
            c.model, "p2/m2",
            "402 credits must still reach local/backup"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_envelope_400_with_inner_429_walks() {
        // Observed Kan failure shape: outer HTTP 400 + previous_errors 429.
        let envelope = serde_json::json!({
            "error": {
                "code": 400,
                "message": "Provider returned error",
                "metadata": { "provider_name": "InferenceNet" }
            },
            "previous_errors": [
                {
                    "code": 429,
                    "message": "Rate limit",
                    "metadata": { "error_type": "rate_limit_exceeded" }
                }
            ]
        });
        let insight = crate::provider::parse_openrouter_envelope(&envelope.to_string())
            .expect("envelope must parse");
        assert!(
            insight.has_transient_inner(),
            "inner 429 / rate_limit_exceeded must classify as transient: {insight:?}"
        );
        assert!(insight.inner_codes.contains(&429));
        assert!(insight
            .error_types
            .iter()
            .any(|t| t == "rate_limit_exceeded"));

        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(400).set_body_json(envelope),
        )
        .await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p2/m2");
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_connect_fail_walks_to_backup() {
        // Primary points at a closed local port (never HTTP); backup is healthy.
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let mut providers = std::collections::HashMap::new();
        let dead: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p1", "http://127.0.0.1:1".to_string(), "m1"),
        ));
        let live: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p2", server.uri(), "m2"),
        ));
        providers.insert("p1".to_string(), dead);
        providers.insert("p2".to_string(), live);
        let reg = Arc::new(ProviderRegistry::new(providers, "p1".to_string()));
        let llm = LlmClient::new(reg).with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p2/m2", "connect/network fail must walk");
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_empty_chain_is_pure_adr0009() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()));
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("429"));
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_unknown_provider_entry_skipped_primary_error_kept() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["nonexistent/some-model".to_string()]);
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("p1 API error"),
            "primary's error must survive a skipped entry: {err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_nontransient_on_backup_stops_walk_and_surfaces() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        mount_model(
            &server,
            "m2",
            ResponseTemplate::new(401).set_body_string("no key"),
        )
        .await;
        mount_model(
            &server,
            "m3",
            ResponseTemplate::new(200).set_body_string(ok_body()),
        )
        .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string(), "p3/m3".to_string()]);
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("401"),
            "misconfigured backup (401) must be surfaced, not masked by later models: {err}"
        );
    }
    // ---------------------------------------------------------------
    // ADR-0029: Ollama / tool-weak mid-tool-loop rewrite (not raw send)
    // ---------------------------------------------------------------

    fn mid_tool_loop_no_user() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::from_text("sys")),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "run_command".into(),
                        arguments: "{}".into(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(MessageContent::from_text("tool-out")),
                tool_calls: None,
                tool_call_id: Some("c1".into()),
            },
        ]
    }

    fn mid_tool_loop_with_user() -> Vec<ChatMessage> {
        let mut m = mid_tool_loop_no_user();
        m.insert(
            1,
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::from_text("list files")),
                tool_calls: None,
                tool_call_id: None,
            },
        );
        m
    }

    fn ollama_cfg(name: &str, base_url: String, model: &str) -> ProviderConfig {
        ProviderConfig {
            name: name.to_string(),
            provider_type: ProviderType::Ollama,
            base_url,
            api_key: None,
            default_model: model.to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 100,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
        }
    }

    #[test]
    fn rewrite_mid_tool_loop_always_has_plain_user() {
        let rewritten = rewrite_messages_for_tool_weak(&mid_tool_loop_no_user());
        assert!(has_plain_user_turn(&rewritten));
        assert!(!is_mid_tool_loop(&rewritten));
        assert_eq!(rewritten[0].role, "system");
        assert_eq!(rewritten.last().unwrap().role, "user");
        let body = rewritten
            .last()
            .unwrap()
            .content
            .as_ref()
            .unwrap()
            .as_text();
        assert!(body.contains("tool-out"));
    }

    #[test]
    fn rewrite_preserves_last_user_text() {
        let rewritten = rewrite_messages_for_tool_weak(&mid_tool_loop_with_user());
        let body = rewritten
            .last()
            .unwrap()
            .content
            .as_ref()
            .unwrap()
            .as_text();
        assert!(body.contains("list files"));
        assert!(body.contains("tool-out"));
    }

    #[test]
    fn user_facing_hides_ollama_no_user_json() {
        let err = anyhow::anyhow!(
            "Ollama API error (500): {{\"error\":\"no user query found, message role must be user before tool / assistant with tool_calls\"}}"
        );
        let msg = user_facing_llm_error(&err);
        assert!(!msg.to_lowercase().contains("no user query found"));
        assert!(!msg.contains('{'));
        assert!(msg.contains("temporarily unavailable") || msg.contains("quota"));
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_ollama_mid_tool_loop_rewrites_and_omits_tools() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        // Reject raw mid-tool payload (no user) — simulates Ollama 500.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({
                "messages": [
                    {"role": "assistant"}
                ]
            })))
            .respond_with(
                ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#),
            )
            .mount(&server)
            .await;
        // Accept rewritten payload: must include a user turn; tools empty.
        let captured = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        let cap = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "qwen" })))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                *cap.lock().unwrap() = Some(body.clone());
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let has_user = msgs.iter().any(|m| m["role"] == "user");
                let tools_empty = body
                    .get("tools")
                    .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
                    .unwrap_or(true);
                if has_user && tools_empty {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                } else {
                    ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#)
                }
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let cloud: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p1", server.uri(), "m1"),
        ));
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        providers.insert("p1".to_string(), cloud);
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "p1".to_string()));
        let llm = LlmClient::new(reg).with_fallback_chain(vec!["ollama/qwen".to_string()]);

        // Primary fails → walk to Ollama with mid-tool no-user history + tools.
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;

        let tool = ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "run_command".into(),
                description: "x".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            },
        };
        let c = llm
            .chat_completion_with_model(&mid_tool_loop_no_user(), &[tool], "p1/m1")
            .await
            .expect("Ollama fallback must succeed after rewrite");
        assert_eq!(c.model, "ollama/qwen");
        let body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("ollama request captured");
        let msgs = body["messages"].as_array().unwrap();
        assert!(
            msgs.iter().any(|m| m["role"] == "user"),
            "rewritten request must include user: {body}"
        );
        assert!(
            !msgs.iter().any(|m| m["role"] == "tool"),
            "raw tool roles must not be forwarded: {body}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_cloud_to_cloud_still_sends_raw_mid_tool_loop() {
        let server = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": "rl"})),
        )
        .await;
        let cap = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "m2" })))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                *cap.lock().unwrap() = Some(body);
                ResponseTemplate::new(200).set_body_string(ok_body())
            })
            .mount(&server)
            .await;
        let llm = LlmClient::new(fb_registry(server.uri()))
            .with_fallback_chain(vec!["p2/m2".to_string()]);
        let c = llm
            .chat_completion_with_model(&mid_tool_loop_no_user(), &[], "p1/m1")
            .await
            .unwrap();
        assert_eq!(c.model, "p2/m2");
        let body = captured.lock().unwrap().clone().unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert!(
            msgs.iter().any(|m| m["role"] == "tool"),
            "cloud→cloud must keep ADR-0019 raw walk: {body}"
        );
    }

    // ---------------------------------------------------------------
    // ADR-0032: primary Ollama shaping + foreign-id routing
    // ---------------------------------------------------------------

    fn first_turn_with_tools_user() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::from_text("sys")),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::from_text("HK weather please")),
                tool_calls: None,
                tool_call_id: None,
            },
        ]
    }

    fn sample_tool() -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "get_weather".into(),
                description: "weather".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            },
        }
    }

    /// #218 / ADR-0029 amend: OpenRouter primary fail → Ollama first turn with
    /// tools + plain user must rewrite + omit tools (never raw tools that 500).
    #[tokio::test(start_paused = true)]
    async fn fallback_ollama_first_turn_with_tools_rewrites_and_omits_tools() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        // If raw tools were forwarded (pre-#218 bug), Ollama-style 500.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({
                "model": "qwen",
                "tools": [{"type": "function"}]
            })))
            .respond_with(
                ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#),
            )
            .mount(&server)
            .await;
        // Accept rewritten omit-tools payload with a plain user turn.
        let captured = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        let cap = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "qwen" })))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                *cap.lock().unwrap() = Some(body.clone());
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let has_user = msgs.iter().any(|m| {
                    m["role"] == "user"
                        && m["content"]
                            .as_str()
                            .map(|c| !c.trim().is_empty())
                            .unwrap_or(false)
                });
                let tools_empty = body
                    .get("tools")
                    .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
                    .unwrap_or(true);
                if has_user && tools_empty {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                } else {
                    ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#)
                }
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let cloud: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p1", server.uri(), "m1"),
        ));
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        providers.insert("p1".to_string(), cloud);
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "p1".to_string()));
        let llm = LlmClient::new(reg).with_fallback_chain(vec!["ollama/qwen".to_string()]);

        // Primary OpenRouter fails (402 credits) → walk to Ollama.
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(402).set_body_json(serde_json::json!({"error": "credits"})),
        )
        .await;

        let c = llm
            .chat_completion_with_model(&first_turn_with_tools_user(), &[sample_tool()], "p1/m1")
            .await
            .expect("Ollama first-turn fallback must succeed after rewrite+omit-tools");
        assert_eq!(c.model, "ollama/qwen");
        let body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("ollama request captured");
        let msgs = body["messages"].as_array().unwrap();
        assert!(
            msgs.iter().any(|m| m["role"] == "user"),
            "rewritten request must include user: {body}"
        );
        let tools_empty = body
            .get("tools")
            .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .unwrap_or(true);
        assert!(
            tools_empty,
            "first-turn tool-weak fallback must omit tools (ADR-0029/#218): {body}"
        );
        // User text from the original plain turn must survive rewrite.
        let user_text = msgs
            .iter()
            .filter(|m| m["role"] == "user")
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            user_text.contains("HK weather"),
            "plain user text must be preserved: {body}"
        );
    }

    #[test]
    fn shape_primary_first_turn_keeps_tools_and_user() {
        let (msgs, tools) =
            shape_primary_ollama_payload(&first_turn_with_tools_user(), &[sample_tool()]).unwrap();
        assert!(has_plain_user_turn(&msgs));
        assert_eq!(tools.len(), 1);
        assert!(!is_mid_tool_loop(&msgs));
        let body = serde_json::to_value(&msgs).unwrap();
        let arr = body.as_array().unwrap();
        assert!(
            arr.iter().any(|m| {
                m["role"] == "user" && m["content"].as_str().is_some_and(|c| !c.trim().is_empty())
            }),
            "serialized payload must include non-empty user text: {body}"
        );
    }

    #[test]
    fn shape_primary_mid_loop_rewrites_but_keeps_tools() {
        let (msgs, tools) =
            shape_primary_ollama_payload(&mid_tool_loop_no_user(), &[sample_tool()]).unwrap();
        assert!(has_plain_user_turn(&msgs));
        assert!(!msgs.iter().any(|m| m.role == "tool"));
        assert_eq!(tools.len(), 1, "ADR-0032 primary keeps tools");
    }

    #[test]
    fn empty_user_role_is_not_plain_user_turn() {
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text("   ")),
            tool_calls: None,
            tool_call_id: None,
        }];
        assert!(!has_plain_user_turn(&msgs));
    }

    #[tokio::test(start_paused = true)]
    async fn primary_ollama_first_turn_with_tools_never_sends_userless() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        let cap = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                *cap.lock().unwrap() = Some(body.clone());
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let has_user = msgs.iter().any(|m| {
                    m["role"] == "user"
                        && m["content"]
                            .as_str()
                            .map(|c| !c.trim().is_empty())
                            .unwrap_or(false)
                });
                if has_user {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                } else {
                    ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#)
                }
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg);

        let c = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[sample_tool()],
                "ollama/qwen",
            )
            .await
            .expect("primary Ollama + tools + user must succeed");
        assert_eq!(c.model, "ollama/qwen");
        let body = captured.lock().unwrap().clone().expect("request captured");
        let msgs = body["messages"].as_array().unwrap();
        assert!(
            msgs.iter().any(|m| m["role"] == "user"),
            "must include user: {body}"
        );
        let tools = body.get("tools").and_then(|t| t.as_array());
        assert!(
            tools.is_some_and(|t| !t.is_empty()),
            "primary must keep tools: {body}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn foreign_prefix_with_ollama_default_does_not_call_ollama() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                // If somehow called with the foreign model string, fail the test path.
                if body["model"].as_str() == Some("deepseek/deepseek-v4-flash") {
                    ResponseTemplate::new(404).set_body_string(r#"{"error":"model not found"}"#)
                } else {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                }
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg);

        let err = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[],
                "deepseek/deepseek-v4-flash",
            )
            .await
            .expect_err("foreign id must not be sent to Ollama");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "Ollama must not be called with foreign model string"
        );
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("not a valid ollama") || msg.contains("temporarily unavailable"),
            "clear RustFox error expected, got: {err:#}"
        );
    }

    #[test]
    fn is_foreign_id_detects_unknown_prefix_only() {
        use crate::provider::OllamaProvider;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            "http://127.0.0.1:9".into(),
            "qwen",
        )));
        providers.insert("ollama".to_string(), ollama);
        let reg = ProviderRegistry::new(providers, "ollama".to_string());
        assert!(is_foreign_id_for_ollama(&reg, "deepseek/deepseek-v4-flash"));
        assert!(!is_foreign_id_for_ollama(&reg, "ollama/qwen"));
        assert!(!is_foreign_id_for_ollama(&reg, "qwen3.8:27b"));
    }

    // ---------------------------------------------------------------
    // ADR-0034: primary Ollama no-user-query → rewrite+omit retry once
    // ---------------------------------------------------------------

    #[test]
    fn is_ollama_no_user_query_detects_message() {
        let err = anyhow::anyhow!(
            "Ollama API error (500): {{\"error\":\"no user query found in messages\"}}"
        );
        assert!(is_ollama_no_user_query_error(&err));
        assert!(!is_ollama_no_user_query_error(&anyhow::anyhow!(
            "rate limited"
        )));
    }

    /// Keep-tools first call 500s with no-user-query; omit-retry on same
    /// primary succeeds (tools empty). ADR-0032 first-pass keep-tools unchanged.
    #[tokio::test(start_paused = true)]
    async fn primary_ollama_no_user_query_omit_retries_once() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        let calls = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let cap = Arc::clone(&calls);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                cap.lock().unwrap().push(body.clone());
                let tools_present = body
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .is_some_and(|a| !a.is_empty());
                if tools_present {
                    // Simulate keep-tools 500 (large tool-set / compaction).
                    ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#)
                } else {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                }
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg);

        let c = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[sample_tool()],
                "ollama/qwen",
            )
            .await
            .expect("ADR-0034 omit-retry must succeed on same primary");
        assert_eq!(c.model, "ollama/qwen");

        let bodies = calls.lock().unwrap().clone();
        assert_eq!(
            bodies.len(),
            2,
            "exactly one keep-tools attempt + one omit-retry: {bodies:?}"
        );
        let tools0 = bodies[0]
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(
            tools0 > 0,
            "first pass must keep tools (ADR-0032): {}",
            bodies[0]
        );
        let tools1_empty = bodies[1]
            .get("tools")
            .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .unwrap_or(true);
        assert!(
            tools1_empty,
            "omit-retry must omit tools (ADR-0034): {}",
            bodies[1]
        );
        let msgs1 = bodies[1]["messages"].as_array().unwrap();
        assert!(
            msgs1.iter().any(|m| m["role"] == "user"),
            "omit-retry payload must include user: {}",
            bodies[1]
        );
    }

    /// Omit-retry also fails → existing fallback chain (ADR-0029) unchanged.
    #[tokio::test(start_paused = true)]
    async fn primary_omit_retry_fail_walks_fallback() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;

        // Primary Ollama: keep-tools → 500; omit-retry → 500 again.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "qwen" })))
            .respond_with(
                ResponseTemplate::new(500).set_body_string(r#"{"error":"no user query found"}"#),
            )
            .expect(2)
            .mount(&server)
            .await;

        // Fallback cloud answers.
        let fb_hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&fb_hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "m2" })))
            .respond_with(move |_req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(ok_body())
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        let cloud: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p2", server.uri(), "m2"),
        ));
        providers.insert("ollama".to_string(), ollama);
        providers.insert("p2".to_string(), cloud);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg).with_fallback_chain(vec!["p2/m2".to_string()]);

        let c = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[sample_tool()],
                "ollama/qwen",
            )
            .await
            .expect("fallback must run after primary omit-retry fails");
        assert_eq!(c.model, "p2/m2");
        assert_eq!(
            fb_hits.load(Ordering::SeqCst),
            1,
            "fallback cloud must be tried once"
        );
    }

    /// Non no-user-query primary failure must NOT trigger omit-retry (no second
    /// same-model call); walk fallback directly when chain is set.
    #[tokio::test(start_paused = true)]
    async fn primary_other_error_skips_omit_retry() {
        use crate::provider::OllamaProvider;

        let server = MockServer::start().await;
        let ollama_hits = Arc::new(AtomicUsize::new(0));
        let oh = Arc::clone(&ollama_hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "qwen" })))
            .respond_with(move |_req: &wiremock::Request| {
                oh.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(500).set_body_string(r#"{"error":"model overloaded"}"#)
            })
            .mount(&server)
            .await;

        let fb_hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&fb_hits);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "model": "m2" })))
            .respond_with(move |_req: &wiremock::Request| {
                h.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(ok_body())
            })
            .mount(&server)
            .await;

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> = Arc::new(OllamaProvider::new(ollama_cfg(
            "ollama",
            server.uri(),
            "qwen",
        )));
        let cloud: Arc<dyn crate::provider::Provider> = Arc::new(OpenRouterProvider::new(
            fb_provider_config("p2", server.uri(), "m2"),
        ));
        providers.insert("ollama".to_string(), ollama);
        providers.insert("p2".to_string(), cloud);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg).with_fallback_chain(vec!["p2/m2".to_string()]);

        let c = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[sample_tool()],
                "ollama/qwen",
            )
            .await
            .expect("fallback after non-userless primary error");
        assert_eq!(c.model, "p2/m2");
        assert_eq!(
            ollama_hits.load(Ordering::SeqCst),
            1,
            "omit-retry must not run for non no-user-query errors"
        );
        assert_eq!(fb_hits.load(Ordering::SeqCst), 1);
    }

    /// Live soft-fail regression (tip 25dc563 / PID 455070): Active registered as
    /// `openai_compatible` named `ollama` (not `ProviderType::Ollama`), keep-tools
    /// 500 with nested OpenAI-shaped `no user query found in messages`, and
    /// `rate_limit_retry_limit > 0`. Pre-fix: ADR-0009 Rate/transient same-payload
    /// retries then Fallback — no ADR-0034 WARN because `primary_tool_weak` was false.
    #[tokio::test(start_paused = true)]
    async fn soft_omit_retry_openai_compat_ollama_live_skip() {
        use crate::provider::OpenAICompatibleProvider;

        let server = MockServer::start().await;
        let calls = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let cap = Arc::clone(&calls);
        // Live Ollama OpenAI-compat envelope (not the flat `{"error":"..."}` string).
        const LIVE_BODY: &str = r#"{"error":{"message":"no user query found in messages","type":"api_error","param":null,"code":null}}"#;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                cap.lock().unwrap().push(body.clone());
                let tools_present = body
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .is_some_and(|a| !a.is_empty());
                if tools_present {
                    ResponseTemplate::new(500).set_body_string(LIVE_BODY)
                } else {
                    ResponseTemplate::new(200).set_body_string(ok_body())
                }
            })
            .mount(&server)
            .await;

        let mut cfg = ollama_cfg("ollama", server.uri(), "qwen");
        // Live Kan misconfig / OpenAI-compat listener: type openai_compatible,
        // name ollama, rate_limit_retry_limit default 3.
        cfg.provider_type = ProviderType::OpenAICompatible;
        cfg.rate_limit_retry_limit = 3;
        assert!(
            is_tool_weak_provider(&cfg),
            "openai_compatible named ollama must be tool-weak"
        );

        let mut providers = HashMap::new();
        let ollama: Arc<dyn crate::provider::Provider> =
            Arc::new(OpenAICompatibleProvider::new(cfg));
        providers.insert("ollama".to_string(), ollama);
        let reg = Arc::new(ProviderRegistry::new(providers, "ollama".to_string()));
        let llm = LlmClient::new(reg);

        let c = llm
            .chat_completion_with_model(
                &first_turn_with_tools_user(),
                &[sample_tool()],
                "ollama/qwen",
            )
            .await
            .expect("ADR-0034 omit-retry must arm for openai-compat Ollama");
        assert_eq!(c.model, "ollama/qwen");

        let bodies = calls.lock().unwrap().clone();
        assert_eq!(
            bodies.len(),
            2,
            "fail-fast no-user-query (no ADR-0009 keep-tools storm) + one omit-retry: {bodies:?}"
        );
        let tools0 = bodies[0]
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(tools0 > 0, "first pass must keep tools: {}", bodies[0]);
        let tools1_empty = bodies[1]
            .get("tools")
            .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .unwrap_or(true);
        assert!(
            tools1_empty,
            "omit-retry must omit tools (ADR-0034): {}",
            bodies[1]
        );
    }

    #[test]
    fn tool_weak_detects_openai_compat_ollama_by_name_or_port() {
        let mut by_name = ollama_cfg("ollama", "http://example:9999/v1".into(), "qwen");
        by_name.provider_type = ProviderType::OpenAICompatible;
        assert!(is_tool_weak_provider(&by_name));

        let mut by_port = ollama_cfg("local", "http://127.0.0.1:11434/v1".into(), "qwen");
        by_port.provider_type = ProviderType::OpenAICompatible;
        assert!(is_tool_weak_provider(&by_port));

        let mut other = ollama_cfg("lmstudio", "http://127.0.0.1:1234/v1".into(), "qwen");
        other.provider_type = ProviderType::OpenAICompatible;
        assert!(!is_tool_weak_provider(&other));

        assert!(is_tool_weak_provider(&ollama_cfg(
            "ollama",
            "http://localhost:11434/v1".into(),
            "qwen"
        )));
    }

    /// #231: hanging provider HTTP must fail via client timeout (not spin forever).
    #[tokio::test(start_paused = true)]
    async fn llm_request_timeout_fails_hanging_http() {
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(60))
                    .set_body_string(ok_body()),
            )
            .mount(&server)
            .await;

        let llm = LlmClient::with_request_timeout_secs(fb_registry(server.uri()), 1);
        let err = llm
            .chat_completion_with_model(&fb_msgs(), &[], "p1/m1")
            .await
            .expect_err("hanging HTTP must time out");
        let s = format!("{err:#}").to_lowercase();
        assert!(
            s.contains("timed out") || s.contains("timeout"),
            "expected timeout error, got: {s}"
        );
    }
}
