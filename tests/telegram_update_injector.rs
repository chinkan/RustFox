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

use rustfox::agent::{session_key, Agent};
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

/// ADR-0020: chat turns spawn under TurnGate; wait until the key is idle.
async fn wait_turn_idle(agent: &Agent, bot_id: &str, user_id: u64) {
    let key = session_key(bot_id, &user_id.to_string());
    for _ in 0..500 {
        if !agent.turn_gate.is_active(&key).await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("turn gate still active after 5s");
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

    /// Bot API `getFile` + file bytes for a voice-note fixture (`voice/note.wav`).
    async fn stub_voice_wav(&self) {
        Mock::given(method("POST"))
            .and(path_regex(r"(?i)^/bot[^/]+/getfile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": {
                    "file_id": "voice-file-1",
                    "file_unique_id": "voice-uniq-1",
                    "file_size": 11,
                    "file_path": "voice/note.wav"
                }
            })))
            .expect(0..)
            .mount(&self.server)
            .await;

        // `file_path` is one URL segment, so `voice/note.wav` is `voice%2Fnote.wav`.
        Mock::given(method("GET"))
            .and(path_regex(r"(?i)^/file/bot.+note\.wav$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFFfake-wav"))
            .expect(0..)
            .mount(&self.server)
            .await;
    }

    async fn stub_send_voice(&self) {
        let counter = Arc::clone(&self.next_msg_id);
        Mock::given(method("POST"))
            .and(path_regex(r"(?i)^/bot[^/]+/sendvoice$"))
            .respond_with(move |_req: &Request| {
                let mid = counter.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(ok_message_result(ALLOWED as i64, mid, ""))
            })
            .expect(0..)
            .mount(&self.server)
            .await;
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
                extra_headers: vec![],
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
                reasoning_content: None,
            }
        } else {
            rustfox::llm::ChatMessage {
                role: "assistant".into(),
                content: Some(rustfox::llm::MessageContent::from_text(self.reply.clone())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
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

/// Tool feedback that `/verbose` Silent and `fully_silent` must never post,
/// including a completed bubble and a briefly-then-delete placeholder.
fn is_forbidden_tool_ui(text: &str) -> bool {
    is_tool_progress(text) || text.contains("Completed") || text.contains("Final answer sent below")
}

async fn remember_tool_ui_mode(agent: &Agent, mode: &str) {
    agent
        .memory
        .remember("settings", &format!("tool_ui_mode_{ALLOWED}"), mode, None)
        .await
        .expect("store tool ui mode");
}

fn assert_no_tool_ui(sent: &[(i32, String)], deleted: &[i32]) {
    let posted: Vec<(i32, String)> = sent
        .iter()
        .filter(|(_, text)| is_forbidden_tool_ui(text))
        .cloned()
        .collect();
    assert!(
        posted.is_empty(),
        "must not send Thinking / Working / Running / completed tool UI, including post-then-delete; posted={posted:?} deleted={deleted:?} sent={sent:?}"
    );
}

fn assert_final_text_posted(sent: &[(i32, String)], deleted: &[i32], needle: &str) {
    let copies: Vec<i32> = sent
        .iter()
        .filter(|(_, text)| text.contains(needle))
        .map(|(id, _)| *id)
        .collect();
    assert!(
        !copies.is_empty(),
        "final assistant text must be sent; sent={sent:?}"
    );
    assert!(
        copies.iter().any(|id| !deleted.contains(id)),
        "a final assistant copy must remain posted; copies={copies:?} deleted={deleted:?}"
    );
}

async fn assert_typing_chat_action_allowed(api: &MockTelegramApi) {
    let requests = api.server.received_requests().await.unwrap_or_default();
    let mut saw_typing = false;
    for req in &requests {
        let path = req.url.path().to_ascii_lowercase();
        if !path.contains("chataction") {
            continue;
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
        let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(
            action, "typing",
            "send_chat_action may stay only as Typing; path={path} body={body}"
        );
        saw_typing = true;
    }
    let paths: Vec<String> = requests.iter().map(|r| r.url.path().to_string()).collect();
    assert!(
        saw_typing,
        "Typing chat action is still sent and is not tool UI; paths={paths:?}"
    );
}

/// Owns temp home/config + Agent wired to a mock Bot and [`FixtureLlm`].
struct HandleMessageHarness {
    _tmp: TempDir,
    agent: Arc<Agent>,
    bot: Bot,
    api: MockTelegramApi,
    /// Stripped model ids seen by FixtureLlm::chat_completion (ADR-0036).
    models_seen: Arc<Mutex<Vec<String>>>,
    /// Message lists passed to FixtureLlm::chat_completion, in call order.
    messages_seen: Arc<Mutex<Vec<Vec<rustfox::llm::ChatMessage>>>>,
}

impl HandleMessageHarness {
    async fn new(stub_reply: &str) -> Self {
        Self::with_config_suffix(stub_reply, "").await
    }

    /// `suffix` is extra TOML appended after the fixture agent section
    /// (for example a `[voice]` block).
    async fn with_config_suffix(stub_reply: &str, suffix: &str) -> Self {
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
        let mut toml = toml;
        toml.push_str(suffix);
        std::fs::write(&cfg_path, toml).unwrap();
        let config = Config::load(&cfg_path).expect("load fixture config");

        let api = MockTelegramApi::start().await;
        api.stub_bot_api().await;
        let bot = api.bot();
        let bot_arc = Arc::new(bot.clone());

        let models_seen = Arc::new(Mutex::new(Vec::new()));
        let llm = FixtureLlm::with_models_seen(stub_reply, Arc::clone(&models_seen));
        let messages_seen = llm.messages_seen();
        let registry = Arc::new(llm.into_registry());
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

        // Prefer Silent UI so these no-tool chat turns do not spawn the
        // tool notifier. Silent must not post a Thinking placeholder either.
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
            models_seen,
            messages_seen,
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
            models_seen: Arc::new(Mutex::new(Vec::new())),
            messages_seen: Arc::new(Mutex::new(Vec::new())),
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
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|t| t.contains(STUB_REPLY)),
        "expected fixture LLM reply in outbound SendMessage/sendRichMessage; got {texts:?}"
    );
}

/// ADR-0036: after `set_model`, next chat without restart uses live Active
/// (not boot-frozen `registry.default_qualified_model`) when no sticky model.
#[tokio::test]
async fn after_set_model_next_chat_uses_live_active_not_boot() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let boot = h.agent.registry.default_qualified_model();
    assert_eq!(boot, "fixture/stub", "harness boot Active");

    // Live switch (same in-memory path as set_model after validation).
    h.agent
        .set_model_live("fixture/live-after-switch")
        .await
        .expect("set_model_live");
    let active = h.agent.current_model.read().await.clone();
    assert_eq!(active, "fixture/live-after-switch");
    assert_ne!(active, boot);

    let upd = UpdateInjector::parse_update_file(fixture("chat_hello.json")).unwrap();
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive chat after set_model");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let seen = h.models_seen.lock().unwrap().clone();
    assert!(
        !seen.is_empty(),
        "expected at least one llm_call; got {seen:?}"
    );
    // Provider receives stripped actual_model (`live-after-switch`), not boot `stub`.
    assert!(
        seen.iter().any(|m| m == "live-after-switch"),
        "next chat must use live Active, not boot registry; seen={seen:?} boot={boot}"
    );
    assert!(
        seen.iter().all(|m| m != "stub"),
        "must not call boot default model after set_model; seen={seen:?}"
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
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

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
    wait_turn_idle(&h.agent, bot_id, ALLOWED).await;
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
    assert!(
        sent.iter().all(|(_, text)| !text.contains("Thinking")),
        "Minimal must not post a Thinking placeholder; sent={sent:?}"
    );
}

#[tokio::test]
async fn verbose_tool_turn_still_shows_tool_ui_and_final_reply() {
    let h = HandleMessageHarness::tool_turn(false).await;
    assert!(!h.agent.config.bot_fully_silent("main"));
    remember_tool_ui_mode(&h.agent, "verbose").await;
    drive_tool_turn(&h, "main").await;

    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert!(
        sent.iter()
            .any(|(_, text)| text.contains("Running:") || text.contains("Working")),
        "Verbose must still show Running / Working tool UI; sent={sent:?}"
    );
    let running: Vec<i32> = sent
        .iter()
        .filter(|(_, text)| text.contains("Running:"))
        .map(|(id, _)| *id)
        .collect();
    assert!(
        !running.is_empty(),
        "Verbose command UI still posts Running:; sent={sent:?}"
    );
    for id in &running {
        assert!(
            !deleted.contains(id),
            "Verbose keeps the Running bubble and edits it; id={id} deleted={deleted:?}"
        );
    }
    assert_final_text_posted(&sent, &deleted, TOOL_TURN_REPLY);
}

#[tokio::test]
async fn per_chat_silent_sends_no_tool_ui_and_keeps_final_reply() {
    let h = HandleMessageHarness::tool_turn(false).await;
    assert!(!h.agent.config.bot_fully_silent("main"));
    remember_tool_ui_mode(&h.agent, "silent").await;
    drive_tool_turn(&h, "main").await;

    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert_no_tool_ui(&sent, &deleted);
    assert_final_text_posted(&sent, &deleted, TOOL_TURN_REPLY);
    assert_typing_chat_action_allowed(&h.api).await;
}

#[tokio::test]
async fn fully_silent_suppresses_verbose_stored_mode() {
    let h = HandleMessageHarness::tool_turn(true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    remember_tool_ui_mode(&h.agent, "verbose").await;
    drive_tool_turn(&h, "main").await;

    let stored = h
        .agent
        .memory
        .recall("settings", &format!("tool_ui_mode_{ALLOWED}"))
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("verbose"));
    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert_no_tool_ui(&sent, &deleted);
    assert_final_text_posted(&sent, &deleted, TOOL_TURN_REPLY);
    assert_typing_chat_action_allowed(&h.api).await;
}

#[tokio::test]
async fn fully_silent_suppresses_minimal_stored_mode() {
    let h = HandleMessageHarness::tool_turn(true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    remember_tool_ui_mode(&h.agent, "minimal").await;
    drive_tool_turn(&h, "main").await;

    let stored = h
        .agent
        .memory
        .recall("settings", &format!("tool_ui_mode_{ALLOWED}"))
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("minimal"));
    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert_no_tool_ui(&sent, &deleted);
    assert_final_text_posted(&sent, &deleted, TOOL_TURN_REPLY);
}

#[tokio::test]
async fn fully_silent_verbose_command_still_confirms_stored_mode() {
    let h = HandleMessageHarness::tool_turn(true).await;
    assert!(h.agent.config.bot_fully_silent("main"));
    injector()
        .drive_handle_message(
            &plain_text_message("/verbose"),
            h.bot.clone(),
            Arc::clone(&h.agent),
        )
        .await
        .expect("/verbose");

    let stored = h
        .agent
        .memory
        .recall("settings", &format!("tool_ui_mode_{ALLOWED}"))
        .await
        .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some("verbose"),
        "fully_silent must still cycle the stored per-chat mode"
    );
    let sent = h.api.sent_snapshot();
    assert!(
        sent.iter()
            .any(|(_, text)| text.contains("Tool UI verbose")),
        "/verbose confirmation is a command reply, not tool feedback; sent={sent:?}"
    );

    drive_tool_turn(&h, "main").await;
    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert_no_tool_ui(&sent, &deleted);
    assert_final_text_posted(&sent, &deleted, TOOL_TURN_REPLY);
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

fn voice_message(caption: Option<&str>) -> teloxide::types::Update {
    let mut message = json!({
        "message_id": 50,
        "date": 1700000000,
        "from": { "id": ALLOWED, "is_bot": false, "first_name": "QA" },
        "chat": { "id": ALLOWED, "type": "private", "first_name": "QA" },
        "voice": {
            "file_id": "voice-file-1",
            "file_unique_id": "voice-uniq-1",
            "duration": 2,
            "mime_type": "audio/ogg"
        }
    });
    if let Some(caption) = caption {
        message["caption"] = json!(caption);
    }
    UpdateInjector::parse_update_value(&json!({
        "update_id": 9001,
        "message": message
    }))
    .unwrap()
}

fn audio_message() -> teloxide::types::Update {
    UpdateInjector::parse_update_value(&json!({
        "update_id": 9002,
        "message": {
            "message_id": 51,
            "date": 1700000000,
            "from": { "id": ALLOWED, "is_bot": false, "first_name": "QA" },
            "chat": { "id": ALLOWED, "type": "private", "first_name": "QA" },
            "audio": {
                "file_id": "audio-file-1",
                "file_unique_id": "audio-uniq-1",
                "duration": 3
            }
        }
    }))
    .unwrap()
}

fn user_texts(messages: &[rustfox::llm::ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_ref().map(|content| content.as_text()))
        .collect()
}

/// ADR-0058: a caption-less voice note must not return before any reply.
#[tokio::test]
async fn voice_only_download_failure_is_not_silently_dropped() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    assert!(
        !h.agent.config.voice.openrouter_stt,
        "OpenRouter STT must default off"
    );
    assert!(!h.agent.config.voice.tts, "TTS must default off");
    assert_eq!(h.agent.config.voice.engine, "sensevoice");

    let upd = voice_message(None);
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("voice note must not error the dispatcher");

    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|text| {
            text.contains("voice note")
                && text.contains("type your message")
                && (text.contains("couldn't download")
                    || text.contains("cloud transcription is off"))
        }),
        "voice-only failure must tell the user; got {texts:?}"
    );
    assert!(
        texts.iter().all(|text| !text.contains(STUB_REPLY)),
        "failed voice note must not enter the LLM; got {texts:?}"
    );
    let paths = h.api.received_requests_paths().await;
    assert!(
        paths
            .iter()
            .all(|path| !path.to_ascii_lowercase().contains("sendvoice")),
        "TTS is off; paths={paths:?}"
    );
}

