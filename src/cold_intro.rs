//! Anti cold-intro for Active peers (ADR-0043 / #277 / #280).
//!
//! When continuity already has ≥1 prior user or assistant turn (excluding pure
//! system / summary markers), the final user-visible reply must not be a
//! capability cold-intro. Mechanism: light prompt (when history non-empty) +
//! structural final-reply guard on the shared `AgenticLoop` path (all Active
//! peers). Matcher = W1 ∪ W2; prefer false-negatives over false-positives.

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

/// Capability cold-intro matcher (ADR-0043 / #280 / #315 W2++): **W1 ∪ W2**.
///
/// - **W1:** greeting + ≥2 menu lines (help-offer no longer required)
/// - **W2:** explicit lost-context / cannot-see-prior (+ #283 P1/E1 / #315 W2++)
///   + ≥2 menu lines
///
/// Prefer false-negatives (F1): greeting and menu-line definitions are
/// unchanged from #277. Lost-context alone without ≥2 menu does not match (C1)
/// on this matcher. ADR-0045 / #315 closes the post–empty-progress C1 hole on
/// the shared FinalResponse path (lost-context alone after empty-progress
/// force-clarify → rewrite), not by widening C1 globally here.
pub fn is_capability_cold_intro(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let menu = count_menu_lines(text) >= 2;
    if !menu {
        return false;
    }
    // W1 ∪ W2
    has_greeting_signal(text) || has_lost_context_signal(text)
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

/// Explicit lost-context / cannot-see-prior / empty-inbound language
/// (locale-aware; #280 / #283 / #315 W2++).
///
/// Prefer FN: keep the list tight so normal “I can’t see file X” without a menu
/// never reaches the capability-cold-intro matcher alone (C1 still requires ≥2
/// menu at `is_capability_cold_intro`). Callers on the post–empty-progress
/// path (#315) may use this signal **without** a menu requirement.
///
/// W2+ (#283): P1 CJK (`收唔到` / `只見到系統…` / `冇見到` ≠ `見唔到`) + E1 EN.
/// W2++ (#315): `訊息係空`, `掉失` / `掉失咗` (near-synonyms OOS).
pub fn has_lost_context_signal(text: &str) -> bool {
    let lower = text.to_lowercase();

    // Cantonese / CJK lost-prior / broken-context (+ #283 P1 lost-content).
    if text.contains("斷 context")
        || text.contains("斷咗 context")
        || text.contains("context 斷")
        || text.contains("見唔到")
        || text.contains("睇唔到先前")
        || text.contains("睇唔到之前")
        || text.contains("冇咗 context")
        || text.contains("冇先前")
        || text.contains("斷咗線")
        || text.contains("上下文斷")
        || text.contains("丢失上下文")
        || text.contains("丟失上下文")
        || text.contains("看不到之前")
        || text.contains("看不到先前")
        || text.contains("收唔到")
        || text.contains("收唔到你今次")
        || text.contains("只見到系統 context")
        || text.contains("只見到系統")
        || text.contains("今次嘅內容")
        || text.contains("冇見到")
        // #315 W2++ (live MAIN hit): empty-inbound / dropped-context claims.
        || text.contains("訊息係空")
        || text.contains("掉失")
    {
        return true;
    }

    const ENGLISH: &[&str] = &[
        "lost context",
        "lost the context",
        "lost our context",
        "context is lost",
        "context was lost",
        "lost track of our conversation",
        "lost track of the conversation",
        "lost track of our context",
        "lost track of the context",
        "lost track of our thread",
        "lost track of the thread",
        "cannot see prior",
        "can't see prior",
        "cannot see the prior",
        "can't see the prior",
        "can't see previous",
        "cannot see previous",
        "don't have prior context",
        "do not have prior context",
        "no prior context",
        "prior context is gone",
        "don't remember our conversation",
        "don't remember the conversation",
        "don't remember our context",
        "do not remember our conversation",
        "do not remember the conversation",
        // #283 E1
        "didn't get your",
        "only see system context",
        "only seeing system",
    ];
    ENGLISH.iter().any(|o| lower.contains(o))
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

    /// Live FN on tip 29e4461: greeting + ≥2 `- ` menu, **no** help-offer → W1.
    #[test]
    fn w1_greeting_plus_menu_without_help_offer() {
        let cold = "嗨 Fox 喺度 👋 有咩想做？\n\
                    - 📰 出帖 (Threads / 廣東話)、fetch 新聞\n\
                    - 📁 Obsidian vault 筆記整理、inbox sweep、每日筆記\n\
                    - 💰 財務：信用卡月結單 parse、記帳、Google Sheet 同步";
        assert!(is_capability_cold_intro(cold));
        assert!(!cold.contains("我可以幫你"));
        assert!(!cold.to_lowercase().contains("i can help"));
    }

    #[test]
    fn w2_lost_context_plus_menu() {
        let cantonese = "對唔住，我斷 context 見唔到先前嘅對話。不如揀返：\n\
                         - 📰 出帖 / fetch 新聞\n\
                         - 📁 Obsidian vault 整理\n\
                         - 💻 寫 code / debug";
        assert!(is_capability_cold_intro(cantonese));

        let english = "Sorry — I lost context and can't see prior turns. What should I pick up?\n\
                       - Fetching news and posting to Threads\n\
                       - Organizing your Obsidian vault\n\
                       - Coding and debugging in the sandbox";
        assert!(is_capability_cold_intro(english));
    }

    #[test]
    fn c1_lost_context_alone_does_not_match() {
        assert!(!is_capability_cold_intro(
            "對唔住，我斷 context / 見唔到先前嘅對話，可唔可以再講一次？"
        ));
        assert!(!is_capability_cold_intro(
            "Sorry, I lost context and can't see the prior turns — can you restate?"
        ));
        assert!(!is_capability_cold_intro(
            "I can't see file X in the workspace."
        ));
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
        // Menu-ish list without greeting / lost-context (F1)
        assert!(!is_capability_cold_intro(
            "- Fix cron\n- Add guard\n- Ship report"
        ));
        // Short Hi / - yes / - no style confirms that fail greeting+≥2-menu (F1)
        assert!(!is_capability_cold_intro("Hi"));
        assert!(!is_capability_cold_intro("Hi\n- yes"));
        assert!(!is_capability_cold_intro("- yes\n- no"));
        // #283: 冇見到 alone (no menu) must stay FN / C1
        assert!(!is_capability_cold_intro("對唔住，我冇見到你今次嘅訊息。"));
    }

    /// Live FN @ tip `9f58afb` / #283: lost-content CJK (no greeting) + ≥2 menu → W2.
    #[test]
    fn w2_lost_content_phrase_plus_menu_p1() {
        let live = "我好似收唔到你今次嘅內容 — 只見到系統 context。不如揀返：\n\
                    - 📰 出帖 / fetch 新聞\n\
                    - 📁 Obsidian vault 整理\n\
                    - 💻 寫 code / debug";
        assert!(is_capability_cold_intro(live));
        assert!(!live.contains('嗨'));
        assert!(!live.contains("你好"));

        let mou = "對唔住，我冇見到你今次嘅訊息。可以揀：\n\
                   - 📰 出帖 / fetch 新聞\n\
                   - 📁 Obsidian vault 整理";
        assert!(is_capability_cold_intro(mou));
    }

    #[test]
    fn w2_english_e1_lost_content_plus_menu() {
        let cold = "Sorry — I didn't get your last message, I only see system context. Pick one:\n\
                    - Fetching news and posting to Threads\n\
                    - Organizing your Obsidian vault\n\
                    - Coding and debugging in the sandbox";
        assert!(is_capability_cold_intro(cold));

        let seeing = "Hmm, I'm only seeing system prompts right now. Options:\n\
                      - Fetching news and posting to Threads\n\
                      - Organizing your Obsidian vault";
        assert!(is_capability_cold_intro(seeing));
    }

    #[test]
    fn c1_new_lost_content_phrases_alone_do_not_match() {
        assert!(!is_capability_cold_intro(
            "我好似收唔到你今次嘅內容 — 只見到系統 context，可唔可以再講一次？"
        ));
        assert!(!is_capability_cold_intro("對唔住，我冇見到你今次嘅訊息。"));
        assert!(!is_capability_cold_intro(
            "Sorry, I didn't get your last message — I only see system context."
        ));
        assert!(!is_capability_cold_intro(
            "I'm only seeing system prompts right now."
        ));
    }

    /// #315 W2++: live MAIN phrases + ≥2 menu → W2 cold-intro.
    #[test]
    fn w2_plus_plus_empty_inbound_phrases_plus_menu() {
        let live = "收到——不過我今次真係收唔到你嗰段內容（訊息係空嘅，之前嘅對話 context 亦都掉失咗）。不如揀返：\n\
                    - 📰 出帖 / fetch 新聞\n\
                    - 📁 Obsidian vault 整理\n\
                    - 💻 寫 code / debug";
        assert!(is_capability_cold_intro(live));
        assert!(has_lost_context_signal(live));

        let diu = "對唔住，context 掉失咗。可以揀：\n\
                   - 📰 出帖 / fetch 新聞\n\
                   - 📁 Obsidian vault 整理";
        assert!(is_capability_cold_intro(diu));
        assert!(has_lost_context_signal(diu));
    }

    /// #315 W2++: phrases alone still C1 for capability matcher; signal is true.
    #[test]
    fn w2_plus_plus_phrases_alone_are_c1_but_signal_matches() {
        let alone = "收到——不過我今次真係收唔到你嗰段內容（訊息係空嘅，之前嘅對話 context 亦都掉失咗）。\n\n你直接再貼一次想我做嘅嘢就得。";
        assert!(!is_capability_cold_intro(alone));
        assert!(has_lost_context_signal(alone));
        assert!(has_lost_context_signal("訊息係空嘅"));
        assert!(has_lost_context_signal("之前嘅對話 context 亦都掉失咗"));
        assert!(has_lost_context_signal("掉失咗"));
    }

    #[test]
    fn clarify_rewrite_is_not_cold_intro() {
        assert!(!is_capability_cold_intro(COLD_INTRO_CLARIFY_REWRITE));
    }
}
