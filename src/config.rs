use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub openrouter: OpenRouterConfig,
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
    #[serde(default = "default_learning_config")]
    pub learning: LearningConfig,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SupervisorConfig {
    #[serde(default = "default_autonomy_mode")]
    pub default_autonomy_mode: String,
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: std::path::PathBuf,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            default_autonomy_mode: default_autonomy_mode(),
            artifacts_dir: default_artifacts_dir(),
        }
    }
}

fn default_autonomy_mode() -> String {
    "standard".to_string()
}

fn default_artifacts_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("supervisor/artifacts")
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
}

#[derive(Debug, Deserialize, Clone)]
pub struct SandboxConfig {
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
    #[serde(default = "default_db_path")]
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
    #[serde(default = "default_skills_dir")]
    pub directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default = "default_agents_dir")]
    pub directory: PathBuf,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct GeneralConfig {
    /// Optional location string injected into the system prompt (e.g. "Tokyo, Japan")
    #[serde(default)]
    pub location: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
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
    #[serde(default = "default_user_model_path")]
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
    "moonshotai/kimi-k2.5".to_string()
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
     - If no matching skill: reason directly, or use code-interpreter for computation/scripting tasks\n\
     - For complex multi-step problems: invoke the problem-solver subagent\n\
     \n\
     ## Sandbox\n\
     File and command tools operate only within the allowed sandbox directory."
        .to_string()
}

fn default_db_path() -> PathBuf {
    PathBuf::from("rustfox.db")
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

fn default_skills_dir() -> PathBuf {
    PathBuf::from("skills")
}

fn default_agents_dir() -> PathBuf {
    PathBuf::from("agents")
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
        database_path: default_db_path(),
        rag_limit: default_rag_limit(),
        max_raw_messages: default_max_raw_messages(),
        summarize_threshold: default_summarize_threshold(),
        summarize_cron: default_summarize_cron(),
        query_rewriter_enabled: false,
    }
}

fn default_skills_config() -> SkillsConfig {
    SkillsConfig {
        directory: default_skills_dir(),
    }
}

fn default_agents_config() -> AgentsConfig {
    AgentsConfig {
        directory: default_agents_dir(),
    }
}

fn default_max_iterations() -> u32 {
    25
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_iterations: default_max_iterations(),
    }
}

fn default_langsmith_project() -> String {
    "default".to_string()
}

fn default_langsmith_base_url() -> String {
    "https://api.smith.langchain.com".to_string()
}

fn default_user_model_path() -> PathBuf {
    PathBuf::from("memory/USER.md")
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
        user_model_path: default_user_model_path(),
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

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file")?;

        // Validate sandbox directory exists
        if !config.sandbox.allowed_directory.exists() {
            std::fs::create_dir_all(&config.sandbox.allowed_directory).with_context(|| {
                format!(
                    "Failed to create sandbox directory: {}",
                    config.sandbox.allowed_directory.display()
                )
            })?;
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            cfg.supervisor.artifacts_dir,
            std::path::PathBuf::from("supervisor/artifacts")
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
}
