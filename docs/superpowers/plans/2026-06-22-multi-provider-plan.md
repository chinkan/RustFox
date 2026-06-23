# Multi-Provider LLM Architecture — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add multiple LLM provider support (Ollama, LM Studio, etc.), fallback chains, and per-subagent provider mixing.

**Architecture:** Define a `Provider` trait with concrete impls (OpenRouter, OpenAICompatible, Ollama). `ProviderRegistry` resolves model strings (`ollama/llama3` → Ollama provider + model `llama3`). `LlmClient` routes calls through the registry. Fallback chains in the agent loop try alternative models on failure.

**Tech Stack:** Rust, reqwest, serde, async_trait, teloxide (inline keyboards)

---

### Task 1: Add config types for providers and fallback

**Files:**
- Modify: `src/config.rs`
- Read: `docs/superpowers/specs/2026-06-22-multi-provider-design.md` (for reference)

- [ ] **Step 1: Add ProviderType enum to config.rs**

Add this after the existing config structs (before `OcrConfig` or similar):

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub enum ProviderType {
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "openai_compatible")]
    OpenAICompatible,
    #[serde(rename = "ollama")]
    Ollama,
}

impl Default for ProviderType {
    fn default() -> Self {
        Self::OpenRouter
    }
}
```

- [ ] **Step 2: Add ProviderSection and FallbackConfig structs**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSection {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub discover_models: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FallbackConfig {
    #[serde(default)]
    pub chain: Vec<String>,
}
```

- [ ] **Step 3: Add fields to Config struct**

Add inside `pub struct Config`:
```rust
    #[serde(default)]
    pub provider: Vec<ProviderSection>,
    #[serde(default)]
    pub fallback: FallbackConfig,
```

- [ ] **Step 4: Add backward compat builder method to Config**

```rust
/// Build the provider list from config, handling legacy [openrouter] backward compat.
/// Returns (providers, default_provider_name, fallback_chain).
pub fn build_providers(&self) -> (Vec<ProviderSection>, String, Vec<String>) {
    let mut providers: Vec<ProviderSection> = self.provider.clone();

    // Backward compat: if [openrouter] section exists and no explicit provider named "openrouter"
    let has_openrouter = providers.iter().any(|p| p.name == "openrouter");
    if has_openrouter {
        tracing::warn!(
            "Explicit [[provider]] name=\"openrouter\" found — legacy [openrouter] section will be ignored"
        );
    } else {
        providers.push(ProviderSection {
            name: "openrouter".to_string(),
            provider_type: ProviderType::OpenRouter,
            base_url: self.openrouter.base_url.clone(),
            api_key: Some(self.openrouter.api_key.clone()),
            model: self.openrouter.model.clone(),
            supports_vision: self.openrouter.supports_vision,
            max_tokens: self.openrouter.max_tokens,
            discover_models: false,
        });
    }

    let default = if providers.is_empty() {
        "openrouter".to_string()
    } else {
        providers[0].name.clone()
    };

    let fallback = self.fallback.chain.clone();
    (providers, default, fallback)
}
```

- [ ] **Step 5: Run tests to verify existing config parsing still works**

```bash
cargo test -p rustfox -- config::tests --nocapture
```
Expected: Existing config tests pass (backward compat).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat: add provider and fallback config types"
```

---

### Task 2: Create `src/provider.rs` — Provider trait, types, and registry

**Files:**
- Create: `src/provider.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add module declaration to lib.rs**

```rust
pub mod provider;
```

- [ ] **Step 2: Create src/provider.rs with the core types**

```rust
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::{ProviderSection, ProviderType};
use crate::llm::{ChatCompletion, ChatMessage, ToolDefinition};

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
```

- [ ] **Step 3: Add ProviderRegistry**

```rust
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    default_provider: String,
}

impl ProviderRegistry {
    pub fn new(
        providers: HashMap<String, Arc<dyn Provider>>,
        default_provider: String,
    ) -> Self {
        Self {
            providers,
            default_provider,
        }
    }

    /// Resolve a model string to (provider, stripped_model).
    /// Never fails — unknown prefixes fall through to default provider.
    pub fn resolve_model(&self, model: &str) -> (&dyn Provider, &str) {
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
```

