//! Device WebSocket (ADR-0056): auth, ping/pong, interactive inbound, revoke.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

use rustfox::config::Config;
use rustfox::device_ws::{
    owner_session_key, submit_for_owner, DeviceInbound, DeviceRegistry, DeviceTurnOutcome,
    DeviceUserText, DeviceWsRuntime, OwnerChat,
};
use rustfox::memory::MemoryStore;
use rustfox::platform::IncomingMessage;
use rustfox::portal::{AgentOps, PortalState, SkillInfo};
use rustfox::scheduler::reminders::ScheduledTaskStore;
use rustfox::tool_registry::ToolUiMode;
use rustfox::turn_gate::TurnGate;

fn owner() -> OwnerChat {
    OwnerChat {
        bot_id: "default".into(),
        user_id: "7".into(),
        chat_id: "7".into(),
        user_name: "user-7".into(),
    }
}

struct GateInbound {
    gate: Arc<TurnGate>,
    owner: OwnerChat,
    seen: Arc<Mutex<Vec<DeviceUserText>>>,
}

#[async_trait]
impl DeviceInbound for GateInbound {
    async fn submit(&self, msg: DeviceUserText) -> anyhow::Result<DeviceTurnOutcome> {
        self.seen.lock().expect("seen").push(msg.clone());
        let expect = msg.text.clone();
        submit_for_owner(&self.gate, &self.owner, msg, move |queued| {
            let expect = expect.clone();
            Box::pin(async move {
                assert_eq!(queued.text, expect);
                Ok(format!("echo:{}", queued.text))
            })
        })
        .await
    }
}

async fn open_registry() -> DeviceRegistry {
    let memory = MemoryStore::open_in_memory().unwrap();
    let secrets = Arc::new(rustfox::secret_store::FakeSecretStore::new());
    DeviceRegistry::open(memory.connection(), secrets)
        .await
        .unwrap()
}

async fn spawn_device_server(inbound: Arc<dyn DeviceInbound>, registry: DeviceRegistry) -> String {
    let app = rustfox::device_ws::router(DeviceWsRuntime {
        registry,
        inbound,
        live: rustfox::device_ws::LiveSockets::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}{}", rustfox::device_ws::WS_PATH)
}

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, bearer: Option<&str>) -> Result<Sock, WsError> {
    let mut last = None;
    for _ in 0..25 {
        let mut req = url.into_client_request().unwrap();
        if let Some(token) = bearer {
            req.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
        }
        match connect_async(req).await {
            Ok((ws, _)) => return Ok(ws),
            Err(e) if is_refused(&e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("connect attempt"))
}

fn is_refused(err: &WsError) -> bool {
    let text = err.to_string();
    text.contains("Connection refused") || text.contains("os error 111")
}

async fn recv_json(ws: &mut Sock) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = ws.next().await.expect("socket closed").expect("ws error");
            if let Message::Text(text) = msg {
                return serde_json::from_str::<Value>(text.as_str()).expect("json frame");
            }
        }
    })
    .await
    .expect("timed out waiting for a text frame");
    msg
}

async fn send_json(ws: &mut Sock, value: Value) {
    ws.send(Message::text(value.to_string())).await.unwrap();
}

fn gate_inbound() -> (
    Arc<GateInbound>,
    Arc<Mutex<Vec<DeviceUserText>>>,
    Arc<TurnGate>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(TurnGate::new());
    let inbound = Arc::new(GateInbound {
        gate: Arc::clone(&gate),
        owner: owner(),
        seen: Arc::clone(&seen),
    });
    (inbound, seen, gate)
}

#[tokio::test]
async fn auth_reject_does_not_enter_the_interactive_lane() {
    let (inbound, seen, gate) = gate_inbound();
    let registry = open_registry().await;
    let url = spawn_device_server(inbound, registry.clone()).await;

    let mut ws = connect(&url, None).await.expect("upgrade without a header");
    send_json(&mut ws, json!({"type": "user_text", "text": "hi"})).await;
    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["code"], "unauthorized");
    assert!(seen.lock().unwrap().is_empty());
    assert!(!gate.interactive_active(&owner_session_key(&owner())).await);

    send_json(&mut ws, json!({"type": "ping"})).await;
    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["code"], "unauthorized");

    let bad = connect(&url, Some("not-a-device-token")).await.unwrap_err();
    match bad {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
        other => panic!("expected HTTP 401, got {other}"),
    }

    let mut ws = connect(&url, None).await.unwrap();
    send_json(&mut ws, json!({"type": "auth", "token": "still-wrong"})).await;
    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["code"], "unauthorized");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bearer_auth_ping_and_user_text_hit_the_interactive_lane() {
    let (inbound, seen, gate) = gate_inbound();
    let registry = open_registry().await;
    let issued = registry.create("kitchen", Some("kitchen")).await.unwrap();
    let url = spawn_device_server(inbound, registry).await;

    let mut ws = connect(&url, Some(&issued.token))
        .await
        .expect("bearer accepted");
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["type"], "device_ack");
    assert_eq!(ack["status"], "authenticated");

    send_json(&mut ws, json!({"type": "ping"})).await;
    let pong = recv_json(&mut ws).await;
    assert_eq!(pong["type"], "pong");

    send_json(
        &mut ws,
        json!({
            "type": "device_hello",
            "device_id": "board-1",
            "room": "kitchen",
            "fw_version": "1.0.9"
        }),
    )
    .await;
    let hello = recv_json(&mut ws).await;
    assert_eq!(hello["type"], "device_ack");
    assert_eq!(hello["device_id"], "board-1");
    assert_eq!(hello["room"], "kitchen");

    send_json(
        &mut ws,
        json!({"type": "user_text", "text": "turn on the lamp"}),
    )
    .await;
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["type"], "assistant_text");
    assert_eq!(reply["text"], "echo:turn on the lamp");

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text, "turn on the lamp");
        assert_eq!(seen[0].device_id, "board-1");
        assert_eq!(seen[0].room.as_deref(), Some("kitchen"));
        assert_eq!(seen[0].token_device_id, issued.record.id);
    }
    assert!(!gate.interactive_active(&owner_session_key(&owner())).await);
}

