# Multi-Provider LLM Architecture — Design Spec

**Date:** 2026-06-22
**Status:** Draft
**Author:** RustFox Brainstorming Session

## 1. Problem

RustFox currently supports a single LLM provider (OpenRouter) via `[openrouter]` config.
This creates three limitations:

1. **No fallback** — if OpenRouter is down, the bot is unusable
2. **No local models** — users cannot use Ollama, LM Studio, llama.cpp, or vLLM
3. **No provider mixing** — subagents always use the same endpoint as the main agent

## 2. Goals

- Support multiple LLM providers simultaneously (OpenRouter + local models)
- Each provider connects via an OpenAI-compatible `/chat/completions` endpoint
- Fallback chains: on provider failure, try the next in the chain
- Subagents can use any provider independently (e.g., main agent on Kimi, subagent on Ollama)
- Auto-discovery of local model lists where possible
- Backward compatible — existing `[openrouter]` config continues to work
- `/models` command gains multi-step provider→model selection via inline keyboards

## 3. Architecture

### 3.1 Provider Trait

New module `src/provider.rs` defines the core abstraction:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn supports_vision(&self) -> bool;
    fn config(&self) -> &ProviderConfig;

    /// Send a chat completion request to this provider's endpoint.
    /// `model` is already stripped of the provider prefix.
    async fn chat_completion(
        &self,
        client: &reqwest::Client,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
    ) -> Result<ChatCompletion>;

    /// Return available model IDs. Used by /models command.
    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>>;
}
```

### 3.2 Provider Types

Three concrete implementations, all in `src/provider.rs`:

| Provider | `list_models()` | Special behavior |
|---|---|---|
| `OpenRouterProvider` | `GET /v1/models` | Parameter sanitization, Kimi tool-call fallback |
| `OpenAICompatibleProvider` | `GET /v1/models` | Pure OpenAI protocol, `api_key` optional |
| `OllamaProvider` | `GET /api/tags` | Delegates chat to OpenAICompatibleProvider, custom discovery |

`OpenAICompatibleProvider` handles LM Studio, llama.cpp, vLLM, Together AI, etc. —
any service exposing `/v1/chat/completions` with the OpenAI schema.

`OllamaProvider` delegates its `chat_completion()` to an inner `OpenAICompatibleProvider`
(since Ollama's `/v1/chat/completions` is identical to the OpenAI protocol), but has its
own `list_models()` that calls `GET /api/tags` with a modified base URL path.
The `base_url` is stored as the full OpenAI-compatible path (e.g. `http://localhost:11434/v1`);
the Ollama provider strips a trailing `/v1`, `/v2`, or trailing slash then appends `/api/tags`
for discovery. If no versioned prefix is found, it appends `/api/tags` directly.
(e.g., `http://localhost:11434` → `http://localhost:11434/api/tags`).

### 3.3 ProviderConfig

Deserialized from TOML, then used to construct a concrete Provider:

```rust
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,              // "ollama", "openrouter", etc.
    pub provider_type: ProviderType,
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
    pub supports_vision: bool,
    pub max_tokens: u32,
    pub discover_models: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderType {
    OpenRouter,
    OpenAICompatible,
    Ollama,
}
```

### 3.4 ProviderRegistry

Holds all configured providers and resolves model strings:

```rust
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    default_provider: String,
}

impl ProviderRegistry {
    /// "ollama/llama3" → (ollama_provider, "llama3")
    /// "moonshotai/kimi-k2.6" → (openrouter_provider, "moonshotai/kimi-k2.6")
    ///   (no provider named "moonshotai" → falls through to default)
    /// "llama3" (no slash) → (default_provider, "llama3")
    /// "nope/llama3" (unknown prefix) → (default_provider, "nope/llama3")
    pub fn resolve_model(&self, model: &str) -> (&dyn Provider, &str) { ... }

    pub fn get_provider(&self, name: &str) -> Option<&dyn Provider> { ... }
    pub fn providers(&self) -> impl Iterator<Item = &dyn Provider> { ... }
    pub fn default_provider_name(&self) -> &str { ... }
}
```

Model string resolution algorithm:
1. Split on first `/`
2. If first segment matches a known provider name → use that provider, rest is model
3. If no match → use default provider, full string is model

This is backward compatible: `moonshotai/kimi-k2.6` with no provider named `moonshotai`
falls through to the default provider (openrouter) and sends `moonshotai/kimi-k2.6` as the
model name — exactly as today.

### 3.5 LlmClient Changes

LlmClient becomes a thin routing layer:

