//! In-process Telegram **Update injector** for QA (no live Bot API / Desktop /
//! OpenRouter).
//!
//! Feed recorded or synthetic Bot API `Update` JSON into the same allowlist +
//! kind-routing path the live dispatcher uses in [`super::telegram::run`].
//! [`UpdateInjector::drive_handle_message`] runs the real
//! [`super::telegram::handle_message`] with a mock Bot; pair with
//! [`FixtureLlm`] so chat turns need no live OpenRouter.
//! Optional outbound asserts go through a wiremock `api.telegram.org` stand-in
//! (see `docs/telegram-update-injector.md` and `tests/telegram_update_injector.rs`).
//!
//! For secret-store harness paths (no live keyring / BotFather), use
//! [`crate::secret_store::FakeSecretStore`].

use anyhow::{Context, Result};
use serde_json::Value;
use teloxide::types::{CallbackQuery, Message, Update, UpdateKind, User};

use crate::platform::{normalize_bot_id, user_on_allowlist, DEFAULT_BOT_ID};

use std::sync::Arc;

use async_trait::async_trait;
use teloxide::Bot;

use crate::agent::Agent;
use crate::llm::{ChatCompletion, ChatMessage, MessageContent, ToolDefinition};
use crate::provider::{Provider, ProviderConfig};

/// Which dispatcher branch an Update would hit (after allowlist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerRoute {
    /// Filtered out by `allowed_user_ids` (message / callback).
    RejectedAllowlist,
    /// `Update::filter_message` → `handle_message`
    Message,
    /// Loop-detector callback (`"type":"loop"` in callback data)
    LoopCallback,
    /// Model-picker / other callback → `handle_model_callback`
    ModelCallback,
    /// No matching handler (default_handler)
    Unhandled,
}

/// Coarse fixture kind for assertions (message text vs media vs callback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectedKind {
    TextMessage,
    Media { has_photo: bool, has_document: bool },
    CallbackQuery,
    Other,
}

/// Allowlist gate result (mirrors the `filter_map` in `telegram::run`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistDecision {
    Accept {
        user_id: u64,
    },
    /// `user_id` is `None` when the Update has no actionable `from` user
    /// (e.g. channel posts without sender) — treated as reject, same as live.
    Reject {
        user_id: Option<u64>,
    },
}

/// Fields the message handler would see **before** any Bot API download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedIncoming {
    pub platform: String,
    pub bot_id: String,
    pub user_id: String,
    pub chat_id: String,
    pub user_name: String,
    /// `text` or `caption`, empty when neither present.
    pub text: String,
    pub has_photo: bool,
    pub has_document: bool,
    pub document_file_name: Option<String>,
    /// Parsed slash command when `text` starts with `/`.
    pub command: Option<(String, String)>,
}

/// Callback fields the callback handlers would see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedCallback {
    pub user_id: u64,
    pub user_name: String,
    pub data: Option<String>,
    pub is_loop_callback: bool,
}

/// Fixture Update injector bound to one bot's allowlist + `bot_id`.
#[derive(Debug, Clone)]
pub struct UpdateInjector {
    allowed_user_ids: Vec<u64>,
    bot_id: String,
}

impl UpdateInjector {
    /// New injector for a single bot allowlist (same as one `[[bots]]` entry).
    pub fn new(allowed_user_ids: impl Into<Vec<u64>>) -> Self {
        Self {
            allowed_user_ids: allowed_user_ids.into(),
            bot_id: DEFAULT_BOT_ID.to_string(),
        }
    }

    pub fn with_bot_id(mut self, bot_id: impl Into<String>) -> Self {
        self.bot_id = normalize_bot_id(&bot_id.into()).to_string();
        self
    }

