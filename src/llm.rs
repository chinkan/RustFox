use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

use std::sync::Arc;

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

#[derive(Clone)]
pub struct LlmClient {
    pub client: reqwest::Client,
    pub registry: Arc<crate::provider::ProviderRegistry>,
    /// Ordered fallback chain of fully-qualified `provider/model` names tried
    /// (after the primary) when a call dies with a *transient* HTTP error
    /// (429/5xx — see [`crate::provider::LlmHttpError`]). Empty (default) =
    /// ADR-0009 behaviour exactly: primary only. ADR-0012.
    pub fallback_chain: Vec<String>,
}

impl LlmClient {
    pub fn new(registry: Arc<crate::provider::ProviderRegistry>) -> Self {
        Self {
            client: reqwest::Client::new(),
            registry,
            fallback_chain: Vec::new(),
        }
    }

    /// Attach a fallback chain (from `[fallback] chain` in config). Entries
    /// whose provider prefix is not in the registry are dropped with a
    /// warning at call time, never attempted against the wrong provider.
    pub fn with_fallback_chain(mut self, chain: Vec<String>) -> Self {
        // Bound worst-case latency (primary budget + N fallback budgets).
        const MAX_FALLBACK_CHAIN: usize = 5;
        if chain.len() > MAX_FALLBACK_CHAIN {
            tracing::warn!(
                "[fallback] chain: {} entries exceeds cap {MAX_FALLBACK_CHAIN}, truncating",
                chain.len()
            );
        }
        self.fallback_chain = chain.into_iter().take(MAX_FALLBACK_CHAIN).collect();
        self
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
        let primary_err = match self.try_model(messages, tools, model).await {
            Ok(completion) => return Ok(completion),
            Err(e) => e,
        };
        // Only transient (429/5xx) failures justify switching model — a 400
        // would fail identically on every candidate (ADR-0012 trigger rule).
        let transient = primary_err
            .downcast_ref::<crate::provider::LlmHttpError>()
            .is_some_and(|e| e.is_transient());
        if !transient || self.fallback_chain.is_empty() {
            return Err(primary_err);
        }
        let mut last_err = primary_err;
        for cand in &self.fallback_chain {
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
            tracing::warn!(
                "Fallback: {} failed ({:#}) → trying {}",
                model,
                last_err,
                cand
            );
            match self.try_model(messages, tools, cand).await {
                Ok(mut completion) => {
                    completion.model = cand.clone();
                    tracing::info!("Fallback: {} answered after {} failed", cand, model);
                    return Ok(completion);
                }
                Err(e) => {
                    let still_transient = e
                        .downcast_ref::<crate::provider::LlmHttpError>()
                        .is_some_and(|x| x.is_transient());
                    if !still_transient {
                        // Non-transient on a fallback: resending elsewhere is
                        // pointless; surface it (usually config/compat error).
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
        let max_tokens = provider.config().max_tokens;

        let mut completion = provider
            .chat_completion(&self.client, messages, tools, actual_model, max_tokens)
            .await?;

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
    // ADR-0012: request-layer fallback chain (wiremock matrix)
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
    async fn fallback_400_never_switches_models() {
        let server = MockServer::start().await;
        mount_model(
            &server,
            "m1",
            ResponseTemplate::new(400).set_body_string("bad"),
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
                .is_some_and(|e| e.status == 400),
            "typed error must carry status"
        );
        // m2 mock exists but must never have been consulted.
        assert_eq!(hits.load(Ordering::SeqCst), 0, "400 must not switch models");
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
}