```rust
pub struct LlmClient {
    client: reqwest::Client,
    registry: Arc<ProviderRegistry>,
}

impl LlmClient {
    pub async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,          // "ollama/llama3" or "moonshotai/kimi-k2.6"
    ) -> Result<ChatCompletion> {
        let (provider, actual_model) = self.registry.resolve_model(model);
        let max_tokens = provider.config().max_tokens;
        provider.chat_completion(
            &self.client, messages, tools, actual_model, max_tokens
        ).await
            .map(|mut completion| {
                completion.model = model.to_string();
                completion
            })
    }

    pub async fn list_all_models(&self) -> Vec<ProviderModel> {
        // Iterate all providers, call list_models() for discoverable ones
    }
}

// --- Migration of existing API ---
// The old `chat_completion(&self, messages, tools)` method (no model param) is **removed**.
// It was only used internally; all callers now pass the model explicitly.
// `chat(&self, messages, tools)` stays as a convenience that reads from
// `self.registry.default_provider` for the model string.
// `chat_with_model()` is replaced by `chat_completion_with_model()`.
// `fetch_models()` becomes provider-specific: call `registry.get_provider("openrouter").list_models()`.

/// A model as shown in the /models command, prefixed with its provider name.
pub struct ProviderModel {
    pub provider: String,
    pub model_id: String,          // e.g. "llama3.1:8b"
    pub qualified_id: String,      // e.g. "ollama/llama3.1:8b"
    pub description: String,       // optional human-readable name from the provider
                                   // OpenRouter: from ModelInfo.name
                                   // Ollama: from /api/tags response name
                                   // OpenAICompatible: from /v1/models name or empty
}
```

### 3.6 Fallback Chain

Fallback lives in the Agent loop, not LlmClient. The agent's main loop and subagent loop
already have retry logic — we extend it to try different models on failure:

```rust
// In process_message main loop:
let fallback_chain = &self.config.fallback.chain; // Vec<String>

for attempt in 0..=fallback_chain.len() {
    let model = if attempt == 0 {
        self.current_model.read().await.clone()
    } else {
        fallback_chain[attempt - 1].clone()
    };

    match self.llm.chat_completion_with_model(&prompt, &all_tools, &model).await {
        Ok(completion) => { /* handle success */ break; }
        Err(e) => {
            warn!("Model '{}' failed: {}", model, e);
            if attempt == fallback_chain.len() {
                return Err(e); // all exhausted
            }
            continue; // try next fallback
        }
    }
}
```

Each model in the fallback chain is a full provider-prefixed string like
`"ollama/llama3.1"` — it resolves through `resolve_model` to whatever provider
owns that model. The same logic works for all providers.

**After successful fallback**, the agent updates `current_model` to the working
fallback model in memory (for subsequent turns in the same conversation) but does
NOT persist to config.toml. The user can explicitly persist the working model
via `/model <id>` if desired.

**Fallback applies only to the main agent loop**, not to subagent loops. Subagents
have an explicit model override (e.g., `model: "ollama/llama3"`) — if that provider
fails, the error propagates to the main agent, which can decide to retry the subagent
with a different model. This keeps subagent behavior predictable and avoids
surprising fallback within a delegated task.

## 4. Config Changes

### 4.1 New Config Struct Fields

Add to `Config` in `src/config.rs`:

```rust
#[serde(default)]
pub provider: Vec<ProviderSection>,

#[serde(default)]
pub fallback: FallbackConfig,
```

```rust
pub struct ProviderSection {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,           // "openrouter", "openai_compatible", "ollama"
                                         // Unrecognized types cause a startup error
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

pub struct FallbackConfig {
    #[serde(default)]
    pub chain: Vec<String>,
}
```

### 4.2 Backward Compatibility

At startup, if the legacy `[openrouter]` section exists, it auto-creates a provider
named `"openrouter"` with those settings. The default provider is `"openrouter"` if
it exists, otherwise the first configured provider.

If both `[openrouter]` AND `[[provider]] name = "openrouter"` are present, the explicit
`[[provider]]` entry wins and the legacy section is ignored (with a warning).
`set_model()` with an openrouter-prefixed model always modifies the same section that
was used to create the provider — there is no split-state path.

### 4.3 Example config.toml

```toml
# Legacy section — auto-creates "openrouter" provider (backward compat)
[openrouter]
api_key = "sk-..."
model = "moonshotai/kimi-k2.6"

# Local Ollama (auto-discover models)
[[provider]]
name = "ollama"
type = "ollama"
base_url = "http://localhost:11434/v1"
model = "llama3.1"
discover_models = true

# LM Studio (no api_key needed for local)
[[provider]]
name = "lmstudio"
type = "openai_compatible"
base_url = "http://localhost:1234/v1"
model = "qwen2.5-7b-instruct"

# Fallback chain
[fallback]
chain = [
    "openrouter/moonshotai/kimi-k2.6",
    "ollama/llama3.1",
    "lmstudio/qwen2.5-7b-instruct",
]
```

## 5. Agent Integration

### 5.1 Agent Constructor

`Agent::new()` receives `Arc<ProviderRegistry>` and `LlmClient`:

```rust
let llm = LlmClient::new(registry.clone());
Self {
    llm,
    registry,
    current_model: RwLock::new(initial_model),  // full provider-prefixed string
    ...
}
```

### 5.2 set_model()

`set_model()` now:
1. Resolves the model string via `registry.resolve_model()` — always succeeds.
   Additionally checks: if the model string contains a `/` but the prefix doesn't match
   any known provider, log a warning (possible typo, but could be a valid model ID
   like `moonshotai/kimi-k2.6` — never block the change).
