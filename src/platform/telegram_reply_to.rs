//! ADR-0048: Telegram inbound `reply_to_message` → compose one Active user message.
//!
//! Pure quote injection for **this turn**. Not Continuity (#279/#282/#285), not
//! Reaction, not a second mid-run ack path. Mid-run still uses ADR-0020 TurnGate;
//! compose before enqueue so `QueuedInput.text` already carries the quote.

/// Max Unicode scalars kept from `{quoted_text}` before appending `…` (ADR-0048).
pub const QUOTE_MAX_SCALARS: usize = 4000;

/// Body of the replied-to message for the locked English template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyQuote {
    /// Quoted text or caption (may be truncated on compose).
    Text(String),
    /// Media with no text/caption.
    MediaOnly,
    /// Reply marker present but body cannot be resolved.
    Unavailable,
}

/// Locked English template (ADR-0048 Decision 7 / 9 / 10).
///
/// One user-role message: reply-to prefix + `{user_text}`. Never omits the
/// marker when a reply was requested; never refuses the user text.
pub fn compose_reply_to_user_text(
    role: &str,
    id: i32,
    quote: &ReplyQuote,
    user_text: &str,
) -> String {
    match quote {
        ReplyQuote::Text(quoted) => {
            let quoted = truncate_quoted_text(quoted);
            format!("[Reply to {role} msg {id}]:\n{quoted}\n\n{user_text}")
        }
        ReplyQuote::MediaOnly => {
            format!("[Reply to {role} msg {id}: media attachment, no text]\n\n{user_text}")
        }
        ReplyQuote::Unavailable => {
            format!("[Reply to {role} msg {id}: unavailable]\n\n{user_text}")
        }
    }
}

/// `bot` if the replied-to sender is a bot, else `user` (ADR-0048 Decision 2).
pub fn role_from_is_bot(is_bot: bool) -> &'static str {
    if is_bot {
        "bot"
    } else {
        "user"
    }
}

/// Classify quote payload from Telegram reply parts (text/caption/media).
pub fn quote_from_reply_parts(
    text: Option<&str>,
    caption: Option<&str>,
    has_media: bool,
) -> ReplyQuote {
    if let Some(t) = text.filter(|s| !s.is_empty()) {
        return ReplyQuote::Text(t.to_string());
    }
    if let Some(c) = caption.filter(|s| !s.is_empty()) {
        return ReplyQuote::Text(c.to_string());
    }
    if has_media {
        ReplyQuote::MediaOnly
    } else {
        ReplyQuote::Unavailable
    }
}

