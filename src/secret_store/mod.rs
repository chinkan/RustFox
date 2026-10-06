//! Host-side secret store.
//!
//! Linux: encrypted file under the RustFox home (keyutils is per-session and
//! in-memory). macOS / Windows: OS keyring, falling back to the encrypted file.
//! Values are never logged (`SecretValue` redacts `Debug` / `Display`).
//!
//! Slice 2: pending claims + Telegram notify + portal claim form.
//! Slice 3: missing→notify, sandbox/tool env inject, redaction hooks.

mod bot_token;
mod bridge;
mod fake;
mod file;
mod keyring_backend;
mod notify;
mod pending;
mod value;

pub use bot_token::{
    bot_token_secret_name, bot_token_secret_ref, is_secret_ref, migrate_plaintext_bot_tokens,
    parse_secret_ref, resolve_bot_token, resolve_openrouter_api_key,
    seal_plaintext_bot_tokens_in_config, store_bot_token, store_secret, OPENROUTER_API_KEY_SECRET,
};
pub use bridge::{
    MissingSecret, MissingSecretError, SecretBridge, SecretNotifyFn, SECRET_REF_PREFIX,
};
pub use fake::FakeSecretStore;
pub use file::EncryptedFileSecretStore;
pub use keyring_backend::KeyringSecretStore;
pub use notify::{
    format_secret_request_notify, secret_request_claim_url, secret_request_notify_text,
};
pub use pending::{PendingCreate, PendingSecretRegistry, PendingView, DEFAULT_CLAIM_TTL};
pub use value::SecretValue;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Which concrete backend [`open`] selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreBackend {
    /// OS credential store (`keyring` crate).
    Keyring,
    /// AES-GCM encrypted vault under the RustFox home.
    EncryptedFile,
}

/// Named secret persistence used by the host (and later by portal / sandbox).
pub trait SecretStore: Send + Sync {
    /// Return the secret if present; `Ok(None)` when the name is unknown.
    fn get(&self, name: &str) -> Result<Option<SecretValue>>;

    /// Create or overwrite a secret.
    fn set(&self, name: &str, value: &str) -> Result<()>;

    /// Remove a secret; no-op / `Ok(())` if it did not exist.
    fn delete(&self, name: &str) -> Result<()>;

    /// Whether a secret with this name is stored.
    fn exists(&self, name: &str) -> Result<bool>;
}

/// Validate a secret name (stable identifier; not a free-form path).
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("secret name must not be empty");
    }
    if name.len() > 128 {
        anyhow::bail!("secret name too long (max 128)");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        anyhow::bail!("secret name must be alphanumeric / `_` / `-` / `.`");
    }
    if name.starts_with('.') || name.ends_with('.') {
        anyhow::bail!("secret name must not start or end with '.'");
    }
    Ok(())
}

/// Open the preferred store.
///
/// - Linux: always the encrypted file vault. The `keyring` crate's Linux
///   backend is kernel keyutils: in-memory, per session, cleared on reboot, so
///   a `systemctl --user` service never sees what the wizard wrote. Secrets
///   still in keyutils are copied into the vault on first read.
/// - macOS / Windows: OS keyring first; encrypted-file fallback.
///
/// `home` is the RustFox home root (typically `~/.rustfox`). The file vault
/// lives at `<home>/secrets/vault` with master key `<home>/secrets/vault.key`.
pub fn open(home: &Path) -> Result<(Box<dyn SecretStore>, SecretStoreBackend)> {
    if cfg!(target_os = "linux") {
        tracing::debug!("secret store: using encrypted file vault (Linux)");
        let store = KeyutilsMigratingVault {
            vault: open_vault(home)?,
            legacy: Box::new(KeyringSecretStore),
        };
        return Ok((Box::new(store), SecretStoreBackend::EncryptedFile));
    }
    match KeyringSecretStore::try_probe_and_open() {
        Ok(store) => {
            tracing::debug!("secret store: using OS keyring");
            Ok((Box::new(store), SecretStoreBackend::Keyring))
        }
        Err(err) => {
            tracing::info!(
                error = %err,
                "OS keyring unavailable; using encrypted-file secret store"
            );
            Ok((
                Box::new(open_vault(home)?),
                SecretStoreBackend::EncryptedFile,
            ))
        }
    }
}

fn open_vault(home: &Path) -> Result<EncryptedFileSecretStore> {
    let vault = default_vault_path(home);
    EncryptedFileSecretStore::open(&vault)
        .with_context(|| format!("open encrypted secret vault at {}", vault.display()))
}

/// Linux vault that copies a secret from the old keyutils store on first miss.
///
/// The vault always wins when it has the name. A copied value is read back
/// from the vault before it is returned; a mismatch is an error, not a
/// silent success. `delete` clears both so a removed secret cannot come back.
struct KeyutilsMigratingVault {
    vault: EncryptedFileSecretStore,
    legacy: Box<dyn SecretStore>,
}

