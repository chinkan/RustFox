//! Telegram Update injector harness — no live Bot API / no Desktop / no OpenRouter.
//!
//! ```bash
//! cargo test --test telegram_update_injector
//! ```
//!
//! See `docs/telegram-update-injector.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use rustfox::agent::Agent;
use rustfox::cancel_registry::CancelRegistry;
use rustfox::config::Config;
use rustfox::langsmith::LangSmithClient;
use rustfox::mcp::McpManager;
use rustfox::memory::MemoryStore;
use rustfox::platform::telegram::{notify_shutdown, notify_startup, TelegramAdapter};
use rustfox::platform::{
    AllowlistDecision, FixtureLlm, HandlerRoute, InjectedKind, UpdateInjector, DEFAULT_BOT_ID,
};
use rustfox::scheduler::reminders::ScheduledTaskStore;
use rustfox::scheduler::Scheduler;
use rustfox::skills::SkillRegistry;
use rustfox::tool_registry::ToolRegistry;
use serde_json::json;
use teloxide::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/telegram");
const ALLOWED: u64 = 111_001;
const TOKEN: &str = "000000000:QA-INJECTOR-MOCK-TOKEN";
const STUB_REPLY: &str = "FIXTURE_LLM_REPLY_deterministic";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(FIXTURE_DIR).join(name)
}

fn injector() -> UpdateInjector {
    UpdateInjector::new([ALLOWED]).with_bot_id("main")
}

/// Minimal Message JSON accepted as `sendMessage` / `editMessageText` result.
fn ok_message_result(chat_id: i64, message_id: i32, text: &str) -> serde_json::Value {
    json!({
        "ok": true,
        "result": {
            "message_id": message_id,
            "date": 1700000000,
            "chat": { "id": chat_id, "type": "private", "first_name": "QA" },
            "text": text
        }
    })
}

/// Wiremock stand-in for `https://api.telegram.org` (teloxide `Bot::set_api_url`).
struct MockTelegramApi {
    server: MockServer,
    token: String,
    next_msg_id: Arc<AtomicI32>,
    /// (assigned message id, text) for SendMessage and sendRichMessage.
    sent: Arc<Mutex<Vec<(i32, String)>>>,
    deleted: Arc<Mutex<Vec<i32>>>,
}

