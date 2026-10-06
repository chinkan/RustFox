//! Bot Telegram tokens and the `[openrouter].api_key` (ADR 0016) in [`SecretStore`].
//!
//! Config holds `secret:NAME` (typically `secret:bot.<id>.token`, or
//! `secret:openrouter.api_key`) — never the plaintext after bind/migrate.
//! Runtime resolves via the store.
//! One-time startup migration moves legacy plaintext into the store and scrubs
//! `config.toml` (bak via [`crate::config_edit::write_config_validated`]).

use super::{set_verified, validate_name, SecretStore, SECRET_REF_PREFIX};
use crate::agents_edit::looks_like_bot_token;
use crate::config::Config;
use crate::config_edit::write_config_validated;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Canonical secret name for a bot's Telegram token.
pub fn bot_token_secret_name(bot_id: &str) -> String {
    format!("bot.{}.token", bot_id.trim())
}

/// Config value form: `secret:bot.<id>.token`.
pub fn bot_token_secret_ref(bot_id: &str) -> String {
    format!("{}{}", SECRET_REF_PREFIX, bot_token_secret_name(bot_id))
}

/// Vault name for `[openrouter].api_key` (ADR 0016).
pub const OPENROUTER_API_KEY_SECRET: &str = "openrouter.api_key";

/// Whether `value` is a `secret:NAME` reference.
pub fn is_secret_ref(value: &str) -> bool {
    value.trim().starts_with(SECRET_REF_PREFIX)
}

