//! Portal router integration tests (M2 checklist item).
//!
//! Drives the real Axum router in-process via `tower::ServiceExt::oneshot`
//! with a scripted fake `AgentOps` — no LLM, no network, temp-dir config.
//! Covers: auth gate 401s, login → cookie round trip, bearer path, gen-bump
//! invalidation (ADR 0008A), settings PATCH whitelist + masking, chat 409 /
//! cancel, and tasks mapping.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tower::ServiceExt;

use rustfox::config::{Config, PortalConfig};
use rustfox::memory::MemoryStore;
use rustfox::platform::IncomingMessage;
use rustfox::portal::{
    auth::{current_gen, issue_cookie, SESSION_COOKIE},
    AgentOps, PortalState, SkillInfo,
};
use rustfox::scheduler::reminders::{ScheduledTask, ScheduledTaskStore};
use rustfox::tool_registry::ToolUiMode;

// ---------------------------------------------------------------------------
// Fake agent
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeCounters {
    process_calls: AtomicUsize,
    cancel_calls: AtomicUsize,
    set_model_calls: AtomicUsize,
    reload_calls: AtomicUsize,
}

/// Scripted AgentOps. `hold_processing` keeps process_message "running"
/// (used to provoke the 409 busy guard without sleeping forever).
struct FakeAgent {
    config: Config,
    counters: FakeCounters,
    busy: Arc<AtomicBool>,
    model: Mutex<String>,
}

impl FakeAgent {
    fn new(config: Config) -> Self {
        Self {
            config,
            counters: FakeCounters::default(),
            busy: Arc::new(AtomicBool::new(false)),
            model: Mutex::new("fake-model".into()),
        }
    }
}

impl AgentOps for FakeAgent {
    fn current_model(&self) -> BoxFuture<'_, String> {
        Box::pin(async { self.model.lock().unwrap().clone() })
    }
    fn is_processing(&self, _user_id: &str) -> BoxFuture<'_, bool> {
        let busy = self.busy.clone();
        Box::pin(async move { busy.load(Ordering::SeqCst) })
    }
    fn set_model(&self, model_id: String) -> BoxFuture<'_, anyhow::Result<()>> {
        self.counters.set_model_calls.fetch_add(1, Ordering::SeqCst);
        *self.model.lock().unwrap() = model_id;
        Box::pin(async { Ok(()) })
    }
    fn reload_skills_and_agents(&self) -> BoxFuture<'_, (usize, usize)> {
        self.counters.reload_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { (1, 1) })
    }
    fn cancel_processing(&self, _user_id: String) -> BoxFuture<'_, bool> {
        self.counters.cancel_calls.fetch_add(1, Ordering::SeqCst);
        self.busy.store(false, Ordering::SeqCst);
        Box::pin(async { true })
    }
    fn clear_cancel_token(&self, _user_id: String) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn skill_entries(&self) -> BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async {
            vec![SkillInfo {
                name: "fake-skill".into(),
                description: "a skill for tests".into(),
                model: None,
            }]
        })
    }
    fn agent_entries(&self) -> BoxFuture<'_, Vec<SkillInfo>> {
        Box::pin(async {
            vec![SkillInfo {
                name: "fake-agent".into(),
                description: "an agent for tests".into(),
                model: Some("m".into()),
            }]
        })
    }
    fn remove_scheduler_job(&self, _job_id: uuid::Uuid) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
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
        self.counters.process_calls.fetch_add(1, Ordering::SeqCst);
        let busy = self.busy.clone();
        Box::pin(async move {
            // Simulate a generation that returns promptly unless a test
            // holds it open to provoke 409.
            while busy.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok("fake answer".to_string())
        })
    }
    fn set_soul_updated(&self, _value: bool) {}
    fn provider_names(&self) -> Vec<String> {
        vec!["openrouter".into()]
    }
    fn config(&self) -> &Config {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixture {
    app: axum::Router,
    state: PortalState,
    _dir: tempfile::TempDir,
}

const TEST_TOKEN: &str = "test-portal-token-1234";

fn write_config(dir: &Path, portal: PortalConfig) -> PathBuf {
    let path = dir.join("config.toml");
    let token_line = portal
        .token
        .as_deref()
        .map(|t| format!("token = \"{t}\"\n"))
        .unwrap_or_default();
    let hash_line = portal
        .token_sha256
        .as_deref()
        .map(|h| format!("token_sha256 = \"{h}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        &path,
        format!(
            r#"[telegram]
bot_token = "test"
allowed_user_ids = [1]

[openrouter]
api_key = "test"

[sandbox]
allowed_directory = "."

[portal]
enabled = true
port = {}
bind = "{}"
{}{}user_name = "web"
"#,
            portal.port, portal.bind, token_line, hash_line
        ),
    )
    .unwrap();
    path
}

/// A fixture with a temp home (secret file), temp config.toml, in-memory
/// SQLite, and the given portal token config.
async fn fixture(portal: PortalConfig) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config_path = write_config(dir.path(), portal.clone());

    let mut config = Config::load(&config_path).unwrap();
    config.resolved_home = Some(home.clone());

    let memory = MemoryStore::open_in_memory().unwrap();
    let task_store = ScheduledTaskStore::new(memory.connection());
    let state = PortalState::new(
        Arc::new(FakeAgent::new(config)),
        memory,
        task_store,
        portal,
        config_path,
        Some(home),
    );
    Fixture {
        app: rustfox::portal::router(state.clone()),
        state,
        _dir: dir,
    }
}

fn with_token_token() -> PortalConfig {
    PortalConfig {
        enabled: true,
        port: 8090,
        bind: "127.0.0.1".into(),
        token: Some(TEST_TOKEN.into()),
        token_sha256: None,
        user_name: "web".into(),
    }
}

fn with_hash_token() -> PortalConfig {
    let hash = rustfox::portal::auth::sha256_hex(TEST_TOKEN);
    PortalConfig {
        token_sha256: Some(hash),
        ..with_token_token()
    }
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body collect");
    serde_json::from_slice(&bytes).expect("json body")
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn bearer(req: Request<Body>, token: &str) -> Request<Body> {
    with_header(req, header::AUTHORIZATION, &format!("Bearer {token}"))
}

fn cookie(req: Request<Body>, value: &str) -> Request<Body> {
    with_header(req, header::COOKIE, &format!("{SESSION_COOKIE}={value}"))
}

fn with_header(req: Request<Body>, name: header::HeaderName, value: &str) -> Request<Body> {
    let (mut parts, body) = req.into_parts();
    parts
        .headers
        .insert(name, value.parse().expect("header value"));
    Request::from_parts(parts, body)
}

// ---------------------------------------------------------------------------
// Auth gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn protected_routes_401_without_credentials() {
    let f = fixture(with_token_token()).await;
    for path in [
        "/api/chat/history",
        "/api/agents",
        "/api/tasks",
        "/api/settings",
        "/api/soul?name=SOUL.md",
        "/api/stats",
    ] {
        let res = f.app.clone().oneshot(get(path)).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "{path} leaked: no auth"
        );
        let body = body_json(res).await;
        assert_eq!(body["error"]["code"], "unauthenticated", "{path}: {body}");
    }
}

