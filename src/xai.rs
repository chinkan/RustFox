//! xAI Active provider peer (ADR-0044 / #266).
//!
//! Direct OpenAI-compat Chat Completions at `https://api.x.ai/v1`.
//! Misconfigured peer (missing key / empty base URL) → typed error; never
//! silent-hop to another provider type.

/// Default OpenAI-compat base (Chat Completions under `/chat/completions`).
pub const DEFAULT_BASE: &str = "https://api.x.ai/v1";

/// Default Active model id (ADR-0044 lock).
pub const DEFAULT_MODEL: &str = "grok-4.7";

/// Reserved resolve alias so bare `xai` and `xai/…` never silent-hop to the
/// default provider when no `[[provider]] type=xai` section is registered
/// (ADR-0044 §9 / #273).
pub const MODEL_ALIAS: &str = "xai";

/// Canonical SecretStore name for the xAI API key (ADR-0016 spirit).
pub const API_KEY_SECRET: &str = "xai.api_key";

/// Peer is usable for Active / chat when base URL and API key are both non-empty.
pub fn peer_configured(base_url: &str, api_key: Option<&str>) -> bool {
    !base_url.trim().is_empty() && api_key.is_some_and(|k| !k.trim().is_empty())
}

/// Typed refusal when xAI is selected or hit while misconfigured (ADR-0044 §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XaiMisconfiguredError {
    /// `chat_completion` / LLM path.
    Chat,
    /// `/models` Active / `set_model` / `default_provider`.
    Active,
}

impl XaiMisconfiguredError {
    pub fn chat() -> Self {
        Self::Chat
    }
    pub fn active() -> Self {
        Self::Active
    }
}

impl std::fmt::Display for XaiMisconfiguredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chat => write!(
                f,
                "xAI is misconfigured (missing API key or base URL); chat is not available (ADR-0044)"
            ),
            Self::Active => write!(
                f,
                "xAI cannot be Active while misconfigured (set api_key + base_url; ADR-0044)"
            ),
        }
    }
}

impl std::error::Error for XaiMisconfiguredError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_configured_requires_base_and_key() {
        assert!(!peer_configured("", Some("sk")));
        assert!(!peer_configured(DEFAULT_BASE, None));
        assert!(!peer_configured(DEFAULT_BASE, Some("")));
        assert!(!peer_configured(DEFAULT_BASE, Some("  ")));
        assert!(peer_configured(DEFAULT_BASE, Some("sk-xai")));
    }

    #[test]
    fn default_model_is_grok_4_7() {
        assert_eq!(DEFAULT_MODEL, "grok-4.7");
    }
}
