//! Setup wizard — web (Axum server + browser) and CLI modes.
//!
//! Extracted from `src/bin/setup.rs` so the main binary can reuse it
//! via `rustfox --setup`.

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

const INDEX_HTML: &str = include_str!("../../setup/index.html");
const SETUP_PORT: u16 = 8719;

fn redirect_uri() -> String {
    format!("http://localhost:{SETUP_PORT}/oauth/callback")
}

/// Run the setup wizard.
/// If `cli` is true, runs in terminal mode. Otherwise starts an Axum web server.
pub async fn run(config_dir: &Path, cli: bool) -> Result<()> {
    if cli {
        return run_cli(config_dir);
    }
    run_web(config_dir).await
}

// ── OAuth session types ────────────────────────────────────────────────

#[derive(Clone)]
struct OAuthSession {
    server_name: String,
    code_verifier: String,
    client_id: String,
    client_secret: Option<String>,
    token_endpoint: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Clone)]
struct WizardState {
    config_path: PathBuf,
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    oauth_sessions: Arc<Mutex<HashMap<String, OAuthSession>>>,
    http_client: reqwest::Client,
}

// ── Request/response types ─────────────────────────────────────────────

#[derive(Deserialize)]
struct SaveRequest {
    config: String,
}

#[derive(Serialize)]
struct SaveResponse {
    ok: bool,
    path: String,
}

#[derive(Serialize, Default)]
pub struct ExistingConfig {
    pub exists: bool,
    pub telegram_token: String,
    pub allowed_user_ids: String,
    pub openrouter_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub location: String,
    pub db_path: String,
    pub supports_vision: bool,
    pub base_url: String,
    pub home_dir: String,
    pub skills_dir: String,
    pub agents_dir: String,
    pub ocr_model_dir: String,
    pub agent_max_iterations: u32,
    pub agent_empty_response_retry_limit: u32,
    pub langsmith_key: String,
    pub langsmith_project: String,
    pub embedding_key: String,
    pub embedding_base_url: String,
    pub embedding_model: String,
    pub embedding_dimensions: u32,
    pub query_rewriter_enabled: bool,
    pub learning_skill_extraction_enabled: bool,
    pub learning_skill_extraction_threshold: u32,
    pub learning_user_model_update_interval: u32,
    pub learning_user_model_cron: String,
    pub mcp_servers: Vec<ExistingMcpServer>,
}

