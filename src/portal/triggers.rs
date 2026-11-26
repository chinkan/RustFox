//! Portal event triggers (ADR-0068).
//!
//! Authenticated list / create / enable / disable / HMAC secret.
//! `POST /api/hooks/{id}` is public on this bind and checks HMAC instead.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{SecondsFormat, Utc};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};

use super::error::PortalError;
use super::PortalState;
use crate::config::BotConfig;
use crate::event_trigger::{
    self, AcceptFailure, AcceptInput, AcceptOutcome, Trigger, BUSY_RETRY_AFTER_SECS,
};
use crate::platform::{normalize_bot_id, DEFAULT_BOT_ID};

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn configured_bots(bots: &[BotConfig]) -> Vec<Value> {
    if bots.is_empty() {
        return vec![json!({ "id": DEFAULT_BOT_ID, "persona": "main" })];
    }
    bots.iter()
        .map(|bot| {
            json!({
                "id": normalize_bot_id(&bot.id),
                "persona": bot.persona,
            })
        })
        .collect()
}

/// Stamp the bot the operator named. Several configured bots and no `botId`
/// is an error: this path does not pick a shim.
fn resolve_bot_id(bots: &[BotConfig], requested: Option<&str>) -> Result<String, PortalError> {
    let requested = requested.map(str::trim).filter(|id| !id.is_empty());
    match requested {
        Some(id) => {
            let norm = normalize_bot_id(id).to_string();
            let known = if bots.is_empty() {
                norm == DEFAULT_BOT_ID
            } else {
                bots.iter().any(|bot| normalize_bot_id(&bot.id) == norm)
            };
            if known {
                Ok(norm)
            } else {
                Err(PortalError::bad_request(
                    "unknown_bot",
                    format!("unknown bot id '{id}'"),
                ))
            }
        }
        None if bots.len() <= 1 => {
            if let Some(bot) = bots.first() {
                Ok(normalize_bot_id(&bot.id).to_string())
            } else {
                Ok(DEFAULT_BOT_ID.to_string())
            }
        }
        None => Err(PortalError::bad_request(
            "bot_id_required",
            "botId is required when more than one bot is configured",
        )),
    }
}

fn trigger_json(trigger: &Trigger) -> Value {
    json!({
        "id": trigger.id,
        "name": trigger.name,
        "prompt": trigger.prompt,
        "botId": trigger.bot_id,
        "enabled": trigger.enabled,
        "webhookPath": event_trigger::webhook_path(&trigger.id),
        "secretName": trigger.secret_name,
        "createdAt": trigger.created_at,
    })
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn error_response(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
    retry_after: Option<u64>,
) -> Response {
    let mut response = (
        status,
        Json(json!({ "error": { "code": code, "message": message.into() } })),
    )
        .into_response();
    if let Some(secs) = retry_after {
        if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

fn failure_response(failure: AcceptFailure) -> Response {
    match failure {
        AcceptFailure::NotFound => PortalError::not_found("trigger").into_response(),
        AcceptFailure::BadSignature => error_response(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "HMAC signature missing or invalid",
            None,
        ),
        AcceptFailure::Disabled => error_response(
            StatusCode::FORBIDDEN,
            "trigger_disabled",
            "trigger is disabled",
            None,
        ),
        AcceptFailure::PayloadTooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!(
                "raw body must be at most {} bytes",
                event_trigger::MAX_RAW_BODY
            ),
            None,
        ),
        AcceptFailure::IdempotencyKey(_) => error_response(
            StatusCode::BAD_REQUEST,
            "idempotency_key_too_long",
            format!(
                "Idempotency-Key must be at most {} bytes",
                event_trigger::MAX_IDEMPOTENCY_KEY_LEN
            ),
            None,
        ),
        AcceptFailure::RateLimited { retry_after_secs } => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many accepted events for this trigger",
            Some(retry_after_secs),
        ),
        AcceptFailure::Busy { retry_after_secs } => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "event_busy",
            "an event run is already active",
            Some(retry_after_secs.max(BUSY_RETRY_AFTER_SECS)),
        ),
        AcceptFailure::Internal(err) => PortalError::internal(err).into_response(),
    }
}

/// POST /api/hooks/{id} — HMAC webhook. No portal session.
pub async fn webhook(
    State(state): State<PortalState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signature = header_value(&headers, event_trigger::SIGNATURE_HEADER);
    let idempotency = header_value(&headers, "idempotency-key");
    let x_idempotency = header_value(&headers, "x-idempotency-key");
    let conn = state.memory.connection();
    let outcome = event_trigger::accept(
        &conn,
        state.secret_store.as_ref(),
        AcceptInput {
            trigger_id: &id,
            body: &body,
            signature: signature.as_deref(),
            idempotency_key: idempotency.as_deref(),
            x_idempotency_key: x_idempotency.as_deref(),
            now: Utc::now(),
        },
    )
    .await;
    let fire = match outcome {
        Ok(AcceptOutcome::Started(fire)) => *fire,
        Ok(AcceptOutcome::Duplicate { run_id }) => {
            return (
                StatusCode::OK,
                Json(json!({ "ok": true, "runId": run_id, "deduped": true })),
            )
                .into_response();
        }
        Err(failure) => return failure_response(failure),
    };
    let run_id = fire.run_id.clone();
    let trigger_id = fire.trigger.id.clone();
    if let Err(err) = state.agent.dispatch_event_run(fire).await {
        if let Err(abandon) =
            event_trigger::abandon_accept(&conn, &trigger_id, &run_id, "dispatch failed").await
        {
            tracing::warn!(error = %abandon, "failed to roll back event accept");
        }
        return PortalError::internal(err).into_response();
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({ "ok": true, "runId": run_id, "deduped": false })),
    )
        .into_response()
}

