use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::{ProviderSection, ProviderType};
use crate::llm::internal::{ChatResponse, Choice};
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
    pub context_window: usize,
    /// Runtime cache for the current model's context window, populated
    /// asynchronously from the provider API. When None, falls back to
    /// `context_window`.
    pub context_window_cache: Arc<RwLock<Option<usize>>>,
    /// Number of retries when the response is missing the `choices` field.
    /// Defaults to 3; set to 0 to disable.
    pub parse_retry_limit: u32,
    /// Number of retries when the provider answers 429 / 5xx (rate limit or
    /// transient upstream failure). Honours `Retry-After` when present.
    /// Defaults to 3; set to 0 to disable (fail fast, pre-429-retry behaviour).
    pub rate_limit_retry_limit: u32,
    /// Extra HTTP headers on chat (and OpenCode Go models list). Used for
    /// OpenCode Go `x-opencode-session` (#271); empty for other providers.
    pub extra_headers: Vec<(String, String)>,
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
            parse_retry_limit: 0,      // overwritten by build_registry
            rate_limit_retry_limit: 0, // overwritten by build_registry
            extra_headers: vec![],
        }
    }
}

/// Typed HTTP failure from a provider's chat-completion call.
///
/// Carrying the status as *data* (ADR-0012) lets upper layers — the fallback
/// chain in [`crate::llm::LlmClient`] and the dead-letter queue in the job
/// runner (ADR-0013) — classify by downcast instead of string-matching the
/// error message. Display is byte-identical to the pre-existing
/// `"{provider} API error ({status}): {body}"` so log greps, existing tests,
/// and the dream-audit heuristics all keep working.
#[derive(Debug, Clone)]
pub struct LlmHttpError {
    pub provider: String,
    pub status: reqwest::StatusCode,
    pub body: String,
}

impl std::fmt::Display for LlmHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} API error ({}): {}",
            self.provider, self.status, self.body
        )
    }
}

impl std::error::Error for LlmHttpError {}

impl LlmHttpError {
    /// Transient for *same-model* ADR-0009 retry / job dead-letter: top-level
    /// 429 or any 5xx. Envelope inner codes do **not** expand this (ADR-0019:
    /// parse is observability + helpers; walk is gated separately).
    pub fn is_transient(&self) -> bool {
        self.status == reqwest::StatusCode::TOO_MANY_REQUESTS || self.status.is_server_error()
    }

    /// Hard auth/permission exclusions — never walk `[fallback] chain`
    /// (ADR-0019). 402 credits is intentionally *not* here.
    pub fn is_auth_hard_exclusion(&self) -> bool {
        self.status == reqwest::StatusCode::UNAUTHORIZED
            || self.status == reqwest::StatusCode::FORBIDDEN
    }

    /// OpenRouter-shaped body insight (numeric codes / documented `error_type`).
    pub fn envelope_insight(&self) -> Option<OpenRouterEnvelopeInsight> {
        parse_openrouter_envelope(&self.body)
    }
}

/// Pre-HTTP failure (missing provider key, invalid client config that aborts
/// before a request is sent). Hard-excludes fallback walk (ADR-0019).
#[derive(Debug, Clone)]
pub struct LlmPreHttpError {
    pub message: String,
}

impl std::fmt::Display for LlmPreHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LlmPreHttpError {}

/// Classify a bubbled error: does its cause chain contain a transient LLM HTTP
/// failure? Used by the job runner to decide dead-letter queueing (ADR-0013).
pub fn is_transient_llm_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LlmHttpError>()
        .is_some_and(|e| e.is_transient())
}

/// Whether `[fallback] chain` should try the next model after this error
/// (ADR-0019). Walks on almost everything; hard-excludes only HTTP 401/403
/// and [`LlmPreHttpError`] (never-sent HTTP).
pub fn should_walk_fallback(err: &anyhow::Error) -> bool {
    if err.downcast_ref::<LlmPreHttpError>().is_some() {
        return false;
    }
    // ADR-0043: detect-only / peer-not-configured must not silent-hop.
    if err
        .downcast_ref::<crate::opencode::OpenCodeDetectOnlyError>()
        .is_some()
    {
        return false;
    }
    // ADR-0044: misconfigured xAI must not silent-hop to another provider type.
    if err
        .downcast_ref::<crate::xai::XaiMisconfiguredError>()
        .is_some()
    {
        return false;
    }
    if let Some(http) = err.downcast_ref::<LlmHttpError>() {
        if http.is_auth_hard_exclusion() {
            return false;
        }
        // Leave-Go (#267 / ADR-0043 §7): OpenCode plain short 429 stays put;
        // 402 and 429+GoUsageLimitError/Policy walk to OpenRouter DeepSeek.
        if is_opencode_http_provider(&http.provider)
            && http.status == reqwest::StatusCode::TOO_MANY_REQUESTS
            && !crate::opencode::is_go_leave_signal(&http.body)
        {
            return false;
        }
        return true;
    }
    // Network / timeout / DNS / JSON-or-body parse / missing-choices exhaustion.
    true
}

/// Provider names that mean OpenCode Go peer (registered name or reserved aliases).
fn is_opencode_http_provider(name: &str) -> bool {
    name == "opencode" || OPENCODE_MODEL_ALIASES.contains(&name)
}

/// Documented OpenRouter `error_type` values that mean upstream transient
/// (rate limit / overload / unavailable / timeout / server). No free-text
/// "rate limit" guessing — exact token match only (ADR-0019).
const TRANSIENT_ERROR_TYPES: &[&str] = &[
    "rate_limit_exceeded",
    "provider_overloaded",
    "provider_unavailable",
    "timeout",
    "server",
];

/// Parsed OpenRouter error envelope for logs + typed inner classification.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenRouterEnvelopeInsight {
    pub inner_codes: Vec<u16>,
    pub error_types: Vec<String>,
}

impl OpenRouterEnvelopeInsight {
    /// True when any collected numeric code is 429/5xx **or** any collected
    /// `error_type` is in the documented transient set.
    pub fn has_transient_inner(&self) -> bool {
        self.inner_codes
            .iter()
            .any(|&c| c == 429 || (500..600).contains(&c))
            || self
                .error_types
                .iter()
                .any(|t| TRANSIENT_ERROR_TYPES.contains(&t.as_str()))
    }
}

fn push_code(out: &mut Vec<u16>, v: &serde_json::Value) {
    if let Some(n) = v.as_u64() {
        if n <= u16::MAX as u64 {
            out.push(n as u16);
        }
    } else if let Some(s) = v.as_str() {
        if let Ok(n) = s.parse::<u16>() {
            out.push(n);
        }
    }
}

fn push_error_type(out: &mut Vec<String>, v: &serde_json::Value) {
    if let Some(s) = v.as_str() {
        if !s.is_empty() && !out.iter().any(|e| e == s) {
            out.push(s.to_string());
        }
    }
}

fn collect_from_error_obj(insight: &mut OpenRouterEnvelopeInsight, err: &serde_json::Value) {
    if let Some(code) = err.get("code") {
        push_code(&mut insight.inner_codes, code);
    }
    if let Some(et) = err.get("error_type") {
        push_error_type(&mut insight.error_types, et);
    }
    if let Some(meta) = err.get("metadata") {
        if let Some(et) = meta.get("error_type") {
            push_error_type(&mut insight.error_types, et);
        }
        // Some providers nest a numeric code under metadata.
        if let Some(code) = meta.get("code") {
            push_code(&mut insight.inner_codes, code);
        }
    }
}

