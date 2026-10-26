//! Anti cold-intro for Active peers (ADR-0043 / #277).
//!
//! When continuity already has ≥1 prior user or assistant turn (excluding pure
//! system / summary markers), the final user-visible reply must not be a
//! capability cold-intro. Mechanism: light prompt (when history non-empty) +
//! structural final-reply guard on the shared `AgenticLoop` path (all Active
//! peers). Narrow matcher: prefer false-negatives over false-positives.

use crate::llm::ChatMessage;

/// Appended to the system message when loaded history already has continuity.
pub const CONTINUITY_LIGHT_PROMPT: &str = "\n\nContinuity note: this conversation already has prior user/assistant turns. Do not open with a capability menu or a cold greeting+menu; continue the prior thread or ask a short clarifying question about the user's latest message.";

/// User nudge after rejecting a cold-intro draft (one regenerate).
pub const COLD_INTRO_REGEN_NUDGE: &str = "[System] Your previous draft was a capability cold-intro (greeting + capability menu). This conversation already has prior turns. Continue that thread or ask a short clarifying question about the user's latest message. Do not list capabilities or greet as if starting fresh.";

/// Structural rewrite when regenerate is still a cold-intro (no capability menu).
pub const COLD_INTRO_CLARIFY_REWRITE: &str =
    "Let's continue from the prior thread — which item should I pick up, or can you clarify what you need next?";

/// True when `messages` contain ≥1 user or assistant continuity turn
/// (excludes pure system messages and `[SUMMARY]` / compacted-summary markers).
pub fn has_continuity_turns(messages: &[ChatMessage]) -> bool {
    messages.iter().any(is_continuity_turn)
}

/// At agentic-loop entry the current user turn is already present. Prior =
/// any continuity turn before that trailing user message.
pub fn has_prior_continuity(messages: &[ChatMessage]) -> bool {
    let turns: Vec<&ChatMessage> = messages.iter().filter(|m| is_continuity_turn(m)).collect();
    if turns.is_empty() {
        return false;
    }
    let without_current = if turns.last().is_some_and(|m| m.role == "user") {
        &turns[..turns.len() - 1]
    } else {
        &turns[..]
    };
    !without_current.is_empty()
}

fn is_continuity_turn(m: &ChatMessage) -> bool {
    match m.role.as_str() {
        "user" | "assistant" => {
            // Legacy summary rows used role=user + tool_call_id=summary.
            if m.role == "user" && m.tool_call_id.as_deref() == Some("summary") {
                return false;
            }
            true
        }
        "system" => false,
        _ => false,
    }
}

/// Narrow matcher for capability cold-intro replies.
///
/// Requires **all** of: greeting-ish opening, "I can help you"-style offer,
/// and ≥2 menu/bullet lines. Prefer false-negatives (normal short replies
/// must not match).
pub fn is_capability_cold_intro(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    has_greeting_signal(text) && has_help_offer(text) && count_menu_lines(text) >= 2
}

fn has_greeting_signal(text: &str) -> bool {
    let head: String = text.chars().take(220).collect();
    let lower = head.to_lowercase();

    // Cantonese / CJK Fox intro (known tip pattern).
    if head.contains('嗨')
        || head.contains("你好")
        || head.contains("哈囉")
        || head.contains("哈喽")
        || head.contains("喺度")
        || head.contains("有咩想做")
    {
        return true;
    }

    // English greetings near the start (word-ish).
    let english = [
        "hello",
        "hi!",
        "hi,",
        "hi ",
        "hey!",
        "hey,",
        "hey ",
        "hi there",
        "hey there",
        "good morning",
        "good afternoon",
        "good evening",
    ];
    for g in english {
        if lower.starts_with(g) || lower.contains(&format!("\n{g}")) {
            return true;
        }
    }
    // "Hi Fox" / "Hello, I'm ..." without requiring trailing space after hi
    if lower.starts_with("hi") && lower.len() > 2 {
        let next = lower.chars().nth(2);
        if next.is_some_and(|c| !c.is_ascii_alphabetic()) {
            return true;
        }
    }
    false
}

fn has_help_offer(text: &str) -> bool {
    let lower = text.to_lowercase();
    if text.contains("我可以幫你") || text.contains("我可以帮你") || text.contains("我能幫你")
    {
        return true;
    }
    const OFFERS: &[&str] = &[
        "i can help you",
        "i can help with",
        "i'm here to help",
        "i am here to help",
        "here's what i can",
        "here is what i can",
        "what i can do",
        "things i can help",
        "ways i can help",
    ];
    OFFERS.iter().any(|o| lower.contains(o))
}