impl MockTelegramApi {
    async fn start() -> Self {
        Self {
            server: MockServer::start().await,
            token: TOKEN.to_string(),
            next_msg_id: Arc::new(AtomicI32::new(100)),
            sent: Arc::new(Mutex::new(Vec::new())),
            deleted: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn bot(&self) -> Bot {
        let url = reqwest::Url::parse(&format!("{}/", self.server.uri()))
            .expect("mock server URI is a valid URL");
        Bot::new(&self.token).set_api_url(url)
    }

    async fn stub_bot_api(&self) {
        let counter = Arc::clone(&self.next_msg_id);
        let sent_log = Arc::clone(&self.sent);
        Mock::given(method("POST"))
            .and(path_regex(r"^/bot[^/]+/SendMessage$"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                let chat_id = body
                    .get("chat_id")
                    .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                    .unwrap_or(0);
                let text = body
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mid = counter.fetch_add(1, Ordering::SeqCst);
                sent_log.lock().expect("sent log").push((mid, text.clone()));
                ResponseTemplate::new(200).set_body_json(ok_message_result(chat_id, mid, &text))
            })
            .expect(0..)
            .mount(&self.server)
            .await;

        let counter_edit = Arc::clone(&self.next_msg_id);
        Mock::given(method("POST"))
            .and(path_regex(r"^/bot[^/]+/EditMessageText$"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                let chat_id = body
                    .get("chat_id")
                    .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                    .unwrap_or(0);
                let mid = body
                    .get("message_id")
                    .and_then(|v| v.as_i64().map(|i| i as i32))
                    .unwrap_or_else(|| counter_edit.load(Ordering::SeqCst));
                let text = body
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                ResponseTemplate::new(200).set_body_json(ok_message_result(chat_id, mid, &text))
            })
            .expect(0..)
            .mount(&self.server)
            .await;

        let deleted_log = Arc::clone(&self.deleted);
        Mock::given(method("POST"))
            .and(path_regex(r"^/bot[^/]+/DeleteMessage$"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                if let Some(mid) = body.get("message_id").and_then(|v| v.as_i64()) {
                    deleted_log.lock().expect("deleted log").push(mid as i32);
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": true
                }))
            })
            .expect(0..)
            .mount(&self.server)
            .await;

        // rich_sender uses camelCase method names against the same api_base
        // (Bot::api_url), not teloxide's PascalCase paths.
        let counter_rich = Arc::clone(&self.next_msg_id);
        let rich_log = Arc::clone(&self.sent);
        Mock::given(method("POST"))
            .and(path_regex(r"^/bot[^/]+/sendRichMessage$"))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                let chat_id = body
                    .get("chat_id")
                    .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                    .unwrap_or(0);
                let text = body
                    .pointer("/rich_message/markdown")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mid = counter_rich.fetch_add(1, Ordering::SeqCst);
                rich_log.lock().expect("rich log").push((mid, text.clone()));
                ResponseTemplate::new(200).set_body_json(ok_message_result(chat_id, mid, &text))
            })
            .expect(0..)
            .mount(&self.server)
            .await;
    }

    /// Backward-compat alias used by the startup/shutdown smoke test.
    async fn stub_send_message(&self) {
        self.stub_bot_api().await;
    }

    async fn received_send_message_bodies(&self) -> Vec<serde_json::Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path().to_ascii_lowercase().contains("sendmessage"))
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect()
    }

    async fn received_requests_paths(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.url.path().to_string())
            .collect()
    }

    /// Outbound user-visible text from teloxide SendMessage (`text`) or
    /// rich_sender sendRichMessage (`rich_message.markdown`).
    async fn outbound_texts(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                let p = r.url.path().to_ascii_lowercase();
                p.contains("sendmessage") || p.contains("sendrichmessage")
            })
            .filter_map(|r| {
                let body: serde_json::Value = serde_json::from_slice(&r.body).ok()?;
                body.get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        body.pointer("/rich_message/markdown")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
            })
            .collect()
    }

    fn uri(&self) -> String {
        self.server.uri()
    }

    fn sent_snapshot(&self) -> Vec<(i32, String)> {
        self.sent.lock().expect("sent log").clone()
    }

    fn deleted_snapshot(&self) -> Vec<i32> {
        self.deleted.lock().expect("deleted log").clone()
    }
}

const TOOL_TURN_REPLY: &str = "FINAL_ASSISTANT_STILL_SENT";

/// First completion is `execute_command`, then a fixed assistant reply.
struct ToolThenReply {
    config: rustfox::provider::ProviderConfig,
    step: AtomicUsize,
    reply: String,
    only_tools: bool,
}

impl ToolThenReply {
    fn new(reply: impl Into<String>) -> Self {
        Self {
            config: rustfox::provider::ProviderConfig {
                name: "fixture".into(),
                provider_type: rustfox::config::ProviderType::OpenRouter,
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
            step: AtomicUsize::new(0),
            reply: reply.into(),
            only_tools: false,
        }
    }

    fn tools_only() -> Self {
        let mut this = Self::new("");
        this.only_tools = true;
        this
    }

    fn into_registry(self) -> rustfox::provider::ProviderRegistry {
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "fixture".to_string(),
            Arc::new(self) as Arc<dyn rustfox::provider::Provider>,
        );
        rustfox::provider::ProviderRegistry::new(providers, "fixture".into())
    }
}

#[async_trait::async_trait]
impl rustfox::provider::Provider for ToolThenReply {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &rustfox::provider::ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        _client: &reqwest::Client,
        _messages: &[rustfox::llm::ChatMessage],
        _tools: &[rustfox::llm::ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<rustfox::llm::ChatCompletion> {
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let message = if self.only_tools || n == 0 {
            rustfox::llm::ChatMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![rustfox::llm::ToolCall {
                    id: "call_exec".into(),
                    call_type: "function".into(),
                    function: rustfox::llm::FunctionCall {
                        name: "execute_command".into(),
                        arguments: r#"{"command":"true"}"#.into(),
                    },
                }]),
                tool_call_id: None,
            }
        } else {
            rustfox::llm::ChatMessage {
                role: "assistant".into(),
                content: Some(rustfox::llm::MessageContent::from_text(self.reply.clone())),
                tool_calls: None,
                tool_call_id: None,
            }
        };
        Ok(rustfox::llm::ChatCompletion {
            message,
            finish_reason: Some(if n == 0 { "tool_calls" } else { "stop" }.into()),
            model: model.to_string(),
        })
    }

    async fn list_models(&self, _client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        Ok(vec![self.config.default_model.clone()])
    }
}

