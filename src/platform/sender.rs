use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

/// Opaque message identifier returned by the platform after sending.
/// Telegram encoding: `"{chat_id_int}:{message_id_int}"`.
/// Each adapter documents its own encoding.
pub type PlatformMessageId = String;

/// Message format mode for responses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageFormat {
    Rich,
    Markdown,
    Auto,
}

#[async_trait]
pub trait PlatformSender: Send + Sync {
    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        format: MessageFormat,
    ) -> Result<PlatformMessageId>;

    async fn send_file(
        &self,
        chat_id: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<PlatformMessageId>;

    async fn show_cancel_button(
        &self,
        chat_id: &str,
        text: &str,
        cancel_id: &str,
    ) -> Result<PlatformMessageId>;

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &PlatformMessageId,
        text: &str,
    ) -> Result<()>;

    async fn delete_message(&self, chat_id: &str, message_id: &PlatformMessageId) -> Result<()>;

    async fn notify_shutdown(&self, chat_id: &str) -> Result<()>;
}