/// ADR-0058: typed caption still enters the turn when the audio cannot be read.
#[tokio::test]
async fn voice_download_failure_still_feeds_the_caption() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = voice_message(Some("remember this"));
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("captioned voice");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|text| text.contains("voice note")),
        "user hears that the audio failed; got {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains(STUB_REPLY)),
        "caption must still reach the model; got {texts:?}"
    );
    let users = user_texts(&h.agent.memory.recent_messages(20).await.unwrap());
    assert!(
        users.iter().any(|text| text == "remember this"),
        "caption is the turn text; got {users:?}"
    );
    assert!(
        users.iter().all(|text| !text.starts_with("[Voice note]:")),
        "no transcript was invented; got {users:?}"
    );
    let seen = h.messages_seen.lock().expect("messages").clone();
    let system = seen
        .last()
        .and_then(|call| call.iter().find(|message| message.role == "system"))
        .and_then(|message| message.content.as_ref().map(|content| content.as_text()))
        .unwrap_or_default();
    assert!(
        !system.contains(rustfox::asr::VOICE_MODE_PROMPT_YUE) && !system.contains("# Voice reply"),
        "a failed note kept as typed caption is not voice-origin; system={system}"
    );
}

/// General Audio stays out of this slice (ADR-0058 Voice notes only).
#[tokio::test]
async fn general_audio_is_not_a_voice_note() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = audio_message();
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("audio message");
    let texts = h.api.outbound_texts().await;
    assert!(
        texts.is_empty(),
        "audio-only messages are not voice notes; got {texts:?}"
    );
}