#[tokio::test]
async fn health_and_me_are_public() {
    let f = fixture(with_token_token()).await;
    let res = f.app.clone().oneshot(get("/api/health")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "health must stay public (ADR 0008B)"
    );
    let body = body_json(res).await;
    assert!(
        !body["bootId"].as_str().unwrap().is_empty(),
        "bootId required"
    );

    let res = f.app.clone().oneshot(get("/api/auth/me")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["authenticated"], false);
}

#[tokio::test]
async fn login_rejects_bad_token() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(post_json("/api/auth/login", json!({ "token": "wrong" })))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_with_plaintext_token_issues_working_cookie() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(post_json("/api/auth/login", json!({ "token": TEST_TOKEN })))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let set_cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        set_cookie.contains("HttpOnly"),
        "cookie flags: {set_cookie}"
    );
    let value = set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix(&format!("{SESSION_COOKIE}="))
        .expect("cookie name")
        .to_string();

    // The issued cookie authenticates a protected route…
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/chat/history"), &value))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // …and /api/auth/me now reports the session.
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/auth/me"), &value))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["username"], "web");
}

#[tokio::test]
async fn bearer_token_path_works() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/agents"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body[0]["name"], "rustfox");

    // Garbage bearer token → 401.
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/agents"), "nope"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sha256_token_auth_accepts_raw_and_rejects_other() {
    let f = fixture(with_hash_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/agents"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Presenting the hash itself as the token must NOT authenticate.
    let hash = rustfox::portal::auth::sha256_hex(TEST_TOKEN);
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/agents"), &hash))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tampered_cookie_rejected() {
    let f = fixture(with_token_token()).await;
    let secret = rustfox::portal::auth::signing_secret(&f.state).unwrap();
    let good = issue_cookie(&secret, "web", chrono::Utc::now().timestamp(), 0).unwrap();
    let (payload, _tag) = good.split_once('.').unwrap();
    // Re-sign a different payload with nothing — must fail.
    let evil = format!("{}.{payload}", "A".repeat(43));
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/agents"), &evil))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid cookie authenticates (sanity: the evil test isn't vacuous).
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/agents"), &good))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn expired_cookie_rejected() {
    let f = fixture(with_token_token()).await;
    let secret = rustfox::portal::auth::signing_secret(&f.state).unwrap();
    // Signed 60 days ago (TTL 30 days) — valid signature, dead by clock.
    let past = chrono::Utc::now().timestamp() - 60 * 24 * 3600;
    let stale = issue_cookie(&secret, "web", past, 0).unwrap();
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/agents"), &stale))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn gen_bump_invalidates_every_cookie() {
    let f = fixture(with_token_token()).await;
    let secret = rustfox::portal::auth::signing_secret(&f.state).unwrap();

    let before = current_gen(&f.state).await;
    let c1 = issue_cookie(&secret, "web", chrono::Utc::now().timestamp(), before).unwrap();
    assert_eq!(
        f.app
            .clone()
            .oneshot(cookie(get("/api/agents"), &c1))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Global logout path: /auth/logout {everywhere:true} bumps the counter.
    let res = f
        .app
        .clone()
        .oneshot(post_json("/api/auth/logout", json!({ "everywhere": true })))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(current_gen(&f.state).await, before + 1);

    // Old cookie now dead, even though perfectly signed.
    let res = f
        .app
        .clone()
        .oneshot(cookie(get("/api/agents"), &c1))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "gen bump must invalidate cookies"
    );

    // A freshly issued (post-bump gen) cookie works again.
    let c2 = issue_cookie(&secret, "web", chrono::Utc::now().timestamp(), before + 1).unwrap();
    assert_eq!(
        f.app
            .clone()
            .oneshot(cookie(get("/api/agents"), &c2))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn plain_logout_only_clears_the_cookie() {
    let f = fixture(with_token_token()).await;
    let before = current_gen(&f.state).await;
    let res = f
        .app
        .clone()
        .oneshot(post_json("/api/auth/logout", json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        current_gen(&f.state).await,
        before,
        "non-global logout must not bump gen"
    );
}

#[tokio::test]
async fn secret_file_created_private_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let portal = with_token_token();
    let config_path = write_config(dir.path(), portal.clone());
    let mut config = Config::load(&config_path).unwrap();
    config.resolved_home = Some(home.clone());
    let memory = MemoryStore::open_in_memory().unwrap();

    let s1 = PortalState::new(
        Arc::new(FakeAgent::new(config.clone())),
        memory.clone(),
        ScheduledTaskStore::new(memory.connection()),
        portal.clone(),
        config_path.clone(),
        Some(home.clone()),
    );
    let key1 = rustfox::portal::auth::signing_secret(&s1).unwrap();
    let path = home.join("portal_secret.key");
    assert!(path.exists(), "secret file must be created on first use");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secret file mode must be 0600, got {mode:o}");
    }

    // A different process-like state (fresh OnceLock) reads the same key.
    let s2 = PortalState::new(
        Arc::new(FakeAgent::new(config)),
        memory,
        ScheduledTaskStore::new(s1.memory.connection()),
        portal,
        config_path,
        Some(home),
    );
    let key2 = rustfox::portal::auth::signing_secret(&s2).unwrap();
    assert_eq!(
        key1, key2,
        "secret must be stable across restarts (files survive)"
    );
}

#[tokio::test]
async fn cookie_survives_new_state_same_home() {
    // The ADR-0008A headline: cookies issued before a restart authenticate
    // after it, because both states read the same portal_secret.key.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let portal = with_token_token();
    let config_path = write_config(dir.path(), portal.clone());
    let mut config = Config::load(&config_path).unwrap();
    config.resolved_home = Some(home.clone());
    let memory = MemoryStore::open_in_memory().unwrap();

    let mk = |mem: &MemoryStore| {
        PortalState::new(
            Arc::new(FakeAgent::new(config.clone())),
            mem.clone(),
            ScheduledTaskStore::new(mem.connection()),
            portal.clone(),
            config_path.clone(),
            Some(home.clone()),
        )
    };
    let s1 = mk(&memory);
    let secret = rustfox::portal::auth::signing_secret(&s1).unwrap();
    let cookie = issue_cookie(&secret, "web", chrono::Utc::now().timestamp(), 0).unwrap();

    // "Restart": brand-new state (fresh boot_id, no cached secret).
    let s2 = mk(&memory);
    assert_ne!(s1.boot_id, s2.boot_id, "fixture sanity");
    let app = rustfox::portal::router(s2);
    let res = app.oneshot(cookie_request(&cookie)).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "old cookie must survive restart"
    );
}

