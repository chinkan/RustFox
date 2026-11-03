//! Schedule runs and interactive chat do not share an LLM history window
//! (ADR-0059).
//!
//! A scheduled turn loads a fresh conversation, not the owner's interactive
//! chat. The prompt and the result are not appended to that chat.
//! `scheduled_task_runs` remains the durable record. Telegram delivery stays
//! on `send_schedule_reply` / the human-gate. The owner pulls a result into
//! a later interactive turn by reply-quoting that Telegram message (ADR-0048).
//!
//! `process_message` still skips its own save when
//! [`crate::platform::IncomingMessage::schedule_id`] is set, so a schedule
//! turn does not write its prompt into whichever row it loaded.

use anyhow::Result;

use crate::config::BotConfig;
use crate::llm::ChatMessage;
use crate::memory::MemoryStore;
use crate::platform::normalize_bot_id;
use crate::scheduler::reminders::ScheduledTask;

/// Marker stored on every schedule user turn and result turn.
pub fn schedule_marker(schedule_id: &str) -> String {
    format!("[schedule:{schedule_id}]")
}

/// Prefix `body` with the schedule marker unless it is already there.
pub fn with_schedule_id(schedule_id: &str, body: &str) -> String {
    let marker = schedule_marker(schedule_id);
    if body.contains(&marker) {
        body.to_string()
    } else if body.is_empty() {
        marker
    } else {
        format!("{marker}\n{body}")
    }
}

/// SQLite address of one conversation the LLM history is loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentConversation {
    pub platform: String,
    pub bot_id: String,
    pub user_id: String,
}

/// Platform of a scheduled turn's private conversation. Not telegram, portal,
/// or web, so it cannot be the interactive row.
pub const SCHEDULE_CONVERSATION_PLATFORM: &str = "schedule";

/// True when `text` is a schedule segment body (`[schedule:{id}]` …).
///
/// Reply-quote turns start with `[Reply to` and are not segments, even when
/// the quoted Telegram result mentions a schedule.
pub fn is_schedule_segment_text(text: &str) -> bool {
    text.trim_start().starts_with("[schedule:")
}

/// True when this stored turn is a schedule segment (ADR-0059).
pub fn is_schedule_segment_message(message: &ChatMessage) -> bool {
    message
        .content
        .as_ref()
        .is_some_and(|content| is_schedule_segment_text(&content.as_text()))
}

/// History window for an interactive prompt. Schedule segments are omitted.
///
/// Rows already stored before this change stay in SQLite. They are left out
/// of `messages[]` here instead of being rewritten.
pub fn without_schedule_segments(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .filter(|message| !is_schedule_segment_message(message))
        .collect()
}

/// Conversation a turn's LLM history is loaded from.
///
/// Interactive turns stay on `(platform, bot_id, user_id)`. A scheduled turn
/// gets a new row every call so it does not read that chat or a previous
/// fire. Checkpoint resume does not use this row (ADR-0054 restores the
/// saved transcript).
pub fn llm_conversation_key(
    platform: &str,
    bot_id: &str,
    user_id: &str,
    schedule_id: Option<&str>,
) -> SegmentConversation {
    if let Some(schedule_id) = schedule_id.map(str::trim).filter(|id| !id.is_empty()) {
        return SegmentConversation {
            platform: SCHEDULE_CONVERSATION_PLATFORM.to_string(),
            bot_id: SCHEDULE_CONVERSATION_PLATFORM.to_string(),
            user_id: format!("{schedule_id}:{}", uuid::Uuid::new_v4()),
        };
    }
    SegmentConversation {
        platform: platform.to_string(),
        bot_id: normalize_bot_id(bot_id).to_string(),
        user_id: user_id.to_string(),
    }
}