    pub fn allowed_user_ids(&self) -> &[u64] {
        &self.allowed_user_ids
    }

    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }

    /// Deserialize a Bot API `Update` from JSON text.
    pub fn parse_update(json: &str) -> Result<Update> {
        serde_json::from_str(json).context("failed to deserialize Telegram Update JSON")
    }

    /// Deserialize from a `serde_json::Value` (fixtures loaded as Value).
    ///
    /// Goes through text rather than `from_value`: teloxide's flattened
    /// `UpdateKind` custom deserializer is unreliable with `Value` round-trips.
    pub fn parse_update_value(value: &Value) -> Result<Update> {
        let raw = serde_json::to_string(value).context("failed to serialize Update JSON value")?;
        Self::parse_update(&raw)
    }

    /// Load + parse a fixture file (UTF-8 JSON).
    pub fn parse_update_file(path: impl AsRef<std::path::Path>) -> Result<Update> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read Update fixture {}", path.display()))?;
        Self::parse_update(&raw)
    }

    /// Classify the Update for fixture assertions.
    pub fn classify(update: &Update) -> InjectedKind {
        match &update.kind {
            UpdateKind::CallbackQuery(_) => InjectedKind::CallbackQuery,
            UpdateKind::Message(m)
            | UpdateKind::EditedMessage(m)
            | UpdateKind::ChannelPost(m)
            | UpdateKind::EditedChannelPost(m)
            | UpdateKind::BusinessMessage(m)
            | UpdateKind::EditedBusinessMessage(m) => classify_message(m),
            _ => InjectedKind::Other,
        }
    }

    /// Allowlist decision for the acting user (message `from` / callback `from`).
    pub fn allowlist_decision(&self, update: &Update) -> AllowlistDecision {
        match acting_user(update) {
            Some(user) => {
                if user_on_allowlist(&self.allowed_user_ids, user.id.0) {
                    AllowlistDecision::Accept { user_id: user.id.0 }
                } else {
                    AllowlistDecision::Reject {
                        user_id: Some(user.id.0),
                    }
                }
            }
            None => AllowlistDecision::Reject { user_id: None },
        }
    }

    /// Dispatcher route this Update would take with this injector's allowlist.
    pub fn route(&self, update: &Update) -> HandlerRoute {
        match &update.kind {
            UpdateKind::Message(m)
            | UpdateKind::EditedMessage(m)
            | UpdateKind::ChannelPost(m)
            | UpdateKind::EditedChannelPost(m)
            | UpdateKind::BusinessMessage(m)
            | UpdateKind::EditedBusinessMessage(m) => {
                if message_passes_allowlist(&self.allowed_user_ids, m) {
                    HandlerRoute::Message
                } else {
                    HandlerRoute::RejectedAllowlist
                }
            }
            UpdateKind::CallbackQuery(q) => {
                if is_loop_callback(q) {
                    // Live dispatcher: loop branch has no allowlist filter.
                    HandlerRoute::LoopCallback
                } else if callback_passes_allowlist(&self.allowed_user_ids, q) {
                    HandlerRoute::ModelCallback
                } else {
                    HandlerRoute::RejectedAllowlist
                }
            }
            _ => HandlerRoute::Unhandled,
        }
    }

    /// Extract message-handler inputs without downloading media.
    ///
    /// Returns `None` when the Update is not a message kind, or allowlist rejects.
    pub fn extract_incoming(&self, update: &Update) -> Option<InjectedIncoming> {
        let msg = match &update.kind {
            UpdateKind::Message(m)
            | UpdateKind::EditedMessage(m)
            | UpdateKind::ChannelPost(m)
            | UpdateKind::EditedChannelPost(m)
            | UpdateKind::BusinessMessage(m)
            | UpdateKind::EditedBusinessMessage(m) => m,
            _ => return None,
        };
        if !message_passes_allowlist(&self.allowed_user_ids, msg) {
            return None;
        }
        let user = msg.from.as_ref()?;
        let text = msg
            .text()
            .or_else(|| msg.caption())
            .unwrap_or("")
            .to_string();
        let has_photo = msg.photo().is_some();
        let has_document = msg.document().is_some();
        let document_file_name = msg.document().and_then(|d| d.file_name.clone());
        let command = super::telegram::parse_command(&text);
        Some(InjectedIncoming {
            platform: "telegram".into(),
            bot_id: self.bot_id.clone(),
            user_id: user.id.0.to_string(),
            chat_id: msg.chat.id.0.to_string(),
            user_name: user.first_name.clone(),
            text,
            has_photo,
            has_document,
            document_file_name,
            command,
        })
    }

    /// Extract callback-handler inputs when allowlist (or loop branch) accepts.
    pub fn extract_callback(&self, update: &Update) -> Option<InjectedCallback> {
        let q = match &update.kind {
            UpdateKind::CallbackQuery(q) => q,
            _ => return None,
        };
        let loop_cb = is_loop_callback(q);
        if !loop_cb && !callback_passes_allowlist(&self.allowed_user_ids, q) {
            return None;
        }
        Some(InjectedCallback {
            user_id: q.from.id.0,
            user_name: q.from.first_name.clone(),
            data: q.data.clone(),
            is_loop_callback: loop_cb,
        })
    }
}

