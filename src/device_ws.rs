//! Device WebSocket adapter (ADR-0056).
//!
//! Opt-in `[device_ws] enabled` (default `false`). The socket lives on the
//! same Axum process and the same bind/port as the portal:
//! `GET /api/device/v1/ws`. When the portal UI is off and this flag is on,
//! the process serves that path only (no SPA).
//!
//! An authenticated device is not a new peer channel. `user_text` enters the
//! owner's interactive [`TurnGate`](crate::turn_gate::TurnGate) (the shim
//! bot's allowlisted user) and the assistant reply comes back as
//! `assistant_text`. Turn metadata may carry `device_id` / room.
//!
//! Auth is a per-device token, distinct from the portal login token. Send
//! `Authorization: Bearer <token>` or a first text frame
//! `{"type":"auth","token":"..."}`. Portal Settings → Devices issues tokens;
//! the plaintext is returned once. The SHA-256 hex is stored on the device
//! row and in SecretStore (`device.<id>.token_sha256`). Revoke deletes both.
//! There is no anonymous socket.
//!
//! Frames are JSON text only (`auth`, `user_text`, `ping`, `device_hello` →
//! `assistant_text`, `device_ack`, `pong`, `error`, plus `assistant_audio`
//! from ADR-0066). `assistant_audio` is a JSON text frame. Binary audio,
//! partial token streaming, and portal-chat-over-WebSocket are out of scope.
//!
//! Network: loopback, Tailscale, or a trusted LAN. Bare public HTTP is not
//! a supported deployment. TLS belongs on an external reverse proxy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use futures::future::BoxFuture;
use rand::Rng;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Mutex};

use crate::agent::{session_key, Agent};
use crate::config::Config;
use crate::platform::IncomingMessage;
use crate::secret_store::SecretStore;
use crate::turn_gate::{QueuedInput, Submit, TurnGate};

pub const WS_PATH: &str = "/api/device/v1/ws";
pub const AUDIO_PATH: &str = "/api/device/v1/audio";
pub const AUDIO_ID_PATH: &str = "/api/device/v1/audio/{id}";

const MAX_USER_TEXT_CHARS: usize = 16_000;
const AUTH_WAIT: Duration = Duration::from_secs(15);

/// SHA-256 hex lives in SecretStore under this name. Not the plaintext token.
pub fn device_secret_name(id: &str) -> String {
    format!("device.{id}.token_sha256")
}

/// Warn when the shared HTTP listener is on every interface.
pub fn warn_if_public_bind(bind: &str) {
    let bind = bind.trim();
    if bind == "0.0.0.0" || bind == "::" || bind == "[::]" {
        tracing::warn!(
            "Device bind {bind} listens on all interfaces. Bare public HTTP is not a supported deployment; use loopback, Tailscale, or a trusted LAN and terminate TLS at a reverse proxy."
        );
    }
}

/// Log the operator warning that belongs next to a successful bind.
pub fn log_listening(local: &str) {
    tracing::info!(
        "Device WebSocket: ws://{local}{WS_PATH} (JSON text). Device audio: http://{local}{AUDIO_PATH}. Bare public HTTP is not a supported deployment; use loopback, Tailscale, or a trusted LAN and terminate TLS at a reverse proxy."
    );
}

#[derive(Debug)]
pub struct InvalidDevice(pub String);

impl std::fmt::Display for InvalidDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidDevice {}

#[derive(Debug)]
pub struct DeviceNotFound;

impl std::fmt::Display for DeviceNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("device not found")
    }
}

impl std::error::Error for DeviceNotFound {}

/// Issued device without the token hash (safe to show in Settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub id: String,
    pub label: String,
    pub room: Option<String>,
    pub created_at: String,
}

/// Plaintext token plus the record. Shown once; `Debug` redacts the token.
pub struct IssuedDevice {
    pub record: DeviceRecord,
    pub token: String,
}

