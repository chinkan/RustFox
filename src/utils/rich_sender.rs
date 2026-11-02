use serde::{Deserialize, Serialize};
use std::future::Future;
use tracing::warn;

/// Error type distinguishing bad-markdown (retriable) from network (fatal).
#[derive(Debug)]
pub enum RichSenderError {
    /// HTTP 400 from Telegram — bad markdown, triggers entity fallback.
    BadMarkdown(String),
    /// HTTP 5xx, network error, etc. — propagated as fatal.
    Network(anyhow::Error),
}

impl std::fmt::Display for RichSenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RichSenderError::BadMarkdown(msg) => write!(f, "bad markdown: {msg}"),
            RichSenderError::Network(e) => write!(f, "network error: {e}"),
        }
    }
}

impl std::error::Error for RichSenderError {}

// ---------------------------------------------------------------------------
// JSON payload shapes for the Telegram Bot API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InputRichMessage {
    markdown: String,
    #[serde(rename = "skip_entity_detection")]
    skip_entity_detection: bool,
}

#[derive(Serialize)]
struct SendRichMessagePayload {
    chat_id: i64,
    rich_message: InputRichMessage,
}

#[derive(Serialize)]
struct EditRichMessagePayload {
    chat_id: i64,
    message_id: i32,
    rich_message: InputRichMessage,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Default Telegram Bot API root (no trailing slash).
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// Normalize a configured or `Bot::api_url()` base to the form used by
/// [`api_url`]: scheme+host[+path], no trailing slash.
pub fn normalize_api_base(api_base: &str) -> &str {
    api_base.trim().trim_end_matches('/')
}

fn build_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Build `{base}/bot{token}/{method}`. `api_base` may carry a trailing slash.
///
/// Does not log or return the token outside the URL string itself (Telegram
/// path shape); callers must not echo the result into user-visible logs.
pub fn api_url(api_base: &str, token: &str, method: &str) -> String {
    let base = normalize_api_base(api_base);
    format!("{base}/bot{token}/{method}")
}

async fn parse_response(response: reqwest::Response) -> Result<serde_json::Value, RichSenderError> {
    let status = response.status();
    let body = response.text().await;

    #[derive(Deserialize)]
    struct TgResponse {
        ok: bool,
        description: Option<String>,
        result: Option<serde_json::Value>,
    }

    let body = match body {
        Ok(b) => b,
        Err(e) => return Err(RichSenderError::Network(e.into())),
    };

    let parsed: TgResponse = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => return Err(RichSenderError::Network(e.into())),
    };

    if parsed.ok {
        Ok(parsed.result.unwrap_or(serde_json::Value::Null))
    } else if status == 400 || status == 422 {
        Err(RichSenderError::BadMarkdown(
            parsed.description.unwrap_or_default(),
        ))
    } else {
        Err(RichSenderError::Network(anyhow::anyhow!(
            "Telegram API error ({}): {}",
            status,
            parsed.description.unwrap_or_default()
        )))
    }
}

