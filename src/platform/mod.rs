pub mod sender;
pub mod telegram;
pub mod telegram_injector;
pub mod telegram_reply_to;
pub mod tool_notifier;

pub use sender::{PlatformMessageId, PlatformSender};
pub use telegram_injector::{
    AllowlistDecision, FixtureLlm, HandlerRoute, InjectedCallback, InjectedIncoming, InjectedKind,
    UpdateInjector,
};

/// What kind of attachment was received
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentKind {
    Image,
    Pdf,
    Docx,
    Other,
}

/// A file attachment received from a platform
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Absolute path to the downloaded temp file
    pub path: std::path::PathBuf,
    pub mime_type: String,
    /// Original filename, if known
    pub file_name: Option<String>,
}

/// Default bot id for single-bot / portal / legacy conversations (design §6.3).
pub const DEFAULT_BOT_ID: &str = "default";

/// Normalize empty/whitespace bot ids to [`DEFAULT_BOT_ID`].
pub fn normalize_bot_id(bot_id: &str) -> &str {
    let trimmed = bot_id.trim();
    if trimmed.is_empty() {
        DEFAULT_BOT_ID
    } else {
        trimmed
    }
}

/// Per-bot Telegram allowlist check (design §3 / §7.7 E2E).
///
/// Each dispatcher filters with its own `allowed_user_ids`; rejection on bot A
/// does not affect bot B.
pub fn user_on_allowlist(allowed_user_ids: &[u64], user_id: u64) -> bool {
    allowed_user_ids.contains(&user_id)
}

/// Fresh thin-setup writes `[0]` so the bot is unowned until the first real sender.
pub fn allowlist_is_unowned(allowed_user_ids: &[u64]) -> bool {
    allowed_user_ids.len() == 1 && allowed_user_ids[0] == 0
}

/// Message gate. The unowned sentinel admits the first positive Telegram user id
/// so that sender can claim the bot. `0` is not a user. Callbacks stay strict.
pub fn user_may_send_message(allowed_user_ids: &[u64], user_id: u64) -> bool {
    if allowlist_is_unowned(allowed_user_ids) {
        return user_id > 0;
    }
    user_on_allowlist(allowed_user_ids, user_id)
}

/// A message received from any platform
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IncomingMessage {
    /// Platform identifier (e.g., "telegram", "discord")
    pub platform: String,
    /// Bot identity within the install (`[[bots]].id`). Portal/web and tests
    /// use [`DEFAULT_BOT_ID`]. Isolates conversation history and cancel keys.
    pub bot_id: String,
    /// Platform-specific user ID as string
    pub user_id: String,
    /// Platform-specific chat/channel ID as string
    pub chat_id: String,
    /// Display name of the user
    pub user_name: String,
    /// The message text
    pub text: String,
    /// Attached files, if any
    pub attachments: Vec<Attachment>,
    /// Schedule that produced this turn, when the message is a scheduled run.
    ///
    /// `None` for ordinary chat. When set, `process_message` does not persist
    /// the turn; the job runner writes the prompt and the result (success,
    /// failure, cancel, and max-iterations) with this id on the segment.
    pub schedule_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_isolation_per_bot() {
        let main_allow = vec![111u64];
        let researcher_allow = vec![222u64];
        // Owner of main is rejected on researcher and vice versa
        assert!(user_on_allowlist(&main_allow, 111));
        assert!(!user_on_allowlist(&main_allow, 222));
        assert!(user_on_allowlist(&researcher_allow, 222));
        assert!(!user_on_allowlist(&researcher_allow, 111));
        // Shared owner on both is fine
        let shared = vec![42u64];
        assert!(user_on_allowlist(&shared, 42));
        assert!(user_on_allowlist(&shared, 42));
    }

    #[test]
    fn normalize_bot_id_empty_to_default() {
        assert_eq!(normalize_bot_id(""), DEFAULT_BOT_ID);
        assert_eq!(normalize_bot_id("  "), DEFAULT_BOT_ID);
        assert_eq!(normalize_bot_id("researcher"), "researcher");
    }
}