- [ ] **Step 4: Add construction helper**

```rust
use crate::config::ProviderSection;

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

    // Ensure default exists
    if !providers.contains_key(default_name) {
        anyhow::bail!("Default provider '{}' not found in configured providers", default_name);
    }

    Ok(ProviderRegistry::new(providers, default_name.to_string()))
}
```

- [ ] **Step 6: Commit**

```bash
git add src/provider.rs src/lib.rs
git commit -m "feat: add Provider trait, ProviderRegistry, ProviderConfig types"
```

---

### Task 3: Implement OpenRouterProvider

**Files:**
- Modify: `src/provider.rs`
- Modify: `src/llm.rs`

- [ ] **Step 0: Export ChatRequest/ChatResponse from llm.rs for reuse by all providers**

Add to `llm.rs`:
```rust
// These are shared with provider implementations in src/provider.rs
#[doc(hidden)]
pub mod internal {
    pub use super::{ChatRequest, ChatResponse};
}
```

Change `ChatRequest` and `ChatResponse` from `struct ChatRequest` / `struct ChatResponse` to `pub struct ChatRequest` / `pub struct ChatResponse`.

- [ ] **Step 1: Add OpenRouterProvider struct and impl**

Append to `src/provider.rs`:

```rust
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
    fn name(&self) -> &str { &self.config.name }
    fn default_model(&self) -> &str { &self.config.default_model }
    fn supports_vision(&self) -> bool { self.config.supports_vision }
    fn config(&self) -> &ProviderConfig { &self.config }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        // Same logic as current LlmClient::chat_completion_with_model:
        let tools_param = if tools.is_empty() {
            None
        } else {
            let sanitized: Vec<ToolDefinition> = tools
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    sanitize_parameters(&mut t.function.parameters);
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
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}",
                self.config.api_key.as_deref().unwrap_or("")))
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenRouter")?;

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
                if let Some(parsed) = parse_kimi_tool_calls(&content.as_text()) {
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
        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_deref().unwrap_or("")))
            .send()
            .await?;
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
```

The `sanitize_parameters` and `parse_kimi_tool_calls` functions need to remain in `llm.rs` (they are referenced from within the OpenRouterProvider impl above via `use crate::llm::{sanitize_parameters, parse_kimi_tool_calls}`).

- [ ] **Step 2: Add the import for sanitize_parameters at top of provider.rs**

```rust
use crate::llm::{sanitize_parameters, parse_kimi_tool_calls};
```

Make `sanitize_parameters` and `parse_kimi_tool_calls` `pub` in `llm.rs`.

- [ ] **Step 3: Commit**

```bash
git add src/provider.rs src/llm.rs
git commit -m "feat: add OpenRouterProvider with param sanitization and Kimi fallback"
```

---

### Task 4: Implement OpenAICompatibleProvider

**Files:**
- Modify: `src/provider.rs`

- [ ] **Step 1: Add OpenAICompatibleProvider**

```rust
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
    fn name(&self) -> &str { &self.config.name }
    fn default_model(&self) -> &str { &self.config.default_model }
    fn supports_vision(&self) -> bool { self.config.supports_vision }
    fn config(&self) -> &ProviderConfig { &self.config }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        // Pure OpenAI-compatible endpoint: no param sanitization, no Kimi fallback.
        let tools_param = if tools.is_empty() { None } else { Some(tools.to_vec()) };

        let request = crate::llm::internal::ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools_param,
            max_tokens,
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        let mut req = client.post(&url).json(&request);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Provider '{}' error ({}): {}", self.config.name, status, body);
        }

        let chat_response = response.json::<crate::llm::internal::ChatResponse>().await?;
        let choice = chat_response.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("No response from provider '{}'", self.config.name))?;

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
        let response = req.send().await?;
        let list: serde_json::Value = response.json().await?;
        let models = list["data"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(models)
    }
}
```

(The `pub mod internal` export was already added in Task 3 Step 0. No additional llm.rs changes needed here.)

- [ ] **Step 2: Commit**

```bash
git add src/provider.rs src/llm.rs
git commit -m "feat: add OpenAICompatibleProvider for any OpenAI-compatible endpoint"
```