/// Parse an OpenRouter-shaped error body: top-level `error`, `previous_errors[*]`,
/// and `metadata` / `error_type`. Returns `None` when the body is not JSON object
/// shaped enough to inspect (callers keep the raw HTTP status).
pub fn parse_openrouter_envelope(body: &str) -> Option<OpenRouterEnvelopeInsight> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let obj = value.as_object()?;
    let mut insight = OpenRouterEnvelopeInsight::default();

    if let Some(err) = obj.get("error") {
        collect_from_error_obj(&mut insight, err);
    }
    if let Some(et) = obj.get("error_type") {
        push_error_type(&mut insight.error_types, et);
    }
    if let Some(meta) = obj.get("metadata") {
        if let Some(et) = meta.get("error_type") {
            push_error_type(&mut insight.error_types, et);
        }
        if let Some(code) = meta.get("code") {
            push_code(&mut insight.inner_codes, code);
        }
    }
    if let Some(prev) = obj.get("previous_errors").and_then(|p| p.as_array()) {
        for entry in prev {
            // Entries may be bare error objects or wrap `{ "error": {...} }`.
            if let Some(nested) = entry.get("error") {
                collect_from_error_obj(&mut insight, nested);
            }
            collect_from_error_obj(&mut insight, entry);
            if let Some(code) = entry.get("status") {
                push_code(&mut insight.inner_codes, code);
            }
        }
    }

    if insight.inner_codes.is_empty() && insight.error_types.is_empty() {
        return None;
    }
    Some(insight)
}

/// Log envelope inner transient signals (observability only — does not gate
/// retry or fallback). Safe to call on every non-success body.
pub fn log_envelope_if_interesting(provider: &str, status: reqwest::StatusCode, body: &str) {
    if let Some(insight) = parse_openrouter_envelope(body) {
        if insight.has_transient_inner() {
            tracing::warn!(
                provider = %provider,
                status = %status,
                inner_codes = ?insight.inner_codes,
                error_types = ?insight.error_types,
                "OpenRouter envelope carries inner transient (429/5xx or documented error_type)"
            );
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
    config: &ProviderConfig,
) -> Result<Choice> {
    let parse_retry_limit = config.parse_retry_limit;
    let mut last_error: Option<anyhow::Error> = None;
    // Attempts consumed on the 429/5xx path; that path has its own budget
    // (`rate_limit_retry_limit`) independent of the missing-`choices` budget.
    let mut rate_limit_attempts: u32 = 0;

    // Overall loop bound: worst case every attempt bounces on a rate-limit
    // status, so allow both budgets plus the original request.
    let max_total = parse_retry_limit + config.rate_limit_retry_limit + 1;
    let mut attempt = 0u32;
    while attempt < max_total {
        let mut req = client.post(url).json(request);
        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        for (name, value) in &config.extra_headers {
            req = req.header(name.as_str(), value.as_str());
        }

        let response = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(e).context("Failed to send request"),
        };

        let status = response.status();
        if !status.is_success() {
            // Grab Retry-After *before* consuming the body (text() moves self).
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());
            let body = response.text().await.unwrap_or_default();
            log_envelope_if_interesting(provider_name, status, &body);
            // Ollama's deterministic "no user query found" 500 is not transient.
            // ADR-0009 same-payload retries burn the rate-limit budget and delay
            // ADR-0034 omit-retry (live soft-fail: Rate/transient 1/3..3/3 then
            // Fallback with no rewrite+omit WARN when omit gate was also skipped).
            let no_user_query = body.to_lowercase().contains("no user query found");
            let err = anyhow::Error::from(LlmHttpError {
                provider: provider_name.to_string(),
                status,
                body,
            });
            let retryable = !no_user_query
                && (status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error());
            if !retryable || rate_limit_attempts >= config.rate_limit_retry_limit {
                // 400/401/… fail fast (unchanged); no-user-query 500 fail fast;
                // 429/5xx after budget spent.
                return Err(err);
            }

            rate_limit_attempts += 1;
            let backoff = rate_limit_backoff(retry_after.as_deref(), rate_limit_attempts);
            tracing::warn!(
                "Rate/transient error from {} ({}) — retry {}/{} after {:.1}s",
                provider_name,
                status,
                rate_limit_attempts,
                config.rate_limit_retry_limit,
                backoff.as_secs_f64()
            );
            // Do NOT count rate-limit tries against the parse budget.
            tokio::time::sleep(backoff).await;
            attempt += 1;
            continue;
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

        // Missing or empty choices. Counted on its own axis so rate-limit
        // retries never inflate this schedule (still 1s, 2s, 4s...).
        let parse_attempt = attempt - rate_limit_attempts;
        let backoff_s = 1u64 << parse_attempt.min(5);
        let err = anyhow::anyhow!(
            "Response from {} missing or empty 'choices' field - attempt {}/{}, retry limit {}",
            provider_name,
            parse_attempt + 1,
            parse_retry_limit + 1,
            parse_retry_limit
        );
        tracing::warn!(
            "Retry {}/{}: {} - backing off {}s",
            parse_attempt + 1,
            parse_retry_limit,
            err,
            backoff_s
        );
        last_error = Some(err);

        attempt += 1;
        if attempt - rate_limit_attempts > parse_retry_limit {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff_s)).await;
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("{}: retry loop exhausted", provider_name)))
}

/// Backoff for a 429/5xx response: honour `Retry-After` when present,
/// otherwise exponential 4s * 2^(n-1). Both paths cap at
/// [`MAX_RATE_LIMIT_BACKOFF_SECS`] and add 0-1s of jitter so two RustFox
/// callers don't re-collide in lockstep.
fn rate_limit_backoff(retry_after: Option<&str>, attempt: u32) -> std::time::Duration {
    let base = match parse_retry_after(retry_after) {
        Some(secs) => secs,
        None => 4u64.saturating_mul(1u64 << (attempt - 1).min(5)),
    };
    let capped = base.min(MAX_RATE_LIMIT_BACKOFF_SECS);
    let jitter = {
        use rand::Rng as _;
        rand::rng().random_range(0..1000u64)
    };
    std::time::Duration::from_secs(capped) + std::time::Duration::from_millis(jitter)
}

const MAX_RATE_LIMIT_BACKOFF_SECS: u64 = 60;

/// Parse a `Retry-After` value: either delta-seconds (`"30"`) or an HTTP-date
/// (`"Wed, 21 Oct 2026 07:28:00 GMT"`). Returns `None` when absent or
/// unparseable; negative seconds clamp to 0 (the caller still caps).
fn parse_retry_after(raw: Option<&str>) -> Option<u64> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<i64>() {
        return Some(secs.max(0) as u64);
    }
    chrono::DateTime::parse_from_rfc2822(raw).ok().map(|when| {
        (when.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .num_seconds()
            .max(0) as u64
    })
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
    async fn fetch_context_window(&self, _client: &reqwest::Client, _model: &str) -> Option<usize> {
        None // default: no API-based detection
    }
}

/// Longest-prefix aliases for OpenCode catalog ids (ADR-0043).
/// Reserved even when no `[[provider]] type=opencode` section exists (#262).
const OPENCODE_MODEL_ALIASES: &[&str] = &["opencode", "opencode-go"];

