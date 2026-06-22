use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::{ProviderSection, ProviderType};
use crate::llm::{ChatCompletion, ChatMessage, ToolDefinition};

/// Runtime configuration for a single LLM provider.
///
/// This is the "live" view of a provider built from a [`ProviderSection`] in
/// the config file. The `From<&ProviderSection>` conversion is the canonical
/// way to construct one.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub provider_type: ProviderType,
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
    pub supports_vision: bool,
    pub max_tokens: u32,
    pub discover_models: bool,
}

impl From<&ProviderSection> for ProviderConfig {
    fn from(s: &ProviderSection) -> Self {
        Self {
            name: s.name.clone(),
            provider_type: s.provider_type.clone(),
            base_url: s.base_url.clone(),
            api_key: s.api_key.clone(),
            default_model: s.model.clone(),
            supports_vision: s.supports_vision,
            max_tokens: s.max_tokens,
            discover_models: s.discover_models,
        }
    }
}

/// Unified LLM provider abstraction.
///
/// Each implementation owns its own HTTP shaping (URL, headers, model list
/// endpoint) and delegates the request/response body shapes to the shared
/// types in [`crate::llm`].
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn supports_vision(&self) -> bool;
    fn config(&self) -> &ProviderConfig;

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion>;

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>>;
}

/// Holds the set of providers available to the agent and the default provider
/// name used when a model string does not carry an explicit `provider/` prefix.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    default_provider: String,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_provider", &self.default_provider)
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new(providers: HashMap<String, Arc<dyn Provider>>, default_provider: String) -> Self {
        Self {
            providers,
            default_provider,
        }
    }

    /// Resolve a model string to (provider, stripped_model).
    /// Never fails — unknown prefixes fall through to default provider.
    pub fn resolve_model<'a>(&'a self, model: &'a str) -> (&'a dyn Provider, &'a str) {
        if let Some((prefix, rest)) = model.split_once('/') {
            if let Some(provider) = self.providers.get(prefix) {
                return (provider.as_ref(), rest);
            }
        }
        // Fall through to default provider with full string
        let default = &self.providers[&self.default_provider];
        (default.as_ref(), model)
    }

    pub fn get_provider(&self, name: &str) -> Option<&dyn Provider> {
        self.providers.get(name).map(|p| p.as_ref())
    }

    pub fn providers(&self) -> impl Iterator<Item = &dyn Provider> {
        self.providers.values().map(|p| p.as_ref())
    }

    pub fn default_provider_name(&self) -> &str {
        &self.default_provider
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}

/// Build a ProviderRegistry from config sections.
pub fn build_registry(
    sections: &[ProviderSection],
    default_name: &str,
) -> Result<ProviderRegistry> {
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

    for section in sections {
        let cfg: ProviderConfig = ProviderConfig::from(section);
        let provider: Arc<dyn Provider> = match section.provider_type {
            ProviderType::OpenRouter => Arc::new(OpenRouterProvider::new(cfg)),
            ProviderType::OpenAICompatible => Arc::new(OpenAICompatibleProvider::new(cfg)),
            ProviderType::Ollama => Arc::new(OllamaProvider::new(cfg)),
        };
        providers.insert(section.name.clone(), provider);
    }

    if providers.is_empty() {
        anyhow::bail!("No LLM providers configured");
    }

    if !providers.contains_key(default_name) {
        anyhow::bail!(
            "Default provider '{}' not found in configured providers",
            default_name
        );
    }

    Ok(ProviderRegistry::new(providers, default_name.to_string()))
}

// === OpenRouterProvider ===

pub struct OpenRouterProvider {
    config: ProviderConfig,
}

