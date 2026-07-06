use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::{ProviderSection, ProviderType};
use crate::llm::{ChatCompletion, ChatMessage, ToolDefinition};
use crate::llm::internal::{ChatResponse, Choice};

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
    pub context_window: usize,
    /// Runtime cache for the current model's context window, populated
    /// asynchronously from the provider API. When None, falls back to
    /// `context_window`.
    pub context_window_cache: Arc<RwLock<Option<usize>>>,
    /// Number of retries when the response is missing the `choices` field.
    /// Defaults to 3; set to 0 to disable.
    pub parse_retry_limit: u32,
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
            context_window: s.context_window,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0, // overwritten by build_registry
        }
    }
}

/// Internal helper: send a chat completion request with retry logic for
/// missing/empty `choices` field. Returns the first valid `Choice` on success.
///
/// The request is re-sent on each retry (up to `parse_retry_limit` times) with
/// exponential backoff (1s, 2s, 4s...). Only retries when the JSON response
/// is valid but `choices` is missing or empty; other errors (network, HTTP
/// status, JSON parse) are returned immediately.
async fn chat_completion_with_retry(
    client: &reqwest::Client,
    url: &str,
    request: &crate::llm::internal::ChatRequest,
    api_key: Option<&str>,
    provider_name: &str,
    parse_retry_limit: u32,
) -> Result<Choice> {
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..=parse_retry_limit {
        let mut req = client.post(url).json(request);
        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(e).context("Failed to send request"),
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "{} API error ({}): {}",
                provider_name,
                status,
                body
            ));
        }

        let body = match response.bytes().await {
            Ok(b) => b,
            Err(e) => return Err(e).context("Failed to read response body"),
        };

        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return Err(e).context("Failed to parse JSON response"),
        };

        let has_choices = value
            .get("choices")
            .and_then(|c| c.as_array())
            .is_some_and(|c| !c.is_empty());

        if has_choices {
            let chat_response: ChatResponse = serde_json::from_value(value)?;
            return chat_response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("No response from {}", provider_name));
        }

        // Missing or empty choices
        let backoff_s = 1u64 << attempt;
        let err = anyhow::anyhow!(
            "Response from {} missing or empty 'choices' field – attempt {}/{}, retry limit {}",
            provider_name,
            attempt + 1,
            parse_retry_limit + 1,
            parse_retry_limit
        );
        tracing::warn!(
            "Retry {}/{}: {} - backing off {}s",
            attempt + 1,
            parse_retry_limit,
            err,
            backoff_s
        );
        last_error = Some(err);

        if attempt < parse_retry_limit {
            tokio::time::sleep(std::time::Duration::from_secs(backoff_s)).await;
        }
    }

    Err(last_error.unwrap())
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

    /// Fetch the context window for a given model from the provider API.
    /// Returns None if the provider doesn't support runtime detection.
    async fn fetch_context_window(
        &self,
        _client: &reqwest::Client,
        _model: &str,
    ) -> Option<usize> {
        None  // default: no API-based detection
    }
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

    /// Return the fully-qualified default model string, e.g. `"openrouter/moonshotai/kimi-k2.6"`.
    pub fn default_qualified_model(&self) -> String {
        let provider = &self.providers[&self.default_provider];
        format!("{}/{}", self.default_provider, provider.default_model())
    }

    /// Return the context window size of the default provider.
    pub fn default_context_window(&self) -> usize {
        let provider = &self.providers[&self.default_provider];
        provider.config().context_window
    }

    /// Return the effective context window for a model: runtime cache
    /// if populated, otherwise static config fallback.
    pub fn effective_context_window(&self, model: &str) -> usize {
        let (provider, _) = self.resolve_model(model);
        let cached = provider.config().context_window_cache.try_read()
            .ok()
            .and_then(|c| *c);
        cached.unwrap_or(provider.config().context_window)
    }
}

/// Build a ProviderRegistry from config sections.
pub fn build_registry(
    sections: &[ProviderSection],
    default_name: &str,
    parse_retry_limit: u32,
) -> Result<ProviderRegistry> {
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

    for section in sections {
        let mut cfg: ProviderConfig = ProviderConfig::from(section);
        cfg.parse_retry_limit = parse_retry_limit;
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
            max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        let api_key = self.config.api_key.as_deref();
        let provider_name = self.config.name.as_str();
        let parse_retry_limit = self.config.parse_retry_limit;

        let mut choice = chat_completion_with_retry(
            client,
            &url,
            &request,
            api_key,
            provider_name,
            parse_retry_limit,
        )
        .await?;

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
        let response = req
            .send()
            .await
            .context("Failed to fetch models from OpenRouter")?;
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

    async fn fetch_context_window(
        &self,
        client: &reqwest::Client,
        model: &str,
    ) -> Option<usize> {
        let url = format!("{}/models", self.config.base_url);
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let list: serde_json::Value = response.json().await.ok()?;
        let ctx = list["data"].as_array()?
            .iter()
            .find(|m| m["id"].as_str() == Some(model))?
            .get("context_length")?
            .as_u64()?;
        Some(ctx as usize)
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
            max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        let api_key = self.config.api_key.as_deref();
        let provider_name = self.config.name.as_str();
        let parse_retry_limit = self.config.parse_retry_limit;

        let choice = chat_completion_with_retry(
            client,
            &url,
            &request,
            api_key,
            provider_name,
            parse_retry_limit,
        )
        .await?;

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

    async fn fetch_context_window(
        &self,
        client: &reqwest::Client,
        model: &str,
    ) -> Option<usize> {
        let url = format!("{}/models", self.config.base_url);
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let list: serde_json::Value = response.json().await.ok()?;
        let ctx = list["data"].as_array()?
            .iter()
            .find(|m| m["id"].as_str() == Some(model))?
            .get("context_length")?
            .as_u64()?;
        Some(ctx as usize)
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
            context_window: 512_000,
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
        assert_eq!(cfg.context_window, 512_000);
        // build_registry overwrites parse_retry_limit; From impl sets 0 as sentinel
        assert_eq!(cfg.parse_retry_limit, 0);
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
        let reg = build_registry(&sections, "alpha", 3).unwrap();
        assert_eq!(reg.provider_count(), 2);
        assert!(reg.get_provider("alpha").is_some());
        assert!(reg.get_provider("beta").is_some());
        assert_eq!(reg.default_provider_name(), "alpha");
    }

    #[test]
    fn build_registry_fails_with_no_providers() {
        let result = build_registry(&[], "alpha", 3);
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
        let result = build_registry(&sections, "missing", 3);
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
        let reg = build_registry(&sections, "alpha", 3).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3).unwrap();
        let mut names = reg.provider_names();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn effective_context_window_falls_back_to_static_when_cache_empty() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let reg = build_registry(&sections, "alpha", 3).unwrap();
        let ctx = reg.effective_context_window("anthropic/claude-sonnet-4-6");
        assert_eq!(ctx, 512_000); // static fallback from section
    }

    #[test]
    fn effective_context_window_returns_cached_value_when_set() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let reg = build_registry(&sections, "alpha", 3).unwrap();
        let provider = reg.get_provider("alpha").unwrap();
        // Set cache manually
        *provider.config().context_window_cache.try_write().unwrap() = Some(200_000);
        let ctx = reg.effective_context_window("anthropic/claude-sonnet-4-6");
        assert_eq!(ctx, 200_000);
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
            context_window: 512_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 3,
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
            context_window: 512_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 3,
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
            context_window: 512_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 3,
        };
        let p = OllamaProvider::new(cfg);
        assert_eq!(p.discovery_url(), "http://localhost:11434/api/tags");
    }
}