#[cfg(unix)]
fn write_exec(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Transcript prefix reaches TurnGate, and enabled TTS sends voice plus text.
#[cfg(unix)]
#[tokio::test]
async fn voice_transcript_is_prefixed_and_tts_dual_sends() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
    std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
    let stt = write_exec(dir.path(), "stt.sh", "#!/bin/sh\nprintf '%s\\n' '早晨'\n");
    let tts = write_exec(
        dir.path(),
        "tts.sh",
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'OggS' > \"$out\"\n",
    );
    let suffix = format!(
        "\n[voice]\nengine = \"sensevoice\"\nmodel_dir = \"{model}\"\nstt_bin = \"{stt}\"\ntts = true\ntts_bin = \"{tts}\"\nopenrouter_stt = false\nlanguage = \"yue\"\n",
        model = model.display(),
        stt = stt.display(),
        tts = tts.display(),
    );
    let h = HandleMessageHarness::with_config_suffix(STUB_REPLY, &suffix).await;
    assert!(!h.agent.config.voice.openrouter_stt);
    assert!(h.agent.config.voice.tts);
    h.api.stub_voice_wav().await;
    h.api.stub_send_voice().await;

    let upd = voice_message(None);
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("voice transcript");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let users = user_texts(&h.agent.memory.recent_messages(20).await.unwrap());
    let texts = h.api.outbound_texts().await;
    let paths = h.api.received_requests_paths().await;
    assert!(
        users.iter().any(|text| text == "[Voice note]:\n早晨"),
        "turn text must be the voice prefix plus transcript; users={users:?} texts={texts:?} paths={paths:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains(STUB_REPLY)),
        "text copy of the reply must be sent; got {texts:?}"
    );
    let voice_at = paths
        .iter()
        .position(|path| path.to_ascii_lowercase().contains("sendvoice"));
    let text_at = paths.iter().position(|path| {
        let lower = path.to_ascii_lowercase();
        lower.contains("sendmessage") || lower.contains("sendrichmessage")
    });
    assert!(
        voice_at.is_some(),
        "enabled TTS must sendVoice; paths={paths:?}"
    );
    assert!(
        text_at.unwrap() < voice_at.unwrap(),
        "text is sent before the voice copy; paths={paths:?}"
    );
}

