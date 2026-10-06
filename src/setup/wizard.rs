//! Setup wizard — web (Axum server + browser) and CLI modes.
//!
//! Extracted from `src/bin/setup.rs` so the main binary can reuse it
//! via `rustfox --setup`.

use anyhow::{bail, Context, Result};
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
        return run_cli(config_dir).await;
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

#[derive(Deserialize)]
struct AddBotRequest {
    id: String,
    bot_token: String,
    /// Telegram user id used for the new bot allowlist (caller / owner).
    allowed_user_id: u64,
}

#[derive(Serialize)]
struct AddBotResponse {
    ok: bool,
    id: String,
    persona: String,
    bak_path: String,
    allowed_user_ids: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
    /// Existing `[[bots]]` entries (tokens redacted in API responses).
    #[serde(default)]
    pub bots: Vec<ExistingBot>,
}

#[derive(Serialize, Default, Clone)]
pub struct ExistingBot {
    pub id: String,
    pub persona: String,
    pub allowed_user_ids: String,
    /// Always redacted (`***`) — never echo raw tokens from the wizard API.
    pub bot_token: String,
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
    #[serde(default)]
    pub bots: Vec<RawBot>,
}

#[derive(Deserialize, Default, Clone)]
pub struct RawBot {
    pub id: Option<String>,
    pub bot_token: Option<String>,
    pub allowed_user_ids: Option<Vec<toml::Value>>,
    pub persona: Option<String>,
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
        .route("/api/thin-preview", post(thin_preview))
        .route("/api/thin-save", post(thin_save))
        .route("/api/ollama/local", get(ollama_local))
        .route("/api/ollama/library", get(ollama_library))
        .route("/api/ollama/pull", post(ollama_pull))
        .route("/api/openrouter/models", get(openrouter_models))
        .route("/api/add-bot", post(add_bot))
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
        if !open_browser(&url) {
            println!("Couldn't open a browser. Open the URL above manually.");
        }
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .context("Server error")?;

    Ok(())
}

/// Launchers to try, in order, for the wizard URL (ADR 0015). WSL hands off
/// to the Windows browser; headless Linux gets none (URL is printed instead).
fn browser_openers(wsl: bool, has_display: bool) -> &'static [&'static [&'static str]] {
    if cfg!(target_os = "macos") {
        &[&["open"]]
    } else if cfg!(windows) {
        &[&["cmd", "/c", "start"]]
    } else if wsl {
        &[&["wslview"], &["cmd.exe", "/c", "start"]]
    } else if has_display {
        &[&["xdg-open"]]
    } else {
        &[]
    }
}

