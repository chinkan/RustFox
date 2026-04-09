use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::OpenRouterConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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

    /// Chat with an explicit model string (used by subagents to override the default).
    pub async fn chat_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatMessage> {
        let tools_param = if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
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
            if choice.message.content.as_deref().is_none_or(str::is_empty)
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

        Ok(choice.message)
    }

    /// Chat using the model configured in config.toml (delegates to chat_with_model).
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatMessage> {
        self.chat_with_model(messages, tools, &self.config.model)
            .await
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
}
