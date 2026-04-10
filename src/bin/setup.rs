//! RustFox setup wizard.
//!
//! Without flags: starts a local Axum HTTP server on port 8719 and opens the
//! browser-based setup wizard.  On form submission the wizard POSTs the
//! generated config to `/api/save-config`, which writes `config.toml` to the
//! project root and then shuts the server down.
//!
//! With `--cli`: runs an interactive terminal wizard instead.
//!
//! OAuth 2.0 routes (generic MCP HTTP server support):
//!   GET /api/oauth/start?server=<name>&url=<mcp-url>
//!       Discovers auth endpoints via .well-known, performs Dynamic Client
//!       Registration, generates PKCE, returns { state, auth_url }.
//!   GET /oauth/callback?code=<code>&state=<state>
//!       Exchanges code+code_verifier for an access token, stores it, renders
//!       a self-closing HTML page.
//!   GET /api/oauth/token?state=<state>
//!       Polling endpoint — returns { ready, token? } once callback completes.

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{oneshot, Mutex};

// The wizard SPA is embedded at compile time — no runtime file needed.
const INDEX_HTML: &str = include_str!("../../setup/index.html");

/// Port the setup wizard listens on.
const SETUP_PORT: u16 = 8719;

/// OAuth redirect URI — must match what was registered with the authorization server.
fn redirect_uri() -> String {
    format!("http://localhost:{SETUP_PORT}/oauth/callback")
}

// ── OAuth session state ────────────────────────────────────────────────────────

/// In-flight OAuth session created by /api/oauth/start and resolved in /oauth/callback.
#[derive(Clone)]
struct OAuthSession {
    server_name: String,
    code_verifier: String,
    client_id: String,
    client_secret: Option<String>,
    token_endpoint: String,
    /// Populated once /oauth/callback completes the token exchange.
    access_token: Option<String>,
}

// ── Shared state ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    config_path: PathBuf,
    /// Consumed once when the browser POSTs a saved config.
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// Keyed by random state string; entries are created by /api/oauth/start
    /// and updated by /oauth/callback.
    oauth_sessions: Arc<Mutex<HashMap<String, OAuthSession>>>,
    /// Shared HTTP client for OAuth discovery / registration / token exchange.
    http_client: reqwest::Client,
}

// ── Request / response types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct SaveRequest {
    config: String,
}

#[derive(Serialize)]
struct SaveResponse {
    ok: bool,
    path: String,
}

// ── Load-config response ────────────────────────────────────────────────────

#[derive(Serialize, Default)]
struct ExistingConfig {
    exists: bool,
    telegram_token: String,
    allowed_user_ids: String, // "123, 456" — ready for the text input
    openrouter_key: String,
    model: String,
    max_tokens: u32, // 0 = not set; frontend treats falsy as "use wizard default (4096)"
    system_prompt: String,
    location: String,
    sandbox_dir: String,
    db_path: String,
    mcp_servers: Vec<ExistingMcpServer>,
}