/// Send a single message via `sendRichMessage`.
///
/// `api_base` is the Bot API root (default [`DEFAULT_API_BASE`]); pass
/// `bot.api_url().as_str()` so wiremock / local Bot API servers work.
pub async fn send_rich_message(
    api_base: &str,
    token: &str,
    chat_id: i64,
    markdown: &str,
) -> Result<serde_json::Value, RichSenderError> {
    let client = build_client();
    let payload = SendRichMessagePayload {
        chat_id,
        rich_message: InputRichMessage {
            markdown: markdown.to_string(),
            skip_entity_detection: true,
        },
    };

    let response = client
        .post(api_url(api_base, token, "sendRichMessage"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RichSenderError::Network(e.into()))?;

    parse_response(response).await
}

/// Edit an existing message via `editMessageText` with `rich_message` param.
pub async fn edit_rich_message(
    api_base: &str,
    token: &str,
    chat_id: i64,
    message_id: i32,
    markdown: &str,
) -> Result<serde_json::Value, RichSenderError> {
    let client = build_client();
    let payload = EditRichMessagePayload {
        chat_id,
        message_id,
        rich_message: InputRichMessage {
            markdown: markdown.to_string(),
            skip_entity_detection: true,
        },
    };

    let response = client
        .post(api_url(api_base, token, "editMessageText"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RichSenderError::Network(e.into()))?;

    parse_response(response).await
}

/// `message_id` from a successful `sendRichMessage` / `sendMessage` result object.
pub(crate) fn message_id_from_result(result: &serde_json::Value) -> Option<i32> {
    let n = result.get("message_id")?.as_i64()?;
    i32::try_from(n).ok()
}

fn indexed_chunk(result: &serde_json::Value, text: &str) -> Option<(i32, String)> {
    let id = message_id_from_result(result)?;
    if text.is_empty() {
        None
    } else {
        Some((id, text.to_string()))
    }
}

/// Send potentially-long markdown split at newline boundaries (max 4090 UTF-16).
///
/// Returns `(message_id, chunk_text)` for each chunk Telegram accepted.
/// Error only if the FIRST chunk fails (subsequent errors logged only).
pub async fn send_rich_messages(
    api_base: &str,
    token: &str,
    chat_id: i64,
    markdown: &str,
) -> Result<Vec<(i32, String)>, RichSenderError> {
    const MAX_UTF16: usize = 4090;

    let total_utf16 = markdown.encode_utf16().count();
    if total_utf16 <= MAX_UTF16 {
        let result = send_rich_message(api_base, token, chat_id, markdown).await?;
        return Ok(indexed_chunk(&result, markdown).into_iter().collect());
    }

    let chunks = split_markdown_at_newlines(markdown, MAX_UTF16);
    let mut sent = Vec::new();

    for (i, chunk) in chunks.iter().enumerate() {
        if i == 0 {
            let result = send_rich_message(api_base, token, chat_id, chunk).await?;
            sent.extend(indexed_chunk(&result, chunk));
        } else {
            match send_rich_message(api_base, token, chat_id, chunk).await {
                Ok(result) => sent.extend(indexed_chunk(&result, chunk)),
                Err(e) => warn!("send_rich_message trailing chunk {i} failed: {e}"),
            }
        }
    }
    Ok(sent)
}

/// Split markdown at newline boundaries so each chunk fits within `max_utf16`.
pub(crate) fn split_markdown_at_newlines(text: &str, max_utf16: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let total = text.encode_utf16().count();

    while start < total {
        let ideal_end = (start + max_utf16).min(total);
        // Find the closest newline before ideal_end
        let mut split_at = ideal_end;
        // Convert byte positions for substring search
        let byte_start = char_boundary_from_utf16(text, start);
        let byte_ideal = char_boundary_from_utf16(text, ideal_end);
        if let Some(newline_byte) = text[byte_start..byte_ideal].rfind('\n') {
            let newline_utf16 = text[..byte_start + newline_byte + 1].encode_utf16().count();
            if newline_utf16 > start {
                split_at = newline_utf16;
            }
        }

        // convert to byte slice
        let byte_start = char_boundary_from_utf16(text, start);
        let byte_end = char_boundary_from_utf16(text, split_at);
        result.push(text[byte_start..byte_end].to_string());
        start = split_at;
    }

    result
}

fn char_boundary_from_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_so_far = 0;
    for (byte_pos, ch) in text.char_indices() {
        if utf16_so_far >= utf16_offset {
            return byte_pos;
        }
        utf16_so_far += ch.len_utf16();
    }
    text.len()
}

/// Try sending via sendRichMessage; on BadMarkdown, call `entity_sender` as fallback.
///
/// The `Ok` vec is the bubbles that actually landed (rich, or entity fallback).
pub async fn try_send_rich_fallback<F, Fut, E>(
    api_base: &str,
    token: &str,
    chat_id: i64,
    markdown: &str,
    entity_sender: F,
) -> Result<Vec<(i32, String)>, RichSenderError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<(i32, String)>, E>>,
    E: std::fmt::Display,
{
    let processed = crate::utils::markdown_entities::preprocess_markdown(markdown);
    match send_rich_messages(api_base, token, chat_id, &processed).await {
        Ok(sent) => Ok(sent),
        Err(RichSenderError::BadMarkdown(msg)) => {
            warn!("sendRichMessage failed (bad markdown), falling back to entities: {msg}");
            entity_sender()
                .await
                .map_err(|e| RichSenderError::Network(anyhow::anyhow!("fallback: {e}")))
        }
        Err(e @ RichSenderError::Network(_)) => {
            warn!("sendRichMessage network error, propagating to caller: {e}");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_markdown_short_text_not_split() {
        let chunks = split_markdown_at_newlines("hello", 4090);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn test_split_markdown_at_newline_boundary() {
        let text = "A".repeat(2000) + "\n" + &"B".repeat(2000);
        let chunks = split_markdown_at_newlines(&text, 3000);
        assert!(chunks.len() >= 2, "should split into at least 2 chunks");
        assert!(
            chunks[0].ends_with('\n'),
            "first chunk should end with newline"
        );
        assert!(
            !chunks[1].starts_with('\n'),
            "second chunk should not start with newline"
        );
    }

    #[test]
    fn test_split_markdown_utf16_cjk() {
        // Each CJK char = 1 UTF-16 unit, "你好" = 2 units
        let text = "你好".repeat(3000); // 6000 UTF-16 units
        let chunks = split_markdown_at_newlines(&text, 4090);
        assert!(chunks.len() > 1, "long CJK text must be split");
        for chunk in &chunks {
            let utf16_len = chunk.encode_utf16().count();
            assert!(
                utf16_len <= 4090,
                "chunk must not exceed max_utf16: {utf16_len} > 4090"
            );
        }
    }

    #[test]
    fn test_preprocess_markdown_pub() {
        // Verify preprocess_markdown is accessible
        let result = crate::utils::markdown_entities::preprocess_markdown("**bold**");
        assert!(
            result.contains("**bold**"),
            "preprocess should pass through normal markdown"
        );
    }

    #[test]
    fn test_split_markdown_exact_small() {
        let chunks = split_markdown_at_newlines("short", 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short");
    }

    #[test]
    fn test_rich_sender_error_type() {
        let bad_md = RichSenderError::BadMarkdown("bad".into());
        let net = RichSenderError::Network(anyhow::anyhow!("timeout"));
        assert!(matches!(bad_md, RichSenderError::BadMarkdown(_)));
        assert!(matches!(net, RichSenderError::Network(_)));
        assert!(!matches!(bad_md, RichSenderError::Network(_)));
        assert!(!matches!(net, RichSenderError::BadMarkdown(_)));
    }

    #[test]
    fn message_id_from_send_result() {
        let hit = serde_json::json!({"message_id": 32886, "text": ""});
        assert_eq!(message_id_from_result(&hit), Some(32886));
        assert_eq!(message_id_from_result(&serde_json::json!({})), None);
        assert_eq!(
            message_id_from_result(&serde_json::json!({"message_id": true})),
            None
        );
    }

    #[test]
    fn test_api_url_default_base() {
        let url = api_url(DEFAULT_API_BASE, "000:TOKEN", "sendRichMessage");
        assert_eq!(url, "https://api.telegram.org/bot000:TOKEN/sendRichMessage");
    }

    #[test]
    fn test_api_url_custom_base_trims_slash() {
        let url = api_url("http://127.0.0.1:9/", "000:TOKEN", "sendRichMessage");
        assert_eq!(url, "http://127.0.0.1:9/bot000:TOKEN/sendRichMessage");
        assert_eq!(
            normalize_api_base("http://127.0.0.1:9/"),
            "http://127.0.0.1:9"
        );
    }
}
