//! Stateless session auth (ADR 0008A) + bearer-token path (ADR 0006).
//!
//! Accepted credentials per request (first match wins):
//! 1. `rustfox_session` cookie — `<payload-b64url>.<hmac-b64url>` where the
//!    payload is `username|issued_unix|expires_unix|gen`, signed with a
//!    per-install secret persisted as `portal_secret.key` (0600) next to the
//!    state DB. Cookies survive process restarts by construction; `gen`
//!    (a row in the SQLite `kv` table) is bumped by global logout / token
//!    rotation to invalidate every issued cookie at once.
//! 2. `Authorization: Bearer <portal token>` — raw token validated against
//!    `[portal] token` (plaintext) or `[portal] token_sha256` (preferred).
//!    This path is stateless too and is what scripted clients use.
//!
//! If no token is configured, a random one is generated at startup and logged
//! once so the operator can log in and then pin `token_sha256`.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::error::PortalError;
use super::PortalState;

pub const SESSION_COOKIE: &str = "rustfox_session";
/// Session lifetime: 30 days (ADR 0008A — restarts must not log anyone out).
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;
const GEN_KEY: &str = "portal_session_gen";

type HmacSha256 = Hmac<Sha256>;

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()
}

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

/// Check a candidate bearer token against the configured token, its hash, or
/// a startup-generated dev token.
pub fn verify_token(state: &PortalState, candidate: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if let Some(hash) = state.config.token_sha256.as_deref() {
        if !hash.is_empty() && hex_eq(&sha256_hex(candidate), &hash.to_ascii_lowercase()) {
            return true;
        }
    }
    if let Some(raw) = state.config.token.as_deref() {
        if !raw.is_empty() && raw == candidate {
            return true;
        }
    }
    // Startup-generated dev token (process lifetime only, dev convenience).
    state
        .dev_tokens
        .lock()
        .map(|v| v.iter().any(|t| t == candidate))
        .unwrap_or(false)
}

/// Generate (once) and log a dev token when none is configured. Called from
/// `serve()` so operators see it on the log at startup.
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
    if let Ok(mut v) = state.dev_tokens.lock() {
        v.push(generated);
    }
}

// ---------------------------------------------------------------------------
// HMAC secret (portal_secret.key — 32 random bytes, hex on disk, mode 0600)
// ---------------------------------------------------------------------------

/// Load or create the per-install signing secret. Cached on the state after
/// first use so cookie verification never touches the disk twice.
pub fn signing_secret(state: &PortalState) -> Result<[u8; 32], PortalError> {
    if let Some(cached) = state.secret.get() {
        return Ok(*cached);
    }
    let home = state
        .home_dir
        .clone()
        .ok_or_else(|| PortalError::internal("cannot resolve home dir for portal_secret.key"))?;
    let path = home.join("portal_secret.key");
    let key: [u8; 32] = match std::fs::read(&path) {
        Ok(bytes) => {
            let hex = String::from_utf8_lossy(&bytes);
            let mut buf = [0u8; 32];
            if !hex_decode_32(hex.trim(), &mut buf) {
                return Err(PortalError::internal("portal_secret.key is corrupt"));
            }
            buf
        }
        Err(_) => {
            let mut buf = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut buf);
            write_secret_file(&path, &buf)?;
            buf
        }
    };
    let _ = state.secret.set(key);
    Ok(key)
}

fn hex_decode_32(raw: &str, out: &mut [u8; 32]) -> bool {
    if raw.len() != 64 {
        return false;
    }
    for (i, byte) in raw.as_bytes().chunks(2).enumerate() {
        let Ok(s) = std::str::from_utf8(byte) else {
            return false;
        };
        let Ok(v) = u8::from_str_radix(s, 16) else {
            return false;
        };
        out[i] = v;
    }
    true
}

fn write_secret_file(path: &std::path::Path, key: &[u8; 32]) -> Result<(), PortalError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(PortalError::internal)?;
    }
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, hex.as_bytes()).map_err(PortalError::internal)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!("Portal: created signing secret at {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Session generation counter (SQLite kv table)
// ---------------------------------------------------------------------------

fn ensure_kv_table(conn: &rusqlite::Connection) {
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    );
}

fn gen_blocking(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT value FROM kv WHERE key = ?1",
        rusqlite::params![GEN_KEY],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(0)
}

/// Read (or initialise to 0) the session generation counter.
pub async fn current_gen(state: &PortalState) -> i64 {
    let conn = state.memory.connection();
    let conn = conn.lock().await;
    ensure_kv_table(&conn);
    gen_blocking(&conn)
}

/// Bump the generation counter — invalidates every previously issued cookie
/// (global logout / token rotation, ADR 0008A).
pub async fn bump_gen(state: &PortalState) {
    let conn = state.memory.connection();
    let conn = conn.lock().await;
    ensure_kv_table(&conn);
    let next = gen_blocking(&conn) + 1;
    let _ = conn.execute(
        "INSERT INTO kv(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![GEN_KEY, next.to_string()],
    );
}

// ---------------------------------------------------------------------------
// Cookie sign / verify
// ---------------------------------------------------------------------------

