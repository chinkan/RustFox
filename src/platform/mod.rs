pub mod telegram;
pub mod tool_notifier;

/// A message received from any platform
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IncomingMessage {
    /// Platform identifier (e.g., "telegram", "discord")
    pub platform: String,
    /// Platform-specific user ID as string
    pub user_id: String,
    /// Platform-specific chat/channel ID as string
    pub chat_id: String,
    /// Display name of the user
    pub user_name: String,
    /// The message text
    pub text: String,
}