/// Strip a reserved OpenCode alias prefix (`opencode/` or `opencode-go/`).
/// Longest alias wins. Returns the remainder after `alias/`.
fn strip_opencode_alias(model: &str) -> Option<&str> {
    let mut best: Option<(&str, &str)> = None;
    for alias in OPENCODE_MODEL_ALIASES {
        let Some(rest) = model.strip_prefix(alias).and_then(|s| s.strip_prefix('/')) else {
            continue;
        };
        match best {
            Some((prev, _)) if alias.len() <= prev.len() => {}
            _ => best = Some((*alias, rest)),
        }
    }
    best.map(|(_, rest)| rest)
}

/// Match reserved xAI Active ids: exact `xai` (#273) or `xai/<model>` (ADR-0044).
///
/// Returns `Some("")` for bare `xai` (caller substitutes the provider default
/// model). Returns `Some(rest)` after `xai/`. Case rules match existing
/// prefix stubs (exact ASCII equality on [`crate::xai::MODEL_ALIAS`]).
fn strip_xai_alias(model: &str) -> Option<&str> {
    if model == crate::xai::MODEL_ALIAS {
        return Some("");
    }
    model
        .strip_prefix(crate::xai::MODEL_ALIAS)
        .and_then(|s| s.strip_prefix('/'))
}

fn opencode_detect_only_stub() -> Arc<dyn Provider> {
    Arc::new(OpenCodeProvider::new(ProviderConfig {
        name: "opencode".to_string(),
        provider_type: ProviderType::OpenCode,
        base_url: String::new(),
        api_key: None,
        default_model: String::new(),
        supports_vision: false,
        max_tokens: 1024,
        discover_models: false,
        context_window: 128_000,
        context_window_cache: Arc::new(RwLock::new(None)),
        parse_retry_limit: 0,
        rate_limit_retry_limit: 0,
        extra_headers: vec![],
    }))
}

fn xai_misconfigured_stub() -> Arc<dyn Provider> {
    Arc::new(XaiProvider::new(ProviderConfig {
        name: crate::xai::MODEL_ALIAS.to_string(),
        provider_type: ProviderType::Xai,
        base_url: crate::xai::DEFAULT_BASE.to_string(),
        api_key: None,
        default_model: crate::xai::DEFAULT_MODEL.to_string(),
        supports_vision: false,
        max_tokens: 8192,
        discover_models: true,
        context_window: 128_000,
        context_window_cache: Arc::new(RwLock::new(None)),
        parse_retry_limit: 0,
        rate_limit_retry_limit: 0,
        extra_headers: vec![],
    }))
}