/// Per-chat Silent (`fully_silent` off) still posts the assistant text and
/// `sendVoice`. Typing may stay. No Thinking / tool bubble is posted.
#[cfg(unix)]
#[tokio::test]
async fn per_chat_silent_still_sends_text_and_voice() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
    std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
    let stt = write_exec(dir.path(), "stt.sh", "#!/bin/sh\nprintf '%s\\n' '早晨'\n");
    let tts = write_exec(
        dir.path(),
        "tts.sh",
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'OggS' > \"$out\"\n",
    );
    let suffix = format!(
        "\n[voice]\nengine = \"sensevoice\"\nmodel_dir = \"{model}\"\nstt_bin = \"{stt}\"\ntts = true\ntts_bin = \"{tts}\"\nopenrouter_stt = false\nlanguage = \"yue\"\n",
        model = model.display(),
        stt = stt.display(),
        tts = tts.display(),
    );
    let h = HandleMessageHarness::with_config_suffix(STUB_REPLY, &suffix).await;
    assert!(!h.agent.config.bot_fully_silent("main"));
    let stored = h
        .agent
        .memory
        .recall("settings", &format!("tool_ui_mode_{ALLOWED}"))
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("silent"));
    h.api.stub_voice_wav().await;
    h.api.stub_send_voice().await;

    injector()
        .drive_handle_message(&voice_message(None), h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("voice note");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let sent = h.api.sent_snapshot();
    let deleted = h.api.deleted_snapshot();
    assert_no_tool_ui(&sent, &deleted);
    assert_final_text_posted(&sent, &deleted, STUB_REPLY);
    let paths = h.api.received_requests_paths().await;
    assert!(
        paths
            .iter()
            .any(|path| path.to_ascii_lowercase().contains("sendvoice")),
        "Silent must still sendVoice; paths={paths:?}"
    );
    assert_typing_chat_action_allowed(&h.api).await;
}