---

### Task 5: Implement OllamaProvider

**Files:**
- Modify: `src/provider.rs`

- [ ] **Step 1: Add OllamaProvider (delegates chat to OpenAICompatibleProvider)**

```rust
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

    /// Build the discovery URL from the OpenAI-compatible base_url.
    /// Trailing `/v1` or `/vN` is stripped and replaced with `/api/tags`.
    /// e.g. "http://localhost:11434/v1" → "http://localhost:11434/api/tags"
    ///      "http://localhost:11434" → "http://localhost:11434/api/tags"
    fn discovery_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        // Strip versioned path suffix (/v1, /v2, etc.)
        let base = base.strip_suffix("/v1")
            .or_else(|| base.strip_suffix("/v2"))
            .or_else(|| base.strip_suffix("/v3"))
            .unwrap_or(base);
        format!("{}/api/tags", base.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str { &self.config.name }
    fn default_model(&self) -> &str { &self.config.default_model }
    fn supports_vision(&self) -> bool { self.config.supports_vision }
    fn config(&self) -> &ProviderConfig { &self.config }

    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        // Delegate to OpenAICompatibleProvider (same API format)
        self.inner.chat_completion(client, messages, tools, model, max_tokens).await
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        let url = self.discovery_url();
        let mut req = client.get(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req.send().await?;
        let body: serde_json::Value = response.json().await?;
        // Ollama /api/tags returns {"models": [{"name": "llama3.1:8b", ...}, ...]}
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
```

- [ ] **Step 2: Commit**

```bash
git add src/provider.rs
git commit -m "feat: add OllamaProvider with /api/tags discovery"
```

---

### Task 6: Refactor LlmClient to use ProviderRegistry

**Files:**
- Modify: `src/llm.rs`

- [ ] **Step 1: Change LlmClient struct**

```rust
pub struct LlmClient {
    pub client: reqwest::Client,
    pub registry: Arc<crate::provider::ProviderRegistry>,
}
```

- [ ] **Step 2: Update constructor**

```rust
impl LlmClient {
    pub fn new(registry: Arc<crate::provider::ProviderRegistry>) -> Self {
        Self {
            client: reqwest::Client::new(),
            registry,
        }
    }
}
```

- [ ] **Step 3: Rewrite chat_completion_with_model as a routing method**

```rust
    pub async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
    ) -> Result<ChatCompletion> {
        let (provider, actual_model) = self.registry.resolve_model(model);
        let max_tokens = provider.config().max_tokens;

        let mut completion = provider
            .chat_completion(&self.client, messages, tools, actual_model, max_tokens)
            .await?;

        // Preserve the original model string in metadata
        completion.model = model.to_string();
        Ok(completion)
    }
```

- [ ] **Step 4: Keep `chat()` as a convenience, remove `chat_with_model()` and `chat_completion()`**

Keep `chat()` as a thin wrapper for backward compat (e.g., soul reflection in agent.rs):
```rust
/// Convenience wrapper that uses the default provider's default model.
pub async fn chat(
    &self,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
) -> Result<ChatMessage> {
    let default_model = self.registry.default_provider_name().to_string();
    self.chat_completion_with_model(messages, tools, &default_model)
        .await
        .map(|c| c.message)
}
```

Remove these methods (no callers):
- `chat_with_model()` — unused after refactor
- `chat_completion()` — unused after refactor

- [ ] **Step 5: Update soul-reflection caller in agent.rs**

```rust
// Old (agent.rs ~line 1059):
// agent.llm.chat(&reflection_messages, &[]).await
// New:
agent.llm.chat_completion_with_model(
    &reflection_messages, &[],
    &*agent.current_model.read().await
).await.map(|c| c.message)
```

- [ ] **Step 6: Remove fetch_models() from LlmClient** (moved to OpenRouterProvider.list_models())

- [ ] **Step 7: Build**

```bash
cargo check --lib
```
Expected: Compilation succeeds (with the Agent changes still pending, but lib check covers llm.rs changes).

- [ ] **Step 8: Commit**