/// Issue a signed session cookie value.
pub fn issue_cookie(
    secret: &[u8; 32],
    username: &str,
    now: i64,
    gen: i64,
) -> Result<String, PortalError> {
    let payload = format!("{username}|{now}|{}|{gen}", now + SESSION_TTL_SECS);
    let mut mac = HmacSha256::new_from_slice(secret).map_err(PortalError::internal)?;
    mac.update(payload.as_bytes());
    Ok(format!("{}.{}", b64(payload.as_bytes()), b64(&mac.finalize().into_bytes())))
}

/// Parse a signed cookie into `(username, expires_at, gen)` after checking
/// the signature. Expiry/gen policy is applied by the async caller.
fn parse_cookie(secret: &[u8; 32], cookie: &str) -> Option<(String, i64, i64)> {
    let (payload_b64, tag_b64) = cookie.split_once('.')?;
    let payload = unb64(payload_b64)?;
    let tag = unb64(tag_b64)?;
    let mut mac = HmacSha256::new_from_slice(secret).ok()?;
    mac.update(&payload);
    mac.verify_slice(&tag).ok()?;
    let text = String::from_utf8(payload).ok()?;
    let mut parts = text.split('|');
    let username = parts.next()?.to_string();
    let _issued: i64 = parts.next()?.parse().ok()?;
    let expires: i64 = parts.next()?.parse().ok()?;
    let gen: i64 = parts.next()?.parse().ok()?;
    Some((username, expires, gen))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct LoginRequest {
    #[serde(default)]
    pub token: String,
}

pub async fn login(
    State(state): State<PortalState>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, PortalError> {
    if !verify_token(&state, &req.token) {
        tracing::warn!("Portal login: invalid token attempt");
        return Err(PortalError::unauthorized("Invalid portal token"));
    }
    let username = state.config.user_name.clone();
    let secret = signing_secret(&state)?;
    let gen = current_gen(&state).await;
    let cookie = issue_cookie(&secret, &username, chrono::Utc::now().timestamp(), gen)?;

    let mut res = Json(json!({ "username": username, "role": "admin" })).into_response();
    if let Ok(v) =
        format!("{SESSION_COOKIE}={cookie}; Path=/; HttpOnly; SameSite=Strict").parse()
    {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(res)
}

#[derive(Deserialize, Default)]
pub struct LogoutRequest {
    /// `true` ⇒ bump the generation counter, invalidating every issued cookie
    /// (ADR 0008A global logout). Default: client-side single-session logout.
    #[serde(default)]
    pub everywhere: bool,
}

pub async fn logout(
    State(state): State<PortalState>,
    body: Option<Json<LogoutRequest>>,
) -> Response {
    if body.map(|Json(b)| b.everywhere).unwrap_or(false) {
        bump_gen(&state).await;
        tracing::info!("Portal: session generation bumped — all cookies invalidated");
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
    let user = authenticated_user(&state, &headers).await;
    Json(json!({
        "authenticated": user.is_some(),
        "username": user.as_deref().unwrap_or(""),
        "role": if user.is_some() { "admin" } else { "" },
    }))
}

/// The authenticated username when the request carries valid credentials:
/// cookie (signature + expiry + gen) first, then bearer token.
pub async fn authenticated_user(state: &PortalState, headers: &HeaderMap) -> Option<String> {
    if let Some(cookie) = cookie_value(headers) {
        let secret = signing_secret(state).ok()?;
        if let Some((username, expires, gen)) = parse_cookie(&secret, &cookie) {
            if chrono::Utc::now().timestamp() <= expires && gen == current_gen(state).await {
                return Some(username);
            }
        }
    }
    if let Some(bearer) = bearer_value(headers) {
        if verify_token(state, &bearer) {
            return Some(state.config.user_name.clone());
        }
    }
    None
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
    if authenticated_user(&state, &headers).await.is_some() {
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

    #[test]
    fn hex_decode_32_roundtrip() {
        let key = [7u8; 32];
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        let mut out = [0u8; 32];
        assert!(hex_decode_32(&hex, &mut out));
        assert_eq!(out, key);
        assert!(!hex_decode_32("nope", &mut out));
        assert!(!hex_decode_32(&hex[..62], &mut out));
        assert!(!hex_decode_32(&hex.replace('7', "z"), &mut out));
    }

    #[test]
    fn cookie_sign_verify_roundtrip_and_tamper() {
        let secret = [3u8; 32];
        let now = 1_800_000_000;
        let cookie = issue_cookie(&secret, "web", now, 5).unwrap();

        let (user, expires, gen) = parse_cookie(&secret, &cookie).expect("valid");
        assert_eq!(user, "web");
        assert_eq!(expires, now + SESSION_TTL_SECS);
        assert_eq!(gen, 5);

        // Wrong key rejects.
        assert!(parse_cookie(&[4u8; 32], &cookie).is_none());
        // Payload tampering rejects.
        let (p, _t) = cookie.split_once('.').unwrap();
        let evil = format!("{}.", b64(b"admin|0|99999999999|0"));
        assert!(parse_cookie(&secret, &format!("{evil}{}", cookie.split('.').nth(1).unwrap())).is_none());
        assert!(!p.is_empty());
        // Garbage rejects.
        assert!(parse_cookie(&secret, "nonsense").is_none());
        assert!(parse_cookie(&secret, "a.b").is_none());
    }
}
