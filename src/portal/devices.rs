//! Portal Settings → Devices (ADR-0056).
//!
//! Create / rotate / revoke per-device tokens. The plaintext token is in the
//! JSON body of create and rotate only. List rows never include it. These
//! tokens are not accepted as a portal login.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{json, Value};

use crate::device_audio::{self, AudioHost, LibraryDeviceAudio};
use crate::device_ws::{
    self, DeviceInbound, DeviceRecord, DeviceTurnOutcome, DeviceUserText, IssuedDevice,
};

use super::error::PortalError;
use super::PortalState;

#[derive(Deserialize)]
pub struct CreateDeviceBody {
    pub label: String,
    #[serde(default)]
    pub room: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceJson {
    id: String,
    label: String,
    room: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IssuedJson {
    id: String,
    label: String,
    room: Option<String>,
    created_at: String,
    token: String,
}

fn view(record: &DeviceRecord) -> DeviceJson {
    DeviceJson {
        id: record.id.clone(),
        label: record.label.clone(),
        room: record.room.clone(),
        created_at: record.created_at.clone(),
    }
}

fn issued_view(issued: IssuedDevice) -> IssuedJson {
    IssuedJson {
        id: issued.record.id,
        label: issued.record.label,
        room: issued.record.room,
        created_at: issued.record.created_at,
        token: issued.token,
    }
}

fn map_store(err: anyhow::Error) -> PortalError {
    if let Some(invalid) = err.downcast_ref::<crate::device_ws::InvalidDevice>() {
        return PortalError::bad_request("invalid_device", invalid.to_string());
    }
    if err
        .downcast_ref::<crate::device_ws::DeviceNotFound>()
        .is_some()
    {
        return PortalError::not_found("device");
    }
    PortalError::from(err)
}

/// GET /api/settings/devices
pub async fn list_devices(State(state): State<PortalState>) -> Result<Json<Value>, PortalError> {
    let registry = state.device_registry().await?;
    let devices = registry.list().await?;
    Ok(Json(json!({
        "enabled": state.agent.config().device_ws.enabled,
        "path": device_ws::WS_PATH,
        "devices": devices.iter().map(view).collect::<Vec<_>>(),
    })))
}

/// POST /api/settings/devices — plaintext `token` is shown once.
pub async fn create_device(
    State(state): State<PortalState>,
    Json(body): Json<CreateDeviceBody>,
) -> Result<Json<Value>, PortalError> {
    let registry = state.device_registry().await?;
    let issued = registry
        .create(&body.label, body.room.as_deref())
        .await
        .map_err(map_store)?;
    Ok(Json(
        serde_json::to_value(issued_view(issued)).map_err(PortalError::internal)?,
    ))
}

/// POST /api/settings/devices/{id}/rotate — new plaintext shown once.
pub async fn rotate_device(
    State(state): State<PortalState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, PortalError> {
    let registry = state.device_registry().await?;
    let issued = registry.rotate(&id).await.map_err(map_store)?;
    Ok(Json(
        serde_json::to_value(issued_view(issued)).map_err(PortalError::internal)?,
    ))
}

/// DELETE /api/settings/devices/{id}
pub async fn revoke_device(
    State(state): State<PortalState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, PortalError> {
    let registry = state.device_registry().await?;
    let revoked = registry.revoke(&id).await?;
    if !revoked {
        return Err(PortalError::not_found("device"));
    }
    Ok(Json(json!({ "revoked": true })))
}

struct PortalDeviceInbound(Arc<dyn super::AgentOps>);

#[async_trait]
impl DeviceInbound for PortalDeviceInbound {
    async fn submit(&self, msg: DeviceUserText) -> anyhow::Result<DeviceTurnOutcome> {
        self.0.submit_device_turn(msg).await
    }
}

/// GET /api/device/v1/ws — mounted only when `[device_ws] enabled`.
pub async fn ws(
    State(state): State<PortalState>,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    if !state.agent.config().device_ws.enabled {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({
                "error": { "code": "not_found", "message": "device websocket is disabled" }
            })),
        )
            .into_response();
    }
    let registry = match state.device_registry().await {
        Ok(registry) => registry,
        Err(e) => return PortalError::internal(e).into_response(),
    };
    let inbound: Arc<dyn DeviceInbound> = Arc::new(PortalDeviceInbound(Arc::clone(&state.agent)));
    device_ws::accept(
        registry,
        inbound,
        state.device_live.clone(),
        headers,
        upgrade,
    )
    .await
}

fn audio_host(state: &PortalState, registry: crate::device_ws::DeviceRegistry) -> AudioHost {
    let config = state.agent.config();
    let engine = Arc::new(LibraryDeviceAudio::new(
        state.voice.snapshot(),
        config.openrouter.base_url.clone(),
        config.openrouter.api_key.clone(),
    ));
    let inbound: Arc<dyn DeviceInbound> = Arc::new(PortalDeviceInbound(Arc::clone(&state.agent)));
    AudioHost {
        registry,
        inbound,
        live: state.device_live.clone(),
        pcm: state.device_pcm.clone(),
        engine,
    }
}

async fn open_registry(
    state: &PortalState,
) -> Result<crate::device_ws::DeviceRegistry, PortalError> {
    state.device_registry().await.map_err(PortalError::internal)
}

/// POST /api/device/v1/audio — one 16 kHz mono WAV, device Bearer token.
pub async fn post_audio(
    State(state): State<PortalState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !state.agent.config().device_ws.enabled {
        return device_disabled();
    }
    let registry = match open_registry(&state).await {
        Ok(registry) => registry,
        Err(err) => return err.into_response(),
    };
    device_audio::post(audio_host(&state, registry), headers, body).await
}

/// GET /api/device/v1/audio/{id} — 16 kHz mono PCM s16le for the uploading device.
pub async fn get_audio(
    State(state): State<PortalState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !state.agent.config().device_ws.enabled {
        return device_disabled();
    }
    let registry = match open_registry(&state).await {
        Ok(registry) => registry,
        Err(err) => return err.into_response(),
    };
    device_audio::get(&registry, &state.device_pcm, &id, &headers).await
}

fn device_disabled() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({
            "error": { "code": "not_found", "message": "device websocket is disabled" }
        })),
    )
        .into_response()
}
