//! Conversation segment for one scheduled run.
//!
//! A successful Telegram-origin run already persisted a user turn (the prompt)
//! and an assistant turn (the result) on the owning bot's chat. Three gaps:
//! portal rows are keyed `user_id = web` / `platform = portal`, so that write
//! is not the Telegram thread; failure, cancel, and max-iterations kept only
//! the prompt; the segment did not name the schedule.
//!
//! The job runner calls [`write_schedule_segment`] for every outcome.
//! `process_message` skips its own save when [`crate::platform::IncomingMessage::schedule_id`]
//! is set, so the segment is written once.

use anyhow::Result;

use crate::config::{BotConfig, Config};
use crate::llm::{ChatMessage, MessageContent};
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

/// Portal create stores `platform = portal` and `user_id` = the portal
/// account name (`web` by default). That pair is not a Telegram thread.
pub fn is_portal_origin(task: &ScheduledTask) -> bool {
    let platform = task.platform.trim();
    platform.eq_ignore_ascii_case("portal") || platform.eq_ignore_ascii_case("web")
}

/// Where one copy of the segment is appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentConversation {
    pub platform: String,
    pub bot_id: String,
    pub user_id: String,
}

/// Conversations that receive the prompt and the result for this run.
///
/// Telegram-origin: the row's own `(platform, bot_id, user_id)` — the same
/// chat a successful run already wrote.
///
/// Portal-origin: that portal/web conversation **and** the owning bot's
/// Telegram conversation, so the write is not only `web`/`portal`.
pub fn schedule_conversations(
    task: &ScheduledTask,
    bots: &[BotConfig],
) -> Vec<SegmentConversation> {
    let owner = normalize_bot_id(&task.bot_id).to_string();
    let mut out = Vec::new();
    if is_portal_origin(task) {
        if let Some(tg_user) = portal_telegram_user_id(task) {
            out.push(SegmentConversation {
                platform: "telegram".to_string(),
                bot_id: portal_telegram_bot_id(&owner, bots),
                user_id: tg_user,
            });
        }
    }
    out.push(SegmentConversation {
        platform: task.platform.clone(),
        bot_id: owner,
        user_id: task.user_id.clone(),
    });
    dedup_conversations(out)
}

/// Telegram user id for a portal row.
///
/// Portal create copies the shim allowlist's first id into `chat_id`.
/// A private Telegram turn uses that same id as `IncomingMessage::user_id`
/// (`from.id`, which equals `chat.id` in a DM). `user_id` on the row is the
/// portal account (`web`), so it is not the lookup key.
pub fn portal_telegram_user_id(task: &ScheduledTask) -> Option<String> {
    let chat = task.chat_id.trim();
    if chat.is_empty() {
        None
    } else {
        Some(chat.to_string())
    }
}

/// Bot id of the Telegram thread a portal task joins.
///
/// The row's `bot_id` is the owner. Portal create always stores `default`.
/// The dispatcher the owner actually replies on is the shim (`main`, else
/// `default`, else the only `[[bots]]` entry). A non-default owner is kept
/// as-is so a bot-scoped row is not redirected onto the shim.
fn portal_telegram_bot_id(owner_bot_id: &str, bots: &[BotConfig]) -> String {
    if owner_bot_id != crate::platform::DEFAULT_BOT_ID || bots.is_empty() {
        return owner_bot_id.to_string();
    }
    normalize_bot_id(Config::shim_bot(bots).id.as_str()).to_string()
}

fn dedup_conversations(addrs: Vec<SegmentConversation>) -> Vec<SegmentConversation> {
    let mut out = Vec::new();
    for addr in addrs {
        let seen = out.iter().any(|e: &SegmentConversation| {
            e.platform == addr.platform && e.bot_id == addr.bot_id && e.user_id == addr.user_id
        });
        if !seen {
            out.push(addr);
        }
    }
    out
}