impl SecretStore for KeyutilsMigratingVault {
    fn get(&self, name: &str) -> Result<Option<SecretValue>> {
        if let Some(v) = self.vault.get(name)? {
            return Ok(Some(v));
        }
        let old = match self.legacy.get(name) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(None),
            Err(e) => {
                tracing::debug!(error = %e, "keyutils lookup skipped");
                return Ok(None);
            }
        };
        set_verified(&self.vault, name, old.expose())
            .with_context(|| format!("migrate secret `{name}` from keyutils to vault"))?;
        tracing::info!("secret store: migrated `{name}` from keyutils to vault");
        Ok(Some(old))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.vault.set(name, value)
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.vault.delete(name)?;
        if let Err(e) = self.legacy.delete(name) {
            tracing::debug!(error = %e, "keyutils delete skipped");
        }
        Ok(())
    }

    fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.get(name)?.is_some())
    }
}

/// `set`, then read back and compare. Fails instead of reporting a fake success.
fn set_verified(store: &dyn SecretStore, name: &str, value: &str) -> Result<()> {
    store.set(name, value)?;
    match store.get(name)? {
        Some(v) if v.expose() == value => Ok(()),
        _ => anyhow::bail!("secret `{name}` did not read back after write"),
    }
}

/// Default encrypted vault path under the RustFox home.
pub fn default_vault_path(home: &Path) -> PathBuf {
    home.join("secrets").join("vault")
}

/// Default plaintext master-key path beside the vault (file-fallback only).
pub fn default_vault_key_path(home: &Path) -> PathBuf {
    home.join("secrets").join("vault.key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn validate_name_accepts_safe_identifiers() {
        assert!(validate_name("OPENROUTER_API_KEY").is_ok());
        assert!(validate_name("bot.token-1").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty_and_path_like() {
        assert!(validate_name("").is_err());
        assert!(validate_name("../etc/passwd").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(".hidden").is_err());
    }

    #[test]
    fn open_selects_a_usable_backend() {
        let dir = tempdir().unwrap();
        let (store, backend) = open(dir.path()).expect("open secret store");
        if cfg!(target_os = "linux") {
            assert_eq!(backend, SecretStoreBackend::EncryptedFile);
        }
        store
            .set("SLICE1_PROBE", "round-trip-value-xyz")
            .expect("set");
        assert!(store.exists("SLICE1_PROBE").unwrap());
        let got = store.get("SLICE1_PROBE").unwrap().unwrap();
        assert_eq!(got.expose(), "round-trip-value-xyz");
        store.delete("SLICE1_PROBE").unwrap();
        assert!(!store.exists("SLICE1_PROBE").unwrap());
    }

    fn vault_with_legacy(home: &Path, legacy: FakeSecretStore) -> KeyutilsMigratingVault {
        KeyutilsMigratingVault {
            vault: open_vault(home).unwrap(),
            legacy: Box::new(legacy),
        }
    }

    #[test]
    fn linux_open_writes_the_vault_another_process_can_read() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let dir = tempdir().unwrap();
        let (store, _) = open(dir.path()).unwrap();
        store_bot_token(store.as_ref(), "default", "123:wizard-token").unwrap();
        drop(store);
        // A fresh open (the systemd service) reads the same vault file.
        let (service, _) = open(dir.path()).unwrap();
        let token = resolve_bot_token(service.as_ref(), "secret:bot.default.token").unwrap();
        assert_eq!(token, "123:wizard-token");
    }

    #[test]
    fn keyutils_only_secret_is_migrated_into_the_vault() {
        let dir = tempdir().unwrap();
        let legacy = FakeSecretStore::with_secrets([("bot.default.token", "123:old")]).unwrap();
        let store = vault_with_legacy(dir.path(), legacy);
        assert_eq!(
            store.get("bot.default.token").unwrap().unwrap().expose(),
            "123:old"
        );
        // Now persisted: a fresh vault with an empty keyutils still has it.
        let fresh = vault_with_legacy(dir.path(), FakeSecretStore::new());
        assert_eq!(
            fresh.get("bot.default.token").unwrap().unwrap().expose(),
            "123:old"
        );
    }

    #[test]
    fn vault_wins_when_both_have_the_secret() {
        let dir = tempdir().unwrap();
        open_vault(dir.path())
            .unwrap()
            .set("bot.default.token", "123:vault")
            .unwrap();
        let legacy = FakeSecretStore::with_secrets([("bot.default.token", "123:old")]).unwrap();
        let store = vault_with_legacy(dir.path(), legacy);
        assert_eq!(
            store.get("bot.default.token").unwrap().unwrap().expose(),
            "123:vault"
        );
    }

    #[test]
    fn delete_clears_keyutils_so_the_secret_does_not_come_back() {
        let dir = tempdir().unwrap();
        let legacy = FakeSecretStore::with_secrets([("OLD", "v")]).unwrap();
        let store = vault_with_legacy(dir.path(), legacy);
        assert!(store.exists("OLD").unwrap());
        store.delete("OLD").unwrap();
        assert!(!store.exists("OLD").unwrap());
    }

    #[test]
    fn set_verified_fails_when_the_value_does_not_read_back() {
        struct Drops;
        impl SecretStore for Drops {
            fn get(&self, _: &str) -> Result<Option<SecretValue>> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<()> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<()> {
                Ok(())
            }
            fn exists(&self, _: &str) -> Result<bool> {
                Ok(false)
            }
        }
        assert!(store_bot_token(&Drops, "default", "123:x").is_err());
    }
}
