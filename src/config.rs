use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub openrouter: OpenRouterConfig,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default = "default_memory_config")]
    pub memory: MemoryConfig,
    #[serde(default = "default_skills_config")]
    pub skills: SkillsConfig,
    #[serde(default = "default_agents_config")]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub general: Option<GeneralConfig>,
    #[serde(default = "default_agent_config")]
    pub agent: AgentConfig,
    pub embedding: Option<EmbeddingApiConfig>,
    #[serde(default)]
    pub langsmith: Option<LangSmithConfig>,
    #[serde(default = "default_ocr_config")]
    pub ocr: OcrConfig,
    #[serde(default = "default_learning_config")]
    pub learning: LearningConfig,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    /// Absolute home root resolved at load time (not read from TOML).
    #[serde(skip)]
    pub resolved_home: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SupervisorConfig {
    #[serde(default = "default_autonomy_mode")]
    pub default_autonomy_mode: String,
    #[serde(default)]
    pub artifacts_dir: std::path::PathBuf,
    #[serde(default)]
    pub risk: RiskThresholdsConfig,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            default_autonomy_mode: default_autonomy_mode(),
            artifacts_dir: default_artifacts_dir(),
            risk: RiskThresholdsConfig::default(),
        }
    }
}

/// Risk-threshold gates that govern when the supervisor may auto-execute a
/// task vs. require explicit user approval.
///
/// Defaults preserve the M1–M6 behavior (Medium-risk tasks auto-execute);
/// flip individual fields in `config.toml` to tighten the gate.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct RiskThresholdsConfig {
    #[serde(default)]
    pub require_approval_for_low: bool,
    #[serde(default)]
    pub require_approval_for_medium: bool,
    /// When `true`, only Low-risk tasks may auto-execute; Medium escalates to
    /// `RequireApproval`. Defaults to `false` to stay backward-compatible
    /// with the M1–M6 policy where Medium-risk tasks auto-execute.
    #[serde(default)]
    pub auto_execute_only_low: bool,
}

fn default_autonomy_mode() -> String {
    "standard".to_string()
}

fn default_artifacts_dir() -> std::path::PathBuf {
    std::path::PathBuf::new()
}