fn is_tool_progress(text: &str) -> bool {
    text.contains("Working")
        || text.contains("Running:")
        || text.contains("Thinking")
        || text.contains("Tool activity")
}

/// Owns temp home/config + Agent wired to a mock Bot and [`FixtureLlm`].
struct HandleMessageHarness {
    _tmp: TempDir,
    agent: Arc<Agent>,
    bot: Bot,
    api: MockTelegramApi,
}

impl HandleMessageHarness {
    async fn new(stub_reply: &str) -> Self {
        // Env beats [general].home — keep ambient clear for deterministic paths.
        std::env::remove_var("RUSTFOX_HOME");

        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join(".rustfox");
        std::fs::create_dir_all(home.join("workspace")).unwrap();
        std::fs::create_dir_all(home.join("skills")).unwrap();
        std::fs::create_dir_all(home.join("agents")).unwrap();

        let cfg_path = tmp.path().join("config.toml");
        let toml = format!(
            r#"
            [[bots]]
            id = "main"
            bot_token = "{TOKEN}"
            allowed_user_ids = [{ALLOWED}]
            persona = "main"

            [openrouter]
            api_key = "fixture-unused"
            model = "fixture/stub"
            base_url = "http://fixture.invalid/v1"
            max_tokens = 256
            system_prompt = "You are a QA fixture bot."

            [general]
            home = "{home}"

            [agent]
            max_iterations = 2
            empty_response_retry_limit = 0
            parse_retry_limit = 0
            rate_limit_retry_limit = 0
            "#,
            TOKEN = TOKEN,
            ALLOWED = ALLOWED,
            home = home.display()
        );
        std::fs::write(&cfg_path, toml).unwrap();
        let config = Config::load(&cfg_path).expect("load fixture config");

        let api = MockTelegramApi::start().await;
        api.stub_bot_api().await;
        let bot = api.bot();
        let bot_arc = Arc::new(bot.clone());

        let registry = Arc::new(FixtureLlm::new(stub_reply).into_registry());
        let memory = MemoryStore::open_in_memory().expect("in-memory sqlite");
        let task_store = ScheduledTaskStore::new(memory.connection());
        let scheduler = Arc::new(Scheduler::new().await.expect("scheduler"));
        let (job_tx, _job_rx) =
            tokio::sync::mpsc::unbounded_channel::<rustfox::agent::ScheduledJobRequest>();
        let cancel_registry = Arc::new(CancelRegistry::new());
        let sender: Arc<dyn rustfox::platform::PlatformSender> =
            Arc::new(TelegramAdapter::new(bot.clone()));
        let tool_registry = ToolRegistry::new();
        let langsmith = Arc::new(LangSmithClient::new(None));
        let restart_pending = Arc::new(AtomicBool::new(false));
        let soul_updated = Arc::new(AtomicBool::new(false));

        let agent = Arc::new_cyclic(|weak: &Weak<Agent>| {
            Agent::new(
                config,
                registry,
                McpManager::new(),
                memory,
                SkillRegistry::new(),
                SkillRegistry::new(),
                task_store,
                scheduler,
                weak.clone(),
                job_tx,
                langsmith,
                cfg_path.clone(),
                cancel_registry,
                tool_registry,
                sender,
                bot_arc,
                Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
                restart_pending,
                soul_updated,
            )
        });

        // Prefer Silent UI so chat path uses Thinking placeholder + stream,
        // without tool-notifier edit chatter (no tools in this harness).
        agent
            .memory
            .remember(
                "settings",
                &format!("tool_ui_mode_{ALLOWED}"),
                "silent",
                None,
            )
            .await
            .ok();
        // Leave message_format at default `auto` so sendRichMessage goes through
        // rich_sender → bot.api_url() (wiremock). Do NOT force markdown.

        Self {
            _tmp: tmp,
            agent,
            bot,
            api,
        }
    }

    /// Tool-using turn. Does not set per-chat `tool_ui_mode` (default Minimal).
    /// `main_fully_silent` is only on bot id `main`; `researcher` stays off.
    async fn tool_turn(main_fully_silent: bool) -> Self {
        Self::tool_turn_cfg(main_fully_silent, 4, false).await
    }

