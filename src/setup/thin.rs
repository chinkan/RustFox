//! Thin setup.
//!
//! Provider is OpenRouter (the default) or Ollama. OpenRouter asks for an
//! API key and a model id: a short shortcut list plus a typed `provider/model`
//! field (ADR 0017). Ollama never asks for a base URL or a typed model id: a
//! running daemon's tags are the already-local choices, and a model that is
//! not local is picked from the Ollama library page (`https://ollama.com/library`,
//! filtered locally — `/search` is paginated) and pulled once after that pick. Bot token and one
//! system-prompt sentence are the other fields. Tools and MCP are not
//! questions. Sandbox-safe tools stay on because the written config does
//! not set a tool whitelist.

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Fixed local daemon. Not a wizard question.
pub const OLLAMA_TAGS_URL: &str = "http://127.0.0.1:11434/api/tags";
pub const OLLAMA_PULL_URL: &str = "http://127.0.0.1:11434/api/pull";
/// OpenAI-compatible base the existing `[[provider]]` schema expects.
pub const OLLAMA_PROVIDER_BASE: &str = "http://127.0.0.1:11434/v1";

pub const OLLAMA_NOT_RUNNING: &str = "Ollama is not running.";

/// Ollama's public library page. One HTML page lists the catalog. Search is a
/// filter over that page: `https://ollama.com/search?q=` is real but paginated
/// (about 20 names per page), so it is not the source of truth.
pub const OLLAMA_LIBRARY_URL: &str = "https://ollama.com/library";
pub const OLLAMA_LIBRARY_UNAVAILABLE: &str = "Could not load the Ollama library.";

/// Short OpenRouter shortcut list. Each id is either the schema default or was
/// present on `https://openrouter.ai/api/v1/models` when this list was set.
/// Not an allowlist — users may type any `provider/model` id (ADR 0017).
pub const OPENROUTER_MODELS: &[&str] = &[
    "moonshotai/kimi-k2.6",
    "anthropic/claude-sonnet-4",
    "openai/gpt-4o",
    "openai/gpt-4o-mini",
    "google/gemini-2.5-flash",
    "qwen/qwen3-32b",
];
/// Same string as `config::default_model`. Kept here so the wizard can select
/// it without calling a private function. It is the first list entry.
pub const OPENROUTER_DEFAULT_MODEL: &str = "moonshotai/kimi-k2.6";

/// Sandbox-safe builtins. A missing whitelist keeps these on (install default).
pub const SANDBOX_SAFE_TOOLS: &[&str] =
    &["read_file", "write_file", "list_files", "execute_command"];

/// Telegram user ids start at 1, so `0` matches nobody. Fresh thin setup
/// boots closed instead of asking for an allowlist (empty is a hard error).
pub const CLOSED_ALLOWLIST_USER: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinProvider {
    OpenRouter,
    Ollama,
}

impl ThinProvider {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "openrouter" => Ok(Self::OpenRouter),
            "ollama" => Ok(Self::Ollama),
            other => bail!("provider must be OpenRouter or Ollama, not {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
        }
    }
}

/// Questions the wizard asks, in order. OpenRouter adds a model pick.
/// There is still no allowlist, tools, or MCP question.
pub fn wizard_fields(provider: ThinProvider) -> Vec<&'static str> {
    match provider {
        ThinProvider::OpenRouter => vec![
            "provider",
            "openrouter_api_key",
            "openrouter_model",
            "bot_token",
            "system_prompt",
        ],
        ThinProvider::Ollama => {
            vec!["provider", "ollama_model", "bot_token", "system_prompt"]
        }
    }
}