impl std::fmt::Debug for IssuedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedDevice")
            .field("id", &self.record.id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

struct StoredDevice {
    id: String,
    label: String,
    room: Option<String>,
    created_at: String,
    token_sha256: String,
}

impl StoredDevice {
    fn record(&self) -> DeviceRecord {
        DeviceRecord {
            id: self.id.clone(),
            label: self.label.clone(),
            room: self.room.clone(),
            created_at: self.created_at.clone(),
        }
    }
}

/// Device catalog. Hashes live in SQLite and SecretStore; plaintext does not.
#[derive(Clone)]
pub struct DeviceRegistry {
    conn: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<dyn SecretStore>,
}

impl DeviceRegistry {
    pub async fn open(
        conn: Arc<Mutex<rusqlite::Connection>>,
        secrets: Arc<dyn SecretStore>,
    ) -> anyhow::Result<Self> {
        {
            let c = conn.lock().await;
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS device_ws_tokens (
                    id TEXT PRIMARY KEY,
                    label TEXT NOT NULL,
                    room TEXT,
                    token_sha256 TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );",
            )?;
        }
        Ok(Self { conn, secrets })
    }

    pub async fn list(&self) -> anyhow::Result<Vec<DeviceRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, label, room, created_at, token_sha256
             FROM device_ws_tokens ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], read_stored)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.record());
        }
        Ok(out)
    }

    pub async fn create(&self, label: &str, room: Option<&str>) -> anyhow::Result<IssuedDevice> {
        let label = normalize_label(label)?;
        let room = normalize_room(room)?;
        let id = uuid::Uuid::new_v4().to_string();
        let token = new_token();
        let hash = sha256_hex(&token);
        let created_at = chrono::Utc::now().to_rfc3339();
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO device_ws_tokens (id, label, room, token_sha256, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, label, room, hash, created_at],
            )?;
        }
        let name = device_secret_name(&id);
        if let Err(e) = self.secrets.set(&name, &hash) {
            let conn = self.conn.lock().await;
            let _ = conn.execute(
                "DELETE FROM device_ws_tokens WHERE id = ?1",
                rusqlite::params![id],
            );
            return Err(e);
        }
        Ok(IssuedDevice {
            record: DeviceRecord {
                id,
                label,
                room,
                created_at,
            },
            token,
        })
    }

    pub async fn rotate(&self, id: &str) -> anyhow::Result<IssuedDevice> {
        let existing = self.get_stored(id).await?.ok_or(DeviceNotFound)?;
        let token = new_token();
        let hash = sha256_hex(&token);
        let name = device_secret_name(id);
        self.secrets.set(&name, &hash)?;
        let updated = {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE device_ws_tokens SET token_sha256 = ?1 WHERE id = ?2",
                rusqlite::params![hash, id],
            )
        };
        match updated {
            Ok(0) => {
                let _ = self.secrets.set(&name, &existing.token_sha256);
                Err(DeviceNotFound.into())
            }
            Ok(_) => Ok(IssuedDevice {
                record: existing.record(),
                token,
            }),
            Err(e) => {
                let _ = self.secrets.set(&name, &existing.token_sha256);
                Err(e.into())
            }
        }
    }

    /// Delete the device and its SecretStore hash. `Ok(false)` if unknown.
    pub async fn revoke(&self, id: &str) -> anyhow::Result<bool> {
        if self.get_stored(id).await?.is_none() {
            return Ok(false);
        }
        self.secrets.delete(&device_secret_name(id))?;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM device_ws_tokens WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(true)
    }

    /// `Ok(None)` when the token is unknown or revoked.
    pub async fn authenticate(&self, token: &str) -> anyhow::Result<Option<DeviceRecord>> {
        let presented = sha256_hex(token);
        let rows = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT id, label, room, created_at, token_sha256 FROM device_ws_tokens",
            )?;
            let mapped = stmt.query_map([], read_stored)?;
            let mut rows = Vec::new();
            for row in mapped {
                rows.push(row?);
            }
            rows
        };
        let mut found = None;
        for row in &rows {
            if ct_eq(&row.token_sha256, &presented) {
                found = Some(row);
            }
        }
        let Some(row) = found else {
            return Ok(None);
        };
        match self.secrets.get(&device_secret_name(&row.id))? {
            Some(value) if ct_eq(value.expose(), &row.token_sha256) => Ok(Some(row.record())),
            _ => Ok(None),
        }
    }

    /// Test helper: the stored verifier, never the plaintext token.
    #[cfg(test)]
    async fn stored_hash(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self.get_stored(id).await?.map(|row| row.token_sha256))
    }

    async fn get_stored(&self, id: &str) -> anyhow::Result<Option<StoredDevice>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, label, room, created_at, token_sha256
                 FROM device_ws_tokens WHERE id = ?1",
                rusqlite::params![id],
                read_stored,
            )
            .optional()?;
        Ok(row)
    }
}

fn read_stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredDevice> {
    Ok(StoredDevice {
        id: row.get(0)?,
        label: row.get(1)?,
        room: row.get(2)?,
        created_at: row.get(3)?,
        token_sha256: row.get(4)?,
    })
}

fn normalize_label(label: &str) -> Result<String, InvalidDevice> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err(InvalidDevice(
            "label must be 1..=64 characters without control characters".into(),
        ));
    }
    Ok(label.to_string())
}

fn normalize_room(room: Option<&str>) -> Result<Option<String>, InvalidDevice> {
    let Some(room) = room.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if room.chars().count() > 64 || room.chars().any(char::is_control) {
        return Err(InvalidDevice(
            "room must be at most 64 characters without control characters".into(),
        ));
    }
    Ok(Some(room.to_string()))
}

fn new_token() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Owner primary chat: shim bot + first positive allowlisted user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerChat {
    pub bot_id: String,
    pub user_id: String,
    pub chat_id: String,
    pub user_name: String,
}