fn count_menu_lines(text: &str) -> usize {
    text.lines().filter(|line| is_menu_line(line)).count()
}

fn is_menu_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return false;
    }
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("• ") || t.starts_with("● ")
    {
        return t.chars().count() > 3;
    }
    // Numbered: "1. foo" / "2) foo" / "1、foo"
    let mut chars = t.chars().peekable();
    let mut saw_digit = false;
    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
        saw_digit = true;
        chars.next();
    }
    if saw_digit {
        if let Some('.' | ')' | '、') = chars.next() {
            let rest: String = chars.collect();
            return rest.trim_start().chars().count() > 1;
        }
    }
    // Emoji / symbol-prefixed menu (e.g. "📰 出帖 …"): non-ASCII, non-CJK-letter start.
    let mut cs = t.chars();
    if let Some(first) = cs.next() {
        if !first.is_ascii() && !is_cjk_letter(first) {
            let rest = cs.as_str().trim_start();
            return rest.chars().count() > 2;
        }
    }
    false
}

fn is_cjk_letter(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // Extension A
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MessageContent;

    fn msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: Some(MessageContent::from_text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn continuity_ignores_system_and_summary_markers() {
        let only_system = vec![msg("system", "you are fox")];
        assert!(!has_continuity_turns(&only_system));
        assert!(!has_prior_continuity(&only_system));

        let with_summary_user = vec![ChatMessage {
            role: "user".into(),
            content: Some(MessageContent::from_text("old summary")),
            tool_calls: None,
            tool_call_id: Some("summary".into()),
            reasoning_content: None,
        }];
        assert!(!has_continuity_turns(&with_summary_user));
    }

    #[test]
    fn prior_continuity_excludes_trailing_current_user() {
        let first_turn = vec![msg("system", "sys"), msg("user", "hello")];
        assert!(!has_prior_continuity(&first_turn));

        let follow_up = vec![
            msg("system", "sys"),
            msg("user", "earlier"),
            msg("assistant", "ack"),
            msg("user", "follow up"),
        ];
        assert!(has_prior_continuity(&follow_up));
        assert!(has_continuity_turns(&follow_up));
    }

    #[test]
    fn matches_cantonese_fox_capability_menu() {
        let cold = "嗨 Fox 喺度 👋 有咩想做？我可以幫你：\n\
                    - 📰 出帖 (Threads / 廣東話)、fetch 新聞\n\
                    - 📁 Obsidian vault 筆記整理、inbox sweep、每日筆記\n\
                    - 💰 財務：信用卡月結單 parse、記帳、Google Sheet 同步\n\
                    - 🔬 arXiv 論文簡報 / deep dive\n\
                    - 💻 寫 code、debug、跑 sandbox 實驗\n\
                    - 🌤️ 天氣、日程、scheduled task 管理";
        assert!(is_capability_cold_intro(cold));
    }

    #[test]
    fn matches_english_capability_menu() {
        let cold = "Hello! I'm Fox. I can help you with:\n\
                    - Fetching news and posting to Threads\n\
                    - Organizing your Obsidian vault\n\
                    - Coding and debugging in the sandbox";
        assert!(is_capability_cold_intro(cold));
    }

    #[test]
    fn false_negative_preferred_for_short_normal_replies() {
        assert!(!is_capability_cold_intro("嗨，收到。我而家補第 25 期。"));
        assert!(!is_capability_cold_intro("Got it — I'll pick up item (1)."));
        assert!(!is_capability_cold_intro(
            "I can help you with that tomorrow."
        ));
        assert!(!is_capability_cold_intro(
            "Hello — which of the three options do you want?"
        ));
        // Greeting + help offer but no menu list
        assert!(!is_capability_cold_intro(
            "Hi! I can help you with the schedule you mentioned."
        ));
        // Menu-ish list without greeting / help offer
        assert!(!is_capability_cold_intro(
            "- Fix cron\n- Add guard\n- Ship report"
        ));
    }

    #[test]
    fn clarify_rewrite_is_not_cold_intro() {
        assert!(!is_capability_cold_intro(COLD_INTRO_CLARIFY_REWRITE));
    }
}