/// `true` if some launcher exited 0.
fn open_browser(url: &str) -> bool {
    let wsl = std::fs::read_to_string("/proc/version")
        .is_ok_and(|v| v.to_lowercase().contains("microsoft"));
    let has_display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    browser_openers(wsl, has_display).iter().any(|cmd| {
        std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

// ── Web handlers ───────────────────────────────────────────────────────

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Open SecretStore beside `config_path` and seal plaintext bot tokens and the
/// `[openrouter].api_key` (ADR 0016) so
/// the written file never contains them (wizard first-save / re-save).
fn seal_credentials_for_wizard_write(config_path: &Path, content: &str) -> anyhow::Result<String> {
    let home = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let (store, _) = crate::secret_store::open(&home)?;
    let (sealed, n) =
        crate::secret_store::seal_plaintext_bot_tokens_in_config(content, store.as_ref())?;
    if n > 0 {
        report_sealed_credentials();
    }
    Ok(sealed)
}

/// Report (without echoing any secret material) that plaintext credentials were
/// sealed. Kept argument-free so no sealing-call output flows into a log sink.
fn report_sealed_credentials() {
    println!("\u{2713} Credentials sealed into SecretStore (config now holds refs only)");
}

async fn save_config(
    State(st): State<WizardState>,
    Json(body): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, StatusCode> {
    // When [[bots]] already exists, merge-preserve those rows so a full wizard
    // rewrite cannot wipe secondary tools/model/persona/allowlist (§7.7 TL HOLD).
    let path = persist_wizard_toml(&st.config_path, body.config, false).await?;
    Ok(Json(SaveResponse { ok: true, path }))
}

#[derive(Debug, Deserialize)]
struct ThinRequest {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    openrouter_api_key: String,
    #[serde(default)]
    openrouter_model: String,
    #[serde(default)]
    ollama_model: String,
    #[serde(default)]
    bot_token: String,
    #[serde(default)]
    system_prompt: String,
}

#[derive(Debug, Serialize)]
struct ThinResponse {
    ok: bool,
    config: String,
    error: Option<String>,
    path: Option<String>,
}

fn thin_answers(body: &ThinRequest) -> Result<super::thin::ThinAnswers> {
    Ok(super::thin::ThinAnswers {
        provider: super::thin::ThinProvider::parse(&body.provider)?,
        openrouter_api_key: body.openrouter_api_key.clone(),
        openrouter_model: body.openrouter_model.clone(),
        ollama_model: body.ollama_model.clone(),
        bot_token: body.bot_token.clone(),
        system_prompt: body.system_prompt.clone(),
    })
}

fn thin_response(result: Result<String>) -> Json<ThinResponse> {
    match result {
        Ok(config) => Json(ThinResponse {
            ok: true,
            config,
            error: None,
            path: None,
        }),
        Err(e) => Json(ThinResponse {
            ok: false,
            config: String::new(),
            error: Some(e.to_string()),
            path: None,
        }),
    }
}

async fn probe_ollama(client: &reqwest::Client) -> super::thin::OllamaDetect {
    let reached = match client
        .get(super::thin::OLLAMA_TAGS_URL)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => return super::thin::detect_from_http(true, &body),
            Err(_) => false,
        },
        _ => false,
    };
    super::thin::detect_from_http(reached, "")
}

async fn ollama_local(State(st): State<WizardState>) -> Json<serde_json::Value> {
    let detect = probe_ollama(&st.http_client).await;
    Json(serde_json::json!({
        "running": detect.running,
        "models": detect.models,
        "message": detect.message,
    }))
}

#[derive(Debug, Deserialize)]
struct LibraryQuery {
    #[serde(default)]
    q: String,
}

async fn load_ollama_library(client: &reqwest::Client, query: &str) -> super::thin::LibraryLoad {
    let url = super::thin::library_request_url(query);
    let reached = match client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "RustFox-setup")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => return super::thin::library_from_http(true, &body, query),
            Err(_) => false,
        },
        _ => false,
    };
    super::thin::library_from_http(reached, "", query)
}

/// Ollama library names from `https://ollama.com/library`. `q` filters that
/// catalog (Ollama's `/search` is paginated, so it is not the source of truth).
/// A failed fetch is an error and an empty list — never a hardcoded catalog.
async fn ollama_library(
    State(st): State<WizardState>,
    Query(query): Query<LibraryQuery>,
) -> Json<serde_json::Value> {
    let load = load_ollama_library(&st.http_client, &query.q).await;
    Json(serde_json::json!({
        "ok": load.ok,
        "models": load.models,
        "error": load.error,
    }))
}

async fn openrouter_models() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "models": super::thin::OPENROUTER_MODELS,
        "default": super::thin::OPENROUTER_DEFAULT_MODEL,
    }))
}

#[derive(Debug, Deserialize)]
struct OllamaPullRequest {
    #[serde(default)]
    model: String,
}