fn plain_text_message(text: &str) -> teloxide::types::Update {
    UpdateInjector::parse_update_value(&json!({
        "update_id": 9100,
        "message": {
            "message_id": 60,
            "date": 1700000100,
            "from": { "id": ALLOWED, "is_bot": false, "first_name": "QA" },
            "chat": { "id": ALLOWED, "type": "private", "first_name": "QA" },
            "text": text
        }
    }))
    .unwrap()
}

fn voice_block_requests_shell_speech(system: &str, user: &str) -> bool {
    let mode = system.split("# Voice reply").nth(1).unwrap_or("");
    let blob = format!("{user}\n{mode}").to_ascii_lowercase();
    [
        "execute_command",
        "sherpa-onnx",
        "whisper",
        "ffmpeg",
        ".wav",
        ".ogg",
    ]
    .iter()
    .any(|cue| blob.contains(cue))
}

fn last_system_and_users(seen: &[Vec<rustfox::llm::ChatMessage>]) -> (String, Vec<String>) {
    let call = seen.last().expect("llm was called");
    let system = call
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_ref().map(|content| content.as_text()))
        .unwrap_or_default();
    (system, user_texts(call))
}

/// Voice note → code STT → voice-mode prompt → short text → code TTS → sendVoice.
/// The prompt must not steer the model into shell STT/TTS.
#[cfg(unix)]
#[tokio::test]
async fn voice_origin_injects_voice_mode_and_speaks_without_shell_stt() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
    std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
    let stt = write_exec(dir.path(), "stt.sh", "#!/bin/sh\nprintf '%s\\n' '早晨'\n");
    let tts = write_exec(
        dir.path(),
        "tts.sh",
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'OggS' > \"$out\"\n",
    );
    let suffix = format!(
        "\n[voice]\nengine = \"sensevoice\"\nmodel_dir = \"{model}\"\nstt_bin = \"{stt}\"\ntts = true\ntts_bin = \"{tts}\"\nopenrouter_stt = false\nlanguage = \"yue\"\n",
        model = model.display(),
        stt = stt.display(),
        tts = tts.display(),
    );
    let h = HandleMessageHarness::with_config_suffix("短答", &suffix).await;
    h.api.stub_voice_wav().await;
    h.api.stub_send_voice().await;

    injector()
        .drive_handle_message(&voice_message(None), h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("voice note");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let seen = h.messages_seen.lock().expect("messages").clone();
    let (system, users) = last_system_and_users(&seen);
    let user = users
        .iter()
        .find(|text| text.contains("[Voice note]:"))
        .expect("transcript must reach the model");
    assert_eq!(user, "[Voice note]:\n早晨");
    assert!(
        system.contains(rustfox::asr::VOICE_MODE_PROMPT_YUE),
        "empty soul stays Cantonese; system tail={}",
        system.chars().rev().take(80).collect::<String>()
    );
    assert!(
        !voice_block_requests_shell_speech(&system, user),
        "normal transcript must not steer shell STT/TTS"
    );
    assert!(
        !user.contains("note.wav") && !user.contains("sherpa"),
        "the model sees text, not the audio file; user={user}"
    );

    let texts = h.api.outbound_texts().await;
    let paths = h.api.received_requests_paths().await;
    assert!(
        texts.iter().any(|text| text.contains("短答")),
        "text copy is still sent; got {texts:?}"
    );
    let voice_at = paths
        .iter()
        .position(|path| path.to_ascii_lowercase().contains("sendvoice"));
    let text_at = paths.iter().position(|path| {
        let lower = path.to_ascii_lowercase();
        lower.contains("sendmessage") || lower.contains("sendrichmessage")
    });
    assert!(
        voice_at.is_some(),
        "code TTS must sendVoice; paths={paths:?}"
    );
    assert!(
        text_at.unwrap() < voice_at.unwrap(),
        "text is sent before the voice copy; paths={paths:?}"
    );
    let voice_sends = paths
        .iter()
        .filter(|path| path.to_ascii_lowercase().contains("sendvoice"))
        .count();

    injector()
        .drive_handle_message(
            &plain_text_message("and typed"),
            h.bot.clone(),
            Arc::clone(&h.agent),
        )
        .await
        .expect("typed follow-up");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let seen = h.messages_seen.lock().expect("messages").clone();
    let (system, users) = last_system_and_users(&seen);
    assert!(
        users.iter().any(|text| text == "and typed"),
        "typed follow-up is unchanged; users={users:?}"
    );
    assert!(
        !system.contains(rustfox::asr::VOICE_MODE_PROMPT_YUE) && !system.contains("# Voice reply"),
        "voice-mode prompt is only for the voice-origin turn"
    );
    let paths = h.api.received_requests_paths().await;
    let voice_sends_after = paths
        .iter()
        .filter(|path| path.to_ascii_lowercase().contains("sendvoice"))
        .count();
    assert_eq!(
        voice_sends, voice_sends_after,
        "typed follow-up must not sendVoice; paths={paths:?}"
    );
}