    async fn tool_turn_cfg(main_fully_silent: bool, max_iterations: u32, only_tools: bool) -> Self {
        std::env::remove_var("RUSTFOX_HOME");
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join(".rustfox");
        std::fs::create_dir_all(home.join("workspace")).unwrap();
        std::fs::create_dir_all(home.join("skills")).unwrap();
        std::fs::create_dir_all(home.join("agents")).unwrap();

        let cfg_path = tmp.path().join("config.toml");
        let silent_line = if main_fully_silent {
            "fully_silent = true\n"
        } else {
            ""
        };
        let toml = format!(
            r#"
            [[bots]]
            id = "main"
            bot_token = "{TOKEN}"
            allowed_user_ids = [{ALLOWED}]
            persona = "main"
            {silent_line}
            [[bots]]
            id = "researcher"
            bot_token = "000000000:QA-INJECTOR-OTHER-BOT"
            allowed_user_ids = [{ALLOWED}]
            persona = "researcher"

            [openrouter]
            api_key = "fixture-unused"
            model = "fixture/stub"
            base_url = "http://fixture.invalid/v1"
            max_tokens = 256
            system_prompt = "You are a QA fixture bot."

            [general]
            home = "{home}"

            [agent]
            max_iterations = {max_iterations}
            empty_response_retry_limit = 0
            parse_retry_limit = 0
            rate_limit_retry_limit = 0
            "#,
            TOKEN = TOKEN,
            ALLOWED = ALLOWED,
            home = home.display(),
            silent_line = silent_line,
            max_iterations = max_iterations,
        );
        std::fs::write(&cfg_path, &toml).unwrap();
        let config = Config::load(&cfg_path).expect("load fixture config");

        let api = MockTelegramApi::start().await;
        api.stub_bot_api().await;
        let bot = api.bot();
        let bot_arc = Arc::new(bot.clone());

        let registry = Arc::new(if only_tools {
            ToolThenReply::tools_only().into_registry()
        } else {
            ToolThenReply::new(TOOL_TURN_REPLY).into_registry()
        });
        let memory = MemoryStore::open_in_memory().expect("in-memory sqlite");
        let task_store = ScheduledTaskStore::new(memory.connection());
        let scheduler = Arc::new(Scheduler::new().await.expect("scheduler"));
        let (job_tx, _job_rx) =
            tokio::sync::mpsc::unbounded_channel::<rustfox::agent::ScheduledJobRequest>();
        let cancel_registry = Arc::new(CancelRegistry::new());
        let sender: Arc<dyn rustfox::platform::PlatformSender> =
            Arc::new(TelegramAdapter::new(bot.clone()));
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(rustfox::command_tool::CommandTool::new(
            config.sandbox.allowed_directory.clone(),
            Arc::clone(&cancel_registry),
        )));
        let langsmith = Arc::new(LangSmithClient::new(None));
        let restart_pending = Arc::new(AtomicBool::new(false));
        let soul_updated = Arc::new(AtomicBool::new(false));

        let agent = Arc::new_cyclic(|weak: &Weak<Agent>| {
            Agent::new(
                config,
                registry,
                McpManager::new(),
                memory,
                SkillRegistry::new(),
                SkillRegistry::new(),
                task_store,
                scheduler,
                weak.clone(),
                job_tx,
                langsmith,
                cfg_path.clone(),
                cancel_registry,
                tool_registry,
                sender,
                bot_arc,
                Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
                restart_pending,
                soul_updated,
            )
        });

        Self {
            _tmp: tmp,
            agent,
            bot,
            api,
        }
    }
}

#[test]
fn fixture_text_message_accepted() {
    let upd = UpdateInjector::parse_update_file(fixture("text_message.json")).unwrap();
    assert_eq!(UpdateInjector::classify(&upd), InjectedKind::TextMessage);
    let inj = injector();
    assert_eq!(inj.route(&upd), HandlerRoute::Message);
    let incoming = inj.extract_incoming(&upd).unwrap();
    assert_eq!(incoming.bot_id, "main");
    assert_eq!(incoming.user_id, ALLOWED.to_string());
    assert_eq!(incoming.command, Some(("start".into(), "".into())));
    assert_eq!(incoming.platform, "telegram");
}