/// Pulls one library model. Refuses a typed id. Does not run during detect.
async fn ollama_pull(
    State(st): State<WizardState>,
    Json(body): Json<OllamaPullRequest>,
) -> Json<serde_json::Value> {
    let library = load_ollama_library(&st.http_client, "").await;
    if !library.ok {
        return Json(serde_json::json!({
            "ok": false,
            "error": library.error.unwrap_or(super::thin::OLLAMA_LIBRARY_UNAVAILABLE),
        }));
    }
    let payload = match super::thin::pull_request_body(&body.model, &library.models) {
        Ok(body) => body,
        Err(e) => {
            return Json(serde_json::json!({ "ok": false, "error": e.to_string() }));
        }
    };
    let client = st.http_client.clone();
    let result = client
        .post(super::thin::OLLAMA_PULL_URL)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(60 * 30))
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            Json(serde_json::json!({ "ok": true, "model": body.model.trim() }))
        }
        Ok(resp) => {
            let status = resp.status();
            Json(serde_json::json!({
                "ok": false,
                "error": format!("Ollama pull failed ({status})"),
            }))
        }
        Err(_) => Json(serde_json::json!({
            "ok": false,
            "error": super::thin::OLLAMA_NOT_RUNNING,
        })),
    }
}

async fn thin_preview(Json(body): Json<ThinRequest>) -> Json<ThinResponse> {
    thin_response(thin_answers(&body).and_then(|a| super::thin::render_config(&a)))
}

async fn thin_save(
    State(st): State<WizardState>,
    Json(body): Json<ThinRequest>,
) -> Json<ThinResponse> {
    let answers = match thin_answers(&body) {
        Ok(a) => a,
        Err(e) => return thin_response(Err(e)),
    };
    if answers.provider == super::thin::ThinProvider::Ollama {
        let detect = probe_ollama(&st.http_client).await;
        if !detect.running {
            return thin_response(Err(anyhow::anyhow!(super::thin::OLLAMA_NOT_RUNNING)));
        }
        if !super::thin::ollama_choice_allowed(&answers.ollama_model, &detect.models) {
            return thin_response(Err(anyhow::anyhow!("pick a detected Ollama model")));
        }
    }
    let rendered = match super::thin::render_config(&answers) {
        Ok(text) => text,
        Err(e) => return thin_response(Err(e)),
    };
    match persist_wizard_toml(&st.config_path, rendered.clone(), true).await {
        Ok(path) => Json(ThinResponse {
            ok: true,
            config: rendered,
            error: None,
            path: Some(path),
        }),
        Err(_) => Json(ThinResponse {
            ok: false,
            config: String::new(),
            error: Some("failed to save config".into()),
            path: None,
        }),
    }
}