#[tokio::test]
async fn first_frame_auth_then_ping() {
    let (inbound, seen, _gate) = gate_inbound();
    let registry = open_registry().await;
    let issued = registry.create("hall", None).await.unwrap();
    let url = spawn_device_server(inbound, registry).await;

    let mut ws = connect(&url, None).await.unwrap();
    send_json(&mut ws, json!({"type": "auth", "token": issued.token})).await;
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["type"], "device_ack");
    assert_eq!(ack["status"], "authenticated");
    assert_eq!(ack["device_id"], issued.record.id);

    send_json(&mut ws, json!({"type": "ping"})).await;
    let pong = recv_json(&mut ws).await;
    assert_eq!(pong["type"], "pong");

    send_json(&mut ws, json!({"type": "user_text", "text": "status"})).await;
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["text"], "echo:status");
    assert_eq!(seen.lock().unwrap()[0].device_id, issued.record.id);
}

#[tokio::test]
async fn revoke_rejects_header_and_first_frame() {
    let (inbound, seen, _) = gate_inbound();
    let registry = open_registry().await;
    let issued = registry.create("porch", None).await.unwrap();
    let url = spawn_device_server(
        Arc::clone(&inbound) as Arc<dyn DeviceInbound>,
        registry.clone(),
    )
    .await;

    let mut ws = connect(&url, Some(&issued.token)).await.unwrap();
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["status"], "authenticated");
    drop(ws);

    assert!(registry.revoke(&issued.record.id).await.unwrap());

    let rejected = connect(&url, Some(&issued.token)).await.unwrap_err();
    match rejected {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
        other => panic!("expected HTTP 401 after revoke, got {other}"),
    }

    let mut ws = connect(&url, None).await.unwrap();
    send_json(&mut ws, json!({"type": "auth", "token": issued.token})).await;
    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["code"], "unauthorized");
    assert!(seen.lock().unwrap().is_empty());
}

// --- portal Settings API ----------------------------------------------------

struct PortalFake {
    config: Config,
    seen: Mutex<Vec<String>>,
}

impl PortalFake {
    fn new(config: Config) -> Self {
        Self {
            config,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl AgentOps for PortalFake {
    fn current_model(&self) -> BoxFuture<'_, String> {
        Box::pin(async { "fake".into() })
    }
    fn is_processing(&self, _user_id: &str) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn set_model(&self, _model_id: String) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn reload_skills_and_agents(&self) -> BoxFuture<'_, (usize, usize)> {
        Box::pin(async { (0, 0) })
    }
    fn cancel_processing(&self, _user_id: String) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn clear_cancel_token(&self, _user_id: String) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn skill_entries(&self) -> BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async { Vec::new() })
    }
    fn agent_entries(&self) -> BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async { Vec::new() })
    }
    fn remove_scheduler_job(&self, _job_id: uuid::Uuid) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn arm_task(
        &self,
        _task: rustfox::scheduler::reminders::ScheduledTask,
    ) -> BoxFuture<'_, anyhow::Result<uuid::Uuid>> {
        Box::pin(async { Ok(uuid::Uuid::nil()) })
    }
    fn disarm_task(
        &self,
        _task: rustfox::scheduler::reminders::ScheduledTask,
    ) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn process_message(
        &self,
        _incoming: IncomingMessage,
        _tool_event_tx: Option<
            tokio::sync::mpsc::Sender<rustfox::platform::tool_notifier::ToolEvent>,
        >,
        _stream_token_tx: Option<tokio::sync::mpsc::Sender<String>>,
        _ui_mode: ToolUiMode,
    ) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
    fn set_soul_updated(&self, _value: bool) {}
    fn provider_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn tool_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn config(&self) -> &Config {
        &self.config
    }
    fn submit_device_turn(
        &self,
        turn: DeviceUserText,
    ) -> BoxFuture<'_, anyhow::Result<DeviceTurnOutcome>> {
        self.seen.lock().expect("seen").push(turn.text.clone());
        let text = turn.text;
        Box::pin(async move { Ok(DeviceTurnOutcome::Replies(vec![format!("portal:{text}")])) })
    }
}

