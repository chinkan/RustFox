//! Telegram `/fallback` chain list / add / remove (ADR-0037 / #237).
//!
//! Pure helpers (no Telegram types) so unit tests cover empty list, add,
//! duplicate/cap/bad-model reject, and remove by index or string.

use crate::llm::MAX_FALLBACK_CHAIN;
use crate::provider::ProviderRegistry;

/// Usage tip appended to reject replies.
pub const FALLBACK_USAGE: &str =
    "Usage: `/fallback` | `/fallback add <provider/model>` | `/fallback remove <index|provider/model>`";

/// Validate a fully-qualified `provider/model` that registry-resolves.
///
/// Requires a known provider prefix and a non-empty model id. Bare models and
/// unknown prefixes are rejected (clear English error for Telegram).
pub fn validate_fallback_entry(registry: &ProviderRegistry, entry: &str) -> Result<String, String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(format!(
            "Missing model. Provide a fully-qualified `provider/model`.\n{FALLBACK_USAGE}"
        ));
    }
    let Some((provider, model)) = entry.split_once('/') else {
        return Err(format!(
            "`{entry}` is not fully-qualified. Use `provider/model` (e.g. `ollama/qwen3:8b`).\n{FALLBACK_USAGE}"
        ));
    };
    if provider.is_empty() || model.is_empty() {
        return Err(format!(
            "`{entry}` is not a valid `provider/model`.\n{FALLBACK_USAGE}"
        ));
    }
    if registry.get_provider(provider).is_none() {
        return Err(format!(
            "Unknown provider `{provider}` in `{entry}`. Check `/models` for known providers.\n{FALLBACK_USAGE}"
        ));
    }
    Ok(entry.to_string())
}

/// Append `entry` to `chain` or reject (duplicate / at-cap).
pub fn try_add(chain: &mut Vec<String>, entry: String) -> Result<(), String> {
    if chain.iter().any(|e| e == &entry) {
        return Err(format!(
            "`{entry}` is already in the fallback chain.\n{FALLBACK_USAGE}"
        ));
    }
    if chain.len() >= MAX_FALLBACK_CHAIN {
        return Err(format!(
            "Fallback chain is full ({MAX_FALLBACK_CHAIN}/{MAX_FALLBACK_CHAIN}). Remove an entry before adding.\n{FALLBACK_USAGE}"
        ));
    }
    chain.push(entry);
    Ok(())
}

/// Resolve remove arg to a 0-based index, or reject with tip.
///
/// Accepts a 1-based index (as shown in list) **or** exact `provider/model`.
pub fn resolve_remove_index(chain: &[String], arg: &str) -> Result<usize, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err(format!(
            "Missing remove target. Use a 1-based index or exact `provider/model`.\n{FALLBACK_USAGE}"
        ));
    }

    // Numeric → 1-based index (entries are always `provider/model`, never bare digits).
    if arg.chars().all(|c| c.is_ascii_digit()) {
        let n: usize = arg.parse().unwrap_or(0);
        if n == 0 || n > chain.len() {
            return Err(format!(
                "Index {arg} is out of range (chain has {} entr{}).\n{FALLBACK_USAGE}",
                chain.len(),
                if chain.len() == 1 { "y" } else { "ies" }
            ));
        }
        return Ok(n - 1);
    }

    match chain.iter().position(|e| e == arg) {
        Some(i) => Ok(i),
        None => Err(format!("No chain entry matches `{arg}`.\n{FALLBACK_USAGE}")),
    }
}

/// Remove by arg; returns the removed entry.
pub fn try_remove(chain: &mut Vec<String>, arg: &str) -> Result<String, String> {
    let idx = resolve_remove_index(chain, arg)?;
    Ok(chain.remove(idx))
}

