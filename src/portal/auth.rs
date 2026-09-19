//! Static bearer-token auth + in-memory sessions (ADR 0006).
//!
//! Accepted credentials per request (first match wins):
//! 1. `rustfox_session` cookie — random 64-hex id issued by `/api/auth/login`,
//!    held in `PortalState::sessions` (all sessions cleared on restart).
//! 2. `Authorization: Bearer <portal token>` — raw token validated against
//!    `[portal] token` (plaintext) or `[portal] token_sha256` (preferred).
//!
//! If neither is configured, a random token is generated at startup and logged
//! once so the operator can log in and set a stable one.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::error::PortalError;
use super::PortalState;

pub const SESSION_COOKIE: &str = "rustfox_session";

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Length-checked, XOR-folded comparison of two hex strings.
fn hex_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Check a candidate token against the configured token or its hash.
pub fn verify_token(state: &PortalState, candidate: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if let Some(hash) = state.config.token_sha256.as_deref() {
        if !hash.is_empty() && hex_eq(&sha256_hex(candidate), &hash.to_ascii_lowercase()) {
            return true;
        }
    }
    match state.config.token.as_deref() {
        Some(raw) => !raw.is_empty() && raw == candidate,
        None => false,
    }
}

/// Generate (once) and log a dev token when none is configured. Called from
/// `serve()` so operators see it on stderr at startup.
pub fn ensure_startup_token(state: &PortalState) {
    let has_token = state.config.token.as_deref().is_some_and(|t| !t.is_empty());
    let has_hash = state
        .config
        .token_sha256
        .as_deref()
        .is_some_and(|h| !h.is_empty());
    if has_token || has_hash {
        return;
    }
    let generated = uuid::Uuid::new_v4().to_string().replace('-', "");
    tracing::warn!(
        "Portal: no [portal] token configured — temporary login token: {generated}\n\
         Portal: log in, then set [portal] token_sha256 = \"{}\" in config.toml for stable auth.",
        sha256_hex(&generated)
    );
    let mut guard = state
        .sessions
        .try_lock()
        .expect("sessions map uncontended at startup");
    // Store the generated token as a bearer credential via the session map
    // (session value is the username; key is the presented token).
    guard.insert(generated, state.config.user_name.clone());
}

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub token: String,
}

pub async fn login(
    State(state): State<PortalState>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, PortalError> {
    // Accept the configured token, or a startup-generated token stored in the
    // session map (dev convenience when nothing was configured).
    let stored_session = {
        let sessions = state.sessions.lock().await;
        sessions.get(&req.token).cloned()
    };
    let username = match stored_session {
        Some(user) => Some(user),
        None if verify_token(&state, &req.token) => Some(state.config.user_name.clone()),
        None => None,
    };
    let Some(username) = username else {
        tracing::warn!("Portal login: invalid token attempt");
        return Err(PortalError::unauthorized("Invalid portal token"));
    };

    let session_id = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), username.clone());

    let mut res = Json(json!({ "username": username, "role": "admin" })).into_response();
    if let Ok(v) = format!("{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Strict")
        .parse()
    {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(res)
}

pub async fn logout(State(state): State<PortalState>, headers: HeaderMap) -> Response {
    if let Some(sid) = cookie_value(&headers) {
        state.sessions.lock().await.remove(&sid);
    }
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
        )],
    )
        .into_response()
}

/// Auth status for the SPA guard — public per API contract; reports whether
/// the presented credentials (if any) are valid.
pub async fn me(State(state): State<PortalState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let authed = is_authenticated(&state, &headers).await;
    Json(json!({
        "authenticated": authed,
        "username": if authed { state.config.user_name.as_str() } else { "" },
        "role": if authed { "admin" } else { "" },
    }))
}

async fn is_authenticated(state: &PortalState, headers: &HeaderMap) -> bool {
    if let Some(sid) = cookie_value(headers) {
        if state.sessions.lock().await.contains_key(&sid) {
            return true;
        }
    }
    if let Some(bearer) = bearer_value(headers) {
        if verify_token(state, &bearer) {
            return true;
        }
    }
    false
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        part.trim()
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .map(str::to_string)
            .filter(|v| !v.is_empty())
    })
}

fn bearer_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Middleware guarding all protected routes.
pub async fn require_auth(
    State(state): State<PortalState>,
    req: Request,
    next: Next,
) -> Result<Response, PortalError> {
    let headers = req.headers().clone();
    if is_authenticated(&state, &headers).await {
        return Ok(next.run(req).await);
    }
    Err(PortalError::new(
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "Valid session cookie or bearer token required",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        // echo -n "abc" | sha256sum
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hex_eq_exact_bytes_length_checked() {
        assert!(hex_eq("abcd", "abcd"));
        assert!(!hex_eq("ABCD", "abcd")); // exact bytes; verify_token lowercases first
        assert!(!hex_eq("abc", "abcd"));
        assert!(!hex_eq("abcd", "abce"));
    }

    #[test]
    fn verify_token_accepts_hash_lowercase_form() {
        // Mirrors verify_token's hash path without needing a full PortalState.
        let hash = sha256_hex("s3cret");
        assert!(hex_eq(&sha256_hex("s3cret"), &hash.to_ascii_lowercase()));
        let upper = hash.to_ascii_uppercase();
        assert!(!hex_eq(&sha256_hex("s3cret"), &upper)); // caller must lowercase
    }

    #[test]
    fn cookie_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=1; rustfox_session=deadbeef; x=y".parse().unwrap(),
        );
        assert_eq!(cookie_value(&headers).as_deref(), Some("deadbeef"));
        headers.insert(header::COOKIE, "other=1".parse().unwrap());
        assert_eq!(cookie_value(&headers), None);
    }

    #[test]
    fn bearer_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer tok123".parse().unwrap());
        assert_eq!(bearer_value(&headers).as_deref(), Some("tok123"));
        headers.insert(header::AUTHORIZATION, "Basic xyz".parse().unwrap());
        assert_eq!(bearer_value(&headers), None);
    }
}