pub fn owner_chat(cfg: &Config) -> anyhow::Result<OwnerChat> {
    if cfg.bots.is_empty() {
        anyhow::bail!("device WebSocket has no owner chat: no bots configured");
    }
    let bot = Config::shim_bot(&cfg.bots);
    let user_id = bot
        .allowed_user_ids
        .iter()
        .copied()
        .find(|id| *id > 0)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "device WebSocket has no owner chat: set a positive allowed_user_ids on the primary bot"
            )
        })?;
    let user_id = user_id.to_string();
    Ok(OwnerChat {
        bot_id: crate::platform::normalize_bot_id(&bot.id).to_string(),
        chat_id: user_id.clone(),
        user_name: format!("user-{user_id}"),
        user_id,
    })
}

pub fn owner_session_key(owner: &OwnerChat) -> String {
    session_key(&owner.bot_id, &owner.user_id)
}

/// One authenticated `user_text` handed to the interactive lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceUserText {
    pub text: String,
    pub device_id: String,
    pub room: Option<String>,
    pub fw_version: Option<String>,
    /// Registry id of the token that authenticated the socket.
    pub token_device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceTurnOutcome {
    Replies(Vec<String>),
    Queued,
    QueueFull,
}

/// Production inbound: owner interactive lane, then `process_message`.
#[async_trait]
pub trait DeviceInbound: Send + Sync {
    async fn submit(&self, msg: DeviceUserText) -> anyhow::Result<DeviceTurnOutcome>;
}

pub async fn submit_on_agent(
    agent: &Arc<Agent>,
    msg: DeviceUserText,
) -> anyhow::Result<DeviceTurnOutcome> {
    let owner = owner_chat(&agent.config)?;
    let gate = Arc::clone(&agent.turn_gate);
    let agent = Arc::clone(agent);
    let owner_run = owner.clone();
    submit_for_owner(&gate, &owner, msg, move |queued| {
        let agent = Arc::clone(&agent);
        let incoming = incoming_from_owner(&owner_run, &queued);
        Box::pin(async move {
            agent
                .process_message(
                    &incoming,
                    None,
                    None,
                    crate::tool_registry::ToolUiMode::Silent,
                )
                .await
        })
    })
    .await
}

/// Submit `msg` on the owner's interactive lane and run each started input.
pub async fn submit_for_owner<F>(
    gate: &TurnGate,
    owner: &OwnerChat,
    msg: DeviceUserText,
    run_turn: F,
) -> anyhow::Result<DeviceTurnOutcome>
where
    F: FnMut(QueuedInput) -> BoxFuture<'static, anyhow::Result<String>> + Send,
{
    let key = owner_session_key(owner);
    let input = queued_from_device(&msg, owner);
    drive_interactive(gate, &key, input, run_turn).await
}

pub async fn drive_interactive<F>(
    gate: &TurnGate,
    key: &str,
    input: QueuedInput,
    mut run_turn: F,
) -> anyhow::Result<DeviceTurnOutcome>
where
    F: FnMut(QueuedInput) -> BoxFuture<'static, anyhow::Result<String>> + Send,
{
    let mut batch = match gate.submit(key, input).await {
        Submit::Accepted => return Ok(DeviceTurnOutcome::Queued),
        Submit::Full(rejected) => {
            rejected.cleanup().await;
            return Ok(DeviceTurnOutcome::QueueFull);
        }
        Submit::Start(inputs) => inputs,
    };
    let mut replies = Vec::new();
    loop {
        let mut iter = batch.into_iter();
        let Some(first) = iter.next() else {
            break;
        };
        let rest: Vec<_> = iter.collect();
        gate.restore_pending(key, rest).await;
        let text = match run_turn(first).await {
            Ok(text) => text,
            Err(e) => {
                let _ = gate.finish_user(key, None).await;
                return Err(e);
            }
        };
        replies.push(text);
        match gate
            .finish_user(key, Some(crate::agent::RunStop::FinalResponse))
            .await
        {
            crate::turn_gate::Next::User { inputs, .. } => batch = inputs,
            crate::turn_gate::Next::Idle
            | crate::turn_gate::Next::Scheduled
            | crate::turn_gate::Next::StepLimit { .. } => break,
        }
    }
    Ok(DeviceTurnOutcome::Replies(replies))
}

fn queued_from_device(msg: &DeviceUserText, owner: &OwnerChat) -> QueuedInput {
    QueuedInput {
        text: msg.text.clone(),
        chat_id: owner.chat_id.clone(),
        user_name: owner.user_name.clone(),
        device_id: Some(msg.device_id.clone()),
        device_room: msg.room.clone(),
        device_fw: msg.fw_version.clone(),
        ..Default::default()
    }
}