/// Typed chat keeps its text reply and does not auto-sendVoice when TTS is on.
#[cfg(unix)]
#[tokio::test]
async fn typed_chat_does_not_send_voice_when_tts_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
    std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
    let stt = write_exec(dir.path(), "stt.sh", "#!/bin/sh\nprintf '%s\\n' '早晨'\n");
    let tts = write_exec(
        dir.path(),
        "tts.sh",
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'OggS' > \"$out\"\n",
    );
    let suffix = format!(
        "\n[voice]\nengine = \"sensevoice\"\nmodel_dir = \"{model}\"\nstt_bin = \"{stt}\"\ntts = true\ntts_bin = \"{tts}\"\nopenrouter_stt = false\nlanguage = \"yue\"\n",
        model = model.display(),
        stt = stt.display(),
        tts = tts.display(),
    );
    let h = HandleMessageHarness::with_config_suffix(STUB_REPLY, &suffix).await;
    h.api.stub_send_voice().await;

    injector()
        .drive_handle_message(
            &plain_text_message("hello there"),
            h.bot.clone(),
            Arc::clone(&h.agent),
        )
        .await
        .expect("typed chat");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let seen = h.messages_seen.lock().expect("messages").clone();
    let (system, users) = last_system_and_users(&seen);
    assert!(
        users.iter().any(|text| text == "hello there"),
        "typed text is unchanged; users={users:?}"
    );
    assert!(
        !system.contains(rustfox::asr::VOICE_MODE_PROMPT_YUE)
            && !system.contains(rustfox::asr::VOICE_MODE_PROMPT_EN)
            && !system.contains("# Voice reply"),
        "typed chat must not get the voice-mode prompt"
    );
    let texts = h.api.outbound_texts().await;
    assert!(
        texts.iter().any(|text| text.contains(STUB_REPLY)),
        "typed reply text is still sent; got {texts:?}"
    );
    let paths = h.api.received_requests_paths().await;
    assert!(
        paths
            .iter()
            .all(|path| !path.to_ascii_lowercase().contains("sendvoice")),
        "tts=true must not sendVoice for typed chat; paths={paths:?}"
    );
}