/// Holds the set of providers available to the agent and the default provider
/// name used when a model string does not carry an explicit `provider/` prefix.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    default_provider: String,
    /// Detect-only stub for reserved `opencode*` aliases when no
    /// `[[provider]] type=opencode` is registered (ADR-0043 §6 / #262).
    /// Not listed in [`Self::provider_names`].
    opencode_detect_only: Option<Arc<dyn Provider>>,
    /// Misconfigured stub for reserved `xai` / `xai/` when no `type=xai`
    /// section is registered (ADR-0044 §9 / #273). Not listed in [`Self::provider_names`].
    xai_stub: Option<Arc<dyn Provider>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_provider", &self.default_provider)
            .field("opencode_detect_only", &self.opencode_detect_only.is_some())
            .field("xai_stub", &self.xai_stub.is_some())
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new(providers: HashMap<String, Arc<dyn Provider>>, default_provider: String) -> Self {
        Self {
            providers,
            default_provider,
            opencode_detect_only: None,
            xai_stub: None,
        }
    }

    /// Resolve a model string to (provider, stripped_model).
    ///
    /// Longest-prefix match on registered provider names (ADR-0043): among
    /// names `N` where `model == N` is false and `model` starts with `N/`,
    /// pick the longest `N`. Reserved OpenCode aliases (`opencode`,
    /// `opencode-go`) resolve to the detect-only stub even when no
    /// `[[provider]] type=opencode` section exists (#262) — never silent-hop
    /// to the default provider. Reserved xAI ids (`xai`, `xai/…`) likewise
    /// resolve to the xAI provider or misconfigured stub (#273 / ADR-0044 §9).
    /// Other unknown prefixes fall through to the default provider
    /// (unchanged). Never fails.
    pub fn resolve_model<'a>(&'a self, model: &'a str) -> (&'a dyn Provider, &'a str) {
        let mut best: Option<(&str, &str)> = None;
        for name in self.providers.keys() {
            let Some(rest) = model
                .strip_prefix(name.as_str())
                .and_then(|s| s.strip_prefix('/'))
            else {
                continue;
            };
            match best {
                Some((prev, _)) if name.len() <= prev.len() => {}
                _ => best = Some((name.as_str(), rest)),
            }
        }
        if let Some((name, rest)) = best {
            return (self.providers[name].as_ref(), rest);
        }
        // ADR-0043 §6 / #262: reserved OpenCode aliases — typed detect-only,
        // even when no type=opencode section is registered.
        if let Some(stub) = &self.opencode_detect_only {
            if let Some(rest) = strip_opencode_alias(model) {
                return (stub.as_ref(), rest);
            }
        }
        // ADR-0044 §9 / #273: reserved bare `xai` and `xai/<model>` — typed
        // misconfigured error, never silent-hop to the default provider.
        if let Some(rest) = strip_xai_alias(model) {
            if let Some(p) = self.providers.get(crate::xai::MODEL_ALIAS) {
                let model_id = if rest.is_empty() {
                    p.default_model()
                } else {
                    rest
                };
                return (p.as_ref(), model_id);
            }
            if let Some(stub) = &self.xai_stub {
                let model_id = if rest.is_empty() {
                    stub.default_model()
                } else {
                    rest
                };
                return (stub.as_ref(), model_id);
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
        let cached = provider
            .config()
            .context_window_cache
            .try_read()
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
    rate_limit_retry_limit: u32,
) -> Result<ProviderRegistry> {
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

    for section in sections {
        let mut cfg: ProviderConfig = ProviderConfig::from(section);
        cfg.parse_retry_limit = parse_retry_limit;
        cfg.rate_limit_retry_limit = rate_limit_retry_limit;
        // OpenCode peer (#267): empty base_url + api_key → Go OpenAI-compat default.
        if section.provider_type == ProviderType::OpenCode
            && cfg.base_url.trim().is_empty()
            && cfg.api_key.as_ref().is_some_and(|k| !k.trim().is_empty())
        {
            cfg.base_url = crate::opencode::GO_OPENAI_BASE.to_string();
        }
        if section.provider_type == ProviderType::OpenCode && cfg.default_model.trim().is_empty() {
            cfg.default_model = crate::opencode::GO_DEFAULT_MODEL.to_string();
        }
        // ADR-0044: empty base_url / model → xAI defaults when type=xai.
        if section.provider_type == ProviderType::Xai {
            if cfg.base_url.trim().is_empty() {
                cfg.base_url = crate::xai::DEFAULT_BASE.to_string();
            }
            if cfg.default_model.trim().is_empty() {
                cfg.default_model = crate::xai::DEFAULT_MODEL.to_string();
            }
        }
        let provider: Arc<dyn Provider> = match section.provider_type {
            ProviderType::OpenRouter => Arc::new(OpenRouterProvider::new(cfg)),
            ProviderType::OpenAICompatible => Arc::new(OpenAICompatibleProvider::new(cfg)),
            ProviderType::Ollama => Arc::new(OllamaProvider::new(cfg)),
            ProviderType::OpenCode => Arc::new(OpenCodeProvider::new(cfg)),
            ProviderType::Xai => Arc::new(XaiProvider::new(cfg)),
        };
        providers.insert(section.name.clone(), provider.clone());
        // Catalog ids are double-segmented (`opencode-go/kimi-k3`). Alias so
        // longest-prefix resolve does not silent-hop to the default provider.
        if section.provider_type == ProviderType::OpenCode {
            for alias in OPENCODE_MODEL_ALIASES {
                if *alias != section.name.as_str() {
                    providers
                        .entry((*alias).to_string())
                        .or_insert_with(|| provider.clone());
                }
            }
        }
        // ADR-0044: also alias `xai/` when the section uses a different name.
        if section.provider_type == ProviderType::Xai
            && section.name.as_str() != crate::xai::MODEL_ALIAS
        {
            providers
                .entry(crate::xai::MODEL_ALIAS.to_string())
                .or_insert_with(|| provider.clone());
        }
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
    if let Some(p) = providers.get(default_name) {
        if p.config().provider_type == ProviderType::OpenCode
            && !crate::opencode::peer_configured(
                &p.config().base_url,
                p.config().api_key.as_deref(),
            )
        {
            anyhow::bail!(
                "Default provider '{}' is OpenCode without peer path (ADR-0043); set base_url + api_key for Go Active, or choose openrouter/ollama",
                default_name
            );
        }
    }
    if let Some(p) = providers.get(default_name) {
        if p.config().provider_type == ProviderType::Xai
            && !crate::xai::peer_configured(&p.config().base_url, p.config().api_key.as_deref())
        {
            anyhow::bail!(
                "Default provider '{}' is xAI without api_key/base_url (ADR-0044); set credentials or choose another Active",
                default_name
            );
        }
    }

    // #262: when no type=opencode section exists, keep a resolve-only stub so
    // reserved aliases cannot silent-hop to the default provider.
    let opencode_detect_only = if providers
        .values()
        .any(|p| p.config().provider_type == ProviderType::OpenCode)
    {
        None
    } else {
        Some(opencode_detect_only_stub())
    };

    // ADR-0044 §9 / #273: when no type=xai section, stub bare `xai` and `xai/`
    // so they cannot silent-hop.
    let xai_stub = if providers
        .values()
        .any(|p| p.config().provider_type == ProviderType::Xai)
    {
        None
    } else {
        Some(xai_misconfigured_stub())
    };

    Ok(ProviderRegistry {
        providers,
        default_provider: default_name.to_string(),
        opencode_detect_only,
        xai_stub,
    })
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

        let mut choice = chat_completion_with_retry(
            client,
            &url,
            &request,
            api_key,
            provider_name,
            &self.config,
        )
        .await?;

        // Kimi / DSML tool-markup recover (#307; shared with OpenAI-compat / OpenCode).
        if crate::llm::recover_leaked_tool_markup(&mut choice.message) {
            choice.finish_reason = Some("tool_calls".to_string());
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

    async fn fetch_context_window(&self, client: &reqwest::Client, model: &str) -> Option<usize> {
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
        let ctx = list["data"]
            .as_array()?
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

        let mut choice = chat_completion_with_retry(
            client,
            &url,
            &request,
            api_key,
            provider_name,
            &self.config,
        )
        .await?;

        // OpenCode Go Active (+ openai_compatible): recover DSML/kimi leaked into
        // content when tool_calls is empty; strip closers-only so FinalResponse
        // never posts raw markup to Telegram (#307).
        if crate::llm::recover_leaked_tool_markup(&mut choice.message) {
            choice.finish_reason = Some("tool_calls".to_string());
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

    async fn fetch_context_window(&self, client: &reqwest::Client, model: &str) -> Option<usize> {
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
        let ctx = list["data"]
            .as_array()?
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

// === XaiProvider (ADR-0044 / #266 Active peer) ===

pub struct XaiProvider {
    config: ProviderConfig,
    /// Set when peer path is configured; reuses OpenAI-compat chat.
    inner: Option<OpenAICompatibleProvider>,
}

impl XaiProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let inner = if crate::xai::peer_configured(&config.base_url, config.api_key.as_deref()) {
            Some(OpenAICompatibleProvider::new(config.clone()))
        } else {
            None
        };
        Self { config, inner }
    }

    pub fn is_peer(&self) -> bool {
        self.inner.is_some()
    }
}

#[async_trait]
impl Provider for XaiProvider {
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
        let Some(inner) = &self.inner else {
            return Err(anyhow::Error::from(
                crate::xai::XaiMisconfiguredError::chat(),
            ));
        };
        inner
            .chat_completion(client, messages, tools, model, max_tokens)
            .await
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        let Some(inner) = &self.inner else {
            return Err(anyhow::Error::from(
                crate::xai::XaiMisconfiguredError::chat(),
            ));
        };
        inner.list_models(client).await
    }

    async fn fetch_context_window(&self, client: &reqwest::Client, model: &str) -> Option<usize> {
        let inner = self.inner.as_ref()?;
        inner.fetch_context_window(client, model).await
    }
}

// === OpenCodeProvider (ADR-0043: detect-only stub OR Go Active peer #267) ===

pub struct OpenCodeProvider {
    config: ProviderConfig,
    /// Set when peer path is configured; reuses OpenAI-compat chat.
    inner: Option<OpenAICompatibleProvider>,
}

impl OpenCodeProvider {
    pub fn new(mut config: ProviderConfig) -> Self {
        let inner = if crate::opencode::peer_configured(&config.base_url, config.api_key.as_deref())
        {
            // #271: stable x-opencode-session for the process lifetime of this peer.
            if !config
                .extra_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case(crate::opencode::SESSION_HEADER))
            {
                config.extra_headers.push((
                    crate::opencode::SESSION_HEADER.to_string(),
                    crate::opencode::mint_session_id(),
                ));
            }
            if !config
                .extra_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            {
                config.extra_headers.push((
                    "User-Agent".to_string(),
                    format!("RustFox/{}", env!("CARGO_PKG_VERSION")),
                ));
            }
            Some(OpenAICompatibleProvider::new(config.clone()))
        } else {
            None
        };
        Self { config, inner }
    }

    pub fn is_peer(&self) -> bool {
        self.inner.is_some()
    }
}

#[async_trait]
impl Provider for OpenCodeProvider {
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
        let Some(inner) = &self.inner else {
            return Err(anyhow::Error::from(
                crate::opencode::OpenCodeDetectOnlyError::chat(),
            ));
        };
        inner
            .chat_completion(client, messages, tools, model, max_tokens)
            .await
    }

    async fn list_models(&self, client: &reqwest::Client) -> Result<Vec<String>> {
        if let Some(_inner) = &self.inner {
            let url = crate::opencode::go_models_url(&self.config.base_url);
            let mut req = client.get(&url);
            if let Some(ref key) = self.config.api_key {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            for (name, value) in &self.config.extra_headers {
                req = req.header(name.as_str(), value.as_str());
            }
            let response = req
                .send()
                .await
                .context("Failed to fetch models from OpenCode Go")?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("OpenCode Go models API error ({}): {}", status, body);
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
            return Ok(models);
        }
        let status = crate::opencode::detect_async().await;
        if status.state != crate::opencode::STATE_READY {
            anyhow::bail!(
                "OpenCode models unavailable (state={}): {}",
                status.state,
                status.message.unwrap_or_default()
            );
        }
        Ok(status.models)
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
        assert_eq!(reg.provider_count(), 2);
        assert!(reg.get_provider("alpha").is_some());
        assert!(reg.get_provider("beta").is_some());
        assert_eq!(reg.default_provider_name(), "alpha");
    }

    #[test]
    fn build_registry_fails_with_no_providers() {
        let result = build_registry(&[], "alpha", 3, 0);
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
        let result = build_registry(&sections, "missing", 3, 0);
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
        let (provider, model) = reg.resolve_model("beta/llama3.1:8b");
        assert_eq!(provider.name(), "beta");
        assert_eq!(model, "llama3.1:8b");
    }

    /// ADR-0043: longest-prefix so `opencode-go/kimi-k3` hits OpenCode, not default.
    #[test]
    fn resolve_model_longest_prefix_opencode_go_does_not_silent_hop() {
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "moonshotai/kimi-k2.6",
            ),
            make_section(
                "opencode",
                ProviderType::OpenCode,
                "",
                "opencode-go/kimi-k3",
            ),
        ];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, model) = reg.resolve_model("opencode-go/kimi-k3");
        assert_eq!(
            provider.config().provider_type,
            ProviderType::OpenCode,
            "must resolve to OpenCode, not silent-hop to openrouter"
        );
        assert_eq!(model, "kimi-k3");
        // Nested catalog id under the rustfox provider prefix.
        let (provider2, model2) = reg.resolve_model("opencode/opencode-go/kimi-k3");
        assert_eq!(provider2.config().provider_type, ProviderType::OpenCode);
        assert_eq!(model2, "opencode-go/kimi-k3");
        // Longer alias wins over shorter when both registered.
        let (provider3, model3) = reg.resolve_model("opencode/glm-5.3-flash-free");
        assert_eq!(provider3.config().provider_type, ProviderType::OpenCode);
        assert_eq!(model3, "glm-5.3-flash-free");
    }

    #[test]
    fn build_registry_rejects_opencode_as_default_without_peer() {
        let mut section = make_section("opencode", ProviderType::OpenCode, "", "opencode/glm");
        section.api_key = None; // no peer path
        let sections = vec![section];
        let err = build_registry(&sections, "opencode", 3, 0).unwrap_err();
        assert!(
            err.to_string().contains("OpenCode")
                && (err.to_string().contains("peer")
                    || err.to_string().contains("detect-only")
                    || err.to_string().contains("api_key")),
            "{err}"
        );
    }

    #[test]
    fn build_registry_allows_opencode_as_default_when_peer_configured() {
        let mut section = make_section(
            "opencode",
            ProviderType::OpenCode,
            crate::opencode::GO_OPENAI_BASE,
            crate::opencode::GO_DEFAULT_MODEL,
        );
        section.api_key = Some("sk-go-test".into());
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "deepseek/deepseek-chat",
            ),
            section,
        ];
        let reg = build_registry(&sections, "opencode", 3, 0).unwrap();
        assert_eq!(reg.default_provider_name(), "opencode");
        let p = reg.get_provider("opencode").unwrap();
        assert!(p.config().api_key.as_ref().is_some_and(|k| !k.is_empty()));
    }

    #[tokio::test]
    async fn opencode_chat_returns_detect_only_when_peer_not_configured() {
        let mut oc = make_section("opencode", ProviderType::OpenCode, "", "opencode/glm");
        oc.api_key = None;
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "moonshotai/kimi-k2.6",
            ),
            oc,
        ];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, _) = reg.resolve_model("opencode/glm");
        let client = reqwest::Client::new();
        let err = provider
            .chat_completion(&client, &[], &[], "glm", 16)
            .await
            .unwrap_err();
        let typed = err.downcast_ref::<crate::opencode::OpenCodeDetectOnlyError>();
        assert!(
            typed.is_some(),
            "expected OpenCodeDetectOnlyError, got {err:#}"
        );
        assert!(
            !should_walk_fallback(&err),
            "must not silent-hop via fallback"
        );
    }

    /// #262 soft repro: no `[[provider]] type=opencode` + `opencode/…` must
    /// resolve to detect-only, not silent-hop to the default provider.
    #[test]
    fn resolve_model_unregistered_opencode_does_not_silent_hop() {
        let sections = vec![make_section(
            "openrouter",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "moonshotai/kimi-k2.6",
        )];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        // Stub is resolve-only — must not appear in provider_names / get_provider.
        assert!(
            reg.get_provider("opencode").is_none(),
            "detect-only stub must not pollute registered provider list"
        );
        assert!(!reg
            .provider_names()
            .iter()
            .any(|n| n == "opencode" || n == "opencode-go"));

        let (provider, model) = reg.resolve_model("opencode/glm-5.3-flash-free");
        assert_eq!(
            provider.config().provider_type,
            ProviderType::OpenCode,
            "must not silent-hop to openrouter"
        );
        assert_eq!(model, "glm-5.3-flash-free");

        let (provider2, model2) = reg.resolve_model("opencode-go/kimi-k3");
        assert_eq!(provider2.config().provider_type, ProviderType::OpenCode);
        assert_eq!(model2, "kimi-k3");

        // Non-OpenCode unknown prefix still falls through to default.
        let (provider3, model3) = reg.resolve_model("moonshotai/kimi-k2.5");
        assert_eq!(provider3.name(), "openrouter");
        assert_eq!(model3, "moonshotai/kimi-k2.5");
    }

    /// #262: Active / chat path with unregistered OpenCode id → typed error.
    #[tokio::test]
    async fn unregistered_opencode_chat_returns_detect_only_typed_error() {
        let sections = vec![make_section(
            "openrouter",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "moonshotai/kimi-k2.6",
        )];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, _) = reg.resolve_model("opencode/glm");
        assert_eq!(provider.config().provider_type, ProviderType::OpenCode);
        let client = reqwest::Client::new();
        let err = provider
            .chat_completion(&client, &[], &[], "glm", 16)
            .await
            .unwrap_err();
        let typed = err.downcast_ref::<crate::opencode::OpenCodeDetectOnlyError>();
        assert!(
            typed.is_some(),
            "expected OpenCodeDetectOnlyError, got {err:#}"
        );
        assert!(
            !should_walk_fallback(&err),
            "must not silent-hop via fallback"
        );
    }

    #[test]
    fn resolve_model_falls_back_to_default_on_unknown_prefix() {
        let sections = vec![make_section(
            "alpha",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4-6",
        )];
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
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
        let reg = build_registry(&sections, "alpha", 3, 0).unwrap();
        let provider = reg.get_provider("alpha").unwrap();
        // Set cache manually
        *provider.config().context_window_cache.try_write().unwrap() = Some(200_000);
        let ctx = reg.effective_context_window("anthropic/claude-sonnet-4-6");
        assert_eq!(ctx, 200_000);
    }

    // -----------------------------------------------------------------------
    // 429 / 5xx retry policy (see PLAN-2026-09-19-429-retry.md)
    // -----------------------------------------------------------------------

    use crate::llm::{ChatMessage, MessageContent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn retry_config(base_url: String, parse_limit: u32, rate_limit: u32) -> ProviderConfig {
        ProviderConfig {
            name: "mock".to_string(),
            provider_type: ProviderType::OpenRouter,
            base_url,
            api_key: Some("k".to_string()),
            default_model: "m".to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 100,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: parse_limit,
            rate_limit_retry_limit: rate_limit,
            extra_headers: vec![],
        }
    }

    fn test_request() -> crate::llm::internal::ChatRequest {
        crate::llm::internal::ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hi".to_string())),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            max_tokens: 10,
        }
    }

    fn ok_body() -> String {
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#
            .to_string()
    }

    fn empty_choices_body() -> String {
        r#"{"choices":[],"error":"upstream returned nothing"}"#.to_string()
    }

    /// Mount a script of responses (one per incoming request); the last entry
    /// repeats forever. Returns the shared request counter.
    async fn mount_script(
        server: &MockServer,
        script: Vec<ResponseTemplate>,
    ) -> StdArc<AtomicUsize> {
        let counter = StdArc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let last = script.len() - 1;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |_: &wiremock::Request| {
                let n = c.fetch_add(1, Ordering::SeqCst);
                script[n.min(last)].clone()
            })
            .mount(server)
            .await;
        counter
    }

    async fn call_with_retry(config: &ProviderConfig) -> Result<Choice> {
        let client = reqwest::Client::new();
        let url = format!("{}/chat/completions", config.base_url);
        chat_completion_with_retry(&client, &url, &test_request(), Some("k"), "mock", config).await
    }

    fn not_found() -> ResponseTemplate {
        ResponseTemplate::new(404)
    }

    #[tokio::test(start_paused = true)]
    async fn test_429_then_success_retries_once() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![
                ResponseTemplate::new(429),
                ResponseTemplate::new(200).set_body_string(ok_body()),
            ],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        let result = call_with_retry(&config).await;
        assert!(
            result.is_ok(),
            "should succeed after one 429: {:?}",
            result.err()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_always_429_exhausts_budget_and_counts_requests() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![ResponseTemplate::new(429).set_body_string(r#"{"error":"rate limited"}"#)],
        )
        .await;
        let config = retry_config(server.uri(), 3, 2);
        let err = call_with_retry(&config).await.unwrap_err().to_string();
        assert!(err.contains("429"), "error should mention 429: {err}");
        // 1 original + 2 retries = 3 requests
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_retry_after_header_honoured_not_default_backoff() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![
                ResponseTemplate::new(429).insert_header("Retry-After", "1"),
                ResponseTemplate::new(200).set_body_string(ok_body()),
                not_found(),
            ],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        assert!(call_with_retry(&config).await.is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_400_fails_fast_no_retry() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![ResponseTemplate::new(400).set_body_string("bad request")],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        assert!(call_with_retry(&config).await.is_err());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "4xx (except 429) must never retry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_user_query_500_fails_fast_no_adr0009_retry() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![ResponseTemplate::new(500).set_body_string(
                r#"{"error":{"message":"no user query found in messages","type":"api_error"}}"#,
            )],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        let err = call_with_retry(&config).await.unwrap_err();
        assert!(
            format!("{err:#}")
                .to_lowercase()
                .contains("no user query found"),
            "{err:#}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "no-user-query 500 must not burn ADR-0009 rate-limit retries"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_503_then_success() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![
                ResponseTemplate::new(503),
                ResponseTemplate::new(200).set_body_string(ok_body()),
            ],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        assert!(call_with_retry(&config).await.is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_missing_choices_retry_behaviour_unchanged() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![
                ResponseTemplate::new(200).set_body_string(empty_choices_body()),
                ResponseTemplate::new(200).set_body_string(ok_body()),
                not_found(),
            ],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        assert!(call_with_retry(&config).await.is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_rate_limit_zero_disables_retry_fail_fast() {
        let server = MockServer::start().await;
        let hits = mount_script(&server, vec![ResponseTemplate::new(429)]).await;
        let config = retry_config(server.uri(), 3, 0);
        assert!(call_with_retry(&config).await.is_err());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "limit 0 = pre-fix fail-fast"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_mixed_429_then_missing_choices_both_budgets_independent() {
        let server = MockServer::start().await;
        let hits = mount_script(
            &server,
            vec![
                ResponseTemplate::new(429), // spends 1 of the rate-limit budget
                ResponseTemplate::new(200).set_body_string(empty_choices_body()), // spends 1 parse retry
                ResponseTemplate::new(200).set_body_string(ok_body()),
                not_found(),
            ],
        )
        .await;
        let config = retry_config(server.uri(), 3, 3);
        assert!(call_with_retry(&config).await.is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn parse_retry_after_delta_seconds_and_garbage() {
        assert_eq!(parse_retry_after(Some("30")), Some(30));
        assert_eq!(parse_retry_after(Some("0")), Some(0));
        assert_eq!(parse_retry_after(Some("-5")), Some(0));
        assert_eq!(parse_retry_after(Some("")), None);
        assert_eq!(parse_retry_after(None), None);
        assert_eq!(parse_retry_after(Some("not-a-date")), None);
    }

    #[test]
    fn parse_retry_after_http_date_past_clamps_zero_future_in_range() {
        // clearly in the past -> 0
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(0)
        );
        let future = (chrono::Utc::now() + chrono::Duration::seconds(45))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let secs = parse_retry_after(Some(&future)).unwrap();
        assert!((30..=50).contains(&secs), "got {secs}");
    }

    #[test]
    fn rate_limit_backoff_uses_header_then_caps() {
        // No header: exponential 4s/8s/16s, plus <1s jitter.
        let d1 = rate_limit_backoff(None, 1);
        assert!((4..5).contains(&d1.as_secs()), "attempt1 = {d1:?}");
        let d3 = rate_limit_backoff(None, 3);
        assert!((16..17).contains(&d3.as_secs()), "attempt3 = {d3:?}");
        // Header present wins (even when smaller than exponential).
        let d = rate_limit_backoff(Some("2"), 3);
        assert!((2..3).contains(&d.as_secs()));
        // Absurd header clamps to the 60s cap.
        let d = rate_limit_backoff(Some("99999"), 1);
        assert!((60..61).contains(&d.as_secs()));
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
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
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
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
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
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        };
        let p = OllamaProvider::new(cfg);
        assert_eq!(p.discovery_url(), "http://localhost:11434/api/tags");
    }

    // ---------------------------------------------------------------
    // ADR-0019: envelope parse + fallback walk classifier
    // ---------------------------------------------------------------

    #[test]
    fn envelope_parses_previous_errors_429_and_error_type() {
        let body = r#"{
            "error": {"code": 400, "message": "Provider returned error"},
            "previous_errors": [
                {
                    "code": 429,
                    "message": "upstream rl",
                    "metadata": {"error_type": "rate_limit_exceeded"}
                }
            ]
        }"#;
        let insight = parse_openrouter_envelope(body).unwrap();
        assert!(insight.inner_codes.contains(&400));
        assert!(insight.inner_codes.contains(&429));
        assert!(insight
            .error_types
            .iter()
            .any(|t| t == "rate_limit_exceeded"));
        assert!(insight.has_transient_inner());
    }

    #[test]
    fn envelope_documented_error_type_alone_is_transient() {
        let body = r#"{
            "error": {
                "code": 400,
                "message": "x",
                "metadata": {"error_type": "provider_overloaded"}
            }
        }"#;
        let insight = parse_openrouter_envelope(body).unwrap();
        assert!(insight.has_transient_inner());
        // Free-text "rate limit" in message must NOT be required / used.
        assert!(!insight.error_types.iter().any(|t| t.contains(' ')));
    }

    #[test]
    fn envelope_ignores_non_json_and_empty_objects() {
        assert!(parse_openrouter_envelope("not-json").is_none());
        assert!(parse_openrouter_envelope("{}").is_none());
        assert!(parse_openrouter_envelope(r#"{"error":{"message":"x"}}"#).is_none());
    }

    #[test]
    fn envelope_does_not_string_guess_rate_limit_in_message() {
        let body = r#"{
            "error": {"code": 400, "message": "rate limit exceeded somewhere"}
        }"#;
        let insight = parse_openrouter_envelope(body).unwrap();
        assert!(
            !insight.has_transient_inner(),
            "message text must not classify"
        );
        assert_eq!(insight.inner_codes, vec![400]);
        assert!(insight.error_types.is_empty());
    }

    #[test]
    fn should_walk_fallback_matrix() {
        let http = |status: u16| {
            anyhow::Error::from(LlmHttpError {
                provider: "p".into(),
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                body: "x".into(),
            })
        };
        assert!(should_walk_fallback(&http(400)));
        assert!(should_walk_fallback(&http(402)));
        assert!(should_walk_fallback(&http(429)));
        assert!(should_walk_fallback(&http(503)));
        assert!(!should_walk_fallback(&http(401)));
        assert!(!should_walk_fallback(&http(403)));
        assert!(!should_walk_fallback(&anyhow::Error::from(
            LlmPreHttpError {
                message: "missing OpenRouter API key".into(),
            }
        )));
        assert!(!should_walk_fallback(&anyhow::Error::from(
            crate::opencode::OpenCodeDetectOnlyError::chat()
        )));
        assert!(!should_walk_fallback(&anyhow::Error::from(
            crate::xai::XaiMisconfiguredError::chat()
        )));
        assert!(should_walk_fallback(&anyhow::anyhow!(
            "Failed to send request"
        )));
        assert!(should_walk_fallback(&anyhow::anyhow!(
            "Failed to parse JSON response"
        )));
    }

    #[test]
    fn leave_go_fallback_triggers() {
        let mk = |status: u16, body: &str| {
            anyhow::Error::from(LlmHttpError {
                provider: "opencode".into(),
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                body: body.into(),
            })
        };
        // 402 credits → leave
        assert!(should_walk_fallback(&mk(
            402,
            r#"{"error":"Payment Required"}"#
        )));
        // 429 + GoUsageLimitError → leave
        assert!(should_walk_fallback(&mk(
            429,
            r#"{"error":{"type":"GoUsageLimitError","message":"quota"}}"#
        )));
        // 429 + Policy budget → leave
        assert!(should_walk_fallback(&mk(429, "Policy budget exceeded")));
        // plain short 429 → do NOT leave
        assert!(!should_walk_fallback(&mk(
            429,
            "rate limited, retry shortly"
        )));
        // #271: MissingSessionID body is not a leave-Go signal (header fix, not hop).
        // HTTP 400 still walks under ADR-0019 for non-OpenCode leave rules; leave-Go
        // itself remains 402 / 429+GoUsageLimitError/Policy only.
        assert!(!crate::opencode::is_go_leave_signal(
            r#"{"error":{"type":"MissingSessionID"}}"#
        ));
        // other providers: plain 429 still walks (ADR-0019)
        assert!(should_walk_fallback(&anyhow::Error::from(LlmHttpError {
            provider: "openrouter".into(),
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".into(),
        })));
    }

    #[test]
    fn reasoning_content_round_trips_on_chat_message() {
        let raw = r#"{
            "role": "assistant",
            "content": "pong",
            "reasoning_content": "thinking…"
        }"#;
        let msg: ChatMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(msg.content.as_ref().unwrap().as_text(), "pong");
        assert_eq!(msg.reasoning_content.as_deref(), Some("thinking…"));
        let back = serde_json::to_value(&msg).unwrap();
        assert_eq!(back["reasoning_content"], "thinking…");
        assert_eq!(back["content"], "pong");
        // Absent field stays absent on serialize
        let bare: ChatMessage =
            serde_json::from_str(r#"{"role":"assistant","content":"hi"}"#).unwrap();
        assert!(bare.reasoning_content.is_none());
        let bare_v = serde_json::to_value(&bare).unwrap();
        assert!(bare_v.get("reasoning_content").is_none());
    }

    #[test]
    fn auth_hard_exclusion_and_transient_unchanged() {
        let e401 = LlmHttpError {
            provider: "p".into(),
            status: reqwest::StatusCode::UNAUTHORIZED,
            body: String::new(),
        };
        assert!(e401.is_auth_hard_exclusion());
        assert!(!e401.is_transient());
        let e429 = LlmHttpError {
            provider: "p".into(),
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body: String::new(),
        };
        assert!(!e429.is_auth_hard_exclusion());
        assert!(e429.is_transient());
        let e402 = LlmHttpError {
            provider: "p".into(),
            status: reqwest::StatusCode::PAYMENT_REQUIRED,
            body: String::new(),
        };
        assert!(!e402.is_auth_hard_exclusion());
        assert!(!e402.is_transient()); // still not same-model retryable
    }

    #[test]
    fn build_registry_allows_xai_as_default_when_configured() {
        let mut section = make_section(
            "xai",
            ProviderType::Xai,
            crate::xai::DEFAULT_BASE,
            crate::xai::DEFAULT_MODEL,
        );
        section.api_key = Some("sk-xai-test".into());
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "deepseek/deepseek-chat",
            ),
            section,
        ];
        let reg = build_registry(&sections, "xai", 3, 0).unwrap();
        assert_eq!(reg.default_provider_name(), "xai");
        let p = reg.get_provider("xai").unwrap();
        assert_eq!(p.config().provider_type, ProviderType::Xai);
        assert_eq!(p.default_model(), "grok-4.7");
        assert!(p.config().api_key.as_ref().is_some_and(|k| !k.is_empty()));
    }

    #[test]
    fn build_registry_rejects_xai_as_default_without_key() {
        let mut section = make_section("xai", ProviderType::Xai, "", "grok-4.7");
        section.api_key = None;
        let err = build_registry(&[section], "xai", 3, 0)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("xAI") && (err.contains("api_key") || err.contains("base_url")),
            "{err}"
        );
    }

    #[test]
    fn xai_defaults_filled_when_empty() {
        let mut section = make_section("xai", ProviderType::Xai, "", "");
        section.api_key = Some("sk".into());
        let reg = build_registry(&[section], "xai", 3, 0).unwrap();
        let p = reg.get_provider("xai").unwrap();
        assert_eq!(p.config().base_url, crate::xai::DEFAULT_BASE);
        assert_eq!(p.default_model(), "grok-4.7");
    }

    #[tokio::test]
    async fn xai_chat_returns_typed_error_when_not_configured() {
        let mut xai = make_section(
            "xai",
            ProviderType::Xai,
            crate::xai::DEFAULT_BASE,
            "grok-4.7",
        );
        xai.api_key = None;
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "moonshotai/kimi-k2.6",
            ),
            xai,
        ];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, _) = reg.resolve_model("xai/grok-4.7");
        assert_eq!(provider.config().provider_type, ProviderType::Xai);
        let client = reqwest::Client::new();
        let err = provider
            .chat_completion(&client, &[], &[], "grok-4.7", 16)
            .await
            .unwrap_err();
        let typed = err.downcast_ref::<crate::xai::XaiMisconfiguredError>();
        assert!(
            typed.is_some(),
            "expected XaiMisconfiguredError, got {err:#}"
        );
        assert!(!should_walk_fallback(&err));
    }

    #[tokio::test]
    async fn unregistered_xai_alias_no_silent_hop() {
        let sections = vec![make_section(
            "openrouter",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "moonshotai/kimi-k2.6",
        )];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, model) = reg.resolve_model("xai/grok-4.7");
        assert_eq!(provider.config().provider_type, ProviderType::Xai);
        assert_eq!(model, "grok-4.7");
        let client = reqwest::Client::new();
        let err = provider
            .chat_completion(&client, &[], &[], model, 16)
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<crate::xai::XaiMisconfiguredError>()
                .is_some(),
            "expected typed error, got {err:#}"
        );
        assert!(!should_walk_fallback(&err));
    }

    #[tokio::test]
    async fn opencode_peer_chat_sends_stable_session_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ok_body()))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            name: "opencode".to_string(),
            provider_type: ProviderType::OpenCode,
            base_url: server.uri(),
            api_key: Some("k".to_string()),
            default_model: "deepseek-v4.1-flash".to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 128_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        };
        let provider = OpenCodeProvider::new(cfg);
        assert!(provider.is_peer());
        let stamped = provider
            .config()
            .extra_headers
            .iter()
            .find(|(k, _)| k == crate::opencode::SESSION_HEADER)
            .map(|(_, v)| v.clone());
        let stamped = stamped.expect("peer must stamp x-opencode-session");
        assert!(
            stamped.starts_with("rustfox-") && stamped.len() > "rustfox-".len(),
            "{stamped}"
        );

        let client = reqwest::Client::new();
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];
        provider
            .chat_completion(&client, &messages, &[], "deepseek-v4.1-flash", 10)
            .await
            .expect("chat should succeed");

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let hdr = requests[0]
            .headers
            .get(crate::opencode::SESSION_HEADER)
            .expect("x-opencode-session on chat request");
        assert_eq!(hdr.to_str().unwrap(), stamped.as_str());
    }

    /// #307: OpenCode Active path recovers full DSML leaked into content.
    #[tokio::test]
    async fn opencode_peer_recovers_dsml_tool_calls_from_content() {
        let server = MockServer::start().await;
        let dsml = format!(
            concat!(
                "<{t}tool_calls>\n",
                "<{t}invoke name=\"web_search\">\n",
                "<{t}parameter name=\"query\" string=\"true\">HK weather</{t}parameter>\n",
                "</{t}invoke>\n",
                "</{t}tool_calls>"
            ),
            t = "｜DSML｜"
        );
        let body = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": dsml},
                "finish_reason": "stop"
            }]
        })
        .to_string();
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            name: "opencode".to_string(),
            provider_type: ProviderType::OpenCode,
            base_url: server.uri(),
            api_key: Some("k".to_string()),
            default_model: "deepseek-v4.1-flash".to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 128_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        };
        let provider = OpenCodeProvider::new(cfg);
        let client = reqwest::Client::new();
        let completion = provider
            .chat_completion(&client, &[], &[], "deepseek-v4.1-flash", 10)
            .await
            .expect("chat");
        assert!(
            completion.message.content.is_none(),
            "DSML content must be cleared after recover"
        );
        let calls = completion
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls recovered");
        assert_eq!(calls[0].function.name, "web_search");
        assert_eq!(completion.finish_reason.as_deref(), Some("tool_calls"));
    }

    /// #307: closers-only DSML must not become FinalResponse text on OpenCode path.
    #[tokio::test]
    async fn opencode_peer_strips_dsml_closers_only() {
        let server = MockServer::start().await;
        let closers = format!(
            "</{t} parameter> </{t} invoke> </{t} calls>",
            t = "｜DSML｜"
        );
        let body = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": closers},
                "finish_reason": "stop"
            }]
        })
        .to_string();
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            name: "opencode".to_string(),
            provider_type: ProviderType::OpenCode,
            base_url: server.uri(),
            api_key: Some("k".to_string()),
            default_model: "deepseek-v4.1-flash".to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 128_000,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        };
        let provider = OpenCodeProvider::new(cfg);
        let client = reqwest::Client::new();
        let completion = provider
            .chat_completion(&client, &[], &[], "deepseek-v4.1-flash", 10)
            .await
            .expect("chat");
        let text = completion
            .message
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        assert!(
            text.trim().is_empty(),
            "closers-only must strip to empty on OpenCode path, got {text:?}"
        );
        assert!(!completion.message.has_tool_calls());
    }

    /// #273 soft repro: bare Active id `xai` (no `/model`) must hit the xAI
    /// stub, not silent-hop to the default / OpenRouter alias.
    #[test]
    fn resolve_model_unregistered_bare_xai_does_not_silent_hop() {
        let sections = vec![make_section(
            "openrouter",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "moonshotai/kimi-k2.6",
        )];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        assert!(
            reg.get_provider("xai").is_none(),
            "xAI stub must not pollute registered provider list"
        );
        assert!(!reg.provider_names().iter().any(|n| n == "xai"));

        let (provider, model) = reg.resolve_model("xai");
        assert_eq!(
            provider.config().provider_type,
            ProviderType::Xai,
            "bare xai must not silent-hop to openrouter"
        );
        assert_eq!(
            model,
            crate::xai::DEFAULT_MODEL,
            "bare xai uses provider default model id"
        );

        // Prefixed form still works (unchanged ADR-0044).
        let (provider2, model2) = reg.resolve_model("xai/grok-4.7");
        assert_eq!(provider2.config().provider_type, ProviderType::Xai);
        assert_eq!(model2, "grok-4.7");

        // Unrelated bare / unknown prefix still falls through to default.
        let (provider3, model3) = reg.resolve_model("moonshotai/kimi-k2.5");
        assert_eq!(provider3.name(), "openrouter");
        assert_eq!(model3, "moonshotai/kimi-k2.5");
    }

    /// #273: Active / chat path with bare unregistered `xai` → typed error,
    /// no fallback walk (mirrors #262 bare-alias chat test).
    #[tokio::test]
    async fn unregistered_bare_xai_chat_returns_typed_error() {
        let sections = vec![make_section(
            "openrouter",
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "moonshotai/kimi-k2.6",
        )];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, model) = reg.resolve_model("xai");
        assert_eq!(provider.config().provider_type, ProviderType::Xai);
        assert_eq!(model, crate::xai::DEFAULT_MODEL);
        let client = reqwest::Client::new();
        let err = provider
            .chat_completion(&client, &[], &[], model, 16)
            .await
            .unwrap_err();
        let typed = err.downcast_ref::<crate::xai::XaiMisconfiguredError>();
        assert!(
            typed.is_some(),
            "expected XaiMisconfiguredError, got {err:#}"
        );
        assert!(
            !should_walk_fallback(&err),
            "must not silent-hop via fallback"
        );
    }

    /// #273: registered-but-misconfigured `type=xai` + bare `xai` must still
    /// resolve to Xai (not OpenRouter), so Active validate / chat typed-reject.
    #[test]
    fn resolve_model_registered_bare_xai_does_not_silent_hop() {
        let mut xai = make_section(
            "xai",
            ProviderType::Xai,
            crate::xai::DEFAULT_BASE,
            "grok-4.7",
        );
        xai.api_key = None;
        let sections = vec![
            make_section(
                "openrouter",
                ProviderType::OpenRouter,
                "https://openrouter.ai/api/v1",
                "moonshotai/kimi-k2.6",
            ),
            xai,
        ];
        let reg = build_registry(&sections, "openrouter", 3, 0).unwrap();
        let (provider, model) = reg.resolve_model("xai");
        assert_eq!(provider.config().provider_type, ProviderType::Xai);
        assert_eq!(provider.name(), "xai");
        assert_eq!(model, "grok-4.7");
        assert!(
            !crate::xai::peer_configured(
                &provider.config().base_url,
                provider.config().api_key.as_deref()
            ),
            "fixture must stay misconfigured so Active validate rejects"
        );
    }
}