```bash
git add src/llm.rs src/agent.rs
git commit -m "refactor: LlmClient routes through ProviderRegistry, removed direct config dependency"
```

---

### Task 7: Update Agent for registry, fallback, and per-provider vision

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add registry field to Agent struct**

```rust
pub struct Agent {
    pub llm: LlmClient,
    pub registry: Arc<crate::provider::ProviderRegistry>,
    pub config: Config,
    // ... existing fields unchanged
}
```

- [ ] **Step 2: Update Agent::new() to receive and store registry**

```rust
pub fn new(
    config: Config,
    registry: Arc<crate::provider::ProviderRegistry>,
    mcp: McpManager,
    memory: MemoryStore,
    // ... other params unchanged
) -> Self {
    let llm = LlmClient::new(registry.clone());
    let initial_model = format!("{}/{}",
        registry.default_provider_name(),
        // get default model from default provider
        registry.resolve_model(&registry.default_provider_name()).0.default_model()
    );
    Self {
        llm,
        registry,
        config,
        // ...
        current_model: tokio::sync::RwLock::new(initial_model),
        // ...
    }
}
```

Note: The initial model string is `<default_provider>/<default_model>`, e.g. `"openrouter/moonshotai/kimi-k2.6"`.

- [ ] **Step 3: Update set_model() validation**

```rust
pub async fn set_model(&self, model_id: &str) -> anyhow::Result<()> {
    if model_id.is_empty() {
        anyhow::bail!("Model ID cannot be empty");
    }

    // Validate: resolve succeeds for any string, but warn on unknown prefix
    let (provider, actual_model) = self.registry.resolve_model(model_id);
    // Check if user explicitly specified a prefix that doesn't exist
    if let Some((prefix, _)) = model_id.split_once('/') {
        if self.registry.get_provider(prefix).is_none() {
            tracing::warn!(
                "Model '{}': prefix '{}' does not match any known provider \
                 (falling through to default '{}')",
                model_id, prefix, self.registry.default_provider_name()
            );
        }
    }

    // Persist to config.toml
    let content = tokio::fs::read_to_string(&self.config_path).await?;
    let mut doc: toml::value::Table = toml::from_str(&content)?;

    // Determine which TOML section to update based on the provider name
    let provider_name = provider.name().to_string();

    // For the legacy [openrouter] section:
    if provider_name == "openrouter" && doc.contains_key("openrouter") {
        if let Some(table) = doc.get_mut("openrouter").and_then(|v| v.as_table_mut()) {
            table.insert("model".to_string(), toml::Value::String(actual_model.to_string()));
        }
    } else {
        // For [[provider]] entries, find the right one by name in the array
        if let Some(provider_array) = doc.get_mut("provider").and_then(|v| v.as_array_mut()) {
            for entry in provider_array.iter_mut() {
                if let Some(table) = entry.as_table_mut() {
                    if table.get("name").and_then(|v| v.as_str()) == Some(&provider_name) {
                        table.insert(
                            "model".to_string(),
                            toml::Value::String(actual_model.to_string()),
                        );
                    }
                }
            }
        }
    }

    let new_content = toml::to_string_pretty(&doc)?;
    tokio::fs::write(&self.config_path, &new_content).await?;

    let mut current = self.current_model.write().await;
    *current = model_id.to_string();

    tracing::info!(model = %model_id, provider = %provider_name, "Model changed and persisted");
    Ok(())
}
```

- [ ] **Step 4: Add fallback chain to process_message main loop**

Wrap the LLM call in the main agent loop (find the `chat_completion_with_model` call in `process_message`) with fallback logic:

```rust
let model = self.current_model.read().await.clone();
let fallback_chain = &self.config.fallback.chain;
let mut last_error = None;

for attempt in 0..=fallback_chain.len() {
    let current_model = if attempt == 0 {
        model.clone()
    } else {
        fallback_chain[attempt - 1].clone()
    };

    match self.llm.chat_completion_with_model(
        &prompt.messages, &all_tools, &current_model
    ).await {
        Ok(c) => {
            // If we used a fallback, update current_model in memory (not disk)
            if attempt > 0 {
                tracing::info!(
                    "Fallback succeeded: switched from '{}' to '{}'",
                    model, current_model
                );
                *self.current_model.write().await = current_model.clone();
            }
            completion = c;
            break;
        }
        Err(e) => {
            tracing::warn!("Model '{}' failed (attempt {}/{}): {}",
                current_model, attempt, fallback_chain.len(), e);
            last_error = Some(e);
            if attempt == fallback_chain.len() {
                // All attempts exhausted — return the last error
                return Err(last_error.unwrap());
            }
            continue;
        }
    }
}
```