/// Latin-only SOUL.md adapts the voice-mode sentence. CJK is covered by the
/// empty-soul default above.
#[cfg(unix)]
#[tokio::test]
async fn voice_mode_follows_latin_only_soul() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
    std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
    let stt = write_exec(
        dir.path(),
        "stt.sh",
        "#!/bin/sh\nprintf '%s\\n' 'morning'\n",
    );
    let tts = write_exec(
        dir.path(),
        "tts.sh",
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'OggS' > \"$out\"\n",
    );
    let suffix = format!(
        "\n[voice]\nengine = \"sensevoice\"\nmodel_dir = \"{model}\"\nstt_bin = \"{stt}\"\ntts = true\ntts_bin = \"{tts}\"\nopenrouter_stt = false\nlanguage = \"yue\"\n",
        model = model.display(),
        stt = stt.display(),
        tts = tts.display(),
    );
    let h = HandleMessageHarness::with_config_suffix("Short answer.", &suffix).await;
    let home = h.agent.config.resolved_home.clone().expect("fixture home");
    std::fs::write(home.join("SOUL.md"), "Be brief and kind.\n").unwrap();
    h.api.stub_voice_wav().await;
    h.api.stub_send_voice().await;

    injector()
        .drive_handle_message(&voice_message(None), h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("voice note");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let seen = h.messages_seen.lock().expect("messages").clone();
    let (system, users) = last_system_and_users(&seen);
    assert!(
        users.iter().any(|text| text == "[Voice note]:\nmorning"),
        "code STT still supplies the transcript; users={users:?}"
    );
    assert!(
        system.contains(rustfox::asr::VOICE_MODE_PROMPT_EN),
        "latin-only soul adapts the voice-mode sentence"
    );
    assert!(
        !system.contains(rustfox::asr::VOICE_MODE_PROMPT_YUE),
        "english soul must not also keep the Cantonese sentence"
    );
    let user = users
        .iter()
        .find(|text| text.contains("[Voice note]:"))
        .unwrap();
    assert!(!voice_block_requests_shell_speech(&system, user));
}

fn reply_to_bot_update(
    update_id: u32,
    message_id: i32,
    user_text: &str,
    replied_id: i32,
    quote: Option<&str>,
) -> String {
    let mut message = json!({
        "message_id": message_id,
        "date": 1700000300,
        "from": { "id": ALLOWED, "is_bot": false, "first_name": "QA" },
        "chat": { "id": ALLOWED, "type": "private", "first_name": "QA" },
        "text": user_text,
        "reply_to_message": {
            "message_id": replied_id,
            "date": 1700000200,
            "from": { "id": 1, "is_bot": true, "first_name": "Bot" },
            "chat": { "id": ALLOWED, "type": "private", "first_name": "QA" },
            "text": ""
        }
    });
    if let Some(q) = quote {
        message["quote"] = json!({ "text": q, "position": 0 });
    }
    json!({ "update_id": update_id, "message": message }).to_string()
}