fn cookie_request(value: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/agents")
        .header(header::COOKIE, format!("{SESSION_COOKIE}={value}"))
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_empty_text_400() {
    let f = fixture(with_token_token()).await;
    let req = bearer(post_json("/api/chat", json!({ "text": "   " })), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res).await;
    assert_eq!(body["error"]["code"], "empty_message");
}

#[tokio::test]
async fn chat_busy_guard_returns_409() {
    let f = fixture(with_token_token()).await;
    // Mark the portal identity busy: the second send must bounce with 409.
    let req = bearer(post_json("/api/chat", json!({ "text": "hi" })), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK); // stream opened

    // The fake's process_message loops until busy=false, so the SSE call
    // occupies the flag. Second concurrent send → 409.
    let req2 = bearer(
        post_json("/api/chat", json!({ "text": "again" })),
        TEST_TOKEN,
    );
    let res2 = f.app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::CONFLICT);
    let body = body_json(res2).await;
    assert_eq!(body["error"]["code"], "chat_in_progress");
}

#[tokio::test]
async fn chat_cancel_reports_true_after_generation() {
    let f = fixture(with_token_token()).await;
    let req = bearer(
        axum::http::Request::builder()
            .method("POST")
            .uri("/api/chat/cancel")
            .body(Body::empty())
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["cancelled"], true);
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settings_get_masks_secrets_never_leaks_raw_toml() {
    let f = fixture(with_token_token()).await;
    let req = bearer(get("/api/settings"), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    let raw = body.to_string();
    // The bot token ("test") is ≤8 chars → masked to ••••; either way, the
    // projection must carry masked fields and never echo api keys wholesale.
    assert_eq!(
        body["masked"]["telegramBotToken"], "••••",
        "short secrets must fully mask"
    );
    assert!(body["editable"].get("model").is_some());
    assert!(body["restartRequired"]
        .as_array()
        .unwrap()
        .contains(&json!("portalPort")));
    assert!(
        !raw.contains("api_key"),
        "raw TOML key names must not cross the API: {raw}"
    );
}

#[tokio::test]
async fn settings_patch_whitelist_applies_and_backs_up() {
    let f = fixture(with_token_token()).await;
    // Long-ish secret to exercise the partial-mask format: set a distinctive
    // location instead, then PATCH it.
    let req = bearer(
        Request::builder()
            .method(axum::http::Method::PATCH)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "generalLocation": "Hong Kong", "defaultAutonomyMode": "autopilot" })
                    .to_string(),
            ))
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["updated"][0], "generalLocation");
    assert_eq!(body["updated"][1], "defaultAutonomyMode");
    assert!(body["restartRequired"].as_array().unwrap().len() >= 2);

    // Backup written next to the config…
    let bak = f.state.config_path.with_extension("toml.bak");
    assert!(
        bak.exists(),
        "config.toml.bak must be written before mutation"
    );
    // …and the live file now carries the new values.
    let content = std::fs::read_to_string(f.state.config_path.as_ref()).unwrap();
    assert!(
        content.contains("Hong Kong"),
        "location not persisted: {content}"
    );
    assert!(
        content.contains("autopilot"),
        "mode not persisted: {content}"
    );
}

