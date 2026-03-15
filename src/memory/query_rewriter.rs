use crate::llm::{ChatMessage, LlmClient};

/// Rewrite an ambiguous follow-up question into a self-contained search query.
/// Uses the last ≤3 non-system messages as conversation context.
/// On any failure (LLM error, empty response), returns the original query unchanged.
#[allow(dead_code)]
pub async fn rewrite_for_rag(
    llm: &LlmClient,
    user_message: &str,
    recent_messages: &[ChatMessage],
) -> String {
    let history = format_history(recent_messages);

    let prompt = format!(
        "Rewrite the QUESTION below as a single, self-contained search query.\n\
         Use the CONVERSATION HISTORY to resolve any unclear pronouns or references.\n\
         Output ONLY the rewritten query. No explanation. No punctuation at the end.\n\
         \n\
         RULES:\n\
         - Replace pronouns (he/she/it/they/that/this/there) with the specific name or thing\n\
         - If the question is already clear and self-contained, output it unchanged\n\
         - Maximum 30 words\n\
         \n\
         CONVERSATION HISTORY (most recent last):\n\
         {history}\n\
         \n\
         QUESTION: {user_message}\n\
         \n\
         REWRITTEN QUERY:",
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(
                "You are a query rewriter. Output only the rewritten query, nothing else."
                    .to_string(),
            ),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(prompt),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    match llm.chat(&messages, &[]).await {
        Ok(response) => {
            let rewritten = response
                .content
                .unwrap_or_default()
                .trim()
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();

            if rewritten.is_empty() {
                tracing::debug!(
                    "Query rewriter returned empty — using original: {:?}",
                    user_message
                );
                user_message.to_string()
            } else {
                tracing::debug!("Query rewritten: {:?} → {:?}", user_message, rewritten);
                rewritten
            }
        }
        Err(e) => {
            tracing::debug!("Query rewrite failed (using original): {:#}", e);
            user_message.to_string()
        }
    }
}

/// Format recent messages for the rewrite prompt.
fn format_history(messages: &[ChatMessage]) -> String {
    let relevant: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .collect();

    let window: Vec<&ChatMessage> = relevant.iter().rev().take(3).rev().copied().collect();

    if window.is_empty() {
        return "(no prior context)".to_string();
    }

    window
        .iter()
        .filter_map(|m| {
            m.content.as_ref().map(|c| {
                let snippet = if c.len() > 200 {
                    format!("{}...", &c[..200])
                } else {
                    c.clone()
                };
                format!("{}: {}", m.role, snippet)
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;

    fn msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn test_format_history_empty() {
        let result = format_history(&[]);
        assert_eq!(result, "(no prior context)");
    }

    #[test]
    fn test_format_history_includes_role_and_content() {
        let msgs = vec![
            msg("user", "Who is Linus?"),
            msg("assistant", "Linus is the creator of Linux."),
        ];
        let result = format_history(&msgs);
        assert!(result.contains("user: Who is Linus?"));
        assert!(result.contains("assistant: Linus is the creator of Linux."));
    }

    #[test]
    fn test_format_history_skips_system_messages() {
        let msgs = vec![
            msg("system", "You are a bot."),
            msg("user", "What is Rust?"),
        ];
        let result = format_history(&msgs);
        assert!(
            !result.contains("system"),
            "System messages must not appear in history"
        );
        assert!(result.contains("user: What is Rust?"));
    }

    #[test]
    fn test_format_history_skips_tool_messages() {
        let msgs = vec![
            msg("tool", r#"{"result": "some output"}"#),
            msg("user", "What does that mean?"),
        ];
        let result = format_history(&msgs);
        assert!(
            !result.contains("tool"),
            "Tool messages must not appear in history"
        );
        assert!(result.contains("user: What does that mean?"));
    }

    #[test]
    fn test_format_history_limits_to_last_3() {
        let msgs: Vec<ChatMessage> = (0..10)
            .map(|i| msg("user", &format!("message {}", i)))
            .collect();
        let result = format_history(&msgs);
        assert!(result.contains("message 9"));
        assert!(result.contains("message 8"));
        assert!(result.contains("message 7"));
        assert!(
            !result.contains("message 6"),
            "Older messages must be excluded"
        );
    }

    #[test]
    fn test_format_history_truncates_long_content() {
        let long = "x".repeat(500);
        let msgs = vec![msg("user", &long)];
        let result = format_history(&msgs);
        let line = result.lines().next().unwrap_or("");
        assert!(
            line.len() <= 220,
            "Content should be truncated: len={}",
            line.len()
        );
    }
}