async fn persist_wizard_toml(
    config_path: &Path,
    wizard_toml: String,
    preserve_allowlist: bool,
) -> Result<String, StatusCode> {
    let content = if config_path.exists() {
        match tokio::fs::read_to_string(config_path).await {
            Ok(existing) => {
                let wizard = if preserve_allowlist {
                    super::thin::keep_existing_allowlist(&existing, &wizard_toml).map_err(|e| {
                        eprintln!("thin save allowlist preserve failed: {e}");
                        StatusCode::BAD_REQUEST
                    })?
                } else {
                    wizard_toml
                };
                merge_wizard_save(&existing, &wizard).map_err(|e| {
                    eprintln!("save-config merge failed: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
            }
            Err(_) => wizard_toml,
        }
    } else {
        wizard_toml
    };

    let path_for_seal = config_path.to_path_buf();
    let sealed = tokio::task::spawn_blocking(move || {
        seal_credentials_for_wizard_write(&path_for_seal, &content)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        eprintln!("save-config SecretStore seal failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tokio::fs::write(config_path, &sealed)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let path = config_path.to_string_lossy().to_string();
    println!("\n✓ config.toml saved to {path}");
    Ok(path)
}

/// POST /api/add-bot — append `[[bots]]` via validate → bak → atomic
/// ([`crate::agents_edit::append_bot_binding`]). First multi-bot add materializes
/// legacy `[telegram]` into `[[bots]]`.
async fn add_bot(
    State(st): State<WizardState>,
    Json(body): Json<AddBotRequest>,
) -> Json<AddBotResponse> {
    let path = st.config_path.clone();
    let id = body.id.clone();
    let token = body.bot_token.clone();
    let uid = body.allowed_user_id;
    let result = tokio::task::spawn_blocking(move || {
        let home = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let (store, _) = crate::secret_store::open(&home)?;
        crate::agents_edit::append_bot_binding(&path, &id, &token, uid, store.as_ref())
    })
    .await;

    match result {
        Ok(Ok(r)) => {
            println!(
                "✓ Added bot `{}` (persona=`{}`) → bak {}",
                r.id,
                r.persona,
                r.bak_path.display()
            );
            Json(AddBotResponse {
                ok: true,
                id: r.id,
                persona: r.persona,
                bak_path: r.bak_path.display().to_string(),
                allowed_user_ids: r.allowed_user_ids,
                error: None,
            })
        }
        Ok(Err(e)) => Json(AddBotResponse {
            ok: false,
            id: body.id,
            persona: String::new(),
            bak_path: String::new(),
            allowed_user_ids: vec![],
            error: Some(e.to_string()),
        }),
        Err(e) => Json(AddBotResponse {
            ok: false,
            id: body.id,
            persona: String::new(),
            bak_path: String::new(),
            allowed_user_ids: vec![],
            error: Some(format!("Task join failed: {e}")),
        }),
    }
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
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── CLI mode ───────────────────────────────────────────────────────────

async fn run_cli(config_dir: &Path) -> Result<()> {
    use std::io::{self, Write};

    println!("============================================");
    println!("  RustFox CLI Setup");
    println!("============================================");
    println!("Press Enter to accept [defaults].\n");

    let read_line = |prompt: &str| -> Result<String> {
        io::stdout().write_all(prompt.as_bytes())?;
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim().to_owned())
    };

    let provider_raw = read_line("Provider [OpenRouter/ollama] (OpenRouter): ")?;
    let mut provider = super::thin::ThinProvider::parse(&provider_raw)?;
    let client = reqwest::Client::new();
    let mut ollama_model = String::new();
    let mut or_key = String::new();

    if provider == super::thin::ThinProvider::Ollama {
        let detect = probe_ollama(&client).await;
        if !detect.running {
            println!("{}", super::thin::OLLAMA_NOT_RUNNING);
            println!("Using OpenRouter.");
            provider = super::thin::ThinProvider::OpenRouter;
        } else {
            println!("Ollama models on this machine:");
            for (i, name) in detect.models.iter().enumerate() {
                println!("  {}) {}", i + 1, name);
            }
            println!("  0) Pull from the Ollama library");
            let pick = read_line("Pick a number: ")?;
            if pick == "0" {
                let filter = read_line("Filter the Ollama library (empty shows the library): ")?;
                let loaded = load_ollama_library(&client, &filter).await;
                if !loaded.ok {
                    bail!(
                        "{}",
                        loaded
                            .error
                            .unwrap_or(super::thin::OLLAMA_LIBRARY_UNAVAILABLE)
                    );
                }
                let library =
                    super::thin::library_not_already_local(&loaded.models, &detect.models);
                if library.is_empty() {
                    bail!("no Ollama library models match that filter");
                }
                println!("Ollama library:");
                for (i, name) in library.iter().enumerate() {
                    println!("  {}) {}", i + 1, name);
                }
                let raw = read_line("Pick a number to pull: ")?;
                let idx: usize = raw
                    .parse()
                    .ok()
                    .filter(|n| (1..=library.len()).contains(n))
                    .context("pick a model from the Ollama library list")?;
                let name = library[idx - 1].clone();
                let payload = super::thin::pull_request_body(&name, &loaded.models)?;
                let resp = client
                    .post(super::thin::OLLAMA_PULL_URL)
                    .json(&payload)
                    .timeout(std::time::Duration::from_secs(60 * 30))
                    .send()
                    .await
                    .context(super::thin::OLLAMA_NOT_RUNNING)?;
                if !resp.status().is_success() {
                    bail!("Ollama pull failed ({})", resp.status());
                }
                let again = probe_ollama(&client).await;
                ollama_model = again
                    .models
                    .into_iter()
                    .find(|m| *m == name || m.starts_with(&format!("{name}:")))
                    .unwrap_or_else(|| format!("{name}:latest"));
            } else {
                let idx: usize = pick
                    .parse()
                    .ok()
                    .filter(|n| *n >= 1 && *n <= detect.models.len())
                    .context("pick a detected Ollama model")?;
                ollama_model = detect.models[idx - 1].clone();
            }
        }
    }

    let mut openrouter_model = String::new();
    if provider == super::thin::ThinProvider::OpenRouter {
        or_key = read_line("OpenRouter API key: ")?;
        println!("OpenRouter model:");
        for (i, name) in super::thin::OPENROUTER_MODELS.iter().enumerate() {
            let mark = if *name == super::thin::OPENROUTER_DEFAULT_MODEL {
                " (default)"
            } else {
                ""
            };
            println!("  {}) {name}{mark}", i + 1);
        }
        let other_n = super::thin::OPENROUTER_MODELS.len() + 1;
        println!("  {}) Other", other_n);
        println!("Catalog: {}", super::thin::OPENROUTER_MODELS_URL);
        let prompt = format!(
            "Pick a number [{}]: ",
            super::thin::OPENROUTER_DEFAULT_MODEL
        );
        let pick = read_line(&prompt)?;
        let typed = if pick.trim().parse::<usize>().ok() == Some(other_n) {
            Some(read_line("Model id (provider/model): ")?)
        } else {
            None
        };
        openrouter_model = super::thin::openrouter_model_from_cli_choice(&pick, typed.as_deref())?;
    }
    let tg_token = read_line("Telegram bot token: ")?;
    let sentence = read_line("System prompt (one sentence): ")?;

    let config = super::thin::render_config(&super::thin::ThinAnswers {
        provider,
        openrouter_api_key: or_key,
        openrouter_model,
        ollama_model,
        bot_token: tg_token,
        system_prompt: sentence,
    })?;

    let config_path = config_dir.join("config.toml");
    let to_write = if config_path.exists() {
        let existing = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Could not read {}", config_path.display()))?;
        let wizard = super::thin::keep_existing_allowlist(&existing, &config)?;
        merge_wizard_save(&existing, &wizard)
            .with_context(|| "Failed to merge wizard save with existing [[bots]]")?
    } else {
        config
    };
    let sealed = seal_credentials_for_wizard_write(&config_path, &to_write)
        .context("Failed to seal bot tokens into SecretStore before wizard write")?;
    std::fs::write(&config_path, &sealed)
        .with_context(|| format!("Could not write {}", config_path.display()))?;

    println!("\n✓ config.toml saved to {}", config_path.display());

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

// ── Wizard save merge (preserve [[bots]]) ──────────────────────────────

/// Merge a wizard-generated config with an on-disk config so untouched
/// `[[bots]]` rows keep tools/model/persona/allowlist/system_prompt_file.
///
/// Rules (TL HOLD on full wizard save):
/// - If `existing` has a non-empty `bots` array: take non-bot sections from
///   `wizard`, restore `bots` from `existing` at the TOML Value level
///   (verbatim field preserve), apply wizard `[telegram]` `bot_token` +
///   `allowed_user_ids` onto the shim bot only (`main` → `default` → first),
///   and drop `[telegram]` from the result (`[[bots]]` wins).
/// - Otherwise return `wizard` unchanged (legacy `[telegram]` path; callers
///   may then [`crate::agents_edit::append_bot_binding`]).
pub fn merge_wizard_save(existing: &str, wizard: &str) -> Result<String> {
    let existing_doc: toml::Value =
        toml::from_str(existing).context("Failed to parse existing config.toml")?;
    let existing_bots = existing_doc
        .get("bots")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .cloned();

    let Some(mut bots) = existing_bots else {
        return Ok(wizard.to_string());
    };

    let mut wizard_doc: toml::Value =
        toml::from_str(wizard).context("Failed to parse wizard-generated config")?;
    let table = wizard_doc
        .as_table_mut()
        .context("wizard config root is not a table")?;

    if let Some(telegram) = table.remove("telegram") {
        apply_telegram_to_shim_bot(&mut bots, &telegram);
    }

    table.insert("bots".to_string(), toml::Value::Array(bots));

    toml::to_string_pretty(&wizard_doc).context("Failed to serialize merged wizard config")
}

/// Update only shim-bot token + allowlist from wizard `[telegram]`; leave
/// tools/model/persona/system_prompt_file (and every other bot) untouched.
fn apply_telegram_to_shim_bot(bots: &mut [toml::Value], telegram: &toml::Value) {
    let shim_idx = bots
        .iter()
        .position(|b| b.get("id").and_then(|v| v.as_str()) == Some("main"))
        .or_else(|| {
            bots.iter()
                .position(|b| b.get("id").and_then(|v| v.as_str()) == Some("default"))
        })
        .unwrap_or(0);

    let Some(shim) = bots.get_mut(shim_idx).and_then(|b| b.as_table_mut()) else {
        return;
    };

    if let Some(token) = telegram.get("bot_token").and_then(|v| v.as_str()) {
        if !token.trim().is_empty() {
            shim.insert(
                "bot_token".to_string(),
                toml::Value::String(token.to_string()),
            );
        }
    }
    if let Some(ids) = telegram.get("allowed_user_ids") {
        shim.insert("allowed_user_ids".to_string(), ids.clone());
    }
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

    let bots: Vec<ExistingBot> = raw
        .bots
        .iter()
        .filter_map(|b| {
            let id = b.id.clone().filter(|s| !s.is_empty())?;
            let persona = b
                .persona
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            let allowed_user_ids = b
                .allowed_user_ids
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|v| match v {
                    toml::Value::Integer(i) => i.to_string(),
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(ExistingBot {
                id,
                persona,
                allowed_user_ids,
                bot_token: "***".to_string(),
            })
        })
        .collect();

    // Prefer primary token from [[bots]][0] when [telegram] is absent.
    let telegram_token = tg.bot_token.unwrap_or_else(|| {
        raw.bots
            .first()
            .and_then(|b| b.bot_token.clone())
            .unwrap_or_default()
    });
    let allowed_user_ids = if allowed_user_ids.is_empty() && !bots.is_empty() {
        bots[0].allowed_user_ids.clone()
    } else {
        allowed_user_ids
    };

    let mut cfg = ExistingConfig {
        exists: true,
        telegram_token,
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
        bots,
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

    #[cfg(target_os = "linux")]
    #[test]
    fn wsl_opens_the_windows_browser_not_xdg_open() {
        let got = browser_openers(true, true);
        assert_eq!(got, &[&["wslview"][..], &["cmd.exe", "/c", "start"][..]]);
        assert_eq!(browser_openers(true, false), got, "WSL ignores DISPLAY");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn headless_linux_skips_auto_open_desktop_uses_xdg_open() {
        assert!(browser_openers(false, false).is_empty());
        assert_eq!(browser_openers(false, true), &[&["xdg-open"][..]]);
    }
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
    fn wizard_first_save_seals_botfather_token() {
        use crate::secret_store::{
            seal_plaintext_bot_tokens_in_config, FakeSecretStore, SecretStore,
        };
        let store = FakeSecretStore::new();
        let raw = cfg(
            "111111111:AAWizardCliFirstSaveTokenXX",
            "123456",
            "key",
            "gpt-4o",
            "/tmp",
            "db.db",
            "",
        );
        assert!(raw.contains("AAWizardCliFirstSaveTokenXX"));
        let (sealed, n) = seal_plaintext_bot_tokens_in_config(&raw, &store).unwrap();
        assert_eq!(n, 2, "bot token + [openrouter].api_key");
        assert!(!sealed.contains("AAWizardCliFirstSaveTokenXX"));
        assert!(sealed.contains("secret:bot.default.token"));
        assert!(sealed.contains(r#"api_key = "secret:openrouter.api_key""#));
        assert_eq!(
            store.get("openrouter.api_key").unwrap().unwrap().expose(),
            "key"
        );
        assert_eq!(
            store.get("bot.default.token").unwrap().unwrap().expose(),
            "111111111:AAWizardCliFirstSaveTokenXX"
        );
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

    #[test]
    fn test_parse_bots_array_redacts_tokens() {
        let toml = r#"
[[bots]]
id = "main"
bot_token = "111111111:AASecretMainTokenValueXXXX"
allowed_user_ids = [42]
persona = "main"

[[bots]]
id = "researcher"
bot_token = "222222222:AASecretResearcherTokenYY"
allowed_user_ids = [42, 99]
persona = "researcher"

[openrouter]
api_key = "sk-test"
model = "test"

[sandbox]
allowed_directory = "/tmp"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.bots.len(), 2);
        assert_eq!(cfg.bots[0].id, "main");
        assert_eq!(cfg.bots[1].id, "researcher");
        assert_eq!(cfg.bots[1].persona, "researcher");
        assert_eq!(cfg.bots[1].allowed_user_ids, "42, 99");
        for b in &cfg.bots {
            assert_eq!(b.bot_token, "***");
            assert!(!b.bot_token.contains("AASecret"));
        }
        // Primary fields fall back from bots[0] when [telegram] absent
        assert_eq!(cfg.telegram_token, "111111111:AASecretMainTokenValueXXXX");
        assert_eq!(cfg.allowed_user_ids, "42");
    }

    #[test]
    fn wizard_add_another_bot_appends_via_bak() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[telegram]
bot_token = "111111111:AALegacyTokenSecretValueXX"
allowed_user_ids = [7]

[openrouter]
api_key = "sk-test"
model = "test-model"

[sandbox]
allowed_directory = "/tmp"
"#,
        )
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let store = crate::secret_store::FakeSecretStore::new();
        let r = crate::agents_edit::append_bot_binding(
            &path,
            "researcher",
            "222222222:AANewTokenSecretValueYYYYYY",
            7,
            &store,
        )
        .unwrap();
        assert_eq!(r.id, "researcher");
        assert!(r.bak_path.exists());
        assert_eq!(std::fs::read_to_string(&r.bak_path).unwrap(), before);
        let after = std::fs::read_to_string(&path).unwrap();
        let mut cfg: crate::config::Config = toml::from_str(&after).unwrap();
        cfg.normalize_bots().unwrap();
        assert_eq!(cfg.bots.len(), 2);
        assert!(cfg.bots.iter().any(|b| b.id == "default"));
        assert!(cfg.bots.iter().any(|b| b.id == "researcher"));
    }

    /// Full wizard save must not wipe secondary bot tools/model/persona/allowlist
    /// when `[[bots]]` already exists (TL HOLD on PR #74).
    #[test]
    fn wizard_full_save_preserves_secondary_bot_customizations() {
        let existing = r#"
[[bots]]
id = "main"
bot_token = "111111111:AAMainTokenSecretValueXXXX"
allowed_user_ids = [42]
persona = "main"

[[bots]]
id = "researcher"
bot_token = "222222222:AAResearcherTokenSecretYY"
allowed_user_ids = [42, 99]
persona = "researcher"
model = "moonshotai/kimi-k2.6"
tools = ["read_file", "list_files", "web_search", "invoke_agent"]

[openrouter]
api_key = "sk-old"
model = "old-model"

[sandbox]
allowed_directory = "/tmp"
"#;

        // Mimic generateToml / format_config: writes [telegram] + other sections, no [[bots]].
        let wizard = r#"
[telegram]
bot_token = "111111111:AAMainTokenSecretValueXXXX"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-new"
model = "new-model"
base_url = "https://openrouter.ai/api/v1"
max_tokens = 4096

[memory]
database_path = "rustfox.db"

[general]
# location = "Your City, Country"
"#;

        let merged = merge_wizard_save(existing, wizard).unwrap();
        let mut cfg: crate::config::Config = toml::from_str(&merged).unwrap();
        cfg.normalize_bots().unwrap();

        assert_eq!(cfg.bots.len(), 2, "both bots must survive full wizard save");
        let research = cfg.bots.iter().find(|b| b.id == "researcher").unwrap();
        assert_eq!(research.persona, "researcher");
        assert_eq!(research.allowed_user_ids, vec![42, 99]);
        assert_eq!(
            research.model.as_deref(),
            Some("moonshotai/kimi-k2.6"),
            "secondary model must survive"
        );
        assert_eq!(
            research.tools,
            Some(vec![
                "read_file".into(),
                "list_files".into(),
                "web_search".into(),
                "invoke_agent".into(),
            ]),
            "secondary tools must survive"
        );
        assert_eq!(
            research.bot_token, "222222222:AAResearcherTokenSecretYY",
            "secondary token must survive (not redacted/wiped)"
        );

        // Wizard-edited non-bot sections apply.
        assert_eq!(cfg.openrouter.api_key, "sk-new");
        assert_eq!(cfg.openrouter.model, "new-model");

        // No leftover [telegram] alongside preserved [[bots]] (bots win cleanly).
        assert!(
            !merged.contains("[telegram]"),
            "merged output should drop [telegram] when [[bots]] preserved: {merged}"
        );
    }

    #[test]
    fn wizard_full_save_updates_shim_token_from_telegram_only() {
        let existing = r#"
[[bots]]
id = "main"
bot_token = "111111111:AAOldMainTokenSecretXXXXX"
allowed_user_ids = [7]
persona = "main"
model = "keep-me"
tools = ["read_file"]

[[bots]]
id = "helper"
bot_token = "333333333:AAHelperTokenSecretValueZZ"
allowed_user_ids = [99]
persona = "helper"
tools = ["web_search"]

[openrouter]
api_key = "sk-test"
model = "test"

[sandbox]
allowed_directory = "/tmp"
"#;
        let wizard = r#"
[telegram]
bot_token = "111111111:AANewMainTokenSecretYYYYY"
allowed_user_ids = [7, 8]

[openrouter]
api_key = "sk-test"
model = "test"
"#;
        let merged = merge_wizard_save(existing, wizard).unwrap();
        let mut cfg: crate::config::Config = toml::from_str(&merged).unwrap();
        cfg.normalize_bots().unwrap();

        let main = cfg.bots.iter().find(|b| b.id == "main").unwrap();
        assert_eq!(main.bot_token, "111111111:AANewMainTokenSecretYYYYY");
        assert_eq!(main.allowed_user_ids, vec![7, 8]);
        assert_eq!(main.model.as_deref(), Some("keep-me"));
        assert_eq!(main.tools, Some(vec!["read_file".into()]));

        let helper = cfg.bots.iter().find(|b| b.id == "helper").unwrap();
        assert_eq!(helper.allowed_user_ids, vec![99]);
        assert_eq!(helper.tools, Some(vec!["web_search".into()]));
        assert_eq!(helper.bot_token, "333333333:AAHelperTokenSecretValueZZ");
    }

    #[test]
    fn wizard_full_save_legacy_without_bots_passes_through() {
        let existing = r#"
[telegram]
bot_token = "111111111:AALegacyTokenSecretValueXX"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-old"
model = "old"
"#;
        let wizard = r#"
[telegram]
bot_token = "111111111:AALegacyTokenSecretValueXX"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-new"
model = "new"
"#;
        let merged = merge_wizard_save(existing, wizard).unwrap();
        assert_eq!(merged, wizard);
        assert!(!merged.contains("[[bots]]") && !merged.contains("bots ="));
    }
}