#[test]
fn fixture_text_message_rejected_off_allowlist() {
    let upd = UpdateInjector::parse_update_file(fixture("text_message_rejected.json")).unwrap();
    let inj = injector();
    assert_eq!(inj.route(&upd), HandlerRoute::RejectedAllowlist);
    assert!(matches!(
        inj.allowlist_decision(&upd),
        AllowlistDecision::Reject {
            user_id: Some(999_999)
        }
    ));
    assert!(inj.extract_incoming(&upd).is_none());
}

#[test]
fn fixture_photo_media_handler_shape() {
    let upd = UpdateInjector::parse_update_file(fixture("photo_caption.json")).unwrap();
    assert_eq!(
        UpdateInjector::classify(&upd),
        InjectedKind::Media {
            has_photo: true,
            has_document: false
        }
    );
    let incoming = injector().extract_incoming(&upd).unwrap();
    assert!(incoming.has_photo);
    assert!(!incoming.has_document);
    assert_eq!(incoming.text, "look at this image");
}

#[test]
fn fixture_document_media_handler_shape() {
    let upd = UpdateInjector::parse_update_file(fixture("document_pdf.json")).unwrap();
    assert_eq!(
        UpdateInjector::classify(&upd),
        InjectedKind::Media {
            has_photo: false,
            has_document: true
        }
    );
    let incoming = injector().extract_incoming(&upd).unwrap();
    assert!(incoming.has_document);
    assert_eq!(incoming.document_file_name.as_deref(), Some("report.pdf"));
    assert_eq!(incoming.text, "quarterly report");
}

#[test]
fn fixture_callback_model_accepted() {
    let upd = UpdateInjector::parse_update_file(fixture("callback_model.json")).unwrap();
    assert_eq!(UpdateInjector::classify(&upd), InjectedKind::CallbackQuery);
    let inj = injector();
    assert_eq!(inj.route(&upd), HandlerRoute::ModelCallback);
    let cb = inj.extract_callback(&upd).unwrap();
    assert!(!cb.is_loop_callback);
    assert_eq!(cb.data.as_deref(), Some("model:openai/gpt-4o-mini"));
}

#[test]
fn fixture_callback_loop_route() {
    let upd = UpdateInjector::parse_update_file(fixture("callback_loop.json")).unwrap();
    let inj = injector();
    assert_eq!(inj.route(&upd), HandlerRoute::LoopCallback);
    assert!(inj.extract_callback(&upd).unwrap().is_loop_callback);
}

#[test]
fn allowlist_isolation_across_bots() {
    let upd = UpdateInjector::parse_update_file(fixture("text_message.json")).unwrap();
    let main = UpdateInjector::new([ALLOWED]).with_bot_id("main");
    let researcher = UpdateInjector::new([222u64]).with_bot_id("researcher");
    assert_eq!(main.route(&upd), HandlerRoute::Message);
    assert_eq!(researcher.route(&upd), HandlerRoute::RejectedAllowlist);
}

#[test]
fn default_bot_id_when_empty() {
    let inj = UpdateInjector::new([1u64]).with_bot_id("  ");
    assert_eq!(inj.bot_id(), DEFAULT_BOT_ID);
}

#[test]
fn fixture_slash_and_chat_shapes() {
    for (name, expect_cmd, expect_text) in [
        ("slash_clear.json", Some(("clear", "")), "/clear"),
        ("slash_tools.json", Some(("tools", "")), "/tools"),
        ("slash_verbose.json", Some(("verbose", "")), "/verbose"),
        ("chat_hello.json", None, "hello from QA offline fixture"),
    ] {
        let upd = UpdateInjector::parse_update_file(fixture(name)).unwrap();
        let inj = injector();
        assert_eq!(inj.route(&upd), HandlerRoute::Message, "{name}");
        let incoming = inj.extract_incoming(&upd).unwrap();
        assert_eq!(incoming.text, expect_text, "{name}");
        match expect_cmd {
            Some((c, a)) => assert_eq!(incoming.command, Some((c.into(), a.into())), "{name}"),
            None => assert!(incoming.command.is_none(), "{name}"),
        }
    }
}