- [ ] **Step 5: Update per-provider vision check in agent.rs and file_processor**

**agent.rs** — Replace `self.config.openrouter.supports_vision` references in `process_message`:

Find the line in agent.rs that checks `supports_vision` (for deciding image vs OCR path). Replace:

```rust
// Old: if self.config.openrouter.supports_vision {
// New:
let current = self.current_model.read().await;
let (provider, _) = self.registry.resolve_model(&current);
if provider.supports_vision() {
```

**file_processor/mod.rs** — The `process_attachments` function currently reads `config.openrouter.supports_vision` directly (line 40). Change it to accept the flag as a parameter:

```rust
// Old signature:
pub async fn process_attachments(
    attachments: &[Attachment],
    user_query: &str,
    config: &Config,
    memory: &MemoryStore,
) -> (String, Vec<ContentPart>) {

// Change the call site in agent.rs to pass the active provider's supports_vision:
let supports_vision = {
    let current = self.current_model.read().await;
    let (provider, _) = self.registry.resolve_model(&current);
    provider.supports_vision()
};

// Old: config.openrouter.supports_vision,
// New: supports_vision,
```

Update `process_attachments` signature to accept `supports_vision: bool` instead of reading from config:

```rust
pub async fn process_attachments(
    attachments: &[Attachment],
    user_query: &str,
    config: &Config,
    memory: &MemoryStore,
    supports_vision: bool,  // <-- new parameter, from active provider
) -> (String, Vec<ContentPart>) {
```

Replace line 40:
```rust
// Old:
config.openrouter.supports_vision,
// New:
supports_vision,
```
```rust
// Old: if self.config.openrouter.supports_vision {
// New:
let current = self.current_model.read().await;
let (provider, _) = self.registry.resolve_model(&current);
if provider.supports_vision() {
```

- [ ] **Step 6: Build and fix compilation errors**

```bash
cargo check
```
Expected: Compilation succeeds.

- [ ] **Step 7: Run existing tests**

```bash
cargo test --lib
```
Expected: All existing tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs
git commit -m "feat: integrate ProviderRegistry into Agent with fallback chain and per-provider vision"
```

---

### Task 8: Wire providers in main.rs at startup

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Build registry from config and pass to Agent**

After config is loaded (after `config.load()`), add:

```rust
use crate::provider;

// Build provider registry
let (provider_sections, default_provider, fallback_chain) = config.build_providers();
let registry = Arc::new(provider::build_registry(&provider_sections, &default_provider)
    .context("Failed to build LLM provider registry")?);
```

Then pass `registry.clone()` to `Agent::new()`:

```rust
let agent = Arc::new_cyclic(|weak| {
    Agent::new(
        config,
        registry.clone(),
        // ... existing params unchanged
    )
});
```

- [ ] **Step 2: Remove the old LlmClient::new creation from Agent::new** (it's now created inside Agent::new from the registry)

- [ ] **Step 3: Build**

```bash
cargo check
```
Expected: Compilation succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire ProviderRegistry startup in main.rs"
```

---

### Task 9: Redesign `/models` Telegram command with multi-step provider→model selection

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Update `/models` handler for provider selection**

Replace the current `/models` command handler (around line 672) with:

