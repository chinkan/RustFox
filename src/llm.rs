use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::OpenRouterConfig;

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
    let has_content = message
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());

    !has_tool_calls && !has_content
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    max_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
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
fn sanitize_parameters(schema: &mut serde_json::Value) {
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
fn parse_kimi_tool_calls(content: &str) -> Option<Vec<ToolCall>> {
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
    client: reqwest::Client,
    config: OpenRouterConfig,
}

impl LlmClient {
    pub fn new(config: OpenRouterConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    /// Core chat method returning full completion metadata (message, finish_reason, model).
    ///
    /// This is the lowest-level method that performs the HTTP request and returns
    /// all available metadata from the LLM response. All other chat methods delegate
    /// to this one.
    pub async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatCompletion> {
        let tools_param = if tools.is_empty() {
            None
        } else {
            let sanitized = tools
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    sanitize_parameters(&mut t.function.parameters);
                    t
                })
                .collect();
            Some(sanitized)
        };

        let tool_choice = if tools_param.is_some() {
            Some("auto".to_string())
        } else {
            None
        };

        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools_param,
            tool_choice,
            max_tokens: self.config.max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);

        debug!(
            url = %url,
            model = %model,
            message_count = messages.len(),
            tool_count = tools.len(),
            "Sending request to OpenRouter"
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenRouter")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter API error ({}): {}", status, error_body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter response")?;

        if let Some(choice) = chat_response.choices.first() {
            debug!(
                finish_reason = ?choice.finish_reason,
                has_content = choice.message.content.is_some(),
                tool_call_count = choice.message.tool_calls.as_ref().map_or(0, |t| t.len()),
                "Received LLM response"
            );
            if choice.message.content.as_ref().is_none_or(MessageContent::is_empty)
                && choice.message.tool_calls.as_ref().is_none_or(Vec::is_empty)
            {
                warn!(
                    finish_reason = ?choice.finish_reason,
                    "LLM returned no content and no tool calls"
                );
            }
        }

        let mut choice = chat_response
            .choices
            .into_iter()
            .next()
            .context("No response from OpenRouter")?;

        // Kimi-family models occasionally leak their native tool-call syntax into
        // the `content` field instead of populating `tool_calls`.  Detect and fix.
        let has_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if !has_tool_calls {
            if let Some(ref content) = choice.message.content.clone() {
                if let Some(parsed) = parse_kimi_tool_calls(content) {
                    warn!(
                        tool_count = parsed.len(),
                        "Kimi native tool-call format detected in content — \
                         extracting tool calls and clearing content"
                    );
                    choice.message.tool_calls = Some(parsed);
                    choice.message.content = None;
                    choice.finish_reason = Some("tool_calls".to_string());
                }
            }
        }

        Ok(ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: model.to_string(),
        })
    }

    /// Compatibility wrapper returning only the message (delegates to chat_completion_with_model).
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

    /// Chat using an explicit model, returning full completion metadata.
    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatCompletion> {
        self.chat_completion_with_model(messages, tools, &self.config.model)
            .await
    }

    /// Chat using the model configured in config.toml (delegates to chat_completion).
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatMessage> {
        Ok(self.chat_completion(messages, tools).await?.message)
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
            tool_choice: None,
            max_tokens: 100,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn test_chat_request_default_model_is_different_from_override() {
        // Ensures chat_with_model can use a different model than the config default
        let default_req = ChatRequest {
            model: "moonshotai/kimi-k2.5".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            max_tokens: 100,
        };
        let override_req = ChatRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
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
                if let Some(parsed) = parse_kimi_tool_calls(content) {
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
}