async fn outbound_rows(memory: &MemoryStore) -> Vec<(i64, i32, String)> {
    let conn = memory.connection();
    let guard = conn.lock().await;
    let mut stmt = guard
        .prepare("SELECT chat_id, message_id, body FROM tg_outbound_text")
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, String>(2)?,
        ))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

async fn stored_user_texts(agent: &Agent) -> Vec<String> {
    let saved = agent.memory.recent_messages(40).await.unwrap();
    saved
        .into_iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.content.as_ref().map(|c| c.as_text()))
        .collect()
}

async fn drive_raw(h: &HandleMessageHarness, raw: &str) {
    let upd = UpdateInjector::parse_update(raw).expect("parse injected update");
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive injected update");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;
}

/// #358: rich finals are indexed; deleted stream drafts are not. A reply whose
/// nested body is empty resolves from that map, then from the outer quote.
/// A miss stays on the locked unavailable template.
#[tokio::test]
async fn reply_to_empty_bot_bubble_uses_outbound_map_then_quote() {
    let h = HandleMessageHarness::new(STUB_REPLY).await;
    let upd = UpdateInjector::parse_update_file(fixture("chat_hello.json")).unwrap();
    injector()
        .drive_handle_message(&upd, h.bot.clone(), Arc::clone(&h.agent))
        .await
        .expect("drive chat_hello");
    wait_turn_idle(&h.agent, "main", ALLOWED).await;

    let deleted = h.api.deleted_snapshot();
    let rows = outbound_rows(&h.agent.memory).await;
    for id in &deleted {
        assert!(
            !rows.iter().any(|(_, mid, _)| mid == id),
            "deleted id {id} must not be indexed; rows={rows:?} deleted={deleted:?}"
        );
    }
    let finals: Vec<_> = rows
        .iter()
        .filter(|(_, _, body)| body.contains(STUB_REPLY))
        .collect();
    assert!(
        !finals.is_empty(),
        "final bubble text must be stored; rows={rows:?} sent={:?}",
        h.api.sent_snapshot()
    );
    let chat_id = finals[0].0;
    let final_id = finals[0].1;
    assert_eq!(chat_id, i64::try_from(ALLOWED).unwrap());
    assert!(
        !deleted.contains(&final_id),
        "indexed final {final_id} was deleted"
    );

    drive_raw(
        &h,
        &reply_to_bot_update(
            900100,
            50,
            "see the quote",
            final_id,
            Some("NOT_THE_STORED_BODY"),
        ),
    )
    .await;
    let users = stored_user_texts(&h.agent).await;
    let hit = users
        .iter()
        .find(|t| t.contains("see the quote"))
        .unwrap_or_else(|| panic!("missing hit turn; users={users:?}"));
    assert!(
        hit.contains(STUB_REPLY),
        "local map must supply the body; got {hit}"
    );
    assert!(
        hit.contains(&format!("[Reply to bot msg {final_id}]:")),
        "locked text template; got {hit}"
    );
    assert!(
        !hit.contains("unavailable") && !hit.contains("NOT_THE_STORED_BODY"),
        "map beats outer quote; got {hit}"
    );

    drive_raw(
        &h,
        &reply_to_bot_update(900101, 51, "missing bubble", 99999, None),
    )
    .await;
    let users = stored_user_texts(&h.agent).await;
    let miss = users
        .iter()
        .find(|t| t.contains("missing bubble"))
        .unwrap_or_else(|| panic!("missing miss turn; users={users:?}"));
    assert!(
        miss.contains("[Reply to bot msg 99999: unavailable]"),
        "miss stays unavailable; got {miss}"
    );

    drive_raw(
        &h,
        &reply_to_bot_update(
            900102,
            52,
            "outer quote only",
            88888,
            Some("OUTER_QUOTE_SLICE"),
        ),
    )
    .await;
    let users = stored_user_texts(&h.agent).await;
    let quoted = users
        .iter()
        .find(|t| t.contains("outer quote only"))
        .unwrap_or_else(|| panic!("missing quote turn; users={users:?}"));
    assert!(
        quoted.contains("[Reply to bot msg 88888]:\nOUTER_QUOTE_SLICE"),
        "outer quote before unavailable; got {quoted}"
    );
    assert!(!quoted.contains("unavailable"), "got {quoted}");
}
