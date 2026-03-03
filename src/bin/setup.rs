//! RustFox setup wizard.
//!
//! Without flags: starts a local Axum HTTP server on port 8719 and opens the
//! browser-based setup wizard.  On form submission the wizard POSTs the
//! generated config to `/api/save-config`, which writes `config.toml` to the
//! project root and then shuts the server down.
//!
//! With `--cli`: runs an interactive terminal wizard instead.
//! With `--google-auth`: runs the Google Device Code OAuth flow to obtain a
//! refresh token for the Google Workspace MCP server.

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{oneshot, Mutex};

// The wizard SPA is embedded at compile time — no runtime file needed.
const INDEX_HTML: &str = include_str!("../../setup/index.html");

// ── Shared state ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    config_path: PathBuf,
    /// Consumed once when the browser POSTs a saved config.
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
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

// ── Google Device Code OAuth ────────────────────────────────────────────────────

/// Google OAuth2 device authorization endpoint response.
#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: u64,
    interval: u64,
}

/// Google OAuth2 token endpoint response (success).
#[derive(Deserialize)]
struct TokenResponse {
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The scopes required for full Google Workspace MCP functionality.
const GOOGLE_WORKSPACE_SCOPES: &str = "https://www.googleapis.com/auth/drive \
     https://www.googleapis.com/auth/gmail.modify \
     https://www.googleapis.com/auth/calendar \
     https://www.googleapis.com/auth/documents \
     https://www.googleapis.com/auth/spreadsheets \
     https://www.googleapis.com/auth/presentations";

/// Run the Google Device Code OAuth flow to obtain a refresh token.
///
/// Instructs the user to visit a URL, enter a code, and then polls
/// Google until the user completes authorization.  Prints the resulting
/// refresh token so the user can paste it into config.toml.
async fn run_google_auth() -> Result<()> {
    use std::io::{self, Write};

    println!("=== Google Workspace OAuth Setup ===\n");
    println!("This wizard obtains a refresh token for the Google Workspace MCP server.");
    println!("You will need a Google Cloud OAuth 2.0 Client ID and Secret.");
    println!("Create one at: https://console.cloud.google.com/apis/credentials");
    println!("  → Credentials → Create OAuth Client ID → Desktop app\n");

    let read_line = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim().to_owned())
    };

    let client_id = read_line("Google OAuth Client ID: ")?;
    let client_secret = read_line("Google OAuth Client Secret: ")?;

    if client_id.is_empty() || client_secret.is_empty() {
        anyhow::bail!("Client ID and Client Secret must not be empty.");
    }

    let http = reqwest::Client::new();

    // ── Step 1: Request device code ────────────────────────────────────────
    println!("\nRequesting device code from Google…");
    let device_resp = http
        .post("https://oauth2.googleapis.com/device/code")
        .form(&[
            ("client_id", client_id.as_str()),
            ("scope", GOOGLE_WORKSPACE_SCOPES),
        ])
        .send()
        .await
        .context("Failed to contact Google device auth endpoint")?;

    if !device_resp.status().is_success() {
        let text = device_resp.text().await.unwrap_or_default();
        anyhow::bail!("Google device auth request failed: {text}");
    }

    let device: DeviceCodeResponse = device_resp
        .json()
        .await
        .context("Failed to parse device code response")?;

    // ── Step 2: Instruct the user ──────────────────────────────────────────
    println!("\n────────────────────────────────────────");
    println!("  1. Open this URL in your browser:");
    println!("     {}", device.verification_url);
    println!("  2. Enter this code when prompted:");
    println!("     {}", device.user_code);
    println!("────────────────────────────────────────\n");
    println!(
        "Waiting for you to authorize (expires in {} seconds)…",
        device.expires_in
    );

    // ── Step 3: Poll for token ─────────────────────────────────────────────
    let poll_interval = std::time::Duration::from_secs(device.interval.max(5));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(poll_interval).await;

        if std::time::Instant::now() > deadline {
            anyhow::bail!("Authorization timed out.  Run --google-auth again to retry.");
        }

        let token_resp = http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .context("Failed to poll Google token endpoint")?;

        let body: TokenResponse = token_resp
            .json()
            .await
            .context("Failed to parse token poll response")?;

        match body.error.as_deref() {
            None => {
                // Success
                match body.refresh_token {
                    Some(rt) => {
                        println!("\n✓  Authorization successful!\n");
                        println!("Add the following to your config.toml under [[mcp_servers]] for google-workspace:\n");
                        println!("  GOOGLE_WORKSPACE_CLIENT_ID     = \"{client_id}\"");
                        println!("  GOOGLE_WORKSPACE_CLIENT_SECRET = \"{client_secret}\"");
                        println!("  GOOGLE_WORKSPACE_REFRESH_TOKEN = \"{rt}\"");
                        println!();
                        return Ok(());
                    }
                    None => {
                        anyhow::bail!(
                            "Token response succeeded but contained no refresh_token. \
                             Make sure your OAuth client type is 'Desktop app'."
                        );
                    }
                }
            }
            Some("authorization_pending") => {
                print!(".");
                io::stdout().flush().ok();
            }
            Some("slow_down") => {
                // Back off a bit more
                tokio::time::sleep(poll_interval).await;
            }
            Some("access_denied") => {
                anyhow::bail!("Authorization was denied by the user.");
            }
            Some("expired_token") => {
                anyhow::bail!("The device code expired.  Run --google-auth again to retry.");
            }
            Some(other) => {
                let desc = body.error_description.as_deref().unwrap_or("");
                anyhow::bail!("Unexpected error from Google: {other} — {desc}");
            }
        }
    }
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Resolve project root: prefer RUSTFOX_ROOT env, fall back to cwd.
    let project_root =
        PathBuf::from(std::env::var("RUSTFOX_ROOT").unwrap_or_else(|_| ".".to_string()));

    if args.iter().any(|a| a == "--google-auth") {
        return run_google_auth().await;
    }

    if args.iter().any(|a| a == "--cli") {
        return run_cli(&project_root);
    }

    // ── Web mode ──────────────────────────────────────────────────────────────
    let port: u16 = 8719;
    let config_path = project_root.join("config.toml");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = AppState {
        config_path,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
    };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/load-config", get(load_config))
        .route("/api/save-config", post(save_config))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    println!("RustFox setup wizard → http://localhost:{port}");
    println!("Press Ctrl-C to exit without saving.\n");

    // Open the browser after a short delay.
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        let url = format!("http://localhost:{port}");
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
}