fn incoming_from_owner(owner: &OwnerChat, queued: &QueuedInput) -> IncomingMessage {
    let chat_id = if queued.chat_id.is_empty() {
        owner.chat_id.clone()
    } else {
        queued.chat_id.clone()
    };
    let user_name = if queued.user_name.is_empty() {
        owner.user_name.clone()
    } else {
        queued.user_name.clone()
    };
    IncomingMessage {
        platform: "telegram".to_string(),
        bot_id: owner.bot_id.clone(),
        user_id: owner.user_id.clone(),
        chat_id,
        user_name,
        text: queued.text.clone(),
        attachments: queued.attachments.clone(),
        schedule_id: None,
        device_id: queued.device_id.clone(),
        device_room: queued.device_room.clone(),
        device_fw: queued.device_fw.clone(),
        voice_origin: false,
        voice_spoken_lang: None,
    }
}

/// Append a single device note when metadata is present. Otherwise return `text`.
pub fn annotate_user_text(
    text: &str,
    device_id: Option<&str>,
    room: Option<&str>,
    fw: Option<&str>,
) -> String {
    let id = sanitize_meta(device_id);
    let room = sanitize_meta(room);
    let fw = sanitize_meta(fw);
    if id.is_none() && room.is_none() && fw.is_none() {
        return text.to_string();
    }
    let mut note = String::from("\n\n[device");
    if let Some(id) = &id {
        note.push_str(" device_id=");
        note.push_str(id);
    }
    if let Some(room) = &room {
        note.push_str(" room=");
        note.push_str(room);
    }
    if let Some(fw) = &fw {
        note.push_str(" fw=");
        note.push_str(fw);
    }
    note.push(']');
    let mut out = String::with_capacity(text.len() + note.len());
    out.push_str(text);
    out.push_str(&note);
    out
}

fn sanitize_meta(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in value.chars() {
        if out.chars().count() >= 64 {
            break;
        }
        if c.is_control() || c == '[' || c == ']' {
            continue;
        }
        out.push(c);
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// One authenticated device socket that can receive JSON text frames.
#[derive(Clone, Default)]
pub struct LiveSockets {
    inner: Arc<StdMutex<LiveInner>>,
}

struct LiveInner {
    next: u64,
    slots: HashMap<String, LiveSlot>,
}

impl Default for LiveInner {
    fn default() -> Self {
        Self {
            next: 1,
            slots: HashMap::new(),
        }
    }
}

struct LiveSlot {
    generation: u64,
    tx: mpsc::UnboundedSender<Value>,
    device_id: String,
    room: Option<String>,
    fw: Option<String>,
}

/// Hello metadata from the uploader's current socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLiveMeta {
    pub device_id: String,
    pub room: Option<String>,
    pub fw: Option<String>,
}

pub struct LiveGuard {
    live: LiveSockets,
    id: String,
    generation: u64,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.live.unregister(&self.id, self.generation);
    }
}

impl LiveSockets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace any previous socket for this device token. The new socket is
    /// the one `assistant_audio` is sent on.
    pub fn register(
        &self,
        device: &DeviceRecord,
    ) -> (u64, mpsc::UnboundedReceiver<Value>, LiveGuard) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inner = self.lock();
        let generation = inner.next;
        inner.next = inner.next.saturating_add(1);
        inner.slots.insert(
            device.id.clone(),
            LiveSlot {
                generation,
                tx,
                device_id: device.id.clone(),
                room: device.room.clone(),
                fw: None,
            },
        );
        drop(inner);
        (
            generation,
            rx,
            LiveGuard {
                live: self.clone(),
                id: device.id.clone(),
                generation,
            },
        )
    }

    pub fn note_hello(
        &self,
        registry_id: &str,
        generation: u64,
        device_id: &str,
        room: Option<&str>,
        fw: Option<&str>,
    ) {
        let mut inner = self.lock();
        let Some(slot) = inner.slots.get_mut(registry_id) else {
            return;
        };
        if slot.generation != generation {
            return;
        }
        slot.device_id = device_id.to_string();
        slot.room = room.map(str::to_string);
        slot.fw = fw.map(str::to_string);
    }

    pub fn meta(&self, registry_id: &str) -> Option<DeviceLiveMeta> {
        let inner = self.lock();
        let slot = inner.slots.get(registry_id)?;
        if slot.tx.is_closed() {
            return None;
        }
        Some(DeviceLiveMeta {
            device_id: slot.device_id.clone(),
            room: slot.room.clone(),
            fw: slot.fw.clone(),
        })
    }

    pub fn is_connected(&self, registry_id: &str) -> bool {
        let inner = self.lock();
        inner
            .slots
            .get(registry_id)
            .is_some_and(|slot| !slot.tx.is_closed())
    }

    pub fn try_send(&self, registry_id: &str, frame: &Value) -> bool {
        let tx = {
            let inner = self.lock();
            let Some(slot) = inner.slots.get(registry_id) else {
                return false;
            };
            if slot.tx.is_closed() {
                return false;
            }
            slot.tx.clone()
        };
        tx.send(frame.clone()).is_ok()
    }

    fn unregister(&self, registry_id: &str, generation: u64) {
        let mut inner = self.lock();
        if inner
            .slots
            .get(registry_id)
            .is_some_and(|slot| slot.generation == generation)
        {
            inner.slots.remove(registry_id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LiveInner> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Runtime for a standalone device router (tests and the API-only server's twin).
#[derive(Clone)]
pub struct DeviceWsRuntime {
    pub registry: DeviceRegistry,
    pub inbound: Arc<dyn DeviceInbound>,
    pub live: LiveSockets,
}

pub fn router(runtime: DeviceWsRuntime) -> Router {
    Router::new()
        .route(WS_PATH, get(runtime_ws))
        .with_state(runtime)
}

async fn runtime_ws(
    State(rt): State<DeviceWsRuntime>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    accept(rt.registry, rt.inbound, rt.live, headers, ws).await
}

enum PresentedToken {
    Absent,
    Invalid,
    Token(String),
}

fn presented_token(headers: &HeaderMap) -> PresentedToken {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return PresentedToken::Absent;
    };
    let Ok(value) = value.to_str() else {
        return PresentedToken::Invalid;
    };
    let mut parts = value.splitn(2, char::is_whitespace);
    let scheme = parts.next().unwrap_or("");
    let token = parts.next().unwrap_or("").trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return PresentedToken::Invalid;
    }
    PresentedToken::Token(token.to_string())
}

fn unauthorized_http() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": { "code": "unauthorized", "message": "invalid device token" }
        })),
    )
        .into_response()
}