/// Format ordered list reply (1-based) + usage. Empty-state when none.
pub fn format_fallback_list(chain: &[String]) -> String {
    let mut out = String::from("**Fallback chain**\n");
    if chain.is_empty() {
        out.push_str("_Empty — primary only (no backups)._\n");
    } else {
        for (i, entry) in chain.iter().enumerate() {
            out.push_str(&format!("{}. `{entry}`\n", i + 1));
        }
    }
    out.push('\n');
    out.push_str(FALLBACK_USAGE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{OpenRouterProvider, Provider, ProviderConfig, ProviderRegistry};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_cfg(name: &str, model: &str) -> ProviderConfig {
        use crate::config::ProviderType;
        use tokio::sync::RwLock;
        ProviderConfig {
            name: name.to_string(),
            provider_type: ProviderType::OpenRouter,
            base_url: "http://127.0.0.1:9".into(),
            api_key: Some("k".into()),
            default_model: model.to_string(),
            supports_vision: false,
            max_tokens: 10,
            discover_models: false,
            context_window: 100,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
            rate_limit_retry_limit: 0,
            extra_headers: vec![],
        }
    }

    fn test_registry() -> ProviderRegistry {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "ollama".to_string(),
            Arc::new(OpenRouterProvider::new(test_cfg("ollama", "qwen"))),
        );
        providers.insert(
            "openrouter".to_string(),
            Arc::new(OpenRouterProvider::new(test_cfg("openrouter", "kimi"))),
        );
        ProviderRegistry::new(providers, "openrouter".to_string())
    }

    #[test]
    fn list_empty_shows_empty_state_and_usage() {
        let text = format_fallback_list(&[]);
        assert!(text.contains("Empty"), "{text}");
        assert!(text.contains("/fallback add"), "{text}");
    }

    #[test]
    fn add_ok_appends() {
        let mut chain = vec![];
        try_add(&mut chain, "ollama/qwen".into()).unwrap();
        assert_eq!(chain, vec!["ollama/qwen".to_string()]);
    }

    #[test]
    fn add_duplicate_reject() {
        let mut chain = vec!["ollama/qwen".to_string()];
        let err = try_add(&mut chain, "ollama/qwen".into()).unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn add_at_cap_reject() {
        let mut chain: Vec<String> = (0..MAX_FALLBACK_CHAIN)
            .map(|i| format!("ollama/m{i}"))
            .collect();
        let err = try_add(&mut chain, "ollama/extra".into()).unwrap_err();
        assert!(
            err.contains("full") || err.contains(&MAX_FALLBACK_CHAIN.to_string()),
            "{err}"
        );
        assert_eq!(chain.len(), MAX_FALLBACK_CHAIN);
        assert!(!chain.iter().any(|e| e == "ollama/extra"));
    }

    #[test]
    fn add_bad_model_reject() {
        let reg = test_registry();
        let err = validate_fallback_entry(&reg, "nope/model").unwrap_err();
        assert!(
            err.contains("Unknown provider") || err.contains("nope"),
            "{err}"
        );
        let err = validate_fallback_entry(&reg, "bare-model").unwrap_err();
        assert!(
            err.contains("fully-qualified") || err.contains("provider/model"),
            "{err}"
        );
        assert!(validate_fallback_entry(&reg, "ollama/qwen3:8b").is_ok());
    }

    #[test]
    fn remove_by_index() {
        let mut chain = vec![
            "ollama/a".to_string(),
            "openrouter/b".to_string(),
            "ollama/c".to_string(),
        ];
        let removed = try_remove(&mut chain, "2").unwrap();
        assert_eq!(removed, "openrouter/b");
        assert_eq!(chain, vec!["ollama/a".to_string(), "ollama/c".to_string()]);
    }

    #[test]
    fn remove_by_string() {
        let mut chain = vec!["ollama/a".to_string(), "openrouter/b".to_string()];
        let removed = try_remove(&mut chain, "ollama/a").unwrap();
        assert_eq!(removed, "ollama/a");
        assert_eq!(chain, vec!["openrouter/b".to_string()]);
    }

    #[test]
    fn remove_oor() {
        let mut chain = vec!["ollama/a".to_string()];
        let err = try_remove(&mut chain, "3").unwrap_err();
        assert!(
            err.contains("out of range") || err.contains("range"),
            "{err}"
        );
        let err = try_remove(&mut chain, "0").unwrap_err();
        assert!(
            err.contains("out of range") || err.contains("range"),
            "{err}"
        );
        let err = try_remove(&mut chain, "missing/x").unwrap_err();
        assert!(
            err.contains("No chain entry") || err.contains("matches"),
            "{err}"
        );
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn remove_may_empty_chain() {
        let mut chain = vec!["ollama/a".to_string()];
        try_remove(&mut chain, "1").unwrap();
        assert!(chain.is_empty());
        assert!(format_fallback_list(&chain).contains("Empty"));
    }
}