/// Do not copy the prompt or the result into the interactive conversation.
///
/// The job runner still calls this for every outcome (success, failure,
/// cancel, timeout, max-iterations). Durable evidence is `scheduled_task_runs`.
/// Telegram notify is unchanged. `bots` is retained so callers keep the same
/// signature they used when the segment was written onto the owning chat.
pub fn write_schedule_segment(
    _memory: &MemoryStore,
    task: &ScheduledTask,
    _bots: &[BotConfig],
    result: &str,
) -> Result<()> {
    tracing::debug!(
        schedule_id = %task.id,
        result_bytes = result.len(),
        "schedule segment stays out of the interactive conversation"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::reminders::ScheduledTask;

    fn bot(id: &str) -> BotConfig {
        BotConfig {
            id: id.to_string(),
            bot_token: format!("tok-{id}"),
            allowed_user_ids: vec![555],
            persona: "main".to_string(),
            system_prompt_file: None,
            model: None,
            tools: None,
            fully_silent: false,
        }
    }

    fn task(id: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.to_string(),
            scheduler_job_id: None,
            user_id: "owner".to_string(),
            chat_id: "555".to_string(),
            platform: "telegram".to_string(),
            trigger_type: "recurring".to_string(),
            trigger_value: "0 0 * * * *".to_string(),
            prompt: "check the calendar".to_string(),
            description: "calendar".to_string(),
            status: "active".to_string(),
            created_at: "2026-01-01T00:00:00".to_string(),
            next_run_at: None,
            deleted_at: None,
            bot_id: "researcher".to_string(),
            timezone: "UTC".to_string(),
            expected_artifact: None,
            artifact_grace_secs: crate::scheduler::completion::DEFAULT_ARTIFACT_GRACE_SECS,
        }
    }

    fn portal_task(id: &str) -> ScheduledTask {
        let mut task = task(id);
        task.user_id = "web".to_string();
        task.platform = "portal".to_string();
        task.bot_id = crate::platform::DEFAULT_BOT_ID.to_string();
        task.chat_id = "555".to_string();
        task.prompt = "summarize overnight mail".to_string();
        task
    }

    async fn roles(
        memory: &MemoryStore,
        platform: &str,
        bot_id: &str,
        user_id: &str,
    ) -> Vec<(String, String)> {
        let conv = memory
            .get_or_create_conversation(platform, bot_id, user_id)
            .await
            .unwrap();
        memory
            .load_messages(&conv)
            .await
            .unwrap()
            .into_iter()
            .map(|m| {
                let text = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
                (m.role, text)
            })
            .collect()
    }

    fn turn(role: &str, text: &str) -> crate::llm::ChatMessage {
        crate::llm::ChatMessage {
            role: role.to_string(),
            content: Some(crate::llm::MessageContent::from_text(text)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn joined(rows: &[(String, String)]) -> String {
        rows.iter()
            .map(|(role, text)| format!("{role}: {text}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn interactive_prompt_without_reply_excludes_schedule_run_body() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let task = portal_task("sched-1");
        let bots = vec![bot("main")];
        let existing = memory
            .get_or_create_conversation("telegram", "main", "555")
            .await
            .unwrap();
        memory
            .save_message(&existing, &turn("user", "earlier telegram note"))
            .await
            .unwrap();
        // A row already stored the old way must also stay out of the prompt.
        memory
            .save_message(
                &existing,
                &turn("assistant", "[schedule:sched-old]\nstale vault sync"),
            )
            .await
            .unwrap();

        write_schedule_segment(&memory, &task, &bots, "inbox is quiet").unwrap();

        let tg = roles(&memory, "telegram", "main", "555").await;
        let stored = joined(&tg);
        assert!(
            stored.contains("earlier telegram note"),
            "interactive chat stays: {stored}"
        );
        assert!(
            !stored.contains("inbox is quiet") && !stored.contains("[schedule:sched-1]"),
            "a finished schedule segment must not be written into the interactive chat: {stored}"
        );
        assert!(
            !stored.contains("summarize overnight mail"),
            "schedule prompt must not be written into the interactive chat: {stored}"
        );

        let portal = roles(&memory, "portal", crate::platform::DEFAULT_BOT_ID, "web").await;
        let portal_blob = joined(&portal);
        assert!(
            !portal_blob.contains("[schedule:sched-1]") && !portal_blob.contains("inbox is quiet"),
            "portal chat must not receive the segment either: {portal_blob}"
        );

        let window = without_schedule_segments(memory.load_messages(&existing).await.unwrap());
        let prompt = window
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(prompt.contains("earlier telegram note"), "{prompt}");
        assert!(
            !prompt.contains("[schedule:") && !prompt.contains("stale vault sync"),
            "interactive prompt without a reply must omit schedule bodies: {prompt}"
        );
        assert!(!prompt.contains("inbox is quiet"), "{prompt}");
    }

    #[test]
    fn reply_quote_pulls_schedule_result_for_this_turn_only() {
        let history = vec![
            turn("user", "earlier chat"),
            turn("user", "[schedule:sched-1]\ncheck the calendar"),
            turn("assistant", "[schedule:sched-1]\ninbox is quiet"),
        ];
        let window = without_schedule_segments(history);
        let blob = window
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("earlier chat"), "{blob}");
        assert!(
            !blob.contains("[schedule:sched-1]")
                && !blob.contains("inbox is quiet")
                && !blob.contains("check the calendar"),
            "no reply → schedule result is not in the history window: {blob}"
        );

        let this_turn = crate::platform::telegram_reply_to::compose_reply_to_user_text(
            "bot",
            42,
            &crate::platform::telegram_reply_to::ReplyQuote::Text("inbox is quiet".into()),
            "what did the schedule find?",
        );
        assert!(this_turn.starts_with("[Reply to bot msg 42]:"));
        assert!(
            this_turn.contains("inbox is quiet"),
            "reply-quote injects the Telegram result on this turn: {this_turn}"
        );
        assert!(
            !blob.contains("inbox is quiet"),
            "the quote is not copied into the prior history window"
        );

        let kept = without_schedule_segments(vec![turn("user", &this_turn)]);
        assert_eq!(
            kept.len(),
            1,
            "a reply-quote turn is not a schedule segment"
        );
        assert!(kept[0]
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("inbox is quiet"));

        let next = without_schedule_segments(vec![
            turn("user", "earlier chat"),
            turn("user", "a different question"),
        ]);
        let next_blob = next
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!next_blob.contains("inbox is quiet"));
        assert!(!next_blob.contains("[schedule:"));
        assert!(next_blob.contains("a different question"));
    }

    #[tokio::test]
    async fn schedule_fire_does_not_see_prior_interactive_chat() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let row = task("sched-iso");
        let interactive = memory
            .get_or_create_conversation("telegram", "researcher", "owner")
            .await
            .unwrap();
        memory
            .save_message(&interactive, &turn("user", "unrelated birthday plan"))
            .await
            .unwrap();
        memory
            .save_message(
                &interactive,
                &turn("assistant", "[schedule:old]\nold vault sync"),
            )
            .await
            .unwrap();

        let incoming = crate::agent::Agent::scheduled_incoming(&row);
        assert_eq!(incoming.platform, "telegram");
        assert_eq!(incoming.user_id, "owner");
        assert_eq!(incoming.bot_id, "researcher");
        assert_eq!(incoming.schedule_id.as_deref(), Some("sched-iso"));

        let key = llm_conversation_key(
            &incoming.platform,
            &incoming.bot_id,
            &incoming.user_id,
            incoming.schedule_id.as_deref(),
        );
        assert_ne!(
            (
                key.platform.as_str(),
                key.bot_id.as_str(),
                key.user_id.as_str()
            ),
            (
                incoming.platform.as_str(),
                incoming.bot_id.as_str(),
                incoming.user_id.as_str()
            ),
            "schedule fire must not load the interactive chat"
        );
        let sched = memory
            .get_or_create_conversation(&key.platform, &key.bot_id, &key.user_id)
            .await
            .unwrap();
        assert_ne!(sched, interactive);
        let loaded = memory.load_messages(&sched).await.unwrap();
        let blob = loaded
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !blob.contains("unrelated birthday plan") && !blob.contains("old vault sync"),
            "schedule conversation must not contain prior user chat: {blob}"
        );

        let again = llm_conversation_key(
            &incoming.platform,
            &incoming.bot_id,
            &incoming.user_id,
            incoming.schedule_id.as_deref(),
        );
        assert_ne!(
            again.user_id, key.user_id,
            "each schedule fire gets a fresh conversation"
        );
    }

    #[test]
    fn interactive_llm_key_stays_on_the_owning_chat() {
        let key = llm_conversation_key("telegram", " researcher ", "owner", None);
        assert_eq!(key.platform, "telegram");
        assert_eq!(key.bot_id, "researcher");
        assert_eq!(key.user_id, "owner");
    }

    #[tokio::test]
    async fn portal_run_does_not_claim_legacy_telegram_history() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let task = portal_task("sched-claim");
        let bots = vec![bot("main"), bot("researcher")];
        let legacy = memory
            .get_or_create_conversation("telegram", crate::platform::DEFAULT_BOT_ID, "555")
            .await
            .unwrap();
        memory
            .save_message(&legacy, &turn("user", "legacy history"))
            .await
            .unwrap();

        write_schedule_segment(&memory, &task, &bots, "claimed result").unwrap();

        let opened = memory
            .get_or_create_conversation_with_claim("telegram", "main", "555", true)
            .await
            .unwrap();
        assert_eq!(opened, legacy, "the next reply still claims the legacy row");
        let msgs = memory.load_messages(&opened).await.unwrap();
        let blob = msgs
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("legacy history"));
        assert!(!blob.contains("claimed result"));
        assert!(!blob.contains("[schedule:sched-claim]"));
        assert!(!blob.contains("summarize overnight mail"));
    }

    #[tokio::test]
    async fn failure_cancel_and_max_iterations_stay_out_of_owning_chat() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let bots = vec![bot("researcher")];
        let cases = [
            ("sched-fail", "Scheduled task failed: provider down"),
            ("sched-cancel", "Processing was cancelled."),
            (
                "sched-cap",
                "I've reached the maximum number of tool call iterations. Please try rephrasing your request.",
            ),
        ];
        for (id, result) in cases {
            let mut row = task(id);
            row.prompt = format!("prompt for {id}");
            write_schedule_segment(&memory, &row, &bots, result).unwrap();
            let msgs = roles(&memory, "telegram", "researcher", "owner").await;
            assert!(
                msgs.iter()
                    .all(|(_, t)| !t.contains(&format!("[schedule:{id}]")) && !t.contains(result)),
                "failure segment must not enter the owning chat for {id}: {msgs:?}"
            );
        }
    }

    #[tokio::test]
    async fn telegram_success_does_not_write_the_owning_conversation() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let row = task("sched-ok");
        let bots = vec![bot("main"), bot("researcher")];
        write_schedule_segment(&memory, &row, &bots, "calendar is clear").unwrap();

        let own = roles(&memory, "telegram", "researcher", "owner").await;
        assert!(
            own.iter().all(|(_, t)| !t.contains("calendar is clear")
                && !t.contains("[schedule:sched-ok]")
                && !t.contains("check the calendar")),
            "telegram-origin result must stay out of the owning chat: {own:?}"
        );
    }

    #[test]
    fn scheduled_incoming_keeps_telegram_routing_and_marks_the_schedule() {
        let row = task("sched-in");
        let incoming = crate::agent::Agent::scheduled_incoming(&row);
        assert_eq!(incoming.platform, "telegram");
        assert_eq!(incoming.bot_id, "researcher");
        assert_eq!(incoming.user_id, "owner");
        assert_eq!(incoming.chat_id, row.chat_id);
        assert_eq!(incoming.schedule_id.as_deref(), Some("sched-in"));
        assert!(incoming.text.contains("check the calendar"));
        assert!(incoming.text.contains("[schedule:sched-in]"));

        let portal = portal_task("sched-portal-in");
        let incoming = crate::agent::Agent::scheduled_incoming(&portal);
        // The run itself stays on the row. The Telegram transcript is the
        // runner's segment write, not a retarget of cancel/session keys.
        assert_eq!(incoming.platform, "portal");
        assert_eq!(incoming.user_id, "web");
        assert_eq!(incoming.schedule_id.as_deref(), Some("sched-portal-in"));
        assert!(incoming.text.contains("[schedule:sched-portal-in]"));
    }
}