fn store_http() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": { "code": "internal_error", "message": "device token store unavailable" }
        })),
    )
        .into_response()
}

pub(crate) enum HeaderAuth {
    Absent,
    Device(DeviceRecord),
    Reject(Response),
}

/// Bearer check shared by the socket and the audio routes.
/// Absent is not a rejection: the socket may still send a first `auth` frame.
pub(crate) async fn header_auth(registry: &DeviceRegistry, headers: &HeaderMap) -> HeaderAuth {
    match presented_token(headers) {
        PresentedToken::Absent => HeaderAuth::Absent,
        PresentedToken::Invalid => HeaderAuth::Reject(unauthorized_http()),
        PresentedToken::Token(token) => match registry.authenticate(&token).await {
            Ok(Some(device)) => HeaderAuth::Device(device),
            Ok(None) => HeaderAuth::Reject(unauthorized_http()),
            Err(e) => {
                tracing::warn!(error = %e, "device token check failed");
                HeaderAuth::Reject(store_http())
            }
        },
    }
}

/// Upgrade, or reject a bad bearer before the socket exists.
pub async fn accept(
    registry: DeviceRegistry,
    inbound: Arc<dyn DeviceInbound>,
    live: LiveSockets,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let preauth = match header_auth(&registry, &headers).await {
        HeaderAuth::Absent => None,
        HeaderAuth::Device(device) => Some(device),
        HeaderAuth::Reject(response) => return response,
    };
    ws.max_message_size(256 * 1024)
        .max_frame_size(256 * 1024)
        .on_upgrade(move |socket| session(socket, registry, inbound, live, preauth))
}

struct Hello {
    device_id: Option<String>,
    room: Option<String>,
    fw: Option<String>,
}

async fn session(
    mut socket: WebSocket,
    registry: DeviceRegistry,
    inbound: Arc<dyn DeviceInbound>,
    live: LiveSockets,
    preauth: Option<DeviceRecord>,
) {
    let device = if let Some(device) = preauth {
        if !send_ack(&mut socket, "authenticated", &device).await {
            return;
        }
        device
    } else {
        match await_auth(&mut socket, &registry).await {
            Some(device) => device,
            None => return,
        }
    };
    let (generation, mut outbound, _guard) = live.register(&device);
    let mut hello = Hello {
        device_id: None,
        room: None,
        fw: None,
    };
    let mut outbound_open = true;
    loop {
        tokio::select! {
            biased;
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else {
                    break;
                };
                if !on_live_frame(
                    &mut socket,
                    &registry,
                    &inbound,
                    &device,
                    &mut hello,
                    &LiveWrite {
                        live: &live,
                        generation,
                    },
                    msg,
                )
                .await
                {
                    break;
                }
            }
            outgoing = outbound.recv(), if outbound_open => {
                match outgoing {
                    Some(frame) => {
                        if !send_json(&mut socket, &frame).await {
                            break;
                        }
                    }
                    None => outbound_open = false,
                }
            }
        }
    }
}

enum AuthWait {
    Ready(DeviceRecord),
    /// Frame was not a successful auth. The socket stays up until the deadline.
    Continue,
    Stop,
}

