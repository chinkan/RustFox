//! Work-verification gate (ADR-0011a R7, amended by #418).
//!
//! The parent may skip `invoke_agent(verifier)` only when the work is
//! reversible and already evidenced: all three checks hold, and the parent
//! states each one. The protocol text is hard-coded. There is no config,
//! skill, or per-bot switch that turns the verifier off.

use crate::llm::ChatMessage;
use serde_json::Value;
use std::collections::HashMap;

/// Locked protocol body. A Windows checkout may insert CR; the prompt uses LF.
pub(crate) fn protocol_body() -> String {
    include_str!("work_verification_protocol.txt")
        .replace("\r\n", "\n")
        .replace('\r', "")
}

/// Hard-coded block appended in `Agent::build_system_prompt`.
/// Leading blank line matches the previous section break.
pub(crate) fn protocol_section() -> String {
    format!("\n\n{}", protocol_body())
}

pub(crate) const NUDGE_MUST_VERIFY: &str = "[System] Work verification: do not end yet. Skip invoke_agent(verifier) only when all three checks hold and you state each one. This turn does not. Call the verifier with the already-run evidence before the reply goes out.";

pub(crate) const NUDGE_STATE_CHECKS: &str = "[System] Work verification: the three checks hold. State each one before you end: risk class is not high; this-turn evidence; residual effect is local and reversible, or the change cannot alter behavior.";

pub(crate) const NUDGE_REVERIFY: &str = "[System] Work verification: you may re-verify at most one extra round on this interactive turn. Give the verifier the already-run evidence, then reply with the residual rather than looping.";

pub(crate) const NUDGE_FAIL_CLOSED: &str = "[System] Work verification: scheduled publish stays fail-closed. The verifier did not accept the work. A live-post check is evidence for the verifier, not a skip. Do not end this turn as done.";

/// Tool result substituted for a verifier call that must not run.
/// Kept free of verdict words so it cannot be parsed as a new result.
pub(crate) const TERMINAL_VERIFIER_BLOCK: &str = "[System] Work verification: do not call the verifier again this turn. The no-shell limit on evidence already attached is terminal. Reply with the residual. This is not a skip.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserOverride {
    None,
    /// "Verify first" forces a verifier even when the three checks pass.
    VerifyFirst,
    /// "Don't verify" cannot create a skip and cannot override a failed check.
    DontVerify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    Fail,
    NeedsImprovement,
    Unverified,
}

/// The three skip checks. Size is not one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Checks {
    pub risk_not_high: bool,
    pub this_turn_evidence: bool,
    pub residual_local_or_inert: bool,
}