```rust
"models" => {
    let registry = &agent.registry;
    let providers = registry.provider_names();

    if providers.len() == 1 {
        // Single provider: jump straight to model search for it
        let provider_name = &providers[0];
        let provider = registry.get_provider(provider_name).unwrap();
        return handle_provider_model_select(
            bot, msg.chat.id, agent, provider_name, provider
        ).await;
    }

    // Multiple providers: show inline keyboard
    let current = agent.current_model.read().await;
    let reply = format!(
        "Active model: `{}`\n\nSelect a provider:",
        *current
    );
    use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = providers.iter()
        .map(|name| {
            vec![InlineKeyboardButton::callback(
                name.clone(),
                format!("provider_select:{}", name),
            )]
        })
        .collect();
    keyboard.push(vec![InlineKeyboardButton::callback(
        "❌ Cancel", "model_select:cancel",
    )]);

    bot.send_message(msg.chat.id, &reply)
        .reply_markup(InlineKeyboardMarkup::new(keyboard))
        .await?;
    return Ok(());
}
```

- [ ] **Step 2: Add provider_select callback in handle_model_callback**

`handle_model_callback` receives `CallbackQuery`, not `Message`. The message is accessed via `q.regular_message()`. Add before the existing `model_select:` check:

```rust
if let Some(provider_name) = data.strip_prefix("provider_select:") {
    if let Some(provider) = agent.registry.get_provider(provider_name) {
        let msg = q.regular_message().cloned();
        if let Some(ref m) = msg {
            return handle_provider_model_select(
                bot, m.chat.id, agent, provider_name, provider
            ).await;
        }
    }
    return Ok(());
}
```

- [ ] **Step 3: Add handle_provider_model_select helper (uses `ChatId`, not `Message`)**

```rust
async fn handle_provider_model_select(
    bot: Bot,
    chat_id: ChatId,
    agent: &Arc<Agent>,
    provider_name: &str,
    provider: &dyn Provider,
) -> ResponseResult<()> {

    if !provider.config().discover_models {
        // No discovery: prompt for text search
        let prompt = format!(
            "Send me a model name or ID to search for on **{}**.\nExamples: {}",
            provider_name,
            provider.default_model()
        );
        bot.send_message(chat_id, &prompt).await?;
        return Ok(());
    }

    // Try to discover models
    match provider.list_models(&agent.llm.client).await {
        Ok(models) if models.len() <= 20 => {
            use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
            let mut keyboard: Vec<Vec<InlineKeyboardButton>> = models.iter()
                .map(|m| {
                    let qualified = format!("{}/{}", provider_name, m);
                    vec![InlineKeyboardButton::callback(
                        m.clone(),
                        format!("model_select:{}", qualified),
                    )]
                })
                .collect();
            keyboard.push(vec![InlineKeyboardButton::callback(
                "🔍 Search all", "model_search_prompt",
            )]);
            keyboard.push(vec![InlineKeyboardButton::callback(
                "❌ Cancel", "model_select:cancel",
            )]);

            let reply = format!("Models on **{}** ({}):", provider_name, models.len());
            bot.send_message(chat_id, &reply)
                .reply_markup(InlineKeyboardMarkup::new(keyboard))
                .await?;
        }
        Ok(models) => {
            // Too many models: prompt for text search
            let prompt = format!(
                "**{}** has {} models available.\nSend me a model name or ID to search for.",
                provider_name,
                models.len()
            );
            bot.send_message(chat_id, &prompt).await?;
        }
        Err(e) => {
            // Discovery failed: prompt for search
            let prompt = format!(
                "Could not load model list from **{}**: {}\nSend a model name or ID directly.",
                provider_name, e
            );
            bot.send_message(chat_id, &prompt).await?;
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Update existing model_select callback to handle qualified IDs**

The existing `model_select:{model_id}` callback now receives qualified IDs like `model_select:ollama/llama3.1:8b`. The `set_model()` call already handles the full qualified string — no change needed to the set_model call itself. Just ensure the callback data format is `model_select:{qualified_id}`.

- [ ] **Step 5: Update the model search flow to be provider-scoped**

When a user types a model name after being prompted for search, `handle_model_search` should store which provider they're searching. Use the existing `model_search_pending_{user_id}` memory key to also store the provider name.

In the search prompt flow, after user selects a provider:
```rust
agent.memory.remember("settings",
    &format!("model_search_provider_{}", user_id),
    provider_name,
    None,
).await.ok();
```

In `handle_message`'s model search path (after the user types a search query), read the stored provider and scope the search:
```rust
// When user types a search query (not a command):
let search_provider = agent.memory.recall("settings",
    &format!("model_search_provider_{}", user_id)
).await.ok().flatten();
// Pass it to handle_model_search which filters by provider
```

- [ ] **Step 6: Build**

```bash
cargo check
```
Expected: Compilation succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat: redesign /models with multi-step provider->model selection via inline keyboards"
```