/// First-frame auth window. `user_text` / `ping` before `auth` are rejected
/// and the socket stays open so the client can still send `auth`. A bad
/// token closes. The deadline is one 15s budget for the whole window.
async fn await_auth(socket: &mut WebSocket, registry: &DeviceRegistry) -> Option<DeviceRecord> {
    let deadline = tokio::time::Instant::now() + AUTH_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let _ = send_json(
                socket,
                &error_body("unauthorized", "authentication required"),
            )
            .await;
            close_socket(socket).await;
            return None;
        }
        let msg = match tokio::time::timeout(remaining, socket.recv()).await {
            Ok(Some(Ok(msg))) => msg,
            _ => {
                let _ = send_json(
                    socket,
                    &error_body("unauthorized", "authentication required"),
                )
                .await;
                close_socket(socket).await;
                return None;
            }
        };
        match classify_auth(socket, registry, msg).await {
            AuthWait::Ready(device) => return Some(device),
            AuthWait::Continue => continue,
            AuthWait::Stop => return None,
        }
    }
}

async fn classify_auth(
    socket: &mut WebSocket,
    registry: &DeviceRegistry,
    msg: Message,
) -> AuthWait {
    let text = match msg {
        Message::Text(text) => text,
        Message::Close(_) => return AuthWait::Stop,
        Message::Ping(payload) => {
            if socket.send(Message::Pong(payload)).await.is_err() {
                return AuthWait::Stop;
            }
            return AuthWait::Continue;
        }
        Message::Pong(_) => return AuthWait::Continue,
        Message::Binary(_) => {
            if !send_json(socket, &error_body("bad_frame", "json text frames only")).await {
                return AuthWait::Stop;
            }
            return AuthWait::Continue;
        }
    };
    let frame = match parse_frame(&text) {
        Ok(frame) => frame,
        Err(body) => {
            if !send_json(socket, &body).await {
                return AuthWait::Stop;
            }
            return AuthWait::Continue;
        }
    };
    if frame.kind != "auth" {
        if !send_json(
            socket,
            &error_body("unauthorized", "authentication required"),
        )
        .await
        {
            return AuthWait::Stop;
        }
        return AuthWait::Continue;
    }
    match authenticate_frame(registry, frame.token.as_deref()).await {
        AuthFrame::Ok(device) => {
            if send_ack(socket, "authenticated", &device).await {
                AuthWait::Ready(device)
            } else {
                AuthWait::Stop
            }
        }
        AuthFrame::Reject(body) => {
            let _ = send_json(socket, &body).await;
            close_socket(socket).await;
            AuthWait::Stop
        }
    }
}

enum AuthFrame {
    Ok(DeviceRecord),
    Reject(Value),
}

async fn authenticate_frame(registry: &DeviceRegistry, token: Option<&str>) -> AuthFrame {
    let Some(token) = token.map(str::trim).filter(|s| !s.is_empty()) else {
        return AuthFrame::Reject(error_body("unauthorized", "authentication required"));
    };
    match registry.authenticate(token).await {
        Ok(Some(device)) => AuthFrame::Ok(device),
        Ok(None) => AuthFrame::Reject(error_body("unauthorized", "invalid device token")),
        Err(e) => {
            tracing::warn!(error = %e, "device websocket token check failed");
            AuthFrame::Reject(error_body("internal", "device token store unavailable"))
        }
    }
}

struct LiveWrite<'a> {
    live: &'a LiveSockets,
    generation: u64,
}

async fn on_live_frame(
    socket: &mut WebSocket,
    registry: &DeviceRegistry,
    inbound: &Arc<dyn DeviceInbound>,
    device: &DeviceRecord,
    hello: &mut Hello,
    write: &LiveWrite<'_>,
    msg: Message,
) -> bool {
    let text = match msg {
        Message::Text(text) => text,
        Message::Close(_) => return false,
        Message::Ping(payload) => return socket.send(Message::Pong(payload)).await.is_ok(),
        Message::Pong(_) => return true,
        Message::Binary(_) => {
            return send_json(socket, &error_body("bad_frame", "json text frames only")).await;
        }
    };
    let frame = match parse_frame(&text) {
        Ok(frame) => frame,
        Err(body) => return send_json(socket, &body).await,
    };
    match frame.kind.as_str() {
        "ping" => send_json(socket, &json!({ "type": "pong" })).await,
        "device_hello" => {
            if let Some(id) = sanitize_meta(frame.device_id.as_deref()) {
                hello.device_id = Some(id);
            }
            if let Some(room) = sanitize_meta(frame.room.as_deref()) {
                hello.room = Some(room);
            }
            if let Some(fw) = sanitize_meta(frame.fw_version.as_deref()) {
                hello.fw = Some(fw);
            }
            let (shown, room, fw) = turn_meta(device, hello);
            write.live.note_hello(
                &device.id,
                write.generation,
                &shown,
                room.as_deref(),
                fw.as_deref(),
            );
            send_json(socket, &ack_body("hello", &shown, room.as_deref())).await
        }
        "user_text" => on_user_text(socket, inbound, device, hello, frame.text.as_deref()).await,
        "auth" => match authenticate_frame(registry, frame.token.as_deref()).await {
            AuthFrame::Ok(next) if next.id == device.id => {
                send_ack(socket, "authenticated", device).await
            }
            AuthFrame::Ok(_) => {
                let _ =
                    send_json(socket, &error_body("unauthorized", "invalid device token")).await;
                close_socket(socket).await;
                false
            }
            AuthFrame::Reject(body) => {
                let _ = send_json(socket, &body).await;
                close_socket(socket).await;
                false
            }
        },
        _ => send_json(socket, &error_body("bad_frame", "unknown frame type")).await,
    }
}