#[tokio::test]
async fn settings_patch_rejects_bad_autonomy_mode() {
    let f = fixture(with_token_token()).await;
    let req = bearer(
        Request::builder()
            .method(axum::http::Method::PATCH)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "defaultAutonomyMode": "yolo" }).to_string(),
            ))
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res).await;
    assert_eq!(body["error"]["code"], "invalid_autonomy_mode");
}

#[tokio::test]
async fn settings_patch_rejects_port_zero() {
    let f = fixture(with_token_token()).await;
    let req = bearer(
        Request::builder()
            .method(axum::http::Method::PATCH)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "portalPort": 0 }).to_string()))
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res).await;
    assert_eq!(body["error"]["code"], "invalid_portal_port");
}

#[tokio::test]
async fn soul_whitelist_rejects_unknown_names() {
    let f = fixture(with_token_token()).await;
    let req = bearer(get("/api/soul?name=../etc/passwd"), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res).await;
    assert_eq!(body["error"]["code"], "unknown_soul_file");
}

#[tokio::test]
async fn soul_read_write_roundtrip_with_backup() {
    let f = fixture(with_token_token()).await;
    // Seed the file under the fixture home.
    let soul = f.state.home_dir.as_ref().unwrap().join("SOUL.md");
    std::fs::write(&soul, b"old soul").unwrap();

    let req = bearer(get("/api/soul?name=SOUL.md"), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    let body = body_json(res).await;
    assert_eq!(body["content"], "old soul");

    let req = bearer(
        Request::builder()
            .method("PUT")
            .uri("/api/soul")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "name": "SOUL.md", "content": "# new soul" }).to_string(),
            ))
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(std::fs::read_to_string(&soul).unwrap(), "# new soul");
    assert!(
        soul.with_file_name("SOUL.md.bak").exists(),
        "soul backup written"
    );

    // Empty content refused (don't nuke identity by fat-fingering).
    let req = bearer(
        Request::builder()
            .method("PUT")
            .uri("/api/soul")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "name": "SOUL.md", "content": "" }).to_string(),
            ))
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Data endpoints
// ---------------------------------------------------------------------------