impl Checks {
    fn all_hold(self) -> bool {
        self.risk_not_high && self.this_turn_evidence && self.residual_local_or_inert
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// End without `invoke_agent(verifier)`. The reply states the checks.
    Skip,
    /// Verifier must run before the reply goes out. Blocking.
    MustVerify,
    /// Checks hold, but the reply has not stated them yet.
    StateChecks,
    /// One extra interactive verifier round is still allowed.
    ReverifyOnce,
    /// Interactive turn ends with the residual. Not a skip.
    TerminalResidual,
    /// Verifier returned PASS. The reply may go out.
    VerifierPassed,
    /// Scheduled publish path. Non-PASS does not end the turn.
    FailClosed,
    /// No this-turn observation, and the draft is not claiming the skip.
    /// ADR-0011a R7 is no longer an unconditional verifier on every interactive
    /// turn. A claimed skip without this-turn evidence stays `MustVerify`.
    NoWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Turn {
    pub checks: Checks,
    pub checks_stated: bool,
    pub user_override: UserOverride,
    pub scheduled_publish: bool,
    pub verdict: Option<Verdict>,
    /// Extra interactive re-verify rounds already completed (0 or 1).
    pub extra_rounds_used: u8,
    /// UNVERIFIED because the verifier has no shell, and this-turn evidence
    /// for that check was already attached to the verifier prompt.
    pub no_shell_unverified_on_attached_evidence: bool,
}

pub(crate) fn decide(turn: &Turn) -> Decision {
    if let Some(verdict) = turn.verdict {
        if verdict == Verdict::Pass {
            return Decision::VerifierPassed;
        }
        // Scheduled publish stays on the LLM verifier and does not ship a
        // non-PASS residual. A live-post check is not a skip.
        if turn.scheduled_publish {
            return Decision::FailClosed;
        }
        if verdict == Verdict::Unverified && turn.no_shell_unverified_on_attached_evidence {
            return Decision::TerminalResidual;
        }
        if turn.extra_rounds_used >= 1 {
            return Decision::TerminalResidual;
        }
        return Decision::ReverifyOnce;
    }

    if turn.user_override == UserOverride::VerifyFirst
        || turn.scheduled_publish
        || !turn.checks.risk_not_high
    {
        return Decision::MustVerify;
    }
    // Nothing was observed this turn. Ending is not a skip. Stating the
    // checks anyway, or saying "don't verify", still has to go through the
    // verifier: a remembered run is not evidence, and that phrase cannot
    // waive a failed check.
    if !turn.checks.this_turn_evidence {
        if turn.checks_stated || turn.user_override == UserOverride::DontVerify {
            return Decision::MustVerify;
        }
        return Decision::NoWork;
    }
    if !turn.checks.all_hold() {
        return Decision::MustVerify;
    }
    // "Don't verify" does not grant a skip and does not cancel one.
    if turn.checks_stated {
        Decision::Skip
    } else {
        Decision::StateChecks
    }
}

/// `None` means the draft may be delivered. `Some` is a blocking nudge.
pub(crate) fn draft_nudge(turn: &Turn) -> Option<&'static str> {
    match decide(turn) {
        Decision::Skip
        | Decision::VerifierPassed
        | Decision::TerminalResidual
        | Decision::NoWork => None,
        Decision::MustVerify => Some(NUDGE_MUST_VERIFY),
        Decision::StateChecks => Some(NUDGE_STATE_CHECKS),
        Decision::ReverifyOnce => Some(NUDGE_REVERIFY),
        Decision::FailClosed => Some(NUDGE_FAIL_CLOSED),
    }
}

/// True when this verifier call must not run. The one allowed extra round
/// still runs. A no-shell UNVERIFIED on evidence already attached does not.
pub(crate) fn block_extra_verifier(turn: &Turn) -> bool {
    if turn.no_shell_unverified_on_attached_evidence
        && matches!(turn.verdict, Some(Verdict::Unverified))
    {
        return true;
    }
    matches!(decide(turn), Decision::TerminalResidual)
}

pub(crate) fn is_verifier_invoke(tool_name: &str, arguments: &str) -> bool {
    if tool_name != "invoke_agent" {
        return false;
    }
    invoked_name(arguments).is_some_and(|name| name.eq_ignore_ascii_case("verifier"))
}

/// Read this turn's checks, override, and verifier results from the transcript.
/// `scheduled` is the run lane, not by itself a publish.
pub(crate) fn observe(messages: &[ChatMessage], draft: &str, scheduled: bool) -> Turn {
    let anchor = request_anchor(messages);
    let request = request_text(messages, anchor);
    let start = anchor.map(|index| index + 1).unwrap_or(0);
    let calls = tool_calls_after(messages, start);

    let mut tool_names = Vec::new();
    let mut verifier_hits: Vec<VerifierHit> = Vec::new();
    let mut proving_observation = false;

    for message in &messages[start..] {
        if message.role != "tool" {
            continue;
        }
        let body = message_text(message);
        let id = message.tool_call_id.as_deref().unwrap_or("");
        let Some(call) = calls.get(id) else {
            if !body.trim().is_empty() {
                proving_observation = true;
            }
            continue;
        };
        if is_verifier_invoke(&call.name, &call.arguments) {
            if let Some(verdict) = parse_verdict(&body) {
                verifier_hits.push(VerifierHit {
                    arguments: call.arguments.clone(),
                    body,
                    verdict,
                });
            }
        } else if !body.trim().is_empty() {
            tool_names.push(call.name.clone());
            proving_observation = true;
        }
    }

    let publish = is_publish_path(&request, &tool_names);
    let scheduled_publish = scheduled && publish;
    let risk_not_high = !scheduled_publish
        && !publish
        && !user_is_high_risk(&request)
        && !tools_are_high_risk(&tool_names);
    let (risk_stated, evidence_stated, residual_stated) = stated_checks(draft);
    let last = verifier_hits.last();
    let no_shell = last.is_some_and(|hit| {
        hit.verdict == Verdict::Unverified
            && mentions_no_shell(&hit.body)
            && prompt_has_evidence(&hit.arguments)
            && proving_observation
    });

    Turn {
        checks: Checks {
            risk_not_high,
            this_turn_evidence: proving_observation,
            residual_local_or_inert: residual_stated,
        },
        checks_stated: risk_stated && evidence_stated && residual_stated,
        user_override: user_override(&request),
        scheduled_publish,
        verdict: last.map(|hit| hit.verdict),
        extra_rounds_used: u8::try_from(verifier_hits.len().saturating_sub(1)).unwrap_or(u8::MAX),
        no_shell_unverified_on_attached_evidence: no_shell,
    }
}

struct ToolCallRecord {
    name: String,
    arguments: String,
}

struct VerifierHit {
    arguments: String,
    body: String,
    verdict: Verdict,
}

fn message_text(message: &ChatMessage) -> String {
    message
        .content
        .as_ref()
        .map(|content| content.as_text())
        .unwrap_or_default()
}

fn is_meta_user(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("[System]") || trimmed.starts_with("[Steer]")
}

fn request_anchor(messages: &[ChatMessage]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            if message.role != "user" {
                return None;
            }
            let text = message_text(message);
            if is_meta_user(&text) {
                None
            } else {
                Some(index)
            }
        })
}