async fn on_user_text(
    socket: &mut WebSocket,
    inbound: &Arc<dyn DeviceInbound>,
    device: &DeviceRecord,
    hello: &Hello,
    text: Option<&str>,
) -> bool {
    let Some(text) = text.map(str::trim).filter(|s| !s.is_empty()) else {
        return send_json(socket, &error_body("bad_frame", "user_text requires text")).await;
    };
    if text.chars().count() > MAX_USER_TEXT_CHARS {
        return send_json(socket, &error_body("bad_frame", "user_text is too long")).await;
    }
    let (device_id, room, fw) = turn_meta(device, hello);
    let msg = DeviceUserText {
        text: text.to_string(),
        device_id,
        room,
        fw_version: fw,
        token_device_id: device.id.clone(),
    };
    match inbound.submit(msg).await {
        Ok(DeviceTurnOutcome::Replies(replies)) => {
            for reply in replies {
                if !send_json(socket, &assistant_text_frame(&reply)).await {
                    return false;
                }
            }
            true
        }
        Ok(DeviceTurnOutcome::Queued) => {
            send_json(
                socket,
                &ack_body("queued", &device.id, device.room.as_deref()),
            )
            .await
        }
        Ok(DeviceTurnOutcome::QueueFull) => {
            send_json(
                socket,
                &error_body("queue_full", "too many messages are waiting"),
            )
            .await
        }
        Err(e) => {
            tracing::warn!(error = %e, device_id = %device.id, "device turn failed");
            send_json(socket, &error_body("agent", "the assistant turn failed")).await
        }
    }
}

fn turn_meta(device: &DeviceRecord, hello: &Hello) -> (String, Option<String>, Option<String>) {
    let device_id = hello
        .device_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| device.id.clone());
    let room = hello
        .room
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| device.room.clone());
    (device_id, room, hello.fw.clone())
}

#[derive(Deserialize)]
struct ClientFrame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    fw_version: Option<String>,
}

fn parse_frame(text: &str) -> Result<ClientFrame, Value> {
    if text.len() > 256 * 1024 {
        return Err(error_body("bad_frame", "frame is too large"));
    }
    serde_json::from_str(text).map_err(|_| error_body("bad_frame", "expected a JSON text frame"))
}

pub(crate) fn assistant_text_frame(text: &str) -> Value {
    json!({ "type": "assistant_text", "text": text })
}

pub(crate) fn error_body(code: &str, message: &str) -> Value {
    json!({ "type": "error", "code": code, "message": message })
}

pub(crate) fn ack_body(status: &str, device_id: &str, room: Option<&str>) -> Value {
    let mut body = json!({
        "type": "device_ack",
        "status": status,
        "device_id": device_id,
    });
    if let Some(room) = room {
        body["room"] = json!(room);
    }
    body
}