---

---

### Task 10: Add unit tests for provider system

**Files:**
- Modify: `src/provider.rs` (add `#[cfg(test)] mod tests`)
- Modify: `src/config.rs` (add test for new config types)

- [ ] **Step 1: Add resolve_model tests to provider.rs**

Append at the end of `src/provider.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_registry() -> ProviderRegistry {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "openrouter".to_string(),
            Arc::new(OpenRouterProvider::new(ProviderConfig {
                name: "openrouter".to_string(),
                provider_type: ProviderType::OpenRouter,
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key: Some("sk-test".to_string()),
                default_model: "moonshotai/kimi-k2.6".to_string(),
                supports_vision: false,
                max_tokens: 4096,
                discover_models: false,
            })),
        );
        providers.insert(
            "ollama".to_string(),
            Arc::new(OllamaProvider::new(ProviderConfig {
                name: "ollama".to_string(),
                provider_type: ProviderType::Ollama,
                base_url: "http://localhost:11434/v1".to_string(),
                api_key: None,
                default_model: "llama3.1".to_string(),
                supports_vision: false,
                max_tokens: 4096,
                discover_models: true,
            })),
        );
        ProviderRegistry::new(providers, "openrouter".to_string())
    }

    #[test]
    fn test_resolve_model_explicit_provider() {
        let registry = test_registry();
        let (provider, model) = registry.resolve_model("ollama/llama3");
        assert_eq!(provider.name(), "ollama");
        assert_eq!(model, "llama3");
    }

    #[test]
    fn test_resolve_model_falls_through_to_default() {
        let registry = test_registry();
        // "moonshotai" is not a known provider → falls to default (openrouter)
        let (provider, model) = registry.resolve_model("moonshotai/kimi-k2.6");
        assert_eq!(provider.name(), "openrouter");
        assert_eq!(model, "moonshotai/kimi-k2.6");
    }

    #[test]
    fn test_resolve_model_unknown_prefix_falls_through() {
        let registry = test_registry();
        let (provider, model) = registry.resolve_model("nope/llama3");
        assert_eq!(provider.name(), "openrouter");
        assert_eq!(model, "nope/llama3");
    }

    #[test]
    fn test_resolve_model_no_slash_uses_default() {
        let registry = test_registry();
        let (provider, model) = registry.resolve_model("llama3");
        assert_eq!(provider.name(), "openrouter");
        assert_eq!(model, "llama3");
    }

    #[test]
    fn test_provider_count_and_names() {
        let registry = test_registry();
        assert_eq!(registry.provider_count(), 2);
        let mut names = registry.provider_names();
        names.sort();
        assert_eq!(names, vec!["ollama", "openrouter"]);
    }

    #[test]
    fn test_get_provider_found() {
        let registry = test_registry();
        let p = registry.get_provider("ollama");
        assert!(p.is_some());
        assert_eq!(p.unwrap().name(), "ollama");
    }

    #[test]
    fn test_get_provider_not_found() {
        let registry = test_registry();
        assert!(registry.get_provider("nonexistent").is_none());
    }
}
```

- [ ] **Step 2: Run resolve_model tests**

```bash
cargo test -p rustfox -- provider::tests --nocapture
```
Expected: All 7 tests pass.

- [ ] **Step 3: Add backward-compat config parsing test to config.rs**