#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddingApiConfig {
    pub api_key: String,
    #[serde(default = "default_embedding_base_url")]
    pub base_url: String,
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default = "default_embedding_dimensions")]
    pub dimensions: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_user_ids: Vec<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    /// Whether the configured model supports vision (image inputs).
    /// When true, images are sent as base64-encoded content parts.
    /// When false, OCR is used to extract text from images.
    #[serde(default)]
    pub supports_vision: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OcrConfig {
    /// Directory where OCR model files are cached.
    /// Models are downloaded automatically on first OCR use.
    #[serde(default = "default_ocr_model_dir")]
    pub model_dir: std::path::PathBuf,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SandboxConfig {
    #[serde(default)]
    pub allowed_directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct McpServerConfig {
    pub name: String,
    /// Command to run for stdio-based MCP servers (e.g. "uvx", "npx").
    /// Required for stdio servers; omit for HTTP servers.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// URL for HTTP-based MCP servers using the Streamable HTTP transport.
    /// Required for HTTP servers; omit for stdio servers.
    /// The API key may be embedded as a query parameter (e.g. `?exaApiKey=KEY`)
    /// or provided separately via `auth_token`.
    #[serde(default)]
    pub url: Option<String>,
    /// Bearer token sent in the `Authorization` header for HTTP servers.
    /// Used with `url`; ignored for stdio servers.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// OAuth 2.0 refresh token for long-lived connections.
    /// When set, the bot will automatically exchange this for a new `auth_token`
    /// before the current one expires and persist the updated token to `config.toml`.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds since epoch) at which the current `auth_token`
    /// expires.  Derived from the `expires_in` field of the token response.
    #[serde(default)]
    pub token_expires_at: Option<i64>,
    /// OAuth 2.0 token endpoint used for refresh-token exchanges.
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// OAuth 2.0 client ID used when authenticating refresh-token requests.
    #[serde(default)]
    pub oauth_client_id: Option<String>,
    /// OAuth 2.0 client secret (if applicable) used alongside `oauth_client_id`.
    #[serde(default)]
    pub oauth_client_secret: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MemoryConfig {
    #[serde(default)]
    pub database_path: PathBuf,
    #[serde(default = "default_rag_limit")]
    pub rag_limit: usize,
    #[serde(default = "default_max_raw_messages")]
    pub max_raw_messages: usize,
    #[serde(default = "default_summarize_threshold")]
    #[allow(dead_code)]
    pub summarize_threshold: usize,
    #[serde(default = "default_summarize_cron")]
    #[allow(dead_code)]
    pub summarize_cron: String,
    /// When `true`, an LLM call rewrites ambiguous follow-up questions into
    /// self-contained search queries before the RAG vector search.
    /// Defaults to `false` to avoid the extra LLM round-trip.
    /// Can be toggled per-user at runtime via the `/query-rewrite` command.
    #[serde(default)]
    pub query_rewriter_enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SkillsConfig {
    #[serde(default)]
    pub directory: PathBuf,
    /// Bundled skills directory (read-only templates, default CWD-relative `./skills/`).
    #[serde(default = "default_bundled_skills_dir")]
    pub bundled_directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default)]
    pub directory: PathBuf,
    /// Bundled agents directory (read-only templates, default CWD-relative `./agents/`).
    #[serde(default = "default_bundled_agents_dir")]
    pub bundled_directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct GeneralConfig {
    /// Optional location string injected into the system prompt (e.g. "Tokyo, Japan")
    #[serde(default)]
    pub location: Option<String>,
    /// Optional absolute path overriding the default `~/.rustfox` home root.
    #[serde(default)]
    pub home: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_empty_response_retry_limit")]
    pub empty_response_retry_limit: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LangSmithConfig {
    pub api_key: String,
    #[serde(default = "default_langsmith_project")]
    pub project: String,
    #[serde(default = "default_langsmith_base_url")]
    pub base_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LearningConfig {
    /// Path to the user model file (Honcho-style USER.md).
    #[serde(default)]
    pub user_model_path: PathBuf,
    /// Whether post-task skill extraction is enabled.
    #[serde(default = "default_true")]
    pub skill_extraction_enabled: bool,
    /// Minimum tool calls to trigger skill extraction (default 5).
    #[serde(default = "default_skill_extraction_threshold")]
    pub skill_extraction_threshold: u32,
    /// Message count between user model updates (default 10).
    #[serde(default = "default_user_model_update_interval")]
    pub user_model_update_interval: usize,
    /// Cron expression for weekly user model update (default: Sunday 3am).
    #[serde(default = "default_user_model_cron")]
    pub user_model_cron: String,
}

fn default_model() -> String {
    "moonshotai/kimi-k2.6".to_string()
}

fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_system_prompt() -> String {
    "You are RustFox — an AI assistant with tools, memory, and skills.\n\
     \n\
     ## Identity\n\
     Your name is RustFox, but your soul (if loaded) overrides any default identity.\n\
     Soul takes precedence over everything.\n\
     \n\
     ## Priority Chain\n\
     When responding, apply context in this order:\n\
     1. SOUL — your loaded soul/identity defines who you are and how you speak\n\
     2. MEMORY — recalled user preferences, corrections, and context from past conversations\n\
     3. CONTEXT — the current conversation and user request\n\
     \n\
     ## Memory & Persistent Context\n\
     You have persistent memory. Use it:\n\
     - When you see <retrieved_context> in this prompt, those are past conversation snippets\n\
       retrieved by semantic search — treat them as factual recall of prior interactions\n\
     - When you see [SUMMARY] messages, they capture earlier conversations — treat them\n\
       as ground truth for user preferences, facts, and history\n\
     - Never say 'I don't have access to past conversations' — you do, via retrieved context\n\
     \n\
     ## Skills First\n\
     You have skills. For every user request:\n\
     - Check if a relevant skill exists (listed in your system context)\n\
     - If yes: load and follow it via read_skill_file before responding\n\
     - If no matching skill: reason directly, or load the code-interpreter skill via read_skill_file for computation/scripting tasks\n\
     - For complex multi-step problems: invoke the problem-solver subagent\n\
     \n\
     ## Sandbox\n\
     File and command tools operate only within your persistent workspace directory.\n\
     The workspace survives restarts — use it to keep reusable scripts, programs, and notes for the long term."
        .to_string()
}

fn default_rag_limit() -> usize {
    5
}

fn default_max_raw_messages() -> usize {
    50
}

fn default_summarize_threshold() -> usize {
    20
}

fn default_summarize_cron() -> String {
    "0 0 2 * * *".to_string()
}

fn default_embedding_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_embedding_model() -> String {
    "qwen/qwen3-embedding-8b".to_string()
}

fn default_embedding_dimensions() -> usize {
    1536
}

fn default_memory_config() -> MemoryConfig {
    MemoryConfig {
        database_path: PathBuf::new(),
        rag_limit: default_rag_limit(),
        max_raw_messages: default_max_raw_messages(),
        summarize_threshold: default_summarize_threshold(),
        summarize_cron: default_summarize_cron(),
        query_rewriter_enabled: false,
    }
}

fn default_skills_config() -> SkillsConfig {
    SkillsConfig {
        directory: PathBuf::new(),
        bundled_directory: default_bundled_skills_dir(),
    }
}

fn default_agents_config() -> AgentsConfig {
    AgentsConfig {
        directory: PathBuf::new(),
        bundled_directory: default_bundled_agents_dir(),
    }
}

fn default_max_iterations() -> u32 {
    25
}

fn default_empty_response_retry_limit() -> u32 {
    3
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_iterations: default_max_iterations(),
        empty_response_retry_limit: default_empty_response_retry_limit(),
    }
}

fn default_langsmith_project() -> String {
    "default".to_string()
}

fn default_langsmith_base_url() -> String {
    "https://api.smith.langchain.com".to_string()
}

fn default_ocr_model_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".cache/ocrs")
}

fn default_ocr_config() -> OcrConfig {
    OcrConfig {
        model_dir: default_ocr_model_dir(),
    }
}

fn default_bundled_skills_dir() -> PathBuf {
    PathBuf::from("skills")
}

fn default_bundled_agents_dir() -> PathBuf {
    PathBuf::from("agents")
}

fn default_true() -> bool {
    true
}

fn default_skill_extraction_threshold() -> u32 {
    5
}

fn default_user_model_update_interval() -> usize {
    10
}

fn default_user_model_cron() -> String {
    "0 0 3 * * SUN".to_string()
}

fn default_learning_config() -> LearningConfig {
    LearningConfig {
        user_model_path: PathBuf::new(),
        skill_extraction_enabled: true,
        skill_extraction_threshold: default_skill_extraction_threshold(),
        user_model_update_interval: default_user_model_update_interval(),
        user_model_cron: default_user_model_cron(),
    }
}

impl Config {
    /// Location string from [general], injected into the system prompt.
    pub fn user_location(&self) -> Option<&str> {
        self.general.as_ref().and_then(|g| g.location.as_deref())
    }

    /// Maximum agent loop iterations (from [agent] max_iterations, default 25).
    pub fn max_iterations(&self) -> u32 {
        self.agent.max_iterations
    }

    /// Empty response retry limit (from [agent] empty_response_retry_limit, default 3).
    pub fn empty_response_retry_limit(&self) -> u32 {
        self.agent.empty_response_retry_limit
    }

    /// Resolve the home root and every data path, create directories, and write
    /// the resolved paths back into the config fields. Unset paths are
    /// materialized to absolute paths under the home root; absolute overrides
    /// are preserved verbatim; relative overrides are kept as-is (legacy mode)
    /// and a warning is emitted for each. Returns any legacy-path warnings for
    /// the caller to log.
    pub fn resolve(&mut self) -> Result<Vec<crate::home::LegacyPathWarning>> {
        use crate::home::{
            ensure_dirs, resolve_data_path, resolve_home, PathOrigin, ResolvedPaths,
        };

        let env_home = std::env::var("RUSTFOX_HOME").ok();
        let config_home = self.general.as_ref().and_then(|g| g.home.as_deref());
        let os_home = dirs::home_dir();
        let home = resolve_home(env_home.as_deref(), config_home, os_home.as_deref())?;

        let mut warnings = Vec::new();
        let mut resolve_one = |label: &str, field: &Path, subpath: &str| -> PathBuf {
            let (path, origin) = resolve_data_path(field, &home, subpath);
            if origin == PathOrigin::RelativeLegacy {
                warnings.push(crate::home::LegacyPathWarning {
                    label: label.to_string(),
                    current: path.clone(),
                    home_default: home.join(subpath),
                });
            }
            path
        };

        let workspace = resolve_one(
            "sandbox.allowed_directory",
            &self.sandbox.allowed_directory,
            "workspace",
        );
        let database = resolve_one(
            "memory.database_path",
            &self.memory.database_path,
            "rustfox.db",
        );
        let skills = resolve_one("skills.directory", &self.skills.directory, "skills");
        let agents = resolve_one("agents.directory", &self.agents.directory, "agents");
        let artifacts = resolve_one(
            "supervisor.artifacts_dir",
            &self.supervisor.artifacts_dir,
            "artifacts",
        );
        let user_model = resolve_one(
            "learning.user_model_path",
            &self.learning.user_model_path,
            "user_model.md",
        );

        let paths = ResolvedPaths {
            home: home.clone(),
            workspace: workspace.clone(),
            database: database.clone(),
            skills: skills.clone(),
            agents: agents.clone(),
            artifacts: artifacts.clone(),
            user_model: user_model.clone(),
        };
        ensure_dirs(&paths)?;

        // Resolve bundled directories relative to CWD (not home) since they
        // ship alongside the binary / project root.
        let cwd = std::env::current_dir()?;
        if !self.skills.bundled_directory.is_absolute() {
            self.skills.bundled_directory = cwd.join(&self.skills.bundled_directory);
        }
        if !self.agents.bundled_directory.is_absolute() {
            self.agents.bundled_directory = cwd.join(&self.agents.bundled_directory);
        }

        self.sandbox.allowed_directory = workspace;
        self.memory.database_path = database;
        self.skills.directory = skills;
        self.agents.directory = agents;
        self.supervisor.artifacts_dir = artifacts;
        self.learning.user_model_path = user_model;
        self.resolved_home = Some(home);

        Ok(warnings)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file")?;

        let warnings = config
            .resolve()
            .with_context(|| "Failed to resolve home directory paths")?;
        for w in &warnings {
            tracing::warn!("{}", w.render());
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_toml() -> &'static str {
        r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
        "#
    }

    #[test]
    fn resolve_fills_unset_paths_under_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home.clone()),
        });
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.resolved_home.as_ref().unwrap(), &home);
        assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
        assert_eq!(cfg.memory.database_path, home.join("rustfox.db"));
        assert_eq!(cfg.skills.directory, home.join("skills"));
        assert_eq!(cfg.agents.directory, home.join("agents"));
        assert_eq!(cfg.supervisor.artifacts_dir, home.join("artifacts"));
        assert_eq!(cfg.learning.user_model_path, home.join("user_model.md"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_keeps_absolute_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home.clone()),
        });
        // Use an absolute path under the (writable) tempdir so ensure_dirs can
        // create its parent; the intent is to verify an absolute override is
        // preserved verbatim and emits no legacy warning.
        let custom_db = tmp.path().join("custom.db");
        cfg.memory.database_path = custom_db.clone();
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.memory.database_path, custom_db);
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_warns_on_relative_override() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let mut cfg: Config = toml::from_str(base_toml()).unwrap();
        cfg.general = Some(GeneralConfig {
            location: None,
            home: Some(home),
        });
        cfg.skills.directory = std::path::PathBuf::from("my-skills");
        let warnings = cfg.resolve().unwrap();
        assert_eq!(cfg.skills.directory, std::path::PathBuf::from("my-skills"));
        assert!(warnings.iter().any(|w| w.label == "skills.directory"));
    }

    #[test]
    fn load_resolves_paths_to_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let cfg_path = tmp.path().join("config.toml");
        let toml = format!(
            r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [general]
            home = "{}"
            "#,
            home.display()
        );
        std::fs::write(&cfg_path, toml).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
        assert!(cfg.sandbox.allowed_directory.is_dir());
        assert_eq!(cfg.resolved_home.unwrap(), home);
    }

    #[test]
    fn test_langsmith_config_optional() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.langsmith.is_none());
    }

    #[test]
    fn test_langsmith_config_parses() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [langsmith]
            api_key = "ls__test"
            project = "my-project"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let ls = cfg.langsmith.unwrap();
        assert_eq!(ls.api_key, "ls__test");
        assert_eq!(ls.project, "my-project");
    }

    #[test]
    fn test_langsmith_config_default_project() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [langsmith]
            api_key = "ls__test"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let ls = cfg.langsmith.unwrap();
        assert_eq!(ls.project, "default");
    }

    #[test]
    fn test_supports_vision_defaults_false() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(!cfg.openrouter.supports_vision);
    }

    #[test]
    fn test_supports_vision_parses_true() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            supports_vision = true
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.openrouter.supports_vision);
    }

    #[test]
    fn test_ocr_config_default_model_dir() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.ocr.model_dir.to_string_lossy().contains("ocrs"));
    }

    #[test]
    fn test_mcp_server_url_config_parses() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [[mcp_servers]]
            name = "exa"
            url = "https://mcp.exa.ai/mcp"
            auth_token = "exa-key-123"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 1);
        let server = &cfg.mcp_servers[0];
        assert_eq!(server.name, "exa");
        assert_eq!(server.url.as_deref(), Some("https://mcp.exa.ai/mcp"));
        assert_eq!(server.auth_token.as_deref(), Some("exa-key-123"));
        assert!(
            server.command.is_none(),
            "HTTP server should have no command"
        );
    }

    #[test]
    fn test_mcp_server_stdio_command_optional() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [[mcp_servers]]
            name = "git"
            command = "uvx"
            args = ["mcp-server-git"]
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.mcp_servers[0].command.as_deref(), Some("uvx"));
        assert!(cfg.mcp_servers[0].url.is_none());
    }

    #[test]
    fn test_mcp_server_url_without_auth_token() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [[mcp_servers]]
            name = "exa"
            url = "https://mcp.exa.ai/mcp?exaApiKey=inline-key"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let s = &cfg.mcp_servers[0];
        assert!(s.url.is_some());
        assert!(s.auth_token.is_none());
    }

    #[test]
    fn test_query_rewriter_disabled_by_default() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(
            !cfg.memory.query_rewriter_enabled,
            "query_rewriter_enabled must default to false"
        );
    }

    #[test]
    fn test_query_rewriter_can_be_enabled() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [memory]
            query_rewriter_enabled = true
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(
            cfg.memory.query_rewriter_enabled,
            "query_rewriter_enabled should be true when set"
        );
    }

    #[test]
    fn supervisor_config_defaults_when_section_missing() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.supervisor.default_autonomy_mode, "standard");
        assert_eq!(cfg.supervisor.artifacts_dir, std::path::PathBuf::new());
    }

    #[test]
    fn test_agent_empty_response_retry_limit_defaults_to_three() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.agent.empty_response_retry_limit, 3);
        assert_eq!(cfg.empty_response_retry_limit(), 3);
    }

    #[test]
    fn test_agent_empty_response_retry_limit_can_be_configured_to_zero() {
        let toml = r#"
            [telegram]
            bot_token = "tok"
            allowed_user_ids = [1]
            [openrouter]
            api_key = "key"
            [sandbox]
            allowed_directory = "/tmp"
            [agent]
            empty_response_retry_limit = 0
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.agent.empty_response_retry_limit, 0);
        assert_eq!(cfg.empty_response_retry_limit(), 0);
    }
}