async fn send_ack(socket: &mut WebSocket, status: &str, device: &DeviceRecord) -> bool {
    send_json(
        socket,
        &ack_body(status, &device.id, device.room.as_deref()),
    )
    .await
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> bool {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

async fn close_socket(socket: &mut WebSocket) {
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::FakeSecretStore;
    use crate::turn_gate::TurnGate;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg(toml_text: &str) -> Config {
        let mut cfg: Config = toml::from_str(toml_text).unwrap();
        cfg.normalize_bots().unwrap();
        cfg
    }

    #[test]
    fn device_ws_defaults_off() {
        let cfg: Config = toml::from_str(
            r#"
            [openrouter]
            api_key = "k"
            "#,
        )
        .unwrap();
        assert!(!cfg.device_ws.enabled);

        let cfg: Config = toml::from_str(
            r#"
            [openrouter]
            api_key = "k"
            [device_ws]
            enabled = true
            "#,
        )
        .unwrap();
        assert!(cfg.device_ws.enabled);
    }

    #[test]
    fn owner_chat_uses_shim_positive_allowlist_id() {
        let cfg = cfg(r#"
            [openrouter]
            api_key = "k"

            [[bots]]
            id = "other"
            bot_token = "a"
            allowed_user_ids = [9]

            [[bots]]
            id = "main"
            bot_token = "b"
            allowed_user_ids = [0, 42]
            "#);
        let owner = owner_chat(&cfg).unwrap();
        assert_eq!(owner.bot_id, "main");
        assert_eq!(owner.user_id, "42");
        assert_eq!(owner.chat_id, "42");
        assert_eq!(owner_session_key(&owner), "main:42");
    }

    #[test]
    fn owner_chat_rejects_unowned_sentinel() {
        let cfg = cfg(r#"
            [telegram]
            bot_token = "t"
            allowed_user_ids = [0]

            [openrouter]
            api_key = "k"
            "#);
        assert!(owner_chat(&cfg).is_err());
    }

    #[test]
    fn annotate_is_identity_without_metadata_and_keeps_unicode_room() {
        assert_eq!(annotate_user_text("Hi", None, None, None), "Hi");
        let noted = annotate_user_text("燈", Some("board-1"), Some("客廳"), Some("1.2"));
        assert!(noted.starts_with("燈\n\n[device "));
        assert!(noted.contains("device_id=board-1"));
        assert!(noted.contains("room=客廳"));
        assert!(noted.contains("fw=1.2"));
        assert!(!noted.contains('[') || noted.matches('[').count() == 1);
    }

    async fn registry() -> (DeviceRegistry, Arc<FakeSecretStore>) {
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let secrets = Arc::new(FakeSecretStore::new());
        let registry = DeviceRegistry::open(memory.connection(), secrets.clone())
            .await
            .unwrap();
        (registry, secrets)
    }

    #[tokio::test]
    async fn create_stores_hash_not_plaintext_and_revoke_rotate_reject() {
        let (registry, secrets) = registry().await;
        let issued = registry.create("kitchen", Some("客廳")).await.unwrap();
        let hash = sha256_hex(&issued.token);
        assert_ne!(hash, issued.token);
        assert_eq!(
            registry
                .stored_hash(&issued.record.id)
                .await
                .unwrap()
                .as_deref(),
            Some(hash.as_str())
        );
        let sealed = secrets
            .get(&device_secret_name(&issued.record.id))
            .unwrap()
            .unwrap();
        assert_eq!(sealed.expose(), hash);
        assert_ne!(sealed.expose(), issued.token);

        let listed = registry.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "kitchen");
        assert!(registry
            .authenticate(&issued.token)
            .await
            .unwrap()
            .is_some());
        assert!(registry.authenticate("nope").await.unwrap().is_none());

        let rotated = registry.rotate(&issued.record.id).await.unwrap();
        assert_ne!(rotated.token, issued.token);
        assert!(registry
            .authenticate(&issued.token)
            .await
            .unwrap()
            .is_none());
        assert!(registry
            .authenticate(&rotated.token)
            .await
            .unwrap()
            .is_some());

        assert!(registry.revoke(&issued.record.id).await.unwrap());
        assert!(registry
            .authenticate(&rotated.token)
            .await
            .unwrap()
            .is_none());
        assert!(registry.list().await.unwrap().is_empty());
        assert!(!registry.revoke(&issued.record.id).await.unwrap());
    }

    fn owner() -> OwnerChat {
        OwnerChat {
            bot_id: "default".into(),
            user_id: "42".into(),
            chat_id: "42".into(),
            user_name: "user-42".into(),
        }
    }

    fn user_text(text: &str) -> DeviceUserText {
        DeviceUserText {
            text: text.into(),
            device_id: "board-1".into(),
            room: Some("kitchen".into()),
            fw_version: Some("9".into()),
            token_device_id: "reg".into(),
        }
    }

    #[tokio::test]
    async fn user_text_starts_interactive_lane_and_busy_lane_queues() {
        let gate = TurnGate::new();
        let owner = owner();
        let key = owner_session_key(&owner);
        let seen = Arc::new(Mutex::new(None));
        let seen_run = Arc::clone(&seen);
        let outcome = submit_for_owner(
            &gate,
            &owner,
            user_text("turn on the lamp"),
            move |queued| {
                let seen_run = Arc::clone(&seen_run);
                Box::pin(async move {
                    *seen_run.lock().await = Some((
                        queued.text.clone(),
                        queued.device_id.clone(),
                        queued.device_room.clone(),
                        queued.device_fw.clone(),
                    ));
                    Ok(format!("echo:{}", queued.text))
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            DeviceTurnOutcome::Replies(vec!["echo:turn on the lamp".into()])
        );
        assert!(!gate.interactive_active(&key).await);
        let seen = seen.lock().await.clone().unwrap();
        assert_eq!(seen.0, "turn on the lamp");
        assert_eq!(seen.1.as_deref(), Some("board-1"));
        assert_eq!(seen.2.as_deref(), Some("kitchen"));
        assert_eq!(seen.3.as_deref(), Some("9"));

        let _started = gate
            .submit(
                &key,
                QueuedInput {
                    text: "hold".into(),
                    ..Default::default()
                },
            )
            .await;
        let runs = Arc::new(AtomicUsize::new(0));
        let runs2 = Arc::clone(&runs);
        let queued = submit_for_owner(&gate, &owner, user_text("later"), move |_queued| {
            runs2.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok("no".into()) })
        })
        .await
        .unwrap();
        assert_eq!(queued, DeviceTurnOutcome::Queued);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(gate.pending_len(&key).await, 1);
    }
}