impl OpenRouterProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        let tools_param = if tools.is_empty() {
            None
        } else {
            let sanitized: Vec<ToolDefinition> = tools
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    crate::llm::sanitize_parameters(&mut t.function.parameters);
                    t
                })
                .collect();
            Some(sanitized)
        };

        let request = crate::llm::internal::ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools_param,
            tool_choice: None,
            max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        let mut req = client.post(&url).json(&request);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req.send().await.context("Failed to send request to OpenRouter")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter API error ({}): {}", status, body);
        }

        let chat_response: crate::llm::internal::ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter response")?;

        let mut choice = chat_response
            .choices
            .into_iter()
            .next()
            .context("No response from OpenRouter")?;

        // Kimi tool-call fallback
        let has_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if !has_tool_calls {
            if let Some(ref content) = choice.message.content {
                if let Some(parsed) = crate::llm::parse_kimi_tool_calls(&content.as_text()) {
                    choice.message.tool_calls = Some(parsed);
                    choice.message.content = None;
                }
            }
        }

        Ok(ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: model.to_string(),
        })
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        let url = format!("{}/models", self.config.base_url);
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req.send().await.context("Failed to fetch models from OpenRouter")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter models API error ({}): {}", status, body);
        }
        let list: serde_json::Value = response.json().await?;
        let models = list["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}

// === OpenAICompatibleProvider ===

pub struct OpenAICompatibleProvider {
    config: ProviderConfig,
}

impl OpenAICompatibleProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Provider for OpenAICompatibleProvider {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        let tools_param = if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
        };

        let request = crate::llm::internal::ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools_param,
            tool_choice: None,
            max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        let mut req = client.post(&url).json(&request);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req
            .send()
            .await
            .context("Failed to send request to OpenAI-compatible provider")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Provider '{}' error ({}): {}",
                self.config.name,
                status,
                body
            );
        }

        let chat_response: crate::llm::internal::ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI-compatible provider response")?;
        let choice =
            chat_response.choices.into_iter().next().ok_or_else(|| {
                anyhow::anyhow!("No response from provider '{}'", self.config.name)
            })?;

        Ok(ChatCompletion {
            message: choice.message,
            finish_reason: choice.finish_reason,
            model: model.to_string(),
        })
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        let url = format!("{}/models", self.config.base_url);
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req
            .send()
            .await
            .context("Failed to fetch models from OpenAI-compatible provider")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Provider '{}' models API error ({}): {}",
                self.config.name,
                status,
                body
            );
        }
        let list: serde_json::Value = response.json().await?;
        let models = list["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}

// === OllamaProvider ===

pub struct OllamaProvider {
    config: ProviderConfig,
    inner: OpenAICompatibleProvider,
}

impl OllamaProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let inner_cfg = config.clone();
        let inner = OpenAICompatibleProvider::new(inner_cfg);
        Self { config, inner }
    }

    /// Compute the native Ollama `/api/tags` model-discovery URL.
    ///
    /// Ollama's native endpoint lives at `/api/tags` on the server root
    /// (not under `/v1`). Strip any common version suffix from the configured
    /// base URL before appending the tags path.
    fn discovery_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        let base = base
            .strip_suffix("/v1")
            .or_else(|| base.strip_suffix("/v2"))
            .or_else(|| base.strip_suffix("/v3"))
            .unwrap_or(base);
        format!("{}/api/tags", base.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        self.config.supports_vision
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        self.inner
            .chat_completion(client, messages, tools, model, max_tokens)
            .await
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        let url = self.discovery_url();
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req
            .send()
            .await
            .context("Failed to fetch models from Ollama")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama models API error ({}): {}", status, body);
        }
        let body: serde_json::Value = response.json().await?;
        let models = body["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderSection;

    fn make_section(
        name: &str,
        provider_type: ProviderType,
        base_url: &str,
        model: &str,
    ) -> ProviderSection {
        ProviderSection {
            name: name.to_string(),
            provider_type,
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            model: model.to_string(),
            supports_vision: false,
            max_tokens: 1024,
            discover_models: false,
        }
    }

    #[test]
    fn provider_config_from_section_copies_all_fields() {
        let section = make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        );
        let cfg = ProviderConfig::from(&section);
        assert_eq!(cfg.name, "alpha");
        assert_eq!(cfg.provider_type, ProviderType::OpenRouter);
        assert_eq!(cfg.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cfg.api_key.as_deref(), Some("test-key"));
        assert_eq!(cfg.default_model, "anthropic/claude-sonnet-4-6");
        assert_eq!(cfg.max_tokens, 1024);
    }

    #[test]
    fn build_registry_creates_one_provider_per_section() {
        let sections = vec![
            make_section(
                "alpha",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "anthropic/claude-sonnet-4-6",
            ),
            make_section(
                "beta",
                ProviderType::Ollama,
                "http://localhost:11434/v1",
                "llama3.1",
            ),
        ];
        let reg = build_registry(&sections, "alpha").unwrap();
        assert_eq!(reg.provider_count(), 2);
        assert!(reg.get_provider("alpha").is_some());
        assert!(reg.get_provider("beta").is_some());
        assert_eq!(reg.default_provider_name(), "alpha");
    }

    #[test]
    fn build_registry_fails_with_no_providers() {
        let result = build_registry(&[], "alpha");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No LLM providers"));
    }

    #[test]
    fn build_registry_fails_when_default_missing() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let result = build_registry(&sections, "missing");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Default provider 'missing'"));
    }

    #[test]
    fn resolve_model_uses_prefix_when_provider_known() {
        let sections = vec![
            make_section(
                "alpha",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "anthropic/claude-sonnet-4-6",
            ),
            make_section(
                "beta",
                ProviderType::Ollama,
                "http://localhost:11434/v1",
                "llama3.1",
            ),
        ];
        let reg = build_registry(&sections, "alpha").unwrap();
        let (provider, model) = reg.resolve_model("beta/llama3.1:8b");
        assert_eq!(provider.name(), "beta");
        assert_eq!(model, "llama3.1:8b");
    }

    #[test]
    fn resolve_model_falls_back_to_default_on_unknown_prefix() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let reg = build_registry(&sections, "alpha").unwrap();
        let (provider, model) = reg.resolve_model("moonshotai/kimi-k2.5");
        assert_eq!(provider.name(), "alpha");
        assert_eq!(model, "moonshotai/kimi-k2.5");
    }

    #[test]
    fn resolve_model_uses_default_for_bare_model() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let reg = build_registry(&sections, "alpha").unwrap();
        let (provider, model) = reg.resolve_model("claude-sonnet-4-6");
        assert_eq!(provider.name(), "alpha");
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn provider_names_returns_all_registered_names() {
        let sections = vec![
            make_section(
                "alpha",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "anthropic/claude-sonnet-4-6",
            ),
            make_section(
                "beta",
                ProviderType::Ollama,
                "http://localhost:11434/v1",
                "llama3.1",
            ),
        ];
        let reg = build_registry(&sections, "alpha").unwrap();
        let mut names = reg.provider_names();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn ollama_discovery_url_strips_v1_suffix() {
        let cfg = ProviderConfig {
            name: "local".to_string(),
            provider_type: ProviderType::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: None,
            default_model: "llama3.1".to_string(),
            supports_vision: false,
            max_tokens: 1024,
            discover_models: true,
        };
        let p = OllamaProvider::new(cfg);
        assert_eq!(p.discovery_url(), "http://localhost:11434/api/tags");
    }

    #[test]
    fn ollama_discovery_url_strips_trailing_slash() {
        let cfg = ProviderConfig {
            name: "local".to_string(),
            provider_type: ProviderType::Ollama,
            base_url: "http://localhost:11434/".to_string(),
            api_key: None,
            default_model: "llama3.1".to_string(),
            supports_vision: false,
            max_tokens: 1024,
            discover_models: true,
        };
        let p = OllamaProvider::new(cfg);
        assert_eq!(p.discovery_url(), "http://localhost:11434/api/tags");
    }

    #[test]
    fn ollama_discovery_url_appends_to_bare_host() {
        let cfg = ProviderConfig {
            name: "local".to_string(),
            provider_type: ProviderType::Ollama,
            base_url: "http://localhost:11434".to_string(),
            api_key: None,
            default_model: "llama3.1".to_string(),
            supports_vision: false,
            max_tokens: 1024,
            discover_models: true,
        };
        let p = OllamaProvider::new(cfg);
        assert_eq!(p.discovery_url(), "http://localhost:11434/api/tags");
    }
}
