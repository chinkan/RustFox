#![allow(dead_code)]

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::web::WebState;

// ── Template ───────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "config.html")]
struct ConfigTemplate;

pub async fn page() -> impl IntoResponse {
    Html(ConfigTemplate.render().expect("template render"))
}

// ── Load/save config ───────────────────────────────────────────────────────────

pub async fn load_config(State(state): State<WebState>) -> Json<ExistingConfig> {
    match tokio::fs::read_to_string(&state.config_path).await {
        Ok(content) => Json(parse_existing_config(&content)),
        Err(_) => Json(ExistingConfig::default()),
    }
}

#[derive(Deserialize)]
pub struct SaveConfigRequest {
    pub telegram_token: String,
    pub allowed_user_ids: String,
    pub openrouter_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub location: String,
    pub sandbox_dir: String,
    pub db_path: String,
    pub mcp_servers: Vec<McpServerForm>,
}

#[derive(Deserialize)]
pub struct McpServerForm {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct SaveConfigResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub path: String,
}

pub async fn save_config(
    State(state): State<WebState>,
    Json(body): Json<SaveConfigRequest>,
) -> Result<Json<SaveConfigResponse>, StatusCode> {
    let toml_str = format_config_from_form(&body);
    tokio::fs::write(&state.config_path, &toml_str)
        .await
        .map_err(|e| {
            tracing::error!("Failed to write config: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let path = state.config_path.to_string_lossy().to_string();
    Ok(Json(SaveConfigResponse {
        ok: true,
        error: None,
        path,
    }))
}

// ── Config data types ──────────────────────────────────────────────────────────

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
    pub sandbox_dir: String,
    pub db_path: String,
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

// ── Raw TOML structs (loose — all optional so partial configs load) ────────────

#[derive(serde::Deserialize, Default)]
struct RawConfig {
    telegram: Option<RawTelegram>,
    openrouter: Option<RawOpenRouter>,
    sandbox: Option<RawSandbox>,
    memory: Option<RawMemory>,
    general: Option<RawGeneral>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

#[derive(serde::Deserialize, Default)]
struct RawTelegram {
    bot_token: Option<String>,
    allowed_user_ids: Option<Vec<toml::Value>>,
}

#[derive(serde::Deserialize, Default)]
struct RawOpenRouter {
    api_key: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawSandbox {
    allowed_directory: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawMemory {
    database_path: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawGeneral {
    location: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawMcpServer {
    name: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

// ── Parsing ────────────────────────────────────────────────────────────────────

pub fn parse_existing_config(content: &str) -> ExistingConfig {
    let raw: RawConfig = match toml::from_str(content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Could not parse existing config.toml: {e}");
            return ExistingConfig::default();
        }
    };

    let tg = raw.telegram.unwrap_or_default();
    let or_ = raw.openrouter.unwrap_or_default();
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
        openrouter_key: or_.api_key.unwrap_or_default(),
        model: or_.model.unwrap_or_default(),
        max_tokens: or_.max_tokens.unwrap_or(0),
        system_prompt: or_.system_prompt.unwrap_or_default(),
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

// ── Formatting ─────────────────────────────────────────────────────────────────

pub fn format_config_from_form(p: &SaveConfigRequest) -> String {
    let ids: Vec<&str> = p
        .allowed_user_ids
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

    let system_prompt = p.system_prompt.replace('\\', "\\\\").replace('"', "\\\"");
    let max_tokens = p.max_tokens;
    let tg_token = &p.telegram_token;
    let or_key = &p.openrouter_key;
    let model = &p.model;
    let sandbox = &p.sandbox_dir;
    let db_path = &p.db_path;

    let mut out = format!(
        r#"[telegram]
bot_token = "{tg_token}"
allowed_user_ids = [{ids_str}]

[openrouter]
api_key = "{or_key}"
model = "{model}"
base_url = "https://openrouter.ai/api/v1"
max_tokens = {max_tokens}
system_prompt = "{system_prompt}"

[sandbox]
allowed_directory = "{sandbox}"

[memory]
database_path = "{db_path}"

[skills]
directory = "skills"

[general]
{loc_line}
"#
    );

    // Append MCP servers
    for srv in &p.mcp_servers {
        if srv.name.is_empty() {
            continue;
        }
        out.push_str("\n[[mcp_servers]]\n");
        out.push_str(&format!("name = \"{}\"\n", srv.name));
        out.push_str(&format!("command = \"{}\"\n", srv.command));
        if !srv.args.is_empty() {
            let args_toml = srv
                .args
                .iter()
                .map(|a| format!("\"{}\"", a))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("args = [{args_toml}]\n"));
        }
        if !srv.env.is_empty() {
            out.push_str("[mcp_servers.env]\n");
            for (k, v) in &srv.env {
                out.push_str(&format!("{k} = \"{v}\"\n"));
            }
        }
    }

    out
}

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
    fn test_parse_partial_config_defaults_to_empty() {
        let toml = r#"
[telegram]
bot_token = "partial"
allowed_user_ids = [42]
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "partial");
        assert_eq!(cfg.model, "");
        assert_eq!(cfg.sandbox_dir, "");
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

    fn make_save_req(
        tg_token: &str,
        user_ids: &str,
        or_key: &str,
        model: &str,
        sandbox: &str,
        db_path: &str,
        location: &str,
    ) -> SaveConfigRequest {
        SaveConfigRequest {
            telegram_token: tg_token.into(),
            allowed_user_ids: user_ids.into(),
            openrouter_key: or_key.into(),
            model: model.into(),
            max_tokens: 4096,
            system_prompt: "Be helpful.".into(),
            location: location.into(),
            sandbox_dir: sandbox.into(),
            db_path: db_path.into(),
            mcp_servers: vec![],
        }
    }

    #[test]
    fn test_format_telegram_section() {
        let out = format_config_from_form(&make_save_req(
            "mytoken", "123456", "k", "m", "/tmp", "d.db", "",
        ));
        assert!(out.contains("[telegram]"));
        assert!(out.contains(r#"bot_token = "mytoken""#));
        assert!(out.contains("allowed_user_ids = [123456]"));
    }

    #[test]
    fn test_format_openrouter_section() {
        let out = format_config_from_form(&make_save_req(
            "t",
            "1",
            "sk-or-abc",
            "gpt-4o",
            "/tmp",
            "d.db",
            "",
        ));
        assert!(out.contains("[openrouter]"));
        assert!(out.contains(r#"api_key = "sk-or-abc""#));
        assert!(out.contains(r#"model = "gpt-4o""#));
        assert!(out.contains("max_tokens = 4096"));
    }

    #[test]
    fn test_format_location_set() {
        let out = format_config_from_form(&make_save_req(
            "t",
            "1",
            "k",
            "m",
            "/tmp",
            "d.db",
            "Tokyo, Japan",
        ));
        assert!(out.contains(r#"location = "Tokyo, Japan""#));
    }

    #[test]
    fn test_format_location_empty_commented() {
        let out = format_config_from_form(&make_save_req("t", "1", "k", "m", "/tmp", "d.db", ""));
        assert!(out.contains("# location ="));
        assert!(!out.contains("\nlocation = "));
    }

    #[test]
    fn test_format_multiple_user_ids() {
        let out = format_config_from_form(&make_save_req(
            "t",
            "111, 222, 333",
            "k",
            "m",
            "/tmp",
            "d.db",
            "",
        ));
        assert!(out.contains("allowed_user_ids = [111, 222, 333]"));
    }
}