fn turn(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(MessageContent::from_text(text)),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

/// Append the prompt and the result to every conversation this run should
/// update. `result` is the assistant text for a clean finish, a cancel, a
/// max-iterations stop, or a failure. Both turns include the schedule id.
///
/// Opens each thread with [`Config::bot_claims_legacy_default`], the same
/// claim a live Telegram turn uses, so a portal task lands on the conversation
/// the next reply will load (including a legacy `default` row claimed by
/// `main` or a sole custom bot id).
pub async fn write_schedule_segment(
    memory: &MemoryStore,
    task: &ScheduledTask,
    bots: &[BotConfig],
    result: &str,
) -> Result<()> {
    let prompt = with_schedule_id(&task.id, &task.prompt);
    let assistant = with_schedule_id(&task.id, result);
    let user_msg = turn("user", &prompt);
    let assistant_msg = turn("assistant", &assistant);
    for addr in schedule_conversations(task, bots) {
        let claim = Config::bot_claims_legacy_default(bots, &addr.bot_id);
        let conversation_id = memory
            .get_or_create_conversation_with_claim(
                &addr.platform,
                &addr.bot_id,
                &addr.user_id,
                claim,
            )
            .await?;
        memory.save_message(&conversation_id, &user_msg).await?;
        memory
            .save_message(&conversation_id, &assistant_msg)
            .await?;
    }
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

    #[test]
    fn portal_target_is_the_owning_bots_telegram_conversation() {
        let task = portal_task("sched-portal");
        let bots = vec![bot("main"), bot("researcher")];
        let convs = schedule_conversations(&task, &bots);
        assert!(
            convs
                .iter()
                .any(|c| { c.platform == "telegram" && c.bot_id == "main" && c.user_id == "555" }),
            "portal row must join the shim Telegram thread the owner replies on: {convs:?}"
        );
        assert!(
            convs.iter().any(|c| {
                c.platform == "portal"
                    && c.bot_id == crate::platform::DEFAULT_BOT_ID
                    && c.user_id == "web"
            }),
            "portal/web conversation is still written, not replaced: {convs:?}"
        );
        assert!(
            !convs
                .iter()
                .any(|c| c.platform == "telegram" && c.user_id == "web"),
            "user web is not a Telegram identity: {convs:?}"
        );
    }

    #[test]
    fn portal_nondefault_owner_stays_on_that_bot() {
        let mut task = portal_task("sched-own");
        task.bot_id = "researcher".to_string();
        let convs = schedule_conversations(&task, &[bot("main"), bot("researcher")]);
        assert!(convs.iter().any(|c| {
            c.platform == "telegram" && c.bot_id == "researcher" && c.user_id == "555"
        }));
        assert!(!convs.iter().any(|c| c.bot_id == "main"));
    }

    #[test]
    fn telegram_origin_stays_on_the_row_conversation() {
        let task = task("sched-tg");
        let convs = schedule_conversations(&task, &[bot("main"), bot("researcher")]);
        assert_eq!(
            convs,
            vec![SegmentConversation {
                platform: "telegram".to_string(),
                bot_id: "researcher".to_string(),
                user_id: "owner".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn portal_run_lands_in_telegram_and_segment_has_schedule_id() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let task = portal_task("sched-1");
        let bots = vec![bot("main")];
        // The owner already has a Telegram thread on the shim bot.
        let existing = memory
            .get_or_create_conversation("telegram", "main", "555")
            .await
            .unwrap();
        memory
            .save_message(&existing, &turn("user", "earlier telegram note"))
            .await
            .unwrap();

        write_schedule_segment(&memory, &task, &bots, "inbox is quiet")
            .await
            .unwrap();

        let tg = roles(&memory, "telegram", "main", "555").await;
        assert!(
            tg.iter()
                .any(|(r, t)| r == "user" && t.contains("earlier telegram note")),
            "must append onto the existing Telegram conversation, not a new one"
        );
        assert!(
            tg.iter().any(|(r, t)| {
                r == "user"
                    && t.contains("summarize overnight mail")
                    && t.contains("[schedule:sched-1]")
            }),
            "prompt must be visible to the next Telegram reply: {tg:?}"
        );
        assert!(
            tg.iter().any(|(r, t)| {
                r == "assistant" && t.contains("inbox is quiet") && t.contains("[schedule:sched-1]")
            }),
            "result must be visible to the next Telegram reply: {tg:?}"
        );

        let portal = roles(&memory, "portal", crate::platform::DEFAULT_BOT_ID, "web").await;
        assert!(
            portal
                .iter()
                .any(|(r, t)| r == "assistant" && t.contains("[schedule:sched-1]")),
            "portal conversation still receives the segment: {portal:?}"
        );
    }

    #[tokio::test]
    async fn portal_run_claims_legacy_default_telegram_row() {
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

        write_schedule_segment(&memory, &task, &bots, "claimed result")
            .await
            .unwrap();

        // The next main reply opens this same row via the legacy claim.
        let opened = memory
            .get_or_create_conversation_with_claim("telegram", "main", "555", true)
            .await
            .unwrap();
        assert_eq!(opened, legacy, "must reuse the row the next reply claims");
        let msgs = memory.load_messages(&opened).await.unwrap();
        let blob = msgs
            .iter()
            .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("legacy history"));
        assert!(blob.contains("summarize overnight mail"));
        assert!(blob.contains("claimed result"));
        assert!(blob.contains("[schedule:sched-claim]"));
    }

    #[tokio::test]
    async fn failure_cancel_and_max_iterations_write_a_result() {
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
            write_schedule_segment(&memory, &row, &bots, result)
                .await
                .unwrap();
            let msgs = roles(&memory, "telegram", "researcher", "owner").await;
            assert!(
                msgs.iter()
                    .any(|(r, t)| r == "user" && t.contains(&format!("[schedule:{id}]"))),
                "prompt segment missing schedule id for {id}: {msgs:?}"
            );
            assert!(
                msgs.iter().any(|(r, t)| {
                    r == "assistant"
                        && t.contains(result)
                        && t.contains(&format!("[schedule:{id}]"))
                }),
                "result turn missing for {id}: {msgs:?}"
            );
        }
    }

    #[tokio::test]
    async fn telegram_success_still_writes_the_owning_conversation() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let row = task("sched-ok");
        let bots = vec![bot("main"), bot("researcher")];
        write_schedule_segment(&memory, &row, &bots, "calendar is clear")
            .await
            .unwrap();

        let own = roles(&memory, "telegram", "researcher", "owner").await;
        assert!(own.iter().any(|(r, t)| {
            r == "user" && t.contains("check the calendar") && t.contains("[schedule:sched-ok]")
        }));
        assert!(own.iter().any(|(r, t)| {
            r == "assistant" && t.contains("calendar is clear") && t.contains("[schedule:sched-ok]")
        }));

        let other = roles(&memory, "telegram", "main", "owner").await;
        assert!(
            other.iter().all(|(_, t)| !t.contains("calendar is clear")),
            "a telegram-origin run must not move onto another bot: {other:?}"
        );
        let portal = roles(&memory, "portal", "researcher", "web").await;
        assert!(
            portal
                .iter()
                .all(|(_, t)| t.is_empty() || !t.contains("sched-ok")),
            "telegram-origin must not also invent a portal thread: {portal:?}"
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