/// The catalog request. `query` is not sent to Ollama search; [`filter_library`]
/// applies it after the library page is parsed, so a match past search page 1
/// is still offered.
pub fn library_request_url(_query: &str) -> String {
    OLLAMA_LIBRARY_URL.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaDetect {
    pub running: bool,
    pub models: Vec<String>,
    /// Set only when Ollama is not running. One line, no trailing newline.
    pub message: Option<&'static str>,
}

/// `reached_ok` is a successful tags response. Anything else is "not running".
pub fn detect_from_http(reached_ok: bool, body: &str) -> OllamaDetect {
    if !reached_ok {
        return OllamaDetect {
            running: false,
            models: Vec::new(),
            message: Some(OLLAMA_NOT_RUNNING),
        };
    }
    OllamaDetect {
        running: true,
        models: model_names_from_tags(body),
        message: None,
    }
}

pub fn model_names_from_tags(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let Some(models) = value.get("models").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for model in models {
        let name = model
            .get("name")
            .and_then(|n| n.as_str())
            .or_else(|| model.get("model").and_then(|n| n.as_str()))
            .unwrap_or("")
            .trim();
        if name.is_empty() || names.iter().any(|n| n == name) {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

fn model_base(name: &str) -> &str {
    name.split(':').next().unwrap_or(name).trim()
}

/// Names linked as `/library/{name}` in an Ollama library or search page.
/// Tags and quantization paths are not names: the href is the library id.
pub fn library_names_from_html(html: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = html.as_bytes();
    let needle = b"/library/";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] != needle {
            i += 1;
            continue;
        }
        let start = i + needle.len();
        let mut end = start;
        while end < bytes.len() {
            let c = bytes[end];
            if c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        if end > start {
            let name = &html[start..end];
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
        i = end.max(start);
    }
    names
}

/// Case-insensitive substring filter. Empty query keeps the loaded list.
pub fn filter_library(names: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return names.to_vec();
    }
    names
        .iter()
        .filter(|name| name.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLoad {
    pub ok: bool,
    pub models: Vec<String>,
    /// Set only when the library page could not be loaded. Never a fallback list.
    pub error: Option<&'static str>,
}

/// `reached_ok` is a successful library/search response. Failure does not
/// invent names.
pub fn library_from_http(reached_ok: bool, body: &str, query: &str) -> LibraryLoad {
    if !reached_ok {
        return LibraryLoad {
            ok: false,
            models: Vec::new(),
            error: Some(OLLAMA_LIBRARY_UNAVAILABLE),
        };
    }
    LibraryLoad {
        ok: true,
        models: filter_library(&library_names_from_html(body), query),
        error: None,
    }
}

/// Library names whose base is not already installed. No pull.
pub fn library_not_already_local(library: &[String], local: &[String]) -> Vec<String> {
    let have: Vec<&str> = local.iter().map(|n| model_base(n)).collect();
    library
        .iter()
        .filter(|name| !have.contains(&name.as_str()))
        .cloned()
        .collect()
}

/// Rejects a typed id, a tag/quantization, and anything not in this response.
/// Does not pull. `library_results` is the parsed Ollama page, not a built-in list.
pub fn validate_library_pick(name: &str, library_results: &[String]) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.contains(':') || name.contains('/') || name.contains(' ') {
        bail!("pick a model from the Ollama library list");
    }
    if !library_results.iter().any(|item| item == name) {
        bail!("pick a model from the Ollama library list");
    }
    Ok(())
}

pub fn pull_request_body(name: &str, library_results: &[String]) -> Result<Value> {
    validate_library_pick(name, library_results)?;
    Ok(serde_json::json!({ "name": name.trim(), "stream": false }))
}

/// Catalog link shown next to the model field / printed in CLI (ADR 0017).
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/models";

/// Non-empty after trim and must contain `/` (covers `openrouter/auto` and
/// `:free` suffixes). The shortcut list is not an allowlist (ADR 0017).
pub fn validate_openrouter_model(model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() {
        bail!("OpenRouter model id is required");
    }
    if !model.contains('/') {
        bail!("OpenRouter model id must contain '/' (e.g. provider/model)");
    }
    Ok(())
}

pub fn openrouter_model_allowed(model: &str) -> bool {
    validate_openrouter_model(model).is_ok()
}

/// CLI numbered pick: empty → default; `1..=len` → list entry; `len+1` → Other
/// (then `typed` is validated). Pure so the Other path is unit-testable.
pub fn openrouter_model_from_cli_choice(pick: &str, typed: Option<&str>) -> Result<String> {
    let pick = pick.trim();
    if pick.is_empty() {
        return Ok(OPENROUTER_DEFAULT_MODEL.to_string());
    }
    let other = OPENROUTER_MODELS.len() + 1;
    let idx: usize = pick
        .parse()
        .ok()
        .filter(|n| (1..=other).contains(n))
        .context("pick an OpenRouter model number")?;
    if idx == other {
        let id = typed.unwrap_or("").trim();
        validate_openrouter_model(id)?;
        return Ok(id.to_string());
    }
    Ok(OPENROUTER_MODELS[idx - 1].to_string())
}

/// A detected local name, or a library name after it has been pulled
/// (the caller only passes models the tags endpoint just returned).
pub fn ollama_choice_allowed(chosen: &str, local: &[String]) -> bool {
    let chosen = chosen.trim();
    !chosen.is_empty() && local.iter().any(|name| name == chosen)
}

#[derive(Debug, Clone)]
pub struct ThinAnswers {
    pub provider: ThinProvider,
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub ollama_model: String,
    pub bot_token: String,
    pub system_prompt: String,
}

pub fn render_config(answers: &ThinAnswers) -> Result<String> {
    let token = answers.bot_token.trim();
    if token.is_empty() {
        bail!("bot token is required");
    }
    let sentence = answers.system_prompt.trim();
    if sentence.is_empty() {
        bail!("system prompt sentence is required");
    }

    let mut root = toml::map::Map::new();

    let mut telegram = toml::map::Map::new();
    telegram.insert("bot_token".into(), toml::Value::String(token.to_string()));
    telegram.insert(
        "allowed_user_ids".into(),
        toml::Value::Array(vec![toml::Value::Integer(CLOSED_ALLOWLIST_USER as i64)]),
    );
    root.insert("telegram".into(), toml::Value::Table(telegram));

    let mut openrouter = toml::map::Map::new();
    match answers.provider {
        ThinProvider::OpenRouter => {
            let key = answers.openrouter_api_key.trim();
            if key.is_empty() {
                bail!("OpenRouter API key is required");
            }
            openrouter.insert("api_key".into(), toml::Value::String(key.to_string()));
            let model = answers.openrouter_model.trim();
            validate_openrouter_model(model)?;
            openrouter.insert("model".into(), toml::Value::String(model.to_string()));
        }
        ThinProvider::Ollama => {
            let model = answers.ollama_model.trim();
            if model.is_empty() || model.contains(' ') || model.contains('/') {
                bail!("pick a detected Ollama model");
            }
            let mut provider = toml::map::Map::new();
            provider.insert("name".into(), toml::Value::String("ollama".into()));
            provider.insert("type".into(), toml::Value::String("ollama".into()));
            provider.insert(
                "base_url".into(),
                toml::Value::String(OLLAMA_PROVIDER_BASE.into()),
            );
            provider.insert("model".into(), toml::Value::String(model.to_string()));
            provider.insert("discover_models".into(), toml::Value::Boolean(true));
            root.insert(
                "provider".into(),
                toml::Value::Array(vec![toml::Value::Table(provider)]),
            );
            // Schema requires [openrouter]. The key is unused on this path.
            openrouter.insert("api_key".into(), toml::Value::String(String::new()));
        }
    }
    openrouter.insert(
        "system_prompt".into(),
        toml::Value::String(sentence.to_string()),
    );
    root.insert("openrouter".into(), toml::Value::Table(openrouter));

    let mut memory = toml::map::Map::new();
    memory.insert(
        "database_path".into(),
        toml::Value::String("rustfox.db".into()),
    );
    root.insert("memory".into(), toml::Value::Table(memory));

    toml::to_string(&toml::Value::Table(root)).context("serialize thin config")
}

/// Re-runs keep a real allowlist. A fresh file stays closed (`[0]`).
pub fn keep_existing_allowlist(existing: &str, wizard: &str) -> Result<String> {
    let existing_doc: toml::Value = toml::from_str(existing).context("existing config")?;
    let Some(ids) = allowlist_value(&existing_doc) else {
        return Ok(wizard.to_string());
    };
    let mut wizard_doc: toml::Value = toml::from_str(wizard).context("wizard config")?;
    let table = wizard_doc
        .as_table_mut()
        .context("wizard config root is not a table")?;
    let telegram = table
        .entry("telegram")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let telegram = telegram
        .as_table_mut()
        .context("wizard [telegram] is not a table")?;
    telegram.insert("allowed_user_ids".into(), ids);
    toml::to_string(&wizard_doc).context("serialize wizard config")
}

fn allowlist_value(doc: &toml::Value) -> Option<toml::Value> {
    let from_telegram = doc
        .get("telegram")
        .and_then(|t| t.get("allowed_user_ids"))
        .and_then(|v| v.as_array())
        .filter(|ids| !ids.is_empty())
        .cloned();
    if let Some(ids) = from_telegram {
        return Some(toml::Value::Array(ids));
    }
    doc.get("bots")
        .and_then(|b| b.as_array())
        .and_then(|bots| bots.first())
        .and_then(|bot| bot.get("allowed_user_ids"))
        .and_then(|v| v.as_array())
        .filter(|ids| !ids.is_empty())
        .cloned()
        .map(toml::Value::Array)
}

/// `None` (no whitelist) keeps sandbox-safe tools on. An explicit list keeps
/// them on only when every sandbox-safe name is still present.
pub fn sandbox_safe_tools_stay_on(whitelist: Option<&[String]>) -> bool {
    match whitelist {
        None => true,
        Some(list) => SANDBOX_SAFE_TOOLS
            .iter()
            .all(|name| list.iter().any(|item| item == name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn openrouter_answers() -> ThinAnswers {
        ThinAnswers {
            provider: ThinProvider::OpenRouter,
            openrouter_api_key: "sk-or-test".into(),
            openrouter_model: "openai/gpt-4o".into(),
            ollama_model: String::new(),
            bot_token: "123:abc".into(),
            system_prompt: "Be brief and kind.".into(),
        }
    }

    fn parse(toml_text: &str) -> Config {
        let mut cfg: Config = toml::from_str(toml_text).unwrap();
        cfg.normalize_bots().unwrap();
        cfg
    }

    #[test]
    fn wizard_asks_provider_key_or_model_token_and_sentence() {
        for provider in [ThinProvider::OpenRouter, ThinProvider::Ollama] {
            let fields = wizard_fields(provider);
            assert_eq!(fields[0], "provider");
            assert!(fields.contains(&"bot_token"));
            assert!(fields.contains(&"system_prompt"));
            let joined = fields.join(" ");
            for banned in [
                "tools",
                "mcp",
                "base_url",
                "model_id",
                "quantization",
                "huggingface",
                "langsmith",
                "allowed_user",
            ] {
                assert!(
                    !joined.contains(banned),
                    "{provider:?} field list contains {banned}: {joined}"
                );
            }
        }
        let openrouter = wizard_fields(ThinProvider::OpenRouter);
        assert!(openrouter.contains(&"openrouter_api_key"));
        assert!(openrouter.contains(&"openrouter_model"));
        assert!(!openrouter.contains(&"ollama_model"));
        assert_eq!(openrouter.len(), 5);
        assert_eq!(wizard_fields(ThinProvider::Ollama).len(), 4);
        assert!(wizard_fields(ThinProvider::Ollama).contains(&"ollama_model"));
        assert!(!wizard_fields(ThinProvider::Ollama).contains(&"openrouter_api_key"));
        assert!(ThinProvider::parse("").unwrap() == ThinProvider::OpenRouter);
        assert!(ThinProvider::parse("lmstudio").is_err());
        assert!(ThinProvider::parse("huggingface").is_err());
    }

    #[test]
    fn openrouter_is_default_and_stores_the_sentence() {
        let text = render_config(&openrouter_answers()).unwrap();
        assert!(!text.contains("[[provider]]") && !text.contains("name = \"ollama\""));
        assert!(!text.contains("mcp"));
        assert!(!text.contains("tools"));
        assert!(!text.contains("langsmith"));
        let cfg = parse(&text);
        assert_eq!(cfg.openrouter.api_key, "sk-or-test");
        assert_eq!(cfg.openrouter.system_prompt, "Be brief and kind.");
        // The chosen pick is written, not left to the schema default.
        assert_eq!(cfg.openrouter.model, "openai/gpt-4o");
        assert_ne!(cfg.openrouter.model, OPENROUTER_DEFAULT_MODEL);
        assert_eq!(cfg.openrouter.base_url, "https://openrouter.ai/api/v1");
        assert!(cfg.provider.is_empty());
        assert!(cfg.mcp_servers.is_empty());
        assert!(cfg.bots[0].tools.is_none());
        assert!(sandbox_safe_tools_stay_on(cfg.bots[0].tools.as_deref()));
        assert_eq!(cfg.bots[0].allowed_user_ids, vec![CLOSED_ALLOWLIST_USER]);
        let (providers, default_name, _) = cfg.build_providers();
        assert_eq!(default_name, "openrouter");
        assert_eq!(
            providers[0].provider_type,
            crate::config::ProviderType::OpenRouter
        );
    }

    #[test]
    fn openrouter_default_selection_is_on_the_list_and_rejected_ids_fail() {
        assert!(OPENROUTER_MODELS.contains(&OPENROUTER_DEFAULT_MODEL));
        assert!(OPENROUTER_MODELS.len() >= 4 && OPENROUTER_MODELS.len() <= 8);
        let mut answers = openrouter_answers();
        answers.openrouter_model = OPENROUTER_DEFAULT_MODEL.into();
        let cfg = parse(&render_config(&answers).unwrap());
        assert_eq!(cfg.openrouter.model, OPENROUTER_DEFAULT_MODEL);
        // Typed id with `/` not on the shortcut list is accepted (ADR 0017).
        answers.openrouter_model = "not/a-real-model".into();
        let cfg = parse(&render_config(&answers).unwrap());
        assert_eq!(cfg.openrouter.model, "not/a-real-model");
        answers.openrouter_model = "".into();
        let err = render_config(&answers).unwrap_err().to_string();
        assert!(err.contains("required"), "{err}");
        answers.openrouter_model = "   ".into();
        let err = render_config(&answers).unwrap_err().to_string();
        assert!(err.contains("required"), "{err}");
        answers.openrouter_model = "no-slash".into();
        let err = render_config(&answers).unwrap_err().to_string();
        assert!(err.contains('/'), "{err}");
        assert!(openrouter_model_allowed("openai/gpt-4o"));
        assert!(!openrouter_model_allowed(""));
        assert!(!openrouter_model_allowed("noslash"));
    }

    #[test]
    fn openrouter_cli_choice_other_path_accepts_typed_id() {
        assert_eq!(
            openrouter_model_from_cli_choice("", None).unwrap(),
            OPENROUTER_DEFAULT_MODEL
        );
        assert_eq!(
            openrouter_model_from_cli_choice("1", None).unwrap(),
            OPENROUTER_MODELS[0]
        );
        let other = (OPENROUTER_MODELS.len() + 1).to_string();
        assert_eq!(
            openrouter_model_from_cli_choice(&other, Some("acme/cool-model:free")).unwrap(),
            "acme/cool-model:free"
        );
        assert!(openrouter_model_from_cli_choice(&other, Some("")).is_err());
        assert!(openrouter_model_from_cli_choice(&other, Some("noslash")).is_err());
        assert!(openrouter_model_from_cli_choice("0", None).is_err());
        assert!(openrouter_model_from_cli_choice("99", None).is_err());
    }

    #[test]
    fn ollama_config_uses_detected_model_and_fixed_base() {
        let answers = ThinAnswers {
            provider: ThinProvider::Ollama,
            openrouter_api_key: String::new(),
            openrouter_model: String::new(),
            ollama_model: "llama3.2:latest".into(),
            bot_token: "123:abc".into(),
            system_prompt: "Speak plainly.".into(),
        };
        let text = render_config(&answers).unwrap();
        assert!(!text.contains("mcp"));
        assert!(!text.contains("\ntools"));
        let cfg = parse(&text);
        assert_eq!(cfg.openrouter.system_prompt, "Speak plainly.");
        assert!(cfg.openrouter.api_key.is_empty());
        assert_eq!(cfg.provider.len(), 1);
        assert_eq!(cfg.provider[0].name, "ollama");
        assert_eq!(
            cfg.provider[0].provider_type,
            crate::config::ProviderType::Ollama
        );
        assert_eq!(cfg.provider[0].base_url, OLLAMA_PROVIDER_BASE);
        assert_eq!(cfg.provider[0].model, "llama3.2:latest");
        assert!(cfg.bots[0].tools.is_none());
        assert!(sandbox_safe_tools_stay_on(None));
        assert!(!sandbox_safe_tools_stay_on(Some(
            &["web_search".into()][..]
        )));
        let (_providers, default_name, _) = cfg.build_providers();
        assert_eq!(default_name, "ollama");
    }

    #[test]
    fn ollama_down_is_one_line_and_library_pull_waits_for_a_pick() {
        let down = detect_from_http(false, "");
        assert!(!down.running);
        assert!(down.models.is_empty());
        assert_eq!(down.message, Some(OLLAMA_NOT_RUNNING));
        assert!(!OLLAMA_NOT_RUNNING.contains('\n'));

        let body = r#"{"models":[{"name":"llama3.2:latest"},{"name":"qwen2.5:7b"}]}"#;
        let up = detect_from_http(true, body);
        assert!(up.running);
        assert!(up.message.is_none());
        assert_eq!(
            up.models,
            vec!["llama3.2:latest".to_string(), "qwen2.5:7b".to_string()]
        );
        assert!(ollama_choice_allowed("llama3.2:latest", &up.models));
        assert!(!ollama_choice_allowed("typed-by-hand", &up.models));

        // Library names come from the fetched page, not a built-in eight.
        let html = r#"
            <a href="/library/orca-mini">orca</a>
            <a href="/library/tinyllama">tiny</a>
            <a href="/library/orca-mini">dup</a>
            <a href="/library/llama3.2:q4_K_M">ignored tag path is not a name char wait</a>
        "#;
        // The tag-like href still stops at ':' so it must not become a quant pick.
        let load = library_from_http(true, html, "");
        assert!(load.ok);
        assert!(load.error.is_none());
        assert_eq!(
            load.models,
            vec![
                "orca-mini".to_string(),
                "tinyllama".to_string(),
                "llama3.2".to_string()
            ]
        );
        let filtered = library_from_http(true, html, "orca");
        assert_eq!(filtered.models, vec!["orca-mini".to_string()]);
        let not_local = library_not_already_local(&load.models, &up.models);
        assert!(!not_local.iter().any(|n| n == "llama3.2"));
        assert!(not_local.iter().any(|n| n == "orca-mini"));

        let failed = library_from_http(false, html, "llama");
        assert!(!failed.ok);
        assert!(
            failed.models.is_empty(),
            "must not fall back to a hardcoded list"
        );
        assert_eq!(failed.error, Some(OLLAMA_LIBRARY_UNAVAILABLE));

        assert_eq!(library_request_url(""), OLLAMA_LIBRARY_URL);
        // Search filters the full library page. It does not call paginated /search.
        assert_eq!(library_request_url("llama 3"), OLLAMA_LIBRARY_URL);

        let body = pull_request_body("orca-mini", &load.models).unwrap();
        assert_eq!(body["name"], "orca-mini");
        assert_eq!(body["stream"], false);
        // Not in this response, even if it used to be a hardcoded name.
        assert!(pull_request_body("mistral", &load.models).is_err());
        assert!(pull_request_body("orca-mini:q4_K_M", &load.models).is_err());
        assert!(pull_request_body("huggingface/foo", &load.models).is_err());
        assert!(pull_request_body("", &load.models).is_err());
    }

    #[test]
    fn rerun_keeps_an_existing_allowlist() {
        let existing = r#"
            [telegram]
            bot_token = "old"
            allowed_user_ids = [42, 7]
            [openrouter]
            api_key = "k"
        "#;
        let wizard = render_config(&openrouter_answers()).unwrap();
        let kept = keep_existing_allowlist(existing, &wizard).unwrap();
        let cfg = parse(&kept);
        assert_eq!(cfg.telegram.allowed_user_ids, vec![42, 7]);
        assert_eq!(cfg.openrouter.system_prompt, "Be brief and kind.");
    }

    #[test]
    fn readme_locks_openrouter_default_and_ollama_library() {
        let readme = include_str!("../../README.md");
        assert!(!readme.contains("Self-hosted, no cloud dependency"));
        let lower = readme.to_ascii_lowercase();
        assert!(!lower.contains("inference stays on the machine"));
        assert!(!lower.contains("no cloud"));
        assert!(readme.contains("OpenRouter"));
        assert!(readme.contains("Ollama is the local option"));
        assert!(readme.contains("pulled from the Ollama library"));
    }

    #[test]
    fn wizard_page_has_no_tool_or_mcp_questions() {
        let html = include_str!("../../setup/index.html");
        assert!(html.contains("id=\"f-provider-openrouter\""));
        assert!(html.contains("id=\"f-provider-ollama\""));
        assert!(html.contains("id=\"f-telegram-token\""));
        assert!(html.contains("id=\"f-system-prompt\""));
        assert!(html.contains("id=\"f-openrouter-key\""));
        assert!(html.contains("id=\"f-openrouter-model\""));
        assert!(html.contains("id=\"f-openrouter-model-pick\""));
        assert!(html.contains(OPENROUTER_MODELS_URL));
        // Client mirrors the server '/' rule (server stays authoritative).
        assert!(html.contains("requireField('f-openrouter-model', v => v.includes('/'))"));
        assert!(!html.contains("<select id=\"f-openrouter-model\""));
        assert!(!html.contains("no typed id"));
        assert!(html.contains("<select id=\"f-ollama-model\""));
        assert!(html.contains("<select id=\"f-ollama-library\""));
        assert!(html.contains("id=\"f-ollama-search\""));
        assert!(html.contains("Ollama is not running."));
        assert!(html.contains("Use OpenRouter"));
        assert!(!html.contains("id=\"f-allowed-ids\""));
        assert!(!html.contains("id=\"f-model\""));
        assert!(!html.contains("id=\"f-base-url\""));
        assert!(!html.contains("Add another bot"));
        assert!(!html.contains("id=\"mcp-catalog\""));
        assert!(!html.contains("id=\"step-3\""));
        assert!(!html.contains("Show all settings"));
        assert!(!html.contains("fully_silent"));
        assert!(!html.to_ascii_lowercase().contains("silent"));
    }

    #[test]
    fn wizard_does_not_ask_about_fully_silent() {
        for provider in [ThinProvider::OpenRouter, ThinProvider::Ollama] {
            let joined = wizard_fields(provider).join(" ");
            assert!(
                !joined.contains("silent") && !joined.contains("fully_silent"),
                "{provider:?} wizard asked about silent tools: {joined}"
            );
        }
        let rendered = render_config(&openrouter_answers()).unwrap();
        assert!(!rendered.contains("fully_silent"));
        assert!(!rendered.contains("silent"));
    }
}