```rust
#[test]
fn test_provider_section_parses_ollama() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        model = "moonshotai/kimi-k2.6"
        [[provider]]
        name = "ollama"
        type = "ollama"
        base_url = "http://localhost:11434/v1"
        model = "llama3.1"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.provider.len(), 1);
    assert_eq!(cfg.provider[0].name, "ollama");
    assert_eq!(cfg.provider[0].model, "llama3.1");
}

#[test]
fn test_legacy_openrouter_auto_creates_provider() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        model = "moonshotai/kimi-k2.6"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let (providers, default_name, _) = cfg.build_providers();
    assert!(providers.iter().any(|p| p.name == "openrouter"));
    assert_eq!(default_name, "openrouter");
}

#[test]
fn test_visible_provider_overrides_legacy() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "old-key"
        model = "moonshotai/kimi-k2.6"
        [[provider]]
        name = "openrouter"
        type = "openrouter"
        base_url = "https://openrouter.ai/api/v1"
        api_key = "new-key"
        model = "anthropic/claude-sonnet-4-6"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let (providers, _default, _) = cfg.build_providers();
    // Should not have duplicate openrouter providers
    let or_count = providers.iter().filter(|p| p.name == "openrouter").count();
    assert_eq!(or_count, 1, "should not duplicate openrouter when explicit [[provider]] exists");
    let or = providers.iter().find(|p| p.name == "openrouter").unwrap();
    assert_eq!(or.api_key.as_deref(), Some("new-key"), "explicit [[provider]] should win");
}

#[test]
fn test_fallback_config_parses() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        [fallback]
        chain = ["openrouter/model-a", "ollama/model-b"]
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.fallback.chain.len(), 2);
    assert_eq!(cfg.fallback.chain[0], "openrouter/model-a");
}

#[test]
fn test_fallback_defaults_empty() {
    let toml = r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(cfg.fallback.chain.is_empty());
}
```

- [ ] **Step 4: Run config tests**

```bash
cargo test -p rustfox -- config::tests --nocapture
```
Expected: All new and existing config tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/provider.rs src/config.rs
git commit -m "test: add unit tests for ProviderRegistry, config parsing, backward compat"
```

---

### Task 11: Update config.example.toml with new provider sections

**Files:**
- Modify: `config.example.toml`

- [ ] **Step 1: Add [[provider]] and [fallback] documentation**

Add after the existing `[openrouter]` example section in `config.example.toml`:

```toml
# ── Multi-Provider Configuration (optional) ────────────────────────────
# Define additional LLM providers alongside OpenRouter.
# Each [[provider]] block adds a new provider. Model strings use the
# format `provider_name/model_id` (e.g., `ollama/llama3.1`).
# The first provider becomes the default.

# Example: Ollama (local models)
# [[provider]]
# name = "ollama"
# type = "ollama"
# base_url = "http://localhost:11434/v1"
# model = "llama3.1"
# discover_models = true

# Example: Any OpenAI-compatible endpoint (LM Studio, vLLM, llama.cpp, etc.)
# [[provider]]
# name = "lmstudio"
# type = "openai_compatible"
# base_url = "http://localhost:1234/v1"
# model = "qwen2.5-7b-instruct"

# ── Fallback Chain (optional) ──────────────────────────────────────────
# When the primary model fails, RustFox tries each fallback in order.
# Each entry is a full provider-prefixed model string.
# [fallback]
# chain = [
#     "openrouter/moonshotai/kimi-k2.6",
#     "ollama/llama3.1",
#     "lmstudio/qwen2.5-7b-instruct",
# ]
```

- [ ] **Step 2: Commit**

```bash
git add config.example.toml
git commit -m "docs: add [[provider]] and [fallback] examples to config.example.toml"
```

---

### Self-Review Checklist

- **Spec coverage:** Every section of the design spec has a corresponding task (including §7 config.example.toml → Task 11):
  - §3.1 Provider Trait → Task 2
  - §3.2 Provider Types → Tasks 3, 4, 5
  - §3.3 ProviderConfig → Task 2
  - §3.4 ProviderRegistry → Task 2
  - §3.5 LlmClient → Task 6
  - §3.6 Fallback → Task 7 (step 4)
  - §4 Config → Task 1
  - §5 Agent → Task 7
  - §6 /models → Task 9
  - §7 File changes → covered
  - §8 Error handling → covered inline
  - §9 Testing → next task
- **Placeholder scan:** No TODOs or placeholders remain. All code blocks are complete.
- **Type consistency:** `ProviderConfig` defined in Task 2, used in Tasks 3/4/5/6/7. `ProviderRegistry::resolve_model()` returns `(&dyn Provider, &str)`, used consistently.