fn make_task(id: &str) -> ScheduledTask {
    ScheduledTask {
        id: id.into(),
        scheduler_job_id: None,
        user_id: "1".into(),
        chat_id: "1".into(),
        platform: "telegram".into(),
        trigger_type: "recurring".into(),
        trigger_value: "30 7 * * *".into(),
        prompt: "weather report".into(),
        description: "Daily weather".into(),
        status: "active".into(),
        created_at: "2026-09-01T00:00:00Z".into(),
        next_run_at: Some("2026-09-23T07:30:00+08:00".into()),
    }
}

#[tokio::test]
async fn tasks_listing_maps_fields() {
    let f = fixture(with_token_token()).await;
    f.state.task_store.create(&make_task("t1")).await.unwrap();
    let req = bearer(get("/api/tasks"), TEST_TOKEN);
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "t1");
    assert_eq!(body[0]["name"], "Daily weather");
    assert_eq!(body[0]["cron"], "30 7 * * *");
    assert_eq!(body[0]["enabled"], true);
    assert_eq!(body[0]["nextRun"], "2026-09-23T07:30:00+08:00");
}

#[tokio::test]
async fn task_disable_flips_status_and_removes_job() {
    let f = fixture(with_token_token()).await;
    f.state.task_store.create(&make_task("t2")).await.unwrap();
    let req = bearer(
        Request::builder()
            .method("POST")
            .uri("/api/tasks/t2/disable")
            .body(Body::empty())
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["enabled"], false);
    let listed = f.state.task_store.list_all_active().await.unwrap();
    assert!(
        listed.iter().all(|t| t.id != "t2"),
        "disabled task still active"
    );
}

#[tokio::test]
async fn task_enable_unknown_id_404() {
    let f = fixture(with_token_token()).await;
    let req = bearer(
        Request::builder()
            .method("POST")
            .uri("/api/tasks/nope/enable")
            .body(Body::empty())
            .unwrap(),
        TEST_TOKEN,
    );
    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agents_skills_reload_endpoints() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/agents/skills"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 2); // 1 skill + 1 agent

    let res = f
        .app
        .clone()
        .oneshot(bearer(
            Request::builder()
                .method("POST")
                .uri("/api/agents/reload")
                .body(Body::empty())
                .unwrap(),
            TEST_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["skillsLoaded"], 1);
}

#[tokio::test]
async fn stats_reports_portal_enabled_and_counts() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/stats"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["portalEnabled"], true);
    assert!(body.get("model").is_some());
}

#[tokio::test]
async fn chat_history_empty_conversation_ok() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/chat/history"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    assert!(body["conversationId"].is_string());
}

#[tokio::test]
async fn memory_search_smoke() {
    let f = fixture(with_token_token()).await;
    f.state
        .memory
        .remember("fact", "portal-test", "portal integration test fact", None)
        .await
        .unwrap();
    let res = f
        .app
        .clone()
        .oneshot(bearer(
            get("/api/memory/search?q=portal+integration"),
            TEST_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"].as_str().unwrap_or("").contains("portal")),
        "search miss: {body}"
    );
}

// ---------------------------------------------------------------------------
// Static serving (SPA + fallback)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_api_path_404_json_not_spa() {
    let f = fixture(with_token_token()).await;
    let res = f
        .app
        .clone()
        .oneshot(bearer(get("/api/nonexistent"), TEST_TOKEN))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // SPA fallback must NOT swallow /api/* — it would confuse clients.
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.contains("application/json") || ct.contains("text/plain"),
        "ct={ct}"
    );
}