/// Truncate `{quoted_text}` to [`QUOTE_MAX_SCALARS`] Unicode scalars + `…`.
pub fn truncate_quoted_text(quoted: &str) -> String {
    if quoted.chars().count() <= QUOTE_MAX_SCALARS {
        return quoted.to_string();
    }
    let head: String = quoted.chars().take(QUOTE_MAX_SCALARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::RunStop;
    use crate::turn_gate::{Next, QueuedInput, Submit, TurnGate};

    #[test]
    fn compose_uses_locked_english_template_one_user_message() {
        let out = compose_reply_to_user_text(
            "user",
            7,
            &ReplyQuote::Text("earlier note".into()),
            "what about this?",
        );
        assert_eq!(
            out,
            "[Reply to user msg 7]:\nearlier note\n\nwhat about this?"
        );
    }

    #[test]
    fn compose_bot_role_in_template() {
        let out = compose_reply_to_user_text(
            "bot",
            99,
            &ReplyQuote::Text("I said hello".into()),
            "thanks",
        );
        assert_eq!(out, "[Reply to bot msg 99]:\nI said hello\n\nthanks");
    }

    #[test]
    fn truncate_quoted_text_over_4000_scalars_appends_ellipsis() {
        let long: String = "字".repeat(4001);
        let out = truncate_quoted_text(&long);
        assert_eq!(out.chars().count(), 4001, "4000 scalars + one …");
        assert!(out.ends_with('…'), "must use Unicode ellipsis U+2026");
        let body: String = out.chars().take(4000).collect();
        assert_eq!(body, "字".repeat(4000));
    }

    #[test]
    fn truncate_quoted_text_at_4000_no_ellipsis() {
        let exact: String = "a".repeat(4000);
        assert_eq!(truncate_quoted_text(&exact), exact);
    }

    #[test]
    fn compose_truncates_quote_body_to_4000_plus_ellipsis() {
        let long: String = "x".repeat(4005);
        let out = compose_reply_to_user_text("user", 1, &ReplyQuote::Text(long), "go");
        let expected_quote = format!("{}…", "x".repeat(4000));
        assert_eq!(
            out,
            format!("[Reply to user msg 1]:\n{expected_quote}\n\ngo")
        );
    }

    #[test]
    fn unavailable_placeholder_keeps_role_id_and_accepts_user_text() {
        let out = compose_reply_to_user_text("bot", 3, &ReplyQuote::Unavailable, "still send me");
        assert_eq!(out, "[Reply to bot msg 3: unavailable]\n\nstill send me");
        // Message still accepted: user text is present; marker never omitted.
        assert!(out.contains("still send me"));
        assert!(out.contains("unavailable"));
    }

    #[test]
    fn media_only_placeholder() {
        let out =
            compose_reply_to_user_text("user", 12, &ReplyQuote::MediaOnly, "what's in the photo?");
        assert_eq!(
            out,
            "[Reply to user msg 12: media attachment, no text]\n\nwhat's in the photo?"
        );
    }

    #[test]
    fn quote_from_reply_parts_prefers_text_then_caption_then_media_then_unavailable() {
        assert_eq!(
            quote_from_reply_parts(Some("hi"), Some("cap"), true),
            ReplyQuote::Text("hi".into())
        );
        assert_eq!(
            quote_from_reply_parts(None, Some("cap"), true),
            ReplyQuote::Text("cap".into())
        );
        assert_eq!(
            quote_from_reply_parts(Some(""), Some("cap"), false),
            ReplyQuote::Text("cap".into())
        );
        assert_eq!(
            quote_from_reply_parts(None, None, true),
            ReplyQuote::MediaOnly
        );
        assert_eq!(
            quote_from_reply_parts(None, Some(""), false),
            ReplyQuote::Unavailable
        );
        assert_eq!(
            quote_from_reply_parts(None, None, false),
            ReplyQuote::Unavailable
        );
    }

    #[test]
    fn role_from_is_bot_maps_bot_and_user() {
        assert_eq!(role_from_is_bot(true), "bot");
        assert_eq!(role_from_is_bot(false), "user");
    }

    #[tokio::test]
    async fn mid_run_queued_input_preserves_composed_reply_to_quote() {
        let composed =
            compose_reply_to_user_text("bot", 42, &ReplyQuote::Text("prior".into()), "and now?");
        let g = TurnGate::new();
        assert!(matches!(
            g.submit(
                "k",
                QueuedInput {
                    text: "start".into(),
                    ..Default::default()
                }
            )
            .await,
            Submit::Start(_)
        ));
        assert!(matches!(
            g.submit(
                "k",
                QueuedInput {
                    text: composed.clone(),
                    platform_msg_id: Some(99),
                    ..Default::default()
                }
            )
            .await,
            Submit::Accepted
        ));
        let inputs = match g.finish("k", Some(RunStop::FinalResponse)).await {
            Next::User { inputs, .. } => inputs,
            other => panic!("expected Next::User, got {other:?}"),
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].text, composed);
        assert!(inputs[0].text.contains("[Reply to bot msg 42]:"));
        assert!(inputs[0].text.contains("prior"));
        assert!(inputs[0].text.contains("and now?"));
    }

    #[test]
    fn reply_to_module_does_not_import_cold_intro_paths() {
        // Prod surface only — docs may name Continuity as an explicit non-goal.
        let src = include_str!("telegram_reply_to.rs");
        let prod = src.split("#[cfg(test)]").next().expect("prod section");
        assert!(
            !prod.contains("crate::cold_intro"),
            "must not import cold_intro"
        );
        assert!(
            !prod.contains("anti_cold_intro"),
            "must not reference anti_cold_intro"
        );
    }
}