#[derive(Serialize, Default, Clone)]
struct ExistingMcpServer {
    name: String,
    command: String,
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

// ── Raw TOML parse structs (loose — all fields optional so partial configs load) ──

#[derive(Deserialize, Default)]
struct RawConfig {
    telegram: Option<RawTelegram>,
    openrouter: Option<RawOpenRouter>,
    sandbox: Option<RawSandbox>,
    memory: Option<RawMemory>,
    general: Option<RawGeneral>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

#[derive(Deserialize, Default)]
struct RawTelegram {
    bot_token: Option<String>,
    allowed_user_ids: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Default)]
struct RawOpenRouter {
    api_key: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSandbox {
    allowed_directory: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawMemory {
    database_path: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawGeneral {
    location: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawMcpServer {
    name: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    // `url` and `auth_token` are parsed but not used by the setup wizard;
    // they are accepted so configs with HTTP MCP servers load without error.
    #[serde(default)]
    #[allow(dead_code)]
    url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    auth_token: Option<String>,
}

// ── OAuth API types ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OAuthStartQuery {
    server: String,
    url: String,
}

#[derive(Serialize)]
struct OAuthStartResponse {
    state: String,
    auth_url: String,
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct OAuthTokenQuery {
    state: String,
}

#[derive(Serialize)]
struct OAuthTokenPollResponse {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// Minimal shape of `.well-known/oauth-authorization-server` or
/// `.well-known/openid-configuration` that we need.
#[derive(Deserialize)]
struct OAuthDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

#[derive(Serialize)]
struct ClientRegistrationRequest {
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    token_endpoint_auth_method: String,
}

#[derive(Deserialize)]
struct ClientRegistrationResponse {
    client_id: String,
    client_secret: Option<String>,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
}

// ── OAuth helpers ──────────────────────────────────────────────────────────────

/// Try `{origin}/.well-known/oauth-authorization-server` (MCP spec / RFC 8414),
/// then fall back to `{origin}/.well-known/openid-configuration` (OIDC).
async fn discover_oauth_endpoints(
    client: &reqwest::Client,
    origin: &str,
) -> anyhow::Result<OAuthDiscovery> {
    let urls = [
        format!("{origin}/.well-known/oauth-authorization-server"),
        format!("{origin}/.well-known/openid-configuration"),
    ];
    for url in &urls {
        let resp = client.get(url).send().await?;
        if resp.status().is_success() {
            return resp
                .json::<OAuthDiscovery>()
                .await
                .with_context(|| format!("Failed to parse OAuth discovery from {url}"));
        }
    }
    anyhow::bail!("No OAuth discovery document found at {origin}")
}

/// Generate a PKCE code_verifier (32 random bytes, base64url-encoded).
fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute PKCE code_challenge = BASE64URL(SHA-256(ASCII(code_verifier))).
fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Generate a random hex state string (16 bytes → 32 hex chars).
fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Handlers ───────────────────────────────────────────────────────────────────

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn save_config(
    State(state): State<AppState>,
    Json(body): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, StatusCode> {
    tokio::fs::write(&state.config_path, &body.config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let path = state.config_path.to_string_lossy().to_string();
    println!("\n✓  config.toml saved to {path}");
    println!("   Run the bot with:  cargo run --bin rustfox\n");

    // Signal main to shut down after the response has been sent.
    let tx = state.shutdown_tx.lock().await.take();
    if let Some(tx) = tx {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            let _ = tx.send(());
        });
    }

    Ok(Json(SaveResponse { ok: true, path }))
}

async fn load_config(State(state): State<AppState>) -> Json<ExistingConfig> {
    match tokio::fs::read_to_string(&state.config_path).await {
        Ok(content) => Json(parse_existing_config(&content)),
        Err(_) => Json(ExistingConfig::default()), // file absent or unreadable
    }
}

/// GET /api/oauth/start?server=<name>&url=<mcp-url>
///
/// 1. Discovers OAuth endpoints from the MCP server's .well-known document.
/// 2. Performs Dynamic Client Registration (RFC 7591).
/// 3. Generates a PKCE code_verifier + code_challenge.
/// 4. Stores an in-flight session keyed by a random state value.
/// 5. Returns `{ state, auth_url }` — the frontend opens auth_url in a popup.
async fn oauth_start(
    State(st): State<AppState>,
    Query(params): Query<OAuthStartQuery>,
) -> Result<Json<OAuthStartResponse>, (StatusCode, String)> {
    let err = |status: StatusCode, msg: String| (status, msg);

    // Derive origin (scheme + host + optional port) from the MCP URL.
    let parsed = reqwest::Url::parse(&params.url)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("Invalid MCP URL: {e}")))?;
    let mut origin = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    if let Some(port) = parsed.port() {
        origin = format!("{origin}:{port}");
    }

    // Discover authorization + token + registration endpoints.
    let discovery = discover_oauth_endpoints(&st.http_client, &origin)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;

    let reg_endpoint = discovery.registration_endpoint.ok_or_else(|| {
        err(
            StatusCode::NOT_IMPLEMENTED,
            "MCP server does not advertise a Dynamic Client Registration endpoint".into(),
        )
    })?;

    // Register this client dynamically (RFC 7591) as a public client with PKCE.
    let redir = redirect_uri();
    let reg_body = ClientRegistrationRequest {
        client_name: "RustFox Setup".into(),
        redirect_uris: vec![redir.clone()],
        grant_types: vec!["authorization_code".into()],
        response_types: vec!["code".into()],
        // Public client — PKCE provides the security; no client_secret needed.
        token_endpoint_auth_method: "none".into(),
    };

    let reg_resp: ClientRegistrationResponse = st
        .http_client
        .post(&reg_endpoint)
        .json(&reg_body)
        .send()
        .await
        .map_err(|e| {
            err(
                StatusCode::BAD_GATEWAY,
                format!("Registration request failed: {e}"),
            )
        })?
        .json()
        .await
        .map_err(|e| {
            err(
                StatusCode::BAD_GATEWAY,
                format!("Registration response parse failed: {e}"),
            )
        })?;

    // Generate PKCE pair and opaque state.
    let code_verifier = pkce_verifier();
    let code_challenge = pkce_challenge(&code_verifier);
    let oauth_state = random_state();

    // Build the authorization URL with all required parameters.
    let mut auth_url = reqwest::Url::parse(&discovery.authorization_endpoint).map_err(|e| {
        err(
            StatusCode::BAD_GATEWAY,
            format!("Invalid authorization_endpoint: {e}"),
        )
    })?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg_resp.client_id)
        .append_pair("redirect_uri", &redir)
        .append_pair("state", &oauth_state)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256");

    // Persist the session so /oauth/callback can complete the exchange.
    st.oauth_sessions.lock().await.insert(
        oauth_state.clone(),
        OAuthSession {
            server_name: params.server.clone(),
            code_verifier,
            client_id: reg_resp.client_id,
            client_secret: reg_resp.client_secret,
            token_endpoint: discovery.token_endpoint,
            access_token: None,
        },
    );

    Ok(Json(OAuthStartResponse {
        state: oauth_state,
        auth_url: auth_url.to_string(),
    }))
}

/// GET /oauth/callback?code=<code>&state=<state>
///
/// Receives the authorization code redirect, exchanges it for an access token,
/// stores the token in the session map, and returns a self-closing HTML page so
/// the popup window closes automatically.
async fn oauth_callback(
    State(st): State<AppState>,
    Query(params): Query<OAuthCallbackQuery>,
) -> Html<String> {
    let mut sessions = st.oauth_sessions.lock().await;
    let session =
        match sessions.get_mut(&params.state) {
            Some(s) => s,
            None => return Html(
                "<html><body><p>Unknown OAuth state. Please close this window and try again.</p>\
                 <script>setTimeout(()=>window.close(),3000)</script></body></html>"
                    .into(),
            ),
        };

    // Exchange the authorization code for an access token.
    let redir = redirect_uri();
    let mut token_params = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", params.code.clone()),
        ("redirect_uri", redir),
        ("client_id", session.client_id.clone()),
        ("code_verifier", session.code_verifier.clone()),
    ];
    if let Some(secret) = &session.client_secret {
        token_params.push(("client_secret", secret.clone()));
    }

    let token_result = st
        .http_client
        .post(&session.token_endpoint)
        .form(&token_params)
        .send()
        .await;

    match token_result {
        Ok(resp) if resp.status().is_success() => match resp.json::<OAuthTokenResponse>().await {
            Ok(tok) => {
                let server = session.server_name.clone();
                session.access_token = Some(tok.access_token);
                Html(format!(
                    "<html><head><title>Authorized</title></head><body>\
                         <p style=\"font-family:sans-serif;text-align:center;margin-top:4rem\">\
                         ✅ {server} authorization successful! You can close this window.</p>\
                         <script>window.close();</script>\
                         </body></html>"
                ))
            }
            Err(e) => Html(format!(
                "<html><body><p>Failed to parse token response: {e}</p>\
                     <script>setTimeout(()=>window.close(),5000)</script></body></html>"
            )),
        },
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Html(format!(
                "<html><body><p>Token exchange failed ({status}): {body}</p>\
                 <script>setTimeout(()=>window.close(),5000)</script></body></html>"
            ))
        }
        Err(e) => Html(format!(
            "<html><body><p>Token request error: {e}</p>\
             <script>setTimeout(()=>window.close(),5000)</script></body></html>"
        )),
    }
}

/// GET /api/oauth/token?state=<state>
///
/// Polling endpoint — returns `{ ready: true, token }` once the callback has
/// completed, or `{ ready: false }` while still waiting.
async fn oauth_token_poll(
    State(st): State<AppState>,
    Query(params): Query<OAuthTokenQuery>,
) -> Result<Json<OAuthTokenPollResponse>, StatusCode> {
    let sessions = st.oauth_sessions.lock().await;
    let session = sessions.get(&params.state).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(OAuthTokenPollResponse {
        ready: session.access_token.is_some(),
        token: session.access_token.clone(),
    }))
}