/// Same predicate as the message `filter_map` in `telegram::run`.
pub fn message_passes_allowlist(allowed_user_ids: &[u64], msg: &Message) -> bool {
    msg.from
        .as_ref()
        .is_some_and(|u| crate::platform::user_may_send_message(allowed_user_ids, u.id.0))
}

/// Same predicate as the model-callback `filter_map` in `telegram::run`.
pub fn callback_passes_allowlist(allowed_user_ids: &[u64], q: &CallbackQuery) -> bool {
    user_on_allowlist(allowed_user_ids, q.from.id.0)
}

/// Loop-detector callback data marker (matches `telegram::run` loop branch).
pub fn is_loop_callback(q: &CallbackQuery) -> bool {
    q.data
        .as_deref()
        .is_some_and(|d| d.contains(r#""type":"loop""#))
}

fn acting_user(update: &Update) -> Option<&User> {
    update.from()
}

fn classify_message(msg: &Message) -> InjectedKind {
    let has_photo = msg.photo().is_some();
    let has_document = msg.document().is_some();
    if has_photo || has_document {
        InjectedKind::Media {
            has_photo,
            has_document,
        }
    } else if msg.text().is_some() {
        InjectedKind::TextMessage
    } else {
        InjectedKind::Other
    }
}

// ---------------------------------------------------------------------------
// Offline handle_message driver + stub/fixture LLM
// ---------------------------------------------------------------------------

/// Deterministic offline LLM for CI/QA — no OpenRouter / network.
///
/// Returns a fixed assistant reply (no tool calls) so `Agent::process_message`
/// completes in one turn. Used with [`UpdateInjector::drive_handle_message`].
///
/// `models_seen` records stripped `actual_model` ids from each
/// `chat_completion` (ADR-0036 live Active regression).
pub struct FixtureLlm {
    config: ProviderConfig,
    reply: String,
    models_seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl FixtureLlm {
    /// Build a fixture provider named `fixture` with model `stub`.
    pub fn new(reply: impl Into<String>) -> Self {
        Self::with_models_seen(reply, Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    /// Like [`Self::new`], sharing a caller's recorder for model-id asserts.
    pub fn with_models_seen(
        reply: impl Into<String>,
        models_seen: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            config: ProviderConfig {
                name: "fixture".into(),
                provider_type: crate::config::ProviderType::OpenRouter,
                base_url: "http://fixture.invalid/v1".into(),
                api_key: None,
                default_model: "stub".into(),
                supports_vision: false,
                max_tokens: 256,
                discover_models: false,
                context_window: 4096,
                context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
                parse_retry_limit: 0,
                rate_limit_retry_limit: 0,
            },
            reply: reply.into(),
            models_seen,
        }
    }

    pub fn reply(&self) -> &str {
        &self.reply
    }

    /// Shared list of stripped model ids passed to `chat_completion`.
    pub fn models_seen(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        Arc::clone(&self.models_seen)
    }

    /// Wrap as a one-entry [`crate::provider::ProviderRegistry`] (default = `fixture`).
    pub fn into_registry(self) -> crate::provider::ProviderRegistry {
        let mut providers = std::collections::HashMap::new();
        providers.insert("fixture".to_string(), Arc::new(self) as Arc<dyn Provider>);
        crate::provider::ProviderRegistry::new(providers, "fixture".into())
    }
}

#[async_trait]
impl Provider for FixtureLlm {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        _client: &reqwest::Client,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<ChatCompletion> {
        if let Ok(mut seen) = self.models_seen.lock() {
            seen.push(model.to_string());
        }
        Ok(ChatCompletion {
            message: ChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::from_text(self.reply.clone())),
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: Some("stop".into()),
            model: model.to_string(),
        })
    }

    async fn list_models(&self, _client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        Ok(vec![self.config.default_model.clone()])
    }
}

impl UpdateInjector {
    /// Extract the teloxide [`Message`] when this update is a message kind.
    pub fn message_from_update(update: &Update) -> Option<&Message> {
        match &update.kind {
            UpdateKind::Message(m)
            | UpdateKind::EditedMessage(m)
            | UpdateKind::ChannelPost(m)
            | UpdateKind::EditedChannelPost(m)
            | UpdateKind::BusinessMessage(m)
            | UpdateKind::EditedBusinessMessage(m) => Some(m),
            _ => None,
        }
    }

    /// Drive an Update through the real [`super::telegram::handle_message`] path
    /// when allowlist + message route accept it.
    ///
    /// Non-message / rejected routes return the route without calling the handler
    /// (same as the live dispatcher drop). Bot API calls go to whatever URL the
    /// `Bot` is pointed at (use wiremock in tests). Pair with [`FixtureLlm`] so
    /// chat turns need no live OpenRouter.
    pub async fn drive_handle_message(
        &self,
        update: &Update,
        bot: Bot,
        agent: Arc<Agent>,
    ) -> Result<HandlerRoute> {
        let route = self.route(update);
        if route != HandlerRoute::Message {
            return Ok(route);
        }
        let msg = Self::message_from_update(update)
            .cloned()
            .context("message route but Update has no Message")?;
        super::telegram::handle_message(
            bot,
            msg,
            agent,
            self.bot_id.clone(),
            super::telegram::LiveAllowlist(Arc::new(std::sync::RwLock::new(
                self.allowed_user_ids.clone(),
            ))),
        )
        .await
        .map_err(|e| anyhow::anyhow!("handle_message failed: {e}"))?;
        Ok(HandlerRoute::Message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_update(user_id: u64, text: &str) -> Update {
        let v = json!({
            "update_id": 1001,
            "message": {
                "message_id": 1,
                "date": 1700000000,
                "from": {
                    "id": user_id,
                    "is_bot": false,
                    "first_name": "QA"
                },
                "chat": {
                    "id": user_id as i64,
                    "type": "private",
                    "first_name": "QA"
                },
                "text": text
            }
        });
        UpdateInjector::parse_update_value(&v).unwrap()
    }

    #[test]
    fn allowlist_accepts_and_rejects_text_message() {
        let inj = UpdateInjector::new([42u64]).with_bot_id("main");
        let ok = text_update(42, "/start");
        assert_eq!(inj.route(&ok), HandlerRoute::Message);
        assert!(matches!(
            inj.allowlist_decision(&ok),
            AllowlistDecision::Accept { user_id: 42 }
        ));
        let incoming = inj.extract_incoming(&ok).unwrap();
        assert_eq!(incoming.bot_id, "main");
        assert_eq!(incoming.command, Some(("start".into(), "".into())));

        let bad = text_update(99, "hello");
        assert_eq!(inj.route(&bad), HandlerRoute::RejectedAllowlist);
        assert!(inj.extract_incoming(&bad).is_none());
    }

    #[test]
    fn unowned_sentinel_admits_a_real_sender_message_only() {
        let first = text_update(99, "hello");
        let msg = UpdateInjector::message_from_update(&first).unwrap();
        assert!(message_passes_allowlist(&[0], msg));
        assert!(!message_passes_allowlist(&[42], msg));
        let inj = UpdateInjector::new([0u64]);
        assert_eq!(inj.route(&first), HandlerRoute::Message);
        // A callback is not a claim. User 0 is not on a real allowlist of [42].
        assert!(!crate::platform::user_may_send_message(&[0], 0));
        assert!(crate::platform::user_may_send_message(&[0], 99));
        assert!(!crate::platform::allowlist_is_unowned(&[42]));
    }

    #[test]
    fn classifies_photo_media_fixture() {
        let v = json!({
            "update_id": 1002,
            "message": {
                "message_id": 2,
                "date": 1700000001,
                "from": { "id": 42, "is_bot": false, "first_name": "QA" },
                "chat": { "id": 42, "type": "private", "first_name": "QA" },
                "caption": "see this",
                "photo": [
                    {
                        "file_id": "AgAD-photo-small",
                        "file_unique_id": "uniq1",
                        "width": 90,
                        "height": 90,
                        "file_size": 100
                    },
                    {
                        "file_id": "AgAD-photo-large",
                        "file_unique_id": "uniq2",
                        "width": 800,
                        "height": 600,
                        "file_size": 50000
                    }
                ]
            }
        });
        let upd = UpdateInjector::parse_update_value(&v).unwrap();
        assert_eq!(
            UpdateInjector::classify(&upd),
            InjectedKind::Media {
                has_photo: true,
                has_document: false
            }
        );
        let inj = UpdateInjector::new([42u64]);
        let incoming = inj.extract_incoming(&upd).unwrap();
        assert!(incoming.has_photo);
        assert_eq!(incoming.text, "see this");
    }

    #[test]
    fn callback_routes_model_vs_loop_vs_reject() {
        let inj = UpdateInjector::new([42u64]);
        let model = UpdateInjector::parse_update_value(&json!({
            "update_id": 2001,
            "callback_query": {
                "id": "cq1",
                "from": { "id": 42, "is_bot": false, "first_name": "QA" },
                "chat_instance": "inst",
                "data": "model:openai/gpt-4o"
            }
        }))
        .unwrap();
        assert_eq!(inj.route(&model), HandlerRoute::ModelCallback);
        assert_eq!(
            inj.extract_callback(&model).unwrap().data.as_deref(),
            Some("model:openai/gpt-4o")
        );

        let loop_cb = UpdateInjector::parse_update_value(&json!({
            "update_id": 2002,
            "callback_query": {
                "id": "cq2",
                "from": { "id": 99, "is_bot": false, "first_name": "Stranger" },
                "chat_instance": "inst",
                "data": "{\"type\":\"loop\",\"choice\":\"continue\"}"
            }
        }))
        .unwrap();
        // Loop branch has no allowlist in live dispatcher.
        assert_eq!(inj.route(&loop_cb), HandlerRoute::LoopCallback);
        assert!(inj.extract_callback(&loop_cb).unwrap().is_loop_callback);

        let rejected = UpdateInjector::parse_update_value(&json!({
            "update_id": 2003,
            "callback_query": {
                "id": "cq3",
                "from": { "id": 99, "is_bot": false, "first_name": "Stranger" },
                "chat_instance": "inst",
                "data": "model:x"
            }
        }))
        .unwrap();
        assert_eq!(inj.route(&rejected), HandlerRoute::RejectedAllowlist);
        assert!(inj.extract_callback(&rejected).is_none());
    }
}