fn request_text(messages: &[ChatMessage], anchor: Option<usize>) -> String {
    let Some(anchor) = anchor else {
        return String::new();
    };
    let mut parts = vec![message_text(&messages[anchor])];
    for message in &messages[anchor + 1..] {
        if message.role != "user" {
            continue;
        }
        let text = message_text(message);
        if text.trim_start().starts_with("[Steer]") {
            parts.push(text);
        }
    }
    parts.join("\n")
}

fn tool_calls_after(messages: &[ChatMessage], start: usize) -> HashMap<String, ToolCallRecord> {
    let mut calls = HashMap::new();
    for message in &messages[start..] {
        let Some(tool_calls) = message.tool_calls.as_ref() else {
            continue;
        };
        for call in tool_calls {
            calls.insert(
                call.id.clone(),
                ToolCallRecord {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            );
        }
    }
    calls
}

fn invoked_name(arguments: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    for key in ["bot", "agent", "skill", "name"] {
        if let Some(name) = value.get(key).and_then(Value::as_str) {
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn user_override(text: &str) -> UserOverride {
    let normalized = text.to_ascii_lowercase().replace('\u{2019}', "'");
    if normalized.contains("verify first") {
        return UserOverride::VerifyFirst;
    }
    if normalized.contains("don't verify")
        || normalized.contains("do not verify")
        || normalized.contains("dont verify")
    {
        return UserOverride::DontVerify;
    }
    UserOverride::None
}

fn user_is_high_risk(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "publish",
        "post to threads",
        "threads publish",
        "payment",
        "credential",
        "api key",
        "api_key",
        "password",
        "data migration",
        "migrate the",
        "delete the data",
        "delete the",
        "data delete",
        "drop table",
        "drop the",
    ];
    MARKERS.iter().any(|marker| normalized.contains(marker))
}

fn tool_is_high_risk(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "publish",
        "payment",
        "credential",
        "password",
        "migrate",
        "schedule_post",
        "create_post",
        "threads_post",
        "delete",
    ];
    MARKERS.iter().any(|marker| normalized.contains(marker))
}

fn tools_are_high_risk(names: &[String]) -> bool {
    names.iter().any(|name| tool_is_high_risk(name))
}

fn is_publish_path(user_text: &str, tool_names: &[String]) -> bool {
    let normalized = user_text.to_ascii_lowercase();
    normalized.contains("publish")
        || normalized.contains("post to threads")
        || normalized.contains("threads publish")
        || tool_names.iter().any(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("publish")
                || name.contains("schedule_post")
                || name.contains("create_post")
                || name.contains("threads_post")
        })
}

fn stated_checks(draft: &str) -> (bool, bool, bool) {
    let normalized = draft.to_ascii_lowercase();
    let risk = normalized.contains("risk class is not high")
        || normalized.contains("risk is not high")
        || normalized.contains("not high risk")
        || normalized.contains("not a high risk");
    let evidence = normalized.contains("this-turn evidence")
        || normalized.contains("this turn's evidence")
        || (normalized.contains("this turn")
            && (normalized.contains("evidence")
                || normalized.contains("tool result")
                || normalized.contains("command exit")
                || normalized.contains("exit code")));
    let residual = normalized.contains("local and reversible")
        || normalized.contains("locally reversible")
        || normalized.contains("cannot alter behavior")
        || normalized.contains("can't alter behavior")
        || normalized.contains("cannot change behavior")
        || normalized.contains("does not alter behavior")
        || normalized.contains("will not alter behavior");
    (risk, evidence, residual)
}

fn mentions_no_shell(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("no-shell")
        || normalized.contains("no shell")
        || normalized.contains("without a shell")
        || normalized.contains("without shell")
        || normalized.contains("shell unavailable")
        || normalized.contains("no execute_command")
        || normalized.contains("cannot run commands")
        || normalized.contains("can't run commands")
        || normalized.contains("cannot execute commands")
}

fn prompt_has_evidence(arguments: &str) -> bool {
    let lower = arguments.to_ascii_lowercase();
    let Some(index) = lower.find("evidence:") else {
        return false;
    };
    arguments[index + "evidence:".len()..]
        .chars()
        .any(|ch| ch.is_ascii_alphanumeric())
}

fn parse_verdict(text: &str) -> Option<Verdict> {
    let scoped = evaluation_tag(text).unwrap_or(text);
    let normalized = scoped.to_ascii_lowercase();
    if has_token(&normalized, "unverified") {
        Some(Verdict::Unverified)
    } else if normalized.contains("needs_improvement") || normalized.contains("needs improvement") {
        Some(Verdict::NeedsImprovement)
    } else if has_token(&normalized, "fail") {
        Some(Verdict::Fail)
    } else if has_token(&normalized, "pass") {
        Some(Verdict::Pass)
    } else {
        None
    }
}

fn evaluation_tag(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<evaluation>")? + "<evaluation>".len();
    let end_rel = lower[start..].find("</evaluation>")?;
    Some(text[start..start + end_rel].trim())
}

fn has_token(text: &str, word: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|token| token == word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, MessageContent, ToolCall};

    #[allow(clippy::too_many_arguments)]
    fn sample(
        risk: bool,
        evidence: bool,
        residual: bool,
        stated: bool,
        user: UserOverride,
        scheduled_publish: bool,
        verdict: Option<Verdict>,
        extra: u8,
        no_shell: bool,
    ) -> Turn {
        Turn {
            checks: Checks {
                risk_not_high: risk,
                this_turn_evidence: evidence,
                residual_local_or_inert: residual,
            },
            checks_stated: stated,
            user_override: user,
            scheduled_publish,
            verdict,
            extra_rounds_used: extra,
            no_shell_unverified_on_attached_evidence: no_shell,
        }
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: Some(MessageContent::from_text(text)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn assistant_call(id: &str, name: &str, arguments: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: Some(MessageContent::from_text("")),
            tool_calls: Some(vec![ToolCall {
                id: id.into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: arguments.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn tool_result(id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: Some(MessageContent::from_text(text)),
            tool_calls: None,
            tool_call_id: Some(id.into()),
            reasoning_content: None,
        }
    }

    const STATED: &str = "Risk class is not high: local device change. This-turn evidence: the tool result. Residual effect is local and reversible.";

    #[test]
    fn protocol_is_the_locked_hard_coded_text() {
        let body = protocol_body();
        assert!(
            !body.contains('\r'),
            "prompt keeps the locked LF line endings"
        );
        assert_eq!(
            protocol_section(),
            format!("\n\n{body}"),
            "section break precedes the protocol"
        );
        assert!(body.contains(
            "You may skip invoke_agent(verifier) only when all three hold and you can state each one:"
        ));
        assert!(body.contains("no-shell UNVERIFIED"));
        assert!(body.contains("“Verify first”"));
        assert!(body.contains("“Don’t verify”"));
        assert!(body.contains("Scheduled news / verify-and-publish"));
        assert!(!body.contains("BEFORE ending your response"));
        assert!(!body.contains("Only if the verifier returns PASS may you end"));
        assert_eq!(body.chars().count(), 2145);
    }

    #[test]
    fn three_check_skip_requires_all_three_and_a_statement() {
        assert_eq!(
            decide(&sample(
                true,
                true,
                true,
                true,
                UserOverride::None,
                false,
                None,
                0,
                false
            )),
            Decision::Skip
        );
        assert!(draft_nudge(&sample(
            true,
            true,
            true,
            true,
            UserOverride::None,
            false,
            None,
            0,
            false
        ))
        .is_none());
        for (risk, evidence, residual) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            assert_eq!(
                decide(&sample(
                    risk,
                    evidence,
                    residual,
                    true,
                    UserOverride::None,
                    false,
                    None,
                    0,
                    false
                )),
                Decision::MustVerify,
                "risk={risk} evidence={evidence} residual={residual}"
            );
        }
        assert_eq!(
            decide(&sample(
                true,
                true,
                true,
                false,
                UserOverride::None,
                false,
                None,
                0,
                false
            )),
            Decision::StateChecks
        );
    }

    #[test]
    fn verify_first_forces_verifier_when_three_checks_pass() {
        let turn = sample(
            true,
            true,
            true,
            true,
            UserOverride::VerifyFirst,
            false,
            None,
            0,
            false,
        );
        assert_eq!(decide(&turn), Decision::MustVerify);
        assert!(draft_nudge(&turn).is_some());
    }

    #[test]
    fn dont_verify_does_not_override_a_failed_check() {
        let rejected = sample(
            false,
            true,
            true,
            true,
            UserOverride::DontVerify,
            false,
            None,
            0,
            false,
        );
        assert_eq!(decide(&rejected), Decision::MustVerify);
        let allowed = sample(
            true,
            true,
            true,
            true,
            UserOverride::DontVerify,
            false,
            None,
            0,
            false,
        );
        assert_eq!(
            decide(&allowed),
            Decision::Skip,
            "don't verify may still skip when all three checks already hold"
        );
    }

    #[test]
    fn no_shell_unverified_on_attached_evidence_is_terminal_and_not_a_skip() {
        let turn = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            false,
            Some(Verdict::Unverified),
            0,
            true,
        );
        assert_eq!(decide(&turn), Decision::TerminalResidual);
        assert_ne!(decide(&turn), Decision::Skip);
        assert!(block_extra_verifier(&turn));
        assert!(draft_nudge(&turn).is_none());

        let unexplained = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            false,
            Some(Verdict::Unverified),
            0,
            false,
        );
        assert_eq!(decide(&unexplained), Decision::ReverifyOnce);
        assert!(!block_extra_verifier(&unexplained));
    }

    #[test]
    fn interactive_fail_allows_one_extra_round_then_stops() {
        let first = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            false,
            Some(Verdict::Fail),
            0,
            false,
        );
        assert_eq!(decide(&first), Decision::ReverifyOnce);
        assert!(!block_extra_verifier(&first));
        let second = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            false,
            Some(Verdict::NeedsImprovement),
            1,
            false,
        );
        assert_eq!(decide(&second), Decision::TerminalResidual);
        assert!(block_extra_verifier(&second));
    }

    #[test]
    fn scheduled_publish_stays_fail_closed() {
        let evidenced = sample(
            true,
            true,
            true,
            true,
            UserOverride::DontVerify,
            true,
            None,
            0,
            false,
        );
        assert_eq!(decide(&evidenced), Decision::MustVerify);
        let failed = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            true,
            Some(Verdict::Unverified),
            0,
            true,
        );
        assert_eq!(decide(&failed), Decision::FailClosed);
        assert!(block_extra_verifier(&failed));
        assert!(draft_nudge(&failed).is_some());
        let passed = sample(
            false,
            true,
            false,
            false,
            UserOverride::None,
            true,
            Some(Verdict::Pass),
            0,
            false,
        );
        assert_eq!(decide(&passed), Decision::VerifierPassed);
    }

    #[test]
    fn switchbot_volume_with_this_turn_evidence_may_skip() {
        let messages = vec![
            user("set the SwitchBot volume to 20"),
            assistant_call("c1", "switchbot_command", r#"{"deviceId":"ABC"}"#),
            tool_result("c1", "volume=20"),
        ];
        let turn = observe(&messages, STATED, false);
        assert!(turn.checks.all_hold());
        assert!(turn.checks_stated);
        assert_eq!(decide(&turn), Decision::Skip);
    }

    #[test]
    fn threads_publish_dont_verify_does_not_skip() {
        let messages = vec![
            user("publish the threads post. don't verify"),
            assistant_call("c1", "mcp_threads_schedule_post", "{}"),
            tool_result("c1", "posted id 9"),
        ];
        let turn = observe(&messages, STATED, false);
        assert!(!turn.checks.risk_not_high);
        assert_eq!(turn.user_override, UserOverride::DontVerify);
        assert_eq!(decide(&turn), Decision::MustVerify);
    }

    #[test]
    fn verify_first_on_switchbot_still_runs_the_verifier() {
        let messages = vec![
            user("set the SwitchBot volume to 20. verify first"),
            assistant_call("c1", "switchbot_command", r#"{"deviceId":"ABC"}"#),
            tool_result("c1", "volume=20"),
        ];
        let turn = observe(&messages, STATED, false);
        assert_eq!(turn.user_override, UserOverride::VerifyFirst);
        assert!(turn.checks.all_hold());
        assert_eq!(decide(&turn), Decision::MustVerify);
    }

    #[test]
    fn docs_typo_may_skip_and_a_behavior_change_without_the_claim_may_not() {
        let docs = vec![
            user("fix the typo in the readme"),
            assistant_call("c1", "write_file", r#"{"path":"README.md"}"#),
            tool_result("c1", "diff: teh -> the"),
        ];
        let stated = "Risk class is not high: wording only. This-turn evidence: the diff. The change cannot alter behavior.";
        assert_eq!(decide(&observe(&docs, stated, false)), Decision::Skip);

        let code = vec![
            user("fix the null check"),
            assistant_call("c1", "execute_command", r#"{"command":"cargo test"}"#),
            tool_result("c1", "exit code 0"),
        ];
        let bare = "Fixed the condition. Tests passed.";
        assert_eq!(decide(&observe(&code, bare, false)), Decision::MustVerify);
    }

    #[test]
    fn a_turn_with_no_observation_may_end_until_it_claims_the_skip() {
        let chat = observe(&[user("hello")], "Hi there.", false);
        assert!(!chat.checks.this_turn_evidence);
        assert!(!chat.checks_stated);
        assert_eq!(decide(&chat), Decision::NoWork);
        assert!(draft_nudge(&chat).is_none());

        let claimed = observe(&[user("what did you change?")], STATED, false);
        assert_eq!(decide(&claimed), Decision::MustVerify);

        let waived = observe(&[user("hello, don't verify")], "Hi there.", false);
        assert_eq!(waived.user_override, UserOverride::DontVerify);
        assert_eq!(decide(&waived), Decision::MustVerify);
    }

    #[test]
    fn remembered_tool_result_is_not_this_turn_evidence() {
        let messages = vec![
            user("set the SwitchBot volume to 20"),
            assistant_call("c1", "switchbot_command", "{}"),
            tool_result("c1", "volume=20"),
            user("what did you change?"),
        ];
        let turn = observe(&messages, STATED, false);
        assert!(!turn.checks.this_turn_evidence);
        assert_eq!(decide(&turn), Decision::MustVerify);
    }

    #[test]
    fn no_shell_unverified_blocks_another_round_when_evidence_was_attached() {
        let prompt = r#"{"agent":"verifier","prompt":"TASK: publish\nEVIDENCE: posted id 9"}"#;
        let messages = vec![
            user("publish the threads post"),
            assistant_call("c1", "mcp_threads_schedule_post", "{}"),
            tool_result("c1", "posted id 9"),
            assistant_call("v1", "invoke_agent", prompt),
            tool_result(
                "v1",
                "<evaluation>UNVERIFIED</evaluation>\nno-shell: cannot run commands",
            ),
        ];
        let interactive = observe(&messages, "Residual: the post id was returned.", false);
        assert_eq!(interactive.verdict, Some(Verdict::Unverified));
        assert!(interactive.no_shell_unverified_on_attached_evidence);
        assert_eq!(decide(&interactive), Decision::TerminalResidual);
        assert_ne!(decide(&interactive), Decision::Skip);
        assert!(block_extra_verifier(&interactive));

        let scheduled = observe(&messages, "Residual: the post id was returned.", true);
        assert!(scheduled.scheduled_publish);
        assert_eq!(decide(&scheduled), Decision::FailClosed);
        assert!(block_extra_verifier(&scheduled));

        let missing = prompt.replace("EVIDENCE: posted id 9", "EVIDENCE:");
        let mut without = messages.clone();
        without[3] = assistant_call("v1", "invoke_agent", &missing);
        let turn = observe(&without, "Residual", false);
        assert!(!turn.no_shell_unverified_on_attached_evidence);
        assert_eq!(decide(&turn), Decision::ReverifyOnce);
        assert!(!block_extra_verifier(&turn));
    }

    #[test]
    fn terminal_block_text_is_not_a_new_verdict() {
        assert!(parse_verdict(TERMINAL_VERIFIER_BLOCK).is_none());
        let prompt = r#"{"agent":"verifier","prompt":"EVIDENCE: volume=20"}"#;
        let messages = vec![
            user("set the SwitchBot volume to 20"),
            assistant_call("c1", "switchbot_command", "{}"),
            tool_result("c1", "volume=20"),
            assistant_call("v1", "invoke_agent", prompt),
            tool_result(
                "v1",
                "<evaluation>UNVERIFIED</evaluation>\nno shell: cannot run commands",
            ),
            assistant_call("v2", "invoke_agent", prompt),
            tool_result("v2", TERMINAL_VERIFIER_BLOCK),
        ];
        let turn = observe(&messages, "Residual: volume is 20.", false);
        assert_eq!(turn.verdict, Some(Verdict::Unverified));
        assert_eq!(turn.extra_rounds_used, 0);
        assert_eq!(decide(&turn), Decision::TerminalResidual);
    }

    #[test]
    fn password_in_verifier_feedback_is_not_a_pass_token() {
        let text = "<evaluation>FAIL</evaluation>\npassword is still hardcoded";
        assert_eq!(parse_verdict(text), Some(Verdict::Fail));
    }
}