// ── Config formatting ──────────────────────────────────────────────────────────

struct ConfigParams<'a> {
    tg_token: &'a str,
    user_ids: &'a str,
    or_key: &'a str,
    model: &'a str,
    max_tokens: u32,
    sandbox: &'a str,
    db_path: &'a str,
    location: &'a str,
}

/// Produces a valid config.toml string. Extracted so it can be unit-tested.
fn format_config(p: &ConfigParams<'_>) -> String {
    let ids: Vec<&str> = p
        .user_ids
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let ids_str = ids.join(", ");

    let loc_line = if p.location.is_empty() {
        "# location = \"Your City, Country\"".to_owned()
    } else {
        format!("location = \"{}\"", p.location)
    };

    let tg_token = p.tg_token;
    let or_key = p.or_key;
    let model = p.model;
    let max_tokens = p.max_tokens;
    let sandbox = p.sandbox;
    let db_path = p.db_path;

    format!(
        r#"[telegram]
bot_token = "{tg_token}"
allowed_user_ids = [{ids_str}]

[openrouter]
api_key = "{or_key}"
model = "{model}"
base_url = "https://openrouter.ai/api/v1"
max_tokens = {max_tokens}
system_prompt = """You are a helpful AI assistant with access to tools. \
Use the available tools to help the user with their tasks. \
When using file or terminal tools, operate only within the allowed sandbox directory. \
Be concise and helpful."""

[sandbox]
allowed_directory = "{sandbox}"

[memory]
database_path = "{db_path}"

[skills]
directory = "skills"

[general]
{loc_line}
"#
    )
}