2. Persists the full provider-prefixed model string (e.g., `"ollama/llama3.1"`)
3. Persists to config.toml as the `model` field of the corresponding provider section
   (or the `[openrouter]` model if that's the provider)

Config persistence follows the same approach as the current `set_model()`:
read TOML via `toml::from_str`, modify the `model` field in the right section,
write back via `toml::to_string_pretty`. This does NOT preserve comments/formatting
(consistent with existing behavior). If round-trip preservation is needed later,
a separate migration to `toml_edit` would cover all config writes at once.

Model selection via `set_model()` persists the model to disk so it survives restarts.
At runtime, `current_model` is per-Agent (the bot has one agent instance, so it
appears "global" — affecting all conversations in that process). This is consistent
with the current behavior where model changes affect all users until the next restart.

### 5.3 Subagent Model Resolution

Subagent `model` overrides are full provider-prefixed strings like `"ollama/llama3"`.
The same `resolve_model()` path handles them — no special subagent logic needed.

When a subagent is invoked via `invoke_agent` with `model: "ollama/llama3"`:
- The subagent loop calls `self.llm.chat_completion_with_model(..., "ollama/llama3")`
- LlmClient resolves to the `ollama` provider with model `llama3`
- The subagent runs entirely against that provider

### 5.4 Per-Provider Vision

The `supports_vision` flag moves from `config.openrouter.supports_vision` to
per-provider `ProviderConfig`. The agent checks this via the active provider:

```rust
// In process_message, when deciding whether to send images or OCR:
let current = self.current_model.read().await;
let (provider, _) = self.registry.resolve_model(&current);
if provider.config().supports_vision {
    // Send base64 images as content parts
} else {
    // OCR the images
}
```

## 6. `/models` Command Redesign

### 6.1 Interactive Flow

**Step 1 — `/models` with no args:**
If only one provider → jump directly to Step 2 for that provider.
If multiple providers → show inline keyboard:

```
Bot: Active model: openrouter/moonshotai/kimi-k2.6
     Select a provider:
     [ollama] [openrouter] [lmstudio]
     [❌ Cancel]
```

Callback format: `provider_select:{name}`

**Step 2 — Provider selected:**

*If provider has `discover_models=true` AND <= 20 models (e.g., Ollama):*
```
Bot: Models on ollama (http://localhost:11434/v1):
     [llama3.1:8b]  [mistral:7b]  [qwen2.5:7b]
     [🔍 Search all]  [❌ Cancel]
```

*If provider has no discovery OR 300+ models (e.g., OpenRouter):*
```
Bot: Models on openrouter (300+ available).
     Send me a model name or ID to search for.
     Examples: kimi, claude, gpt-4, gemini
```

**Step 3 — Model selection:**
Inline keyboard tap or text search → `agent.set_model("ollama/llama3.1:8b")`

```
Bot: ✅ Model changed to "ollama/llama3.1:8b"
```

### 6.2 Implementation

- New callback prefix `provider_select:{name}` handled in `handle_model_callback()`
- Existing `model_select:{id}` stores the full provider-prefixed string
- Search in `handle_model_search()` is scoped to the selected provider
- State tracked via `model_search_pending_{user_id}` includes the provider name
- `model_select:cancel` reverts to the provider picker

## 7. File Changes Summary

### New Files
| File | Purpose |
|---|---|
| `src/provider.rs` | `Provider` trait, `ProviderRegistry`, three impls |

### Modified Files
| File | Changes |
|---|---|
| `src/config.rs` | Add `ProviderSection`, `FallbackConfig`; backward compat builder |
| `src/llm.rs` | `LlmClient` takes `Arc<ProviderRegistry>`, routing logic |
| `src/agent.rs` | Receive registry, fallback loop, subagent model resolution |
| `src/main.rs` | Build registry from config, pass to agent |
| `src/platform/telegram.rs` | `/models` multi-step provider→model selection |
| `config.example.toml` | Document `[[provider]]` and `[fallback]` sections |

## 8. Error Handling

- **Unknown provider in model string**: `resolve_model()` always falls through to the default provider. `set_model()` may optionally warn about unrecognized prefixes as a typo check
- **Unknown `type` in config**: Startup error — unrecognized provider type is a hard config error
- **Provider connection failure**: Logged, fallback chain triggered
- **All fallbacks exhausted**: Error propagated to user as today
- **Auto-discovery failure**: Logged, provider still usable with its `default_model`
- **Vision on non-vision provider**: Falls back to OCR as today (per-provider `supports_vision` flag)

## 9. Testing

- `ProviderRegistry::resolve_model()` — various model string formats
- Fallback chain iteration
- Provider chat_completion serialization (each provider type)
- Callback handler parsing for new `provider_select:` prefix
- Backward compat config parsing

## 10. Non-Goals

- Rate limiting per provider (out of scope)
- Cost tracking per provider (out of scope)
- Dynamic provider discovery via mDNS (out of scope)
- Provider health checks / circuit breaker (future improvement)