/// GET /api/triggers
pub async fn list(State(state): State<PortalState>) -> Result<Json<Value>, PortalError> {
    let conn = state.memory.connection();
    let guard = conn.lock().await;
    let triggers = event_trigger::list_triggers(&guard).map_err(PortalError::internal)?;
    Ok(Json(json!({
        "bots": configured_bots(&state.agent.config().bots),
        "triggers": triggers.iter().map(trigger_json).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct CreateTriggerBody {
    name: String,
    prompt: String,
    #[serde(default, rename = "botId")]
    bot_id: Option<String>,
    /// When omitted, a secret is generated and returned once.
    #[serde(default)]
    secret: Option<String>,
}

/// POST /api/triggers
pub async fn create(
    State(state): State<PortalState>,
    Json(body): Json<CreateTriggerBody>,
) -> Result<(StatusCode, Json<Value>), PortalError> {
    let name = body.name.trim();
    let prompt = body.prompt.trim();
    if name.is_empty() {
        return Err(PortalError::bad_request(
            "empty_name",
            "name must not be empty",
        ));
    }
    if name.len() > 200 {
        return Err(PortalError::bad_request(
            "name_too_long",
            "name is too long",
        ));
    }
    if prompt.is_empty() {
        return Err(PortalError::bad_request(
            "empty_prompt",
            "prompt must not be empty",
        ));
    }
    if prompt.len() > 16_000 {
        return Err(PortalError::bad_request(
            "prompt_too_long",
            "prompt is too long",
        ));
    }
    let bot_id = resolve_bot_id(&state.agent.config().bots, body.bot_id.as_deref())?;
    let (secret, generated) = match body
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(secret) => {
            if secret.len() > 512 {
                return Err(PortalError::bad_request(
                    "secret_too_long",
                    "secret is too long",
                ));
            }
            (secret.to_string(), None)
        }
        None => {
            let bytes: [u8; 32] = rand::rng().random();
            let mut hex = String::with_capacity(64);
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            let generated = hex.clone();
            (hex, Some(generated))
        }
    };

    let id = uuid::Uuid::new_v4().to_string();
    let secret_name = event_trigger::secret_name(&id);
    state
        .secret_store
        .set(&secret_name, &secret)
        .map_err(PortalError::internal)?;
    let trigger = Trigger {
        id: id.clone(),
        name: name.to_string(),
        prompt: prompt.to_string(),
        bot_id,
        user_id: state.config.user_name.clone(),
        chat_id: state
            .agent
            .config()
            .telegram
            .allowed_user_ids
            .first()
            .map(|user| user.to_string())
            .unwrap_or_default(),
        platform: "portal".to_string(),
        enabled: true,
        secret_name: secret_name.clone(),
        created_at: now_iso(),
    };
    let insert = {
        let conn = state.memory.connection();
        let guard = conn.lock().await;
        event_trigger::insert_trigger(&guard, &trigger)
    };
    if let Err(err) = insert {
        let _ = state.secret_store.delete(&secret_name);
        return Err(PortalError::internal(err));
    }
    let mut payload = trigger_json(&trigger);
    if let Some(secret) = generated {
        payload["secret"] = json!(secret);
    }
    Ok((StatusCode::CREATED, Json(payload)))
}

async fn set_enabled(
    state: &PortalState,
    id: &str,
    enabled: bool,
) -> Result<Json<Value>, PortalError> {
    let conn = state.memory.connection();
    let guard = conn.lock().await;
    let found =
        event_trigger::set_trigger_enabled(&guard, id, enabled).map_err(PortalError::internal)?;
    if !found {
        return Err(PortalError::not_found("trigger"));
    }
    Ok(Json(json!({ "ok": true, "id": id, "enabled": enabled })))
}

/// POST /api/triggers/{id}/enable
pub async fn enable(
    State(state): State<PortalState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, PortalError> {
    set_enabled(&state, &id, true).await
}

/// POST /api/triggers/{id}/disable
pub async fn disable(
    State(state): State<PortalState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, PortalError> {
    set_enabled(&state, &id, false).await
}

#[derive(Deserialize, Default)]
pub struct RotateSecretBody {
    #[serde(default)]
    secret: Option<String>,
}

/// POST /api/triggers/{id}/secret — set or generate the HMAC secret.
/// A generated value is returned once. A supplied value is not echoed.
pub async fn rotate_secret(
    State(state): State<PortalState>,
    Path(id): Path<String>,
    Json(body): Json<RotateSecretBody>,
) -> Result<Json<Value>, PortalError> {
    let conn = state.memory.connection();
    let guard = conn.lock().await;
    let Some(trigger) = event_trigger::get_trigger(&guard, &id).map_err(PortalError::internal)?
    else {
        return Err(PortalError::not_found("trigger"));
    };
    drop(guard);
    let (secret, generated) = match body
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(secret) => {
            if secret.len() > 512 {
                return Err(PortalError::bad_request(
                    "secret_too_long",
                    "secret is too long",
                ));
            }
            (secret.to_string(), false)
        }
        None => {
            let bytes: [u8; 32] = rand::rng().random();
            let mut hex = String::with_capacity(64);
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            (hex, true)
        }
    };
    state
        .secret_store
        .set(&trigger.secret_name, &secret)
        .map_err(PortalError::internal)?;
    let mut payload = json!({ "ok": true, "id": id, "secretName": trigger.secret_name });
    if generated {
        payload["secret"] = json!(secret);
    }
    Ok(Json(payload))
}