/// Strip `secret:` prefix; `None` if not a ref or empty name.
pub fn parse_secret_ref(value: &str) -> Option<&str> {
    value
        .trim()
        .strip_prefix(SECRET_REF_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Resolve a configured `bot_token` field to the raw BotFather token.
///
/// - `secret:NAME` → store lookup (error if missing)
/// - BotFather-shaped plaintext → returned as-is (pre-migration / tests)
/// - anything else → error
pub fn resolve_bot_token(store: &dyn SecretStore, configured: &str) -> Result<String> {
    let configured = configured.trim();
    if configured.is_empty() {
        bail!("bot_token is empty");
    }
    if let Some(name) = parse_secret_ref(configured) {
        validate_name(name)?;
        match store.get(name)? {
            Some(v) => Ok(v.expose().to_string()),
            None => bail!("secret `{name}` not found in SecretStore (bot token)"),
        }
    } else if looks_like_bot_token(configured) {
        Ok(configured.to_string())
    } else {
        bail!("bot_token is neither a secret:NAME ref nor a BotFather token");
    }
}

/// Resolve `secret:openrouter.api_key` in place (ADR 0016). Call before
/// [`Config::build_providers`] so the legacy `[openrouter]` provider copy gets
/// the real key. Goes through the bridge so the value is seeded for redaction.
/// Plaintext / empty keys are left as-is (pre-migrate, Ollama-only).
pub fn resolve_openrouter_api_key(cfg: &mut Config, bridge: &super::SecretBridge) -> Result<()> {
    if let Some(name) = parse_secret_ref(&cfg.openrouter.api_key) {
        validate_name(name)?;
        let value = bridge.get(name)?.with_context(|| {
            format!("secret `{name}` not found in SecretStore ([openrouter].api_key)")
        })?;
        cfg.openrouter.api_key = value.expose().to_string();
    }
    Ok(())
}

/// Persist `plaintext` under `name` (read back via `set_verified`) and return `secret:<name>`.
pub fn store_secret(store: &dyn SecretStore, name: &str, plaintext: &str) -> Result<String> {
    validate_name(name)?;
    let value = plaintext.trim();
    if value.is_empty() {
        bail!("secret `{name}` cannot be empty");
    }
    set_verified(store, name, value)?;
    Ok(format!("{SECRET_REF_PREFIX}{name}"))
}

/// Persist plaintext token under `bot.<id>.token` and return the `secret:…` config value.
pub fn store_bot_token(store: &dyn SecretStore, bot_id: &str, plaintext: &str) -> Result<String> {
    store_secret(store, &bot_token_secret_name(bot_id), plaintext)
}

/// Seal BotFather-shaped plaintext `bot_token` values in a config TOML string.
///
/// Writes each token into [`SecretStore`] under `bot.<id>.token` and replaces the
/// config value with `secret:bot.<id>.token`. Placeholders and existing
/// `secret:NAME` refs are left alone. Does **not** materialize `[[bots]]` from
/// legacy `[telegram]` — only scrubs in place (wizard first-save stays
/// `[telegram]`-shaped).
///
/// Also seals a non-empty plaintext `[openrouter].api_key` as
/// `secret:openrouter.api_key` (ADR 0016). `[embedding]` and `[[provider]]`
/// keys are deliberately left alone.
///
/// Returns `(sealed_toml, credentials_stored_or_scrubbed)`.
pub fn seal_plaintext_bot_tokens_in_config(
    content: &str,
    store: &dyn SecretStore,
) -> Result<(String, usize)> {
    let mut doc: toml::Value =
        toml::from_str(content).context("Failed to parse config.toml before bot-token seal")?;
    let table = doc
        .as_table_mut()
        .context("config.toml root is not a table")?;

    let mut changed = 0usize;

    // Scrub [[bots]] rows first so [telegram] can reuse the shim ref.
    if let Some(arr) = table.get("bots").and_then(|v| v.as_array()).cloned() {
        let mut new_arr = Vec::with_capacity(arr.len());
        for bot_val in arr {
            let mut bot_val = bot_val;
            if let Some(bot) = bot_val.as_table_mut() {
                let id = bot
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let token = bot
                    .get("bot_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !id.is_empty() && looks_like_bot_token(&token) {
                    let secret_ref = store_bot_token(store, &id, &token)?;
                    bot.insert("bot_token".into(), toml::Value::String(secret_ref));
                    changed += 1;
                }
            }
            new_arr.push(bot_val);
        }
        table.insert("bots".into(), toml::Value::Array(new_arr));
    }

    let telegram_token = table
        .get("telegram")
        .and_then(|v| v.as_table())
        .and_then(|tg| tg.get("bot_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    if let Some(token) = telegram_token {
        if looks_like_bot_token(&token) {
            let shim_id = shim_bot_id_from_table(table).unwrap_or_else(|| "default".to_string());
            let secret_ref = match bot_token_field_for_id(table, &shim_id) {
                Some(existing) if is_secret_ref(&existing) => existing,
                _ => store_bot_token(store, &shim_id, &token)?,
            };
            if let Some(tg) = table.get_mut("telegram").and_then(|v| v.as_table_mut()) {
                tg.insert("bot_token".into(), toml::Value::String(secret_ref));
                changed += 1;
            }
        }
    }

    if let Some(or) = table.get_mut("openrouter").and_then(|v| v.as_table_mut()) {
        let key = or
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !key.is_empty() && !is_secret_ref(&key) {
            let secret_ref = store_secret(store, OPENROUTER_API_KEY_SECRET, &key)?;
            or.insert("api_key".into(), toml::Value::String(secret_ref));
            changed += 1;
        }
    }

    if changed == 0 {
        return Ok((content.to_string(), 0));
    }
    let sealed =
        toml::to_string_pretty(&doc).context("Failed to serialize config after bot-token seal")?;
    Ok((sealed, changed))
}

fn shim_bot_id_from_table(table: &toml::map::Map<String, toml::Value>) -> Option<String> {
    let arr = table.get("bots")?.as_array()?;
    let mut ids: Vec<String> = Vec::new();
    for bot in arr {
        if let Some(id) = bot
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
        {
            if !id.is_empty() {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return None;
    }
    if let Some(main) = ids.iter().find(|id| *id == "main") {
        return Some(main.clone());
    }
    if let Some(default) = ids.iter().find(|id| *id == "default") {
        return Some(default.clone());
    }
    Some(ids[0].clone())
}

fn bot_token_field_for_id(table: &toml::map::Map<String, toml::Value>, id: &str) -> Option<String> {
    let arr = table.get("bots")?.as_array()?;
    for bot in arr {
        let bot_id = bot.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
        if bot_id == id {
            return bot
                .get("bot_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// One-time migrate: BotFather-shaped plaintext in `[[bots]]` / `[telegram]` →
/// SecretStore + scrub on disk (safety net after wizard/bind seal).
///
/// Returns the number of tokens scrubbed. No-op when everything is already a
/// `secret:NAME` ref or a non-token placeholder.
pub fn migrate_plaintext_bot_tokens(config_path: &Path, store: &dyn SecretStore) -> Result<usize> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    // Validate shape early so we do not seal a broken file onto disk.
    let mut cfg: Config =
        toml::from_str(&content).context("Failed to parse config.toml before bot-token migrate")?;
    cfg.normalize_bots()
        .context("bots / telegram validation failed before bot-token migrate")?;

    let (sealed, changed) = seal_plaintext_bot_tokens_in_config(&content, store)?;
    if changed == 0 {
        return Ok(0);
    }
    write_config_validated(config_path, &sealed)
        .context("Failed to write scrubbed config after bot-token migrate")?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::SecretStore;
    use super::*;
    use crate::secret_store::FakeSecretStore;
    use tempfile::TempDir;

    fn minimal_bots_toml() -> String {
        r#"
[[bots]]
id = "main"
bot_token = "111111111:AAMainTokenSecretValueXXXX"
allowed_user_ids = [42]
persona = "main"

[openrouter]
api_key = "sk-test"
model = "test-model"

[sandbox]
allowed_directory = "/tmp"
"#
        .to_string()
    }

    #[test]
    fn naming_and_ref_roundtrip() {
        assert_eq!(bot_token_secret_name("main"), "bot.main.token");
        assert_eq!(bot_token_secret_ref("main"), "secret:bot.main.token");
        assert!(is_secret_ref("secret:bot.main.token"));
        assert_eq!(
            parse_secret_ref("secret:bot.main.token"),
            Some("bot.main.token")
        );
        assert!(!is_secret_ref("111111111:AAxxxx"));
    }

    #[test]
    fn resolve_from_store_and_plaintext() {
        let store = FakeSecretStore::new();
        store
            .set("bot.main.token", "111111111:AAMainTokenSecretValueXXXX")
            .unwrap();
        let got = resolve_bot_token(&store, "secret:bot.main.token").unwrap();
        assert_eq!(got, "111111111:AAMainTokenSecretValueXXXX");
        let plain = resolve_bot_token(&store, "222222222:AAPlainTokenSecretValueYY").unwrap();
        assert_eq!(plain, "222222222:AAPlainTokenSecretValueYY");
        assert!(resolve_bot_token(&store, "secret:missing.token").is_err());
    }

    #[test]
    fn migrate_scrubs_config_and_stores() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, minimal_bots_toml()).unwrap();
        let store = FakeSecretStore::new();
        let n = migrate_plaintext_bot_tokens(&path, &store).unwrap();
        assert_eq!(n, 2, "bot token + [openrouter].api_key");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("AAMainTokenSecretValueXXXX"));
        assert!(after.contains("secret:bot.main.token"));
        assert!(!after.contains("sk-test"));
        assert!(after.contains(r#"api_key = "secret:openrouter.api_key""#));
        assert_eq!(
            store.get("openrouter.api_key").unwrap().unwrap().expose(),
            "sk-test"
        );
        let bak = std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap();
        assert!(bak.contains("sk-test"), ".bak keeps the pre-scrub file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("config.toml.bak"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, ".bak is owner-only");
        }
        assert_eq!(
            store.get("bot.main.token").unwrap().unwrap().expose(),
            "111111111:AAMainTokenSecretValueXXXX"
        );
        // Idempotent
        assert_eq!(migrate_plaintext_bot_tokens(&path, &store).unwrap(), 0);
    }

    #[test]
    fn store_bot_token_writes_ref() {
        let store = FakeSecretStore::new();
        let r = store_bot_token(&store, "researcher", "333333333:AAResearchTokenSecretZZ").unwrap();
        assert_eq!(r, "secret:bot.researcher.token");
        assert!(store.exists("bot.researcher.token").unwrap());
    }

    #[test]
    fn seal_scrubs_telegram_only_first_save() {
        let store = FakeSecretStore::new();
        let raw = r#"
[telegram]
bot_token = "111111111:AAWizardFirstSaveTokenXXXX"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-test"
model = "test-model"
"#;
        let (sealed, n) = seal_plaintext_bot_tokens_in_config(raw, &store).unwrap();
        assert_eq!(n, 2);
        assert!(!sealed.contains("AAWizardFirstSaveTokenXXXX"));
        assert!(!sealed.contains("sk-test"));
        assert!(sealed.contains("secret:bot.default.token"));
        assert!(!sealed.contains("[[bots]]"));
        assert_eq!(
            store.get("bot.default.token").unwrap().unwrap().expose(),
            "111111111:AAWizardFirstSaveTokenXXXX"
        );
        // Idempotent
        let (_, n2) = seal_plaintext_bot_tokens_in_config(&sealed, &store).unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn seal_leaves_placeholder_alone() {
        let store = FakeSecretStore::new();
        let raw = r#"
[telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_user_ids = [1]
"#;
        let (sealed, n) = seal_plaintext_bot_tokens_in_config(raw, &store).unwrap();
        assert_eq!(n, 0);
        assert!(sealed.contains("YOUR_TELEGRAM_BOT_TOKEN"));
    }

    #[test]
    fn seal_openrouter_key_leaves_refs_empty_and_other_keys_alone() {
        let store = FakeSecretStore::new();
        let raw = r#"
[openrouter]
api_key = "secret:openrouter.api_key"
model = "m"

[embedding]
api_key = "sk-embed"
base_url = "https://openrouter.ai/api/v1"
model = "e"
dimensions = 8

[[provider]]
name = "other"
type = "openai"
api_key = "sk-provider"
model = "m"
"#;
        let (_, n) = seal_plaintext_bot_tokens_in_config(raw, &store).unwrap();
        assert_eq!(
            n, 0,
            "ref stays; [embedding]/[[provider]] out of scope (ADR 0016)"
        );

        let ollama_only = "[openrouter]\napi_key = \"\"\nmodel = \"m\"\n";
        let (_, n) = seal_plaintext_bot_tokens_in_config(ollama_only, &store).unwrap();
        assert_eq!(n, 0, "empty key is not sealed");
        assert!(!store.exists(OPENROUTER_API_KEY_SECRET).unwrap());
    }

    #[test]
    fn resolved_openrouter_key_reaches_the_legacy_provider() {
        use crate::secret_store::{PendingSecretRegistry, SecretBridge};
        use std::sync::Arc;
        let store = Arc::new(FakeSecretStore::new());
        store_secret(store.as_ref(), OPENROUTER_API_KEY_SECRET, "sk-or-x").unwrap();
        let bridge = SecretBridge::new(store, Arc::new(PendingSecretRegistry::default()));

        let toml = minimal_bots_toml().replace("sk-test", "secret:openrouter.api_key");
        let mut cfg: Config = toml::from_str(&toml).unwrap();
        resolve_openrouter_api_key(&mut cfg, &bridge).unwrap();
        let (providers, _, _) = cfg.build_providers();
        let or = providers.iter().find(|p| p.name == "openrouter").unwrap();
        assert_eq!(or.api_key.as_deref(), Some("sk-or-x"));

        let toml = minimal_bots_toml().replace("sk-test", "secret:openrouter.missing");
        let mut cfg: Config = toml::from_str(&toml).unwrap();
        assert!(resolve_openrouter_api_key(&mut cfg, &bridge).is_err());

        let mut cfg: Config = toml::from_str(&minimal_bots_toml()).unwrap();
        resolve_openrouter_api_key(&mut cfg, &bridge).unwrap();
        assert_eq!(
            cfg.openrouter.api_key, "sk-test",
            "plaintext passes through"
        );
    }
}