#[derive(Serialize, Default, Clone)]
pub struct ExistingMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawConfig {
    pub telegram: Option<RawTelegram>,
    pub openrouter: Option<RawOpenRouter>,
    pub memory: Option<RawMemory>,
    pub general: Option<RawGeneral>,
    pub agent: Option<RawAgent>,
    pub langsmith: Option<RawLangSmith>,
    pub embedding: Option<RawEmbedding>,
    pub ocr: Option<RawOcr>,
    pub learning: Option<RawLearning>,
    pub supervisor: Option<RawSupervisor>,
    pub subagents: Option<RawSubagents>,
    pub skills: Option<RawSkills>,
    pub agents_config: Option<RawAgentsConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<RawMcpServer>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawTelegram {
    pub bot_token: Option<String>,
    pub allowed_user_ids: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawOpenRouter {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub supports_vision: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawMemory {
    pub database_path: Option<String>,
    pub query_rewriter_enabled: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawGeneral {
    pub location: Option<String>,
    pub home: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawAgent {
    pub max_iterations: Option<u32>,
    pub empty_response_retry_limit: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawLangSmith {
    pub api_key: Option<String>,
    pub project: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawEmbedding {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawOcr {
    pub model_dir: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawLearning {
    pub skill_extraction_enabled: Option<bool>,
    pub skill_extraction_threshold: Option<u32>,
    pub user_model_update_interval: Option<u32>,
    pub user_model_cron: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSupervisor {
    pub default_autonomy_mode: Option<String>,
    pub artifacts_dir: Option<String>,
    pub risk: Option<RawSupervisorRisk>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSupervisorRisk {
    pub require_approval_for_low: Option<bool>,
    pub require_approval_for_medium: Option<bool>,
    pub auto_execute_only_low: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSubagents {
    pub default_tools: Option<Vec<String>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawSkills {
    pub directory: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawAgentsConfig {
    pub directory: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawMcpServer {
    pub name: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
}

// ── OAuth API types ────────────────────────────────────────────────────

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
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth_client_secret: Option<String>,
}

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
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

// ── Web mode ───────────────────────────────────────────────────────────

async fn run_web(config_dir: &Path) -> Result<()> {
    let config_path = config_dir.join("config.toml");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let state = WizardState {
        config_path,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        oauth_sessions: Arc::new(Mutex::new(HashMap::new())),
        http_client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/load-config", get(load_config))
        .route("/api/save-config", post(save_config))
        .route("/api/install-service", post(install_service))
        .route("/api/shutdown", post(shutdown_server))
        .route("/api/oauth/start", get(oauth_start))
        .route("/oauth/callback", get(oauth_callback))
        .route("/api/oauth/token", get(oauth_token_poll))
        .with_state(state);

    let addr = format!("127.0.0.1:{SETUP_PORT}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    println!("\n============================================");
    println!("  RustFox Setup Wizard");
    println!("  http://localhost:{SETUP_PORT}");
    println!("============================================");
    println!("Press Ctrl-C to exit without saving.\n");

    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        let url = format!("http://localhost:{SETUP_PORT}");
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

// ── Web handlers ───────────────────────────────────────────────────────

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn save_config(
    State(st): State<WizardState>,
    Json(body): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, StatusCode> {
    tokio::fs::write(&st.config_path, &body.config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let path = st.config_path.to_string_lossy().to_string();
    println!("\n✓ config.toml saved to {path}");

    Ok(Json(SaveResponse { ok: true, path }))
}

/// POST /api/install-service
///
/// Installs the bot as a background service. Returns JSON with success/error.
/// Called by the frontend after config is saved (user clicks "Install as service").
/// Uses spawn_blocking because service::handle() performs synchronous I/O
/// (std::fs::write, std::process::Command) that would block the async runtime.
async fn install_service(State(_st): State<WizardState>) -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(|| {
        crate::setup::service::handle(crate::setup::service::Action::Install)
    })
    .await
    .unwrap_or(Err(anyhow::anyhow!("Task join failed")));
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// POST /api/shutdown
///
/// Gracefully shuts down the setup server. Called by the frontend when the user
/// clicks "Finish" on the success page — after they've had a chance to install
/// the background service.
async fn shutdown_server(State(st): State<WizardState>) -> Json<serde_json::Value> {
    let tx = st.shutdown_tx.lock().await.take();
    if let Some(tx) = tx {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            let _ = tx.send(());
        });
        Json(serde_json::json!({ "ok": true }))
    } else {
        Json(serde_json::json!({ "ok": false }))
    }
}

async fn load_config(State(st): State<WizardState>) -> Json<ExistingConfig> {
    match tokio::fs::read_to_string(&st.config_path).await {
        Ok(content) => Json(parse_existing_config(&content)),
        Err(_) => Json(ExistingConfig::default()),
    }
}

// ── OAuth handlers ─────────────────────────────────────────────────────

async fn oauth_start(
    State(st): State<WizardState>,
    Query(params): Query<OAuthStartQuery>,
) -> Result<Json<OAuthStartResponse>, (StatusCode, String)> {
    let err = |status: StatusCode, msg: String| (status, msg);

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

    let discovery = discover_oauth_endpoints(&st.http_client, &origin)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;

    let reg_endpoint = discovery.registration_endpoint.ok_or_else(|| {
        err(
            StatusCode::NOT_IMPLEMENTED,
            "MCP server does not advertise a Dynamic Client Registration endpoint".into(),
        )
    })?;

    let redir = redirect_uri();
    let reg_body = ClientRegistrationRequest {
        client_name: "RustFox Setup".into(),
        redirect_uris: vec![redir.clone()],
        grant_types: vec!["authorization_code".into()],
        response_types: vec!["code".into()],
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

    let code_verifier = pkce_verifier();
    let code_challenge = pkce_challenge(&code_verifier);
    let oauth_state = random_state();

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

    st.oauth_sessions.lock().await.insert(
        oauth_state.clone(),
        OAuthSession {
            server_name: params.server.clone(),
            code_verifier,
            client_id: reg_resp.client_id,
            client_secret: reg_resp.client_secret,
            token_endpoint: discovery.token_endpoint,
            access_token: None,
            refresh_token: None,
            expires_in: None,
        },
    );

    Ok(Json(OAuthStartResponse {
        state: oauth_state,
        auth_url: auth_url.to_string(),
    }))
}

async fn oauth_callback(
    State(st): State<WizardState>,
    Query(params): Query<OAuthCallbackQuery>,
) -> Html<String> {
    let (server_name, code_verifier, client_id, client_secret, token_endpoint) =
        {
            let sessions = st.oauth_sessions.lock().await;
            match sessions.get(&params.state) {
            Some(s) => (
                s.server_name.clone(), s.code_verifier.clone(),
                s.client_id.clone(), s.client_secret.clone(),
                s.token_endpoint.clone(),
            ),
            None => return Html(
                "<html><body><p>Unknown OAuth state. Please close this window and try again.</p>\
                 <script>setTimeout(()=>window.close(),3000)</script></body></html>".into(),
            ),
        }
        };

    let redir = redirect_uri();
    let mut token_params = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", params.code.clone()),
        ("redirect_uri", redir),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    if let Some(secret) = client_secret {
        token_params.push(("client_secret", secret));
    }

    match st
        .http_client
        .post(&token_endpoint)
        .form(&token_params)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<OAuthTokenResponse>().await {
            Ok(tok) => {
                if let Some(session) = st.oauth_sessions.lock().await.get_mut(&params.state) {
                    session.access_token = Some(tok.access_token);
                    session.refresh_token = tok.refresh_token;
                    session.expires_in = tok.expires_in;
                }
                Html(format!(
                    "<html><head><title>Authorized</title></head><body>\
                         <p style=\"font-family:sans-serif;text-align:center;margin-top:4rem\">\
                         ✅ {server_name} authorization successful! You can close this window.</p>\
                         <script>window.close();</script></body></html>"
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

async fn oauth_token_poll(
    State(st): State<WizardState>,
    Query(params): Query<OAuthTokenQuery>,
) -> Result<Json<OAuthTokenPollResponse>, StatusCode> {
    let sessions = st.oauth_sessions.lock().await;
    let session = sessions.get(&params.state).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(OAuthTokenPollResponse {
        ready: session.access_token.is_some(),
        token: session.access_token.clone(),
        refresh_token: session.refresh_token.clone(),
        expires_in: session.expires_in,
        token_endpoint: Some(session.token_endpoint.clone()),
        oauth_client_id: Some(session.client_id.clone()),
        oauth_client_secret: session.client_secret.clone(),
    }))
}

// ── OAuth helpers ──────────────────────────────────────────────────────

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

fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── CLI mode ───────────────────────────────────────────────────────────

fn run_cli(config_dir: &Path) -> Result<()> {
    use std::io::{self, Write};

    println!("============================================");
    println!("  RustFox CLI Setup");
    println!("============================================");
    println!("Press Enter to accept [defaults].\n");

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
        read_line("Model [moonshotai/kimi-k2.6]: ")?,
        "moonshotai/kimi-k2.6",
    );
    let db_path = or_default(read_line("Memory DB path [rustfox.db]: ")?, "rustfox.db");
    let location = read_line("Your location (optional, e.g. Tokyo, Japan): ")?;

    let config = format_config(&ConfigParams {
        tg_token: &tg_token,
        user_ids: &user_ids,
        or_key: &or_key,
        model: &model,
        max_tokens: 4096,
        db_path: &db_path,
        location: &location,
    });

    let config_path = config_dir.join("config.toml");
    std::fs::write(&config_path, &config)
        .with_context(|| format!("Could not write {}", config_path.display()))?;

    println!("\n✓ config.toml saved to {}", config_path.display());

    // Offer service installation
    print!("\nInstall as a background service? [Y/n]: ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    if buf.trim().is_empty() || buf.trim().eq_ignore_ascii_case("y") {
        if let Err(e) = crate::setup::service::handle(crate::setup::service::Action::Install) {
            eprintln!("Warning: Service installation failed: {e}");
            eprintln!("You can retry later with: rustfox --service install");
        }
    }

    Ok(())
}

// ── Config formatting ──────────────────────────────────────────────────

pub struct ConfigParams<'a> {
    pub tg_token: &'a str,
    pub user_ids: &'a str,
    pub or_key: &'a str,
    pub model: &'a str,
    pub max_tokens: u32,
    pub db_path: &'a str,
    pub location: &'a str,
}

pub fn format_config(p: &ConfigParams<'_>) -> String {
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

[memory]
database_path = "{db_path}"

[general]
{loc_line}
"#
    )
}

// ── Config parsing ─────────────────────────────────────────────────────

pub fn parse_existing_config(content: &str) -> ExistingConfig {
    let raw: RawConfig = match toml::from_str(content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Could not parse existing config.toml: {e}");
            return ExistingConfig::default();
        }
    };

    let tg = raw.telegram.clone().unwrap_or_default();
    let openrouter = raw.openrouter.clone().unwrap_or_default();
    let mem = raw.memory.clone().unwrap_or_default();

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
        .clone()
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

    let mut cfg = ExistingConfig {
        exists: true,
        telegram_token: tg.bot_token.unwrap_or_default(),
        allowed_user_ids,
        openrouter_key: openrouter.api_key.clone().unwrap_or_default(),
        model: openrouter.model.clone().unwrap_or_default(),
        max_tokens: openrouter.max_tokens.unwrap_or(0),
        system_prompt: openrouter.system_prompt.clone().unwrap_or_default(),
        location: raw
            .general
            .as_ref()
            .and_then(|g| g.location.clone())
            .unwrap_or_default(),
        db_path: mem.database_path.clone().unwrap_or_default(),
        mcp_servers,
        ..ExistingConfig::default()
    };

    if let Some(ref or_cfg) = raw.openrouter {
        cfg.supports_vision = or_cfg.supports_vision.unwrap_or(false);
        cfg.base_url = or_cfg.base_url.clone().unwrap_or_default();
    }
    if let Some(ref general) = raw.general {
        cfg.home_dir = general.home.clone().unwrap_or_default();
    }
    if let Some(ref agent) = raw.agent {
        cfg.agent_max_iterations = agent.max_iterations.unwrap_or(25);
        cfg.agent_empty_response_retry_limit = agent.empty_response_retry_limit.unwrap_or(3);
    }
    if let Some(ref langsmith) = raw.langsmith {
        cfg.langsmith_key = langsmith.api_key.clone().unwrap_or_default();
        cfg.langsmith_project = langsmith.project.clone().unwrap_or_default();
    }
    if let Some(ref embedding) = raw.embedding {
        cfg.embedding_key = embedding.api_key.clone().unwrap_or_default();
        cfg.embedding_base_url = embedding.base_url.clone().unwrap_or_default();
        cfg.embedding_model = embedding.model.clone().unwrap_or_default();
        cfg.embedding_dimensions = embedding.dimensions.unwrap_or(0);
    }
    if let Some(ref ocr) = raw.ocr {
        cfg.ocr_model_dir = ocr.model_dir.clone().unwrap_or_default();
    }
    if let Some(ref learning) = raw.learning {
        cfg.learning_skill_extraction_enabled = learning.skill_extraction_enabled.unwrap_or(false);
        cfg.learning_skill_extraction_threshold = learning.skill_extraction_threshold.unwrap_or(0);
        cfg.learning_user_model_update_interval = learning.user_model_update_interval.unwrap_or(0);
        cfg.learning_user_model_cron = learning.user_model_cron.clone().unwrap_or_default();
    }
    if let Some(ref skills) = raw.skills {
        cfg.skills_dir = skills.directory.clone().unwrap_or_default();
    }
    if let Some(ref agents) = raw.agents_config {
        cfg.agents_dir = agents.directory.clone().unwrap_or_default();
    }

    cfg
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_toml_returns_not_exists() {
        let cfg = parse_existing_config("this is not valid toml !!!");
        assert!(!cfg.exists);
    }

    #[test]
    fn test_pkce_verifier_length() {
        let v = pkce_verifier();
        assert_eq!(v.len(), 43);
    }

    #[test]
    fn test_pkce_challenge_is_base64url() {
        let verifier = pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        assert_eq!(challenge.len(), 43);
    }

    #[test]
    fn test_random_state_is_32_hex_chars() {
        let s = random_state();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    fn cfg(
        tg: &str,
        ids: &str,
        key: &str,
        model: &str,
        _sandbox: &str,
        db: &str,
        loc: &str,
    ) -> String {
        format_config(&ConfigParams {
            tg_token: tg,
            user_ids: ids,
            or_key: key,
            model,
            max_tokens: 4096,
            db_path: db,
            location: loc,
        })
    }

    #[test]
    fn test_telegram_section_present() {
        let out = cfg("mytoken", "123456", "key", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[telegram]"));
        assert!(out.contains(r#"bot_token = "mytoken""#));
    }

    #[test]
    fn test_openrouter_section_present() {
        let out = cfg("t", "1", "sk-or-abc", "gpt-4o", "/tmp", "db.db", "");
        assert!(out.contains("[openrouter]"));
        assert!(out.contains(r#"api_key = "sk-or-abc""#));
    }

    #[test]
    fn test_location_included_when_set() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "Tokyo, Japan");
        assert!(out.contains(r#"location = "Tokyo, Japan""#));
    }

    #[test]
    fn test_location_commented_when_empty() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("# location ="));
        assert!(!out.contains("\nlocation = "));
    }

    #[test]
    fn test_multiple_user_ids_comma_separated() {
        let out = cfg("t", "111, 222, 333", "k", "m", "/tmp", "db.db", "");
        assert!(out.contains("allowed_user_ids = [111, 222, 333]"));
    }

    // ── Tests migrated from src/bin/setup.rs ──

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
        let toml = r#"
[telegram]
bot_token = "partial"
allowed_user_ids = [42]
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "partial");
        assert_eq!(cfg.model, "");
    }

    #[test]
    fn test_parse_string_user_ids() {
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

    #[test]
    fn test_no_relative_skills_directory() {
        let out = cfg("t", "1", "k", "m", "/tmp", "db.db", "");
        assert!(
            !out.contains(r#"directory = "skills""#),
            "Generated config must not hardcode a CWD-relative skills directory"
        );
    }
}
