//! ESP32 HTTP audio (ADR-0066): device-token WAV upload, owner `user_text`, PCM GET.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use rustfox::asr::{AsrError, SttSource, Transcript, TtsError};
use rustfox::device_audio::{AudioHost, PcmStore};
use rustfox::device_ws::{
    DeviceInbound, DeviceRegistry, DeviceTurnOutcome, DeviceUserText, DeviceWsRuntime, LiveSockets,
};

fn wav(sample_rate: u32, channels: u16, frames: &[i16]) -> Vec<u8> {
    let block = channels as usize * 2;
    assert_eq!(frames.len() % channels as usize, 0);
    let data_len = frames.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block as u32).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in frames {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn mono_16k() -> Vec<u8> {
    wav(16_000, 1, &[0, 1000, -1000, 0])
}

struct ScriptedInbound {
    seen: Arc<Mutex<Vec<DeviceUserText>>>,
    replies: Vec<String>,
}

#[async_trait]
impl DeviceInbound for ScriptedInbound {
    async fn submit(&self, msg: DeviceUserText) -> anyhow::Result<DeviceTurnOutcome> {
        self.seen.lock().expect("seen").push(msg);
        Ok(DeviceTurnOutcome::Replies(self.replies.clone()))
    }
}

struct HoldInbound {
    seen: Arc<Mutex<Vec<String>>>,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[async_trait]
impl DeviceInbound for HoldInbound {
    async fn submit(&self, msg: DeviceUserText) -> anyhow::Result<DeviceTurnOutcome> {
        self.seen.lock().expect("seen").push(msg.text);
        if let Some(tx) = self.started.lock().expect("started").take() {
            let _ = tx.send(());
        }
        let release = self.release.lock().expect("release").take();
        if let Some(rx) = release {
            let _ = rx.await;
        }
        Ok(DeviceTurnOutcome::Replies(vec!["kept in the turn".into()]))
    }
}

struct FakeEngine {
    transcript: String,
    lang: Option<String>,
    fail: Option<AsrError>,
    pcm: Vec<u8>,
    calls: Mutex<u32>,
    spoken: Mutex<Vec<(String, String)>>,
}

impl FakeEngine {
    fn ok(transcript: &str, lang: Option<&str>, pcm: Vec<u8>) -> Self {
        Self {
            transcript: transcript.into(),
            lang: lang.map(str::to_string),
            fail: None,
            pcm,
            calls: Mutex::new(0),
            spoken: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl rustfox::device_audio::DeviceAudioEngine for FakeEngine {
    async fn transcribe(&self, wav: &std::path::Path) -> Result<Transcript, AsrError> {
        assert!(wav.is_file(), "transcribe_file input must be a wav path");
        *self.calls.lock().expect("calls") += 1;
        if let Some(err) = &self.fail {
            return Err(err.clone());
        }
        Ok(Transcript {
            text: self.transcript.clone(),
            source: SttSource::SenseVoice,
            detected_lang: self.lang.clone(),
        })
    }

    async fn synthesize_pcm(&self, text: &str, spoken_lang: &str) -> Result<Vec<u8>, TtsError> {
        self.spoken
            .lock()
            .expect("spoken")
            .push((text.to_string(), spoken_lang.to_string()));
        if self.pcm.is_empty() {
            return Err(TtsError {
                message: "empty pcm".into(),
            });
        }
        Ok(self.pcm.clone())
    }
}

struct Harness {
    app: axum::Router,
    seen: Arc<Mutex<Vec<DeviceUserText>>>,
    pcm: PcmStore,
    engine: Arc<FakeEngine>,
    token: String,
    other: String,
}

async fn harness(engine: FakeEngine, replies: Vec<String>) -> Harness {
    let registry = open_registry().await;
    let issued = registry.create("board", Some("kitchen")).await.unwrap();
    let other = registry.create("other", None).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inbound = Arc::new(ScriptedInbound {
        seen: Arc::clone(&seen),
        replies,
    });
    let live = LiveSockets::new();
    let pcm = PcmStore::new();
    let engine = Arc::new(engine);
    let app = merged(registry, inbound, live, pcm.clone(), engine.clone());
    Harness {
        app,
        seen,
        pcm,
        engine,
        token: issued.token,
        other: other.token,
    }
}

fn merged(
    registry: DeviceRegistry,
    inbound: Arc<dyn DeviceInbound>,
    live: LiveSockets,
    pcm: PcmStore,
    engine: Arc<dyn rustfox::device_audio::DeviceAudioEngine>,
) -> axum::Router {
    let ws = rustfox::device_ws::router(DeviceWsRuntime {
        registry: registry.clone(),
        inbound: Arc::clone(&inbound),
        live: live.clone(),
    });
    let audio = rustfox::device_audio::router(AudioHost {
        registry,
        inbound,
        live,
        pcm,
        engine,
    });
    ws.merge(audio)
}

async fn open_registry() -> DeviceRegistry {
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let secrets = Arc::new(rustfox::secret_store::FakeSecretStore::new());
    DeviceRegistry::open(memory.connection(), secrets)
        .await
        .unwrap()
}

async fn oneshot(
    app: axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

fn json_bytes(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(json!({
        "raw": String::from_utf8_lossy(bytes).to_string()
    }))
}

struct Server {
    addr: std::net::SocketAddr,
    live: LiveSockets,
    pcm: PcmStore,
    seen: Arc<Mutex<Vec<String>>>,
    engine: Arc<FakeEngine>,
    token: String,
    device_id: String,
}

async fn spawn_hold() -> (
    Server,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let registry = open_registry().await;
    let issued = registry.create("board", Some("kitchen")).await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inbound = Arc::new(HoldInbound {
        seen: Arc::clone(&seen),
        started: Mutex::new(Some(started_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let live = LiveSockets::new();
    let pcm = PcmStore::new();
    let engine = Arc::new(FakeEngine::ok("開燈", Some("yue"), vec![1, 2, 3, 4]));
    let app = merged(registry, inbound, live.clone(), pcm.clone(), engine.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        Server {
            addr,
            live,
            pcm,
            seen,
            engine,
            token: issued.token,
            device_id: issued.record.id,
        },
        started_rx,
        release_tx,
    )
}

async fn spawn(app: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr, token: &str) -> Sock {
    let url = format!("ws://{addr}{}", rustfox::device_ws::WS_PATH);
    let mut last = None;
    for _ in 0..25 {
        let mut req = url.clone().into_client_request().unwrap();
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => return ws,
            Err(err) if format!("{err}").contains("Connection refused") => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(err) => panic!("connect failed: {err}"),
        }
    }
    panic!("connect refused: {last:?}");
}

async fn recv_json(ws: &mut Sock) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = ws.next().await.expect("socket closed").expect("ws error");
            if let Message::Text(text) = msg {
                return serde_json::from_str::<Value>(text.as_str()).expect("json");
            }
        }
    })
    .await
    .expect("timed out waiting for a text frame");
    msg
}

async fn post_chunked(
    addr: std::net::SocketAddr,
    token: &str,
    content_type: &str,
    body: &[u8],
) -> (StatusCode, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /api/device/v1/audio HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let split = body.len() / 2;
    for part in [&body[..split], &body[split..]] {
        let chunk = format!("{:X}\r\n", part.len());
        stream.write_all(chunk.as_bytes()).await.unwrap();
        stream.write_all(part).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
    }
    stream.write_all(b"0\r\n\r\n").await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    let (head, rest) = text.split_once("\r\n\r\n").expect("http response");
    let status: u16 = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (
        StatusCode::from_u16(status).unwrap(),
        rest.as_bytes().to_vec(),
    )
}

#[tokio::test]
async fn rejects_missing_and_unknown_device_tokens() {
    let h = harness(
        FakeEngine::ok("hello", Some("en"), vec![9, 9]),
        vec!["ok".into()],
    )
    .await;

    let (status, _, body) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        None,
        Some("audio/wav"),
        mono_16k(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_bytes(&body)["error"]["code"], "unauthorized");

    let (status, _, _) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        Some("not-a-device-token"),
        Some("audio/wav"),
        mono_16k(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = oneshot(
        h.app.clone(),
        "GET",
        "/api/device/v1/audio/missing",
        None,
        None,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(h.seen.lock().unwrap().is_empty());
    assert_eq!(*h.engine.calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn rejects_bad_format_and_does_not_start_a_turn() {
    let h = harness(
        FakeEngine::ok("hello", Some("en"), vec![1]),
        vec!["ok".into()],
    )
    .await;

    let (status, _, body) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        Some(&h.token),
        Some("text/plain"),
        b"hello".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json_bytes(&body)["error"]["code"], "unsupported_media_type");

    let (status, _, body) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        Some(&h.token),
        Some("audio/wav"),
        b"not-a-wav".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_bytes(&body)["error"]["code"], "bad_format");

    let (status, _, _) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        Some(&h.token),
        Some("audio/wav"),
        wav(8_000, 1, &[0, 1]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = oneshot(
        h.app.clone(),
        "POST",
        "/api/device/v1/audio",
        Some(&h.token),
        Some("audio/wav"),
        wav(16_000, 2, &[0, 0]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    assert!(h.seen.lock().unwrap().is_empty());
    assert_eq!(*h.engine.calls.lock().unwrap(), 0);
    assert!(h.pcm.is_empty());
}

#[tokio::test]
async fn empty_transcript_does_not_start_a_turn() {
    let mut engine = FakeEngine::ok("", None, vec![1]);
    engine.fail = Some(AsrError::empty("SenseVoice produced no transcript"));
    let h = harness(engine, vec!["should not run".into()]).await;
    let (status, _, body) = oneshot(
        h.app,
        "POST",
        "/api/device/v1/audio",
        Some(&h.token),
        Some("audio/wav"),
        mono_16k(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_bytes(&body)["error"]["code"], "empty_transcript");
    assert!(h.seen.lock().unwrap().is_empty());
    assert!(h.pcm.is_empty());
}

#[tokio::test]
async fn chunked_wav_becomes_user_text_and_pcm_for_that_device_only() {
    let pcm = vec![0x00, 0x01, 0xff, 0x7f, 0x10, 0x00];
    let h = harness(
        FakeEngine::ok("開燈", Some("yue"), pcm.clone()),
        vec!["燈已經開咗".into()],
    )
    .await;
    let addr = spawn(h.app.clone()).await;
    let mut ws = connect(addr, &h.token).await;
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["type"], "device_ack");
    assert_eq!(ack["status"], "authenticated");

    let (status, body) = post_chunked(addr, &h.token, "audio/wav", &mono_16k()).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let posted = json_bytes(&body);
    assert_eq!(posted["transcript"], "開燈");
    assert_eq!(posted["replies"][0], "燈已經開咗");

    let text = recv_json(&mut ws).await;
    assert_eq!(text["type"], "assistant_text");
    assert_eq!(text["text"], "燈已經開咗");
    let audio = recv_json(&mut ws).await;
    assert_eq!(audio["type"], "assistant_audio");
    assert_eq!(audio["sample_rate"], 16000);
    assert_eq!(audio["channels"], 1);
    assert_eq!(audio["encoding"], "s16le");
    let id = audio["id"].as_str().unwrap();
    let url = audio["url"].as_str().unwrap();
    assert_eq!(url, format!("/api/device/v1/audio/{id}"));
    assert_eq!(posted["audio"][0]["url"], url);
    assert!(audio.as_object().unwrap().keys().all(|key| {
        matches!(
            key.as_str(),
            "type" | "id" | "url" | "sample_rate" | "channels" | "encoding"
        )
    }));

    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].text, "開燈");
    assert!(!seen[0].text.contains("[Voice note]"));
    drop(seen);

    let spoken = h.engine.spoken.lock().unwrap().clone();
    assert_eq!(spoken, vec![("燈已經開咗".into(), "yue".into())]);

    let (status, headers, bytes) =
        oneshot(h.app.clone(), "GET", url, Some(&h.token), None, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/octet-stream")
    );
    assert_eq!(bytes, pcm);

    let (status, _, body) = oneshot(h.app, "GET", url, Some(&h.other), None, Vec::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_bytes(&body)["error"]["code"], "unauthorized");
    assert_ne!(body, pcm);
}

#[tokio::test]
async fn speaks_the_full_reply_for_english_and_skips_other_languages() {
    let long = "word ".repeat(rustfox::asr::TTS_MAX_CHARS + 40);
    let h = harness(
        FakeEngine::ok("turn the lamp on", Some("en"), vec![4, 5, 6, 7]),
        vec![long.clone()],
    )
    .await;
    let addr = spawn(h.app.clone()).await;
    let mut ws = connect(addr, &h.token).await;
    let _ = recv_json(&mut ws).await;
    let (status, _) = post_chunked(addr, &h.token, "audio/wav", &mono_16k()).await;
    assert_eq!(status, StatusCode::OK);
    let text = recv_json(&mut ws).await;
    assert_eq!(text["text"], long);
    let audio = recv_json(&mut ws).await;
    assert_eq!(audio["type"], "assistant_audio");
    assert_eq!(audio["encoding"], "s16le");
    let spoken = h.engine.spoken.lock().unwrap().clone();
    assert_eq!(spoken, vec![(long, "en".into())]);

    let zh = harness(
        FakeEngine::ok("你好", Some("zh"), vec![1, 2, 3, 4]),
        vec!["文本".into()],
    )
    .await;
    let addr = spawn(zh.app).await;
    let mut ws = connect(addr, &zh.token).await;
    let _ = recv_json(&mut ws).await;
    let (status, body) = post_chunked(addr, &zh.token, "audio/wav", &mono_16k()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_bytes(&body)["audio"].as_array().unwrap().is_empty());
    let text = recv_json(&mut ws).await;
    assert_eq!(text["type"], "assistant_text");
    assert_eq!(text["text"], "文本");
    let late = tokio::time::timeout(Duration::from_millis(200), recv_json(&mut ws)).await;
    assert!(late.is_err(), "zh reply must not send assistant_audio");
    assert!(zh.engine.spoken.lock().unwrap().is_empty());
    assert!(zh.pcm.is_empty());
}

#[tokio::test]
async fn socket_gone_keeps_the_turn_and_does_not_store_pcm() {
    let (server, started, release) = spawn_hold().await;
    let mut ws = connect(server.addr, &server.token).await;
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["status"], "authenticated");

    let token = server.token.clone();
    let addr = server.addr;
    let posted =
        tokio::spawn(async move { post_chunked(addr, &token, "audio/wav", &mono_16k()).await });
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .expect("turn did not start")
        .unwrap();
    ws.close(None).await.ok();
    drop(ws);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.live.is_connected(&server.device_id) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !server.live.is_connected(&server.device_id),
        "uploader socket should be gone before the reply is notified"
    );
    release.send(()).unwrap();
    let (status, body) = posted.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let posted = json_bytes(&body);
    assert_eq!(posted["transcript"], "開燈");
    assert_eq!(posted["replies"][0], "kept in the turn");
    assert!(posted["audio"].as_array().unwrap().is_empty());
    assert_eq!(server.seen.lock().unwrap().as_slice(), ["開燈"]);
    assert!(server.pcm.is_empty());
    assert!(server.engine.spoken.lock().unwrap().is_empty());

    let mut again = connect(server.addr, &server.token).await;
    let ack = recv_json(&mut again).await;
    assert_eq!(ack["type"], "device_ack");
    let late = tokio::time::timeout(Duration::from_millis(200), recv_json(&mut again)).await;
    assert!(late.is_err(), "pcm must not be replayed on a new socket");
}