#[tokio::test]
async fn mock_bot_api_outbound_send_assert() {
    let api = MockTelegramApi::start().await;
    api.stub_send_message().await;
    let bot = api.bot();

    notify_startup(&bot, &[ALLOWED], "test-model", 0, 0, false).await;

    let bodies = api.received_send_message_bodies().await;
    assert!(
        !bodies.is_empty(),
        "expected at least one SendMessage to mock Bot API"
    );
    let chat_id = bodies[0]
        .get("chat_id")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
        .unwrap();
    assert_eq!(chat_id, ALLOWED as i64);
    let text = bodies[0].get("text").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        text.contains("RustFox is online"),
        "unexpected startup text: {text}"
    );

    notify_shutdown(&bot, &[ALLOWED]).await;
    let bodies = api.received_send_message_bodies().await;
    assert!(
        bodies.iter().any(|b| {
            b.get("text")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t.contains("going offline"))
        }),
        "expected shutdown sendMessage; got {bodies:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_slash_start() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("text_message.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive /start");
    assert_eq!(route, HandlerRoute::Message);

    let texts = h.api.outbound_texts().await;
    assert!(
        texts
            .iter()
            .any(|t| t.contains("AI assistant") || t.contains("Commands")),
        "expected /start help outbound; got {texts:?}"
    );
    // Slash commands must not hit the fixture LLM.
    assert!(
        texts.iter().all(|t| !t.contains(STUB_REPLY)),
        "stub LLM must not run for /start; got {texts:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_slash_clear() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("slash_clear.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive /clear");
    assert_eq!(route, HandlerRoute::Message);
    let texts = h.api.outbound_texts().await;
    assert!(
        texts
            .iter()
            .any(|t| t.contains("archived") || t.contains("Conversation")),
        "expected /clear confirm; got {texts:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_slash_tools() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("slash_tools.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive /tools");
    assert_eq!(route, HandlerRoute::Message);
    let texts = h.api.outbound_texts().await;
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Built-in tools") || t.contains("tools")),
        "expected /tools listing; got {texts:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_chat_with_fixture_llm() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("chat_hello.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive chat_hello");
    assert_eq!(route, HandlerRoute::Message);

    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|t| t.contains(STUB_REPLY)),
        "expected fixture LLM reply in outbound SendMessage/sendRichMessage; got {texts:?}"
    );
}

/// Default `message_format=auto` must hit wiremock via injectable Bot API base
/// (rich_sender uses `bot.api_url()`), never the hard-coded api.telegram.org.
#[tokio::test]
async fn drive_handle_message_chat_auto_hits_wiremock_rich() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    // Explicitly confirm harness left format at auto (no memory override).
    let fmt = h
        .agent
        .memory
        .recall("settings", &format!("message_format_{ALLOWED}"))
        .await
        .unwrap();
    assert!(
        fmt.is_none(),
        "harness must leave message_format unset (default auto); got {fmt:?}"
    );

    let mock_base = h.api.uri();
    assert_eq!(
        h.bot.api_url().as_str().trim_end_matches('/'),
        mock_base.trim_end_matches('/'),
        "Bot must be pointed at wiremock"
    );

    let upd = UpdateInjector::parse_update_file(fixture("chat_hello.json")).unwrap();
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive chat_hello auto");

    let paths = h.api.received_requests_paths().await;
    assert!(
        paths.iter().any(|p| p.contains("sendRichMessage")),
        "auto format must call sendRichMessage on wiremock; paths={paths:?}"
    );
    // Requests recorded by MockServer are definitionally against the mock URI
    // (not api.telegram.org). Rich path must carry the stub reply.
    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|t| t.contains(STUB_REPLY)),
        "fixture reply must arrive via wiremock rich path; got {texts:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_rejects_off_allowlist_without_bot_calls() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("text_message_rejected.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive rejected");
    assert_eq!(route, HandlerRoute::RejectedAllowlist);
    let texts = h.api.outbound_texts().await;
    assert!(
        texts.is_empty(),
        "rejected update must not call SendMessage; got {texts:?}"
    );
}

#[tokio::test]
async fn drive_handle_message_skips_callback_without_bot_calls() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("callback_model.json")).unwrap();
    let route = injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive callback");
    assert_eq!(route, HandlerRoute::ModelCallback);
    let texts = h.api.outbound_texts().await;
    assert!(
        texts.is_empty(),
        "drive_handle_message must not run callbacks; got {texts:?}"
    );
}

async fn drive_tool_turn(h: &HandleMessageHarness, bot_id: &str) {
    let upd = UpdateInjector::parse_update_file(fixture("chat_hello.json")).unwrap();
    UpdateInjector::new([ALLOWED])
        .with_bot_id(bot_id)
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .unwrap_or_else(|e| panic!("drive {bot_id}: {e:#}"));
}