fn write_portal_config(dir: &Path, device_on: bool) -> Config {
    let path = dir.join("config.toml");
    let device = if device_on {
        "[device_ws]\nenabled = true\n"
    } else {
        ""
    };
    std::fs::write(
        &path,
        format!(
            r#"[telegram]
bot_token = "test"
allowed_user_ids = [7]

[openrouter]
api_key = "test"

[sandbox]
allowed_directory = "."

[portal]
enabled = true
port = 8090
bind = "127.0.0.1"
token = "portal-token"
user_name = "web"
{device}"#
        ),
    )
    .unwrap();
    Config::load(&path).unwrap()
}

struct PortalFixture {
    app: axum::Router,
    state: PortalState,
    fake: Arc<PortalFake>,
    _dir: tempfile::TempDir,
}

async fn portal_fixture(device_on: bool) -> PortalFixture {
    let dir = tempfile::tempdir().unwrap();
    let config = write_portal_config(dir.path(), device_on);
    let portal = config.portal.clone();
    let memory = MemoryStore::open_in_memory().unwrap();
    let tasks = ScheduledTaskStore::new(memory.connection());
    let fake = Arc::new(PortalFake::new(config));
    let state = PortalState::new(
        fake.clone(),
        memory,
        tasks,
        portal,
        dir.path().join("config.toml"),
        Some(dir.path().to_path_buf()),
    );
    let app = rustfox::portal::router(state.clone());
    PortalFixture {
        app,
        state,
        fake,
        _dir: dir,
    }
}

fn bearer(path: &str, method: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap()
}

async fn json_body(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or(json!({ "raw": String::from_utf8_lossy(&bytes).to_string() }))
}

#[tokio::test]
async fn settings_devices_show_token_once_and_device_token_is_not_portal_login() {
    let f = portal_fixture(false).await;
    let missing = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/device/v1/ws")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        missing.status(),
        StatusCode::NOT_FOUND,
        "disabled device_ws must not mount the socket"
    );
    let audio_off = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/device/v1/audio")
                .header(header::CONTENT_TYPE, "audio/wav")
                .body(Body::from(b"RIFF".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        audio_off.status(),
        StatusCode::NOT_FOUND,
        "disabled device_ws must not mount audio upload"
    );

    let created = f
        .app
        .clone()
        .oneshot(bearer(
            "/api/settings/devices",
            "POST",
            "portal-token",
            Some(json!({"label": "lamp", "room": "kitchen"})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = json_body(created).await;
    let token = created["token"].as_str().unwrap().to_string();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(!token.is_empty());

    let listed = f
        .app
        .clone()
        .oneshot(bearer("/api/settings/devices", "GET", "portal-token", None))
        .await
        .unwrap();
    let listed = json_body(listed).await;
    assert_eq!(listed["enabled"], false);
    assert_eq!(listed["path"], "/api/device/v1/ws");
    assert_eq!(listed["devices"][0]["label"], "lamp");
    assert!(listed["devices"][0].get("token").is_none());
    assert!(!listed.to_string().contains(&token));

    let rejected = f
        .app
        .clone()
        .oneshot(bearer("/api/settings", "GET", &token, None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let revoked = f
        .app
        .clone()
        .oneshot(bearer(
            &format!("/api/settings/devices/{id}"),
            "DELETE",
            "portal-token",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let registry = f.state.device_registry().await.unwrap();
    assert!(registry.authenticate(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn enabled_portal_mounts_ws_and_user_text_reaches_agent_ops() {
    let f = portal_fixture(true).await;
    let created = f
        .app
        .clone()
        .oneshot(bearer(
            "/api/settings/devices",
            "POST",
            "portal-token",
            Some(json!({"label": "board"})),
        ))
        .await
        .unwrap();
    let created = json_body(created).await;
    let token = created["token"].as_str().unwrap().to_string();

    let probe = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/device/v1/ws")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(probe.status(), StatusCode::NOT_FOUND);
    let audio = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/device/v1/audio")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(audio.status(), StatusCode::UNAUTHORIZED);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = f.app.clone();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("ws://{addr}/api/device/v1/ws");
    let mut ws = connect(&url, Some(&token)).await.expect("portal ws bearer");
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["status"], "authenticated");
    send_json(
        &mut ws,
        json!({"type": "user_text", "text": "from the board"}),
    )
    .await;
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["type"], "assistant_text");
    assert_eq!(reply["text"], "portal:from the board");
    assert_eq!(f.fake.seen.lock().unwrap().as_slice(), ["from the board"]);

    let api_only = rustfox::portal::device_api_router(f.state.clone());
    let health = api_only
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::NOT_FOUND);
    let headless_audio = api_only
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/device/v1/audio")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(headless_audio.status(), StatusCode::UNAUTHORIZED);
}