// ── CLI mode ───────────────────────────────────────────────────────────────────

fn run_cli(project_root: &Path) -> Result<()> {
    use std::io::{self, Write};

    println!("=== RustFox CLI Setup ===\n");

    let read_line = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim().to_owned())
    };

    let or_default = |s: String, default: &str| {
        if s.is_empty() {
            default.to_owned()
        } else {
            s
        }
    };

    let tg_token = read_line("Telegram bot token: ")?;
    let user_ids = read_line("Allowed user IDs (comma-separated): ")?;
    let or_key = read_line("OpenRouter API key: ")?;
    let model = or_default(
        read_line("Model [moonshotai/kimi-k2.5]: ")?,
        "moonshotai/kimi-k2.5",
    );
    let sandbox = or_default(
        read_line("Sandbox directory [/tmp/rustfox-sandbox]: ")?,
        "/tmp/rustfox-sandbox",
    );
    let db_path = or_default(read_line("Memory DB path [rustfox.db]: ")?, "rustfox.db");
    let location = read_line("Your location (optional, e.g. Tokyo, Japan): ")?;

    let config = format_config(&ConfigParams {
        tg_token: &tg_token,
        user_ids: &user_ids,
        or_key: &or_key,
        model: &model,
        max_tokens: 4096,
        sandbox: &sandbox,
        db_path: &db_path,
        location: &location,
    });

    let config_path = project_root.join("config.toml");
    std::fs::write(&config_path, &config)
        .with_context(|| format!("Could not write {}", config_path.display()))?;

    println!("\n✓  config.toml saved to {}", config_path.display());
    println!("   Run the bot with:  cargo run --bin rustfox");
    Ok(())
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Resolve project root: prefer RUSTFOX_ROOT env, fall back to cwd.
    let project_root =
        PathBuf::from(std::env::var("RUSTFOX_ROOT").unwrap_or_else(|_| ".".to_string()));

    if args.iter().any(|a| a == "--cli") {
        return run_cli(&project_root);
    }

    // ── Web mode ──────────────────────────────────────────────────────────────
    let config_path = project_root.join("config.toml");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = AppState {
        config_path,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        oauth_sessions: Arc::new(Mutex::new(HashMap::new())),
        http_client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/load-config", get(load_config))
        .route("/api/save-config", post(save_config))
        .route("/api/oauth/start", get(oauth_start))
        .route("/oauth/callback", get(oauth_callback))
        .route("/api/oauth/token", get(oauth_token_poll))
        .with_state(state);

    let addr = format!("127.0.0.1:{SETUP_PORT}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    println!("RustFox setup wizard → http://localhost:{SETUP_PORT}");
    println!("Press Ctrl-C to exit without saving.\n");

    // Open the browser after a short delay.
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        let url = format!("http://localhost:{SETUP_PORT}");
        // Try xdg-open (Linux), then open (macOS) — ignore errors.
        let _ = std::process::Command::new("xdg-open").arg(&url).status();
        let _ = std::process::Command::new("open").arg(&url).status();
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .context("Server error")?;

    Ok(())
}

// ── Config parsing ─────────────────────────────────────────────────────────────

fn parse_existing_config(content: &str) -> ExistingConfig {
    let raw: RawConfig = match toml::from_str(content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Could not parse existing config.toml: {e}");
            return ExistingConfig::default();
        }
    };

    let tg = raw.telegram.unwrap_or_default();
    let openrouter = raw.openrouter.unwrap_or_default();
    let sb = raw.sandbox.unwrap_or_default();
    let mem = raw.memory.unwrap_or_default();

    let allowed_user_ids = tg
        .allowed_user_ids
        .unwrap_or_default()
        .iter()
        .map(|v| match v {
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let mcp_servers = raw
        .mcp_servers
        .into_iter()
        .filter_map(|s| {
            let name = s.name.filter(|n| !n.is_empty())?;
            Some(ExistingMcpServer {
                name,
                command: s.command.unwrap_or_default(),
                args: s.args,
                env: s.env,
            })
        })
        .collect();

    ExistingConfig {
        exists: true,
        telegram_token: tg.bot_token.unwrap_or_default(),
        allowed_user_ids,
        openrouter_key: openrouter.api_key.unwrap_or_default(),
        model: openrouter.model.unwrap_or_default(),
        max_tokens: openrouter.max_tokens.unwrap_or(0),
        system_prompt: openrouter.system_prompt.unwrap_or_default(),
        location: raw
            .general
            .as_ref()
            .and_then(|g| g.location.clone())
            .unwrap_or_default(),
        sandbox_dir: sb.allowed_directory.unwrap_or_default(),
        db_path: mem.database_path.unwrap_or_default(),
        mcp_servers,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_toml_returns_not_exists() {
        let cfg = parse_existing_config("this is not valid toml !!!");
        assert!(!cfg.exists);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[telegram]
bot_token = "mytoken123"
allowed_user_ids = [111, 222]

[openrouter]
api_key = "sk-or-test"
model = "gpt-4o"
max_tokens = 2048
system_prompt = "Be helpful."

[sandbox]
allowed_directory = "/tmp/test"

[memory]
database_path = "test.db"

[general]
location = "Tokyo, Japan"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "mytoken123");
        assert_eq!(cfg.allowed_user_ids, "111, 222");
        assert_eq!(cfg.openrouter_key, "sk-or-test");
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.max_tokens, 2048);
        assert_eq!(cfg.system_prompt, "Be helpful.");
        assert_eq!(cfg.location, "Tokyo, Japan");
        assert_eq!(cfg.sandbox_dir, "/tmp/test");
        assert_eq!(cfg.db_path, "test.db");
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn test_parse_config_with_mcp_servers() {
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = [1]

[openrouter]
api_key = "k"

[sandbox]
allowed_directory = "/tmp"

[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]

[[mcp_servers]]
name = "brave-search"
command = "npx"
args = ["-y", "@brave/brave-search-mcp-server"]
[mcp_servers.env]
BRAVE_API_KEY = "brave123"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.mcp_servers.len(), 2);
        assert_eq!(cfg.mcp_servers[0].name, "git");
        assert_eq!(cfg.mcp_servers[0].command, "uvx");
        assert_eq!(cfg.mcp_servers[0].args, vec!["mcp-server-git"]);
        assert!(cfg.mcp_servers[0].env.is_empty());
        assert_eq!(cfg.mcp_servers[1].name, "brave-search");
        assert_eq!(
            cfg.mcp_servers[1].env.get("BRAVE_API_KEY").unwrap(),
            "brave123"
        );
    }

    #[test]
    fn test_parse_partial_config_missing_sections_default_to_empty() {
        // Only telegram section — all other fields should be defaults
        let toml = r#"
[telegram]
bot_token = "partial"
allowed_user_ids = [42]
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "partial");
        assert_eq!(cfg.model, ""); // no default injected — that's the wizard's job
        assert_eq!(cfg.sandbox_dir, "");
    }

    #[test]
    fn test_parse_string_user_ids() {
        // String-typed IDs in TOML (toml::Value::String arm)
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = ["111", "222"]

[openrouter]
api_key = "k"

[sandbox]
allowed_directory = "/tmp"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.allowed_user_ids, "111, 222");
    }

    fn cfg(
        tg_token: &str,
        user_ids: &str,
        or_key: &str,
        model: &str,
        sandbox: &str,
        db_path: &str,
        location: &str,
    ) -> String {
        format_config(&ConfigParams {
            tg_token,
            user_ids,
            or_key,
            model,
            max_tokens: 4096,
            sandbox,
            db_path,
            location,
        })
    }

    #[test]
    fn test_telegram_section_present() {
        let out = cfg("mytoken", "123456", "key", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[telegram]"));
        assert!(out.contains(r#"bot_token = "mytoken""#));
        assert!(out.contains("allowed_user_ids = [123456]"));
    }

    #[test]
    fn test_openrouter_section_present() {
        let out = cfg("t", "1", "sk-or-abc", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[openrouter]"));
        assert!(out.contains(r#"api_key = "sk-or-abc""#));
        assert!(out.contains(r#"model = "gpt-4o""#));
        assert!(out.contains("max_tokens = 4096"));
    }

    #[test]
    fn test_location_included_when_set() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "Tokyo, Japan");
        assert!(out.contains("[general]"));
        assert!(out.contains(r#"location = "Tokyo, Japan""#));
        let general_pos = out.find("[general]").expect("[general] not found");
        let location_pos = out
            .find(r#"location = "Tokyo, Japan""#)
            .expect("location not found");
        assert!(
            location_pos > general_pos,
            "location must appear under [general]"
        );
    }

    #[test]
    fn test_location_commented_when_empty() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("[general]"));
        assert!(out.contains("# location ="));
        assert!(!out.contains("\nlocation = "));
        let general_pos = out.find("[general]").expect("[general] not found");
        let location_pos = out.find("# location =").expect("# location not found");
        assert!(
            location_pos > general_pos,
            "commented location must appear under [general]"
        );
    }

    #[test]
    fn test_multiple_user_ids_comma_separated() {
        let out = cfg("t", "111, 222, 333", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("allowed_user_ids = [111, 222, 333]"));
    }

    #[test]
    fn test_sandbox_and_memory_sections() {
        let out = cfg("t", "1", "k", "m", "/my/sandbox", "mem.db", "");
        assert!(out.contains("[sandbox]"));
        assert!(out.contains(r#"allowed_directory = "/my/sandbox""#));
        assert!(out.contains("[memory]"));
        assert!(out.contains(r#"database_path = "mem.db""#));
    }

    #[test]
    fn test_skills_section_present() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("[skills]"));
        assert!(out.contains(r#"directory = "skills""#));
    }

    #[test]
    fn test_pkce_verifier_length() {
        let v = pkce_verifier();
        // 32 bytes → 43 base64url chars (no padding)
        assert_eq!(v.len(), 43);
        assert!(v
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn test_pkce_challenge_is_base64url() {
        let verifier = pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        // SHA-256 → 32 bytes → 43 base64url chars
        assert_eq!(challenge.len(), 43);
        assert!(challenge
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn test_random_state_is_32_hex_chars() {
        let s = random_state();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