#[tokio::test]
async fn default_cleans_completed_tool_messages_and_may_show_progress() {
    let h = HandleMessageHarness::tool_turn(false).await;
    assert!(!h.agent.config.bot_fully_silent("main"));
    assert!(!h.agent.config.bot_fully_silent("researcher"));
    drive_tool_turn(&h, "main").await;

    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    let progress: Vec<(i32, String)> = sent
        .iter()
        .filter(|(_, text)| is_tool_progress(text))
        .cloned()
        .collect();
    assert!(
        progress
            .iter()
            .any(|(_, text)| text.contains("Working") || text.contains("Running:")),
        "default may still emit in-progress Working/Running; sent={sent:?}"
    );
    for (id, text) in &progress {
        assert!(
            deleted.contains(id),
            "completed tool message {id} ({text}) must be cleaned; deleted={deleted:?}"
        );
    }
    assert!(
        sent.iter().any(|(_, text)| text.contains(TOOL_TURN_REPLY)),
        "final assistant text must still be sent; sent={sent:?}"
    );
}

#[tokio::test]
async fn fully_silent_speaking_bot_emits_no_tool_messages() {
    let h = HandleMessageHarness::tool_turn(true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    assert!(!h.agent.config.bot_fully_silent("researcher"));
    drive_tool_turn(&h, "main").await;

    let sent = h.api.sent_snapshot();
    let progress: Vec<&str> = sent
        .iter()
        .filter(|(_, text)| is_tool_progress(text))
        .map(|(_, text)| text.as_str())
        .collect();
    assert!(
        progress.is_empty(),
        "fully silent speaking bot must emit no Working/Running/tool bubble; got {progress:?} all={sent:?}"
    );
    assert!(
        sent.iter().any(|(_, text)| text.contains(TOOL_TURN_REPLY)),
        "final assistant text must still be sent; sent={sent:?}"
    );
}

#[tokio::test]
async fn second_bot_without_fully_silent_does_not_inherit_it() {
    let h = HandleMessageHarness::tool_turn(true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    assert!(
        !h.agent.config.bot_fully_silent("researcher"),
        "researcher must not inherit main's fully_silent"
    );
    drive_tool_turn(&h, "researcher").await;

    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    let progress: Vec<(i32, String)> = sent
        .iter()
        .filter(|(_, text)| is_tool_progress(text))
        .cloned()
        .collect();
    assert!(
        progress
            .iter()
            .any(|(_, text)| text.contains("Working") || text.contains("Running:")),
        "bot without the option must still emit in-progress tool UI; sent={sent:?}"
    );
    for (id, text) in &progress {
        assert!(
            deleted.contains(id),
            "researcher completed tool message {id} ({text}) must still be cleaned"
        );
    }
    assert!(
        sent.iter().any(|(_, text)| text.contains(TOOL_TURN_REPLY)),
        "final assistant text must still be sent; sent={sent:?}"
    );
}

#[tokio::test]
async fn fully_silent_max_iterations_still_sends_the_sentence() {
    let h = HandleMessageHarness::tool_turn_cfg(true, 1, true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    drive_tool_turn(&h, "main").await;

    let sent = h.api.sent_snapshot();
    assert!(
        sent.iter()
            .any(|(_, text)| text.contains("maximum number of tool call iterations")),
        "max-iterations sentence must be sent even when nothing was streamed; sent={sent:?}"
    );
    let progress: Vec<&str> = sent
        .iter()
        .filter(|(_, text)| is_tool_progress(text))
        .map(|(_, text)| text.as_str())
        .collect();
    assert!(
        progress.is_empty(),
        "fully_silent must still hide tool and Thinking bubbles; got {progress:?}"
    );
}

#[tokio::test]
async fn max_iterations_sentence_is_saved_to_history() {
    let h = HandleMessageHarness::tool_turn_cfg(false, 1, true).await;
    drive_tool_turn(&h, "main").await;

    let saved = h.agent.memory.recent_messages(20).await.unwrap();
    assert!(
        saved.iter().any(|m| m.role == "assistant"
            && m.content.as_ref().is_some_and(|c| c
                .as_text()
                .contains("maximum number of tool call iterations"))),
        "the max-iterations sentence sent to the chat must be in history; saved={saved:?}"
    );
}
