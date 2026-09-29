//! One-shot pending secret claims (Slice 2).
//!
//! A pending request holds a secret *name* and an opaque claim token with a
//! short TTL. The token is returned once to the caller (for building a portal
//! claim URL) and must never be logged or echoed into Telegram message *text*
//! (the claim URL path may carry the opaque id).

use super::{validate_name, SecretStore};
use anyhow::{bail, Context, Result};
use rand::RngCore;
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default claim lifetime (short; one-shot).
pub const DEFAULT_CLAIM_TTL: Duration = Duration::from_secs(15 * 60);

const TOKEN_BYTES: usize = 32;

/// View of a pending claim safe to return to the portal (no raw token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingView {
    pub id: String,
    pub name: String,
    pub expires_at_unix: u64,
}

/// Result of creating a pending claim. `claim_token` is returned **once**.
pub struct PendingCreate {
    pub id: String,
    pub name: String,
    /// Opaque one-shot token — put in the claim URL path only; never log.
    pub claim_token: String,
    pub expires_at_unix: u64,
}

impl fmt::Debug for PendingCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingCreate")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("claim_token", &"[REDACTED]")
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

struct PendingEntry {
    id: String,
    name: String,
    expires_at: Instant,
    expires_at_unix: u64,
}

/// In-memory registry of pending secret claims.
pub struct PendingSecretRegistry {
    ttl: Duration,
    /// Keyed by opaque claim token.
    by_token: Mutex<HashMap<String, PendingEntry>>,
}

impl Default for PendingSecretRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_CLAIM_TTL)
    }
}

impl PendingSecretRegistry {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            by_token: Mutex::new(HashMap::new()),
        }
    }

    /// Create a pending claim for `name`. Returns the one-shot token once.
    pub fn request(&self, name: &str) -> Result<PendingCreate> {
        validate_name(name)?;
        let claim_token = generate_token();
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = Instant::now() + self.ttl;
        let expires_at_unix = system_unix_now().saturating_add(self.ttl.as_secs()).max(1);
        let entry = PendingEntry {
            id: id.clone(),
            name: name.to_string(),
            expires_at,
            expires_at_unix,
        };
        self.by_token
            .lock()
            .expect("pending secret registry lock")
            .insert(claim_token.clone(), entry);
        Ok(PendingCreate {
            id,
            name: name.to_string(),
            claim_token,
            expires_at_unix,
        })
    }

    /// Look up a pending claim by token. Expired entries are purged.
    pub fn peek(&self, claim_token: &str) -> Result<PendingView> {
        let mut map = self.by_token.lock().expect("pending secret registry lock");
        Self::purge_expired_locked(&mut map);
        let entry = map
            .get(claim_token)
            .ok_or_else(|| anyhow::anyhow!("pending claim not found or expired"))?;
        Ok(PendingView {
            id: entry.id.clone(),
            name: entry.name.clone(),
            expires_at_unix: entry.expires_at_unix,
        })
    }

    /// Submit value via `SecretStore::set`, then consume the one-shot token.
    ///
    /// The pending entry is only removed after a successful `set`. On store
    /// failure the claim stays usable (token restored).
    pub fn submit(&self, claim_token: &str, value: &str, store: &dyn SecretStore) -> Result<()> {
        if value.is_empty() {
            bail!("secret value must not be empty");
        }
        let mut map = self.by_token.lock().expect("pending secret registry lock");
        Self::purge_expired_locked(&mut map);
        let entry = map
            .remove(claim_token)
            .ok_or_else(|| anyhow::anyhow!("pending claim not found or expired"))?;
        // Drop the lock before touching the store (backends may block).
        drop(map);
        if let Err(e) = store.set(&entry.name, value) {
            // Restore so the one-shot claim remains usable after a store failure.
            self.by_token
                .lock()
                .expect("pending secret registry lock")
                .insert(claim_token.to_string(), entry);
            return Err(e)
                .with_context(|| "SecretStore::set for pending (token restored)".to_string());
        }
        Ok(())
    }

    /// Cancel / clear a pending claim by opaque token. Returns true if removed.
    pub fn cancel(&self, claim_token: &str) -> bool {
        let mut map = self.by_token.lock().expect("pending secret registry lock");
        Self::purge_expired_locked(&mut map);
        map.remove(claim_token).is_some()
    }

    /// Cancel by pending id (portal list / admin). Returns true if removed.
    pub fn cancel_by_id(&self, id: &str) -> bool {
        let mut map = self.by_token.lock().expect("pending secret registry lock");
        Self::purge_expired_locked(&mut map);
        let token = map.iter().find(|(_, e)| e.id == id).map(|(t, _)| t.clone());
        match token {
            Some(t) => map.remove(&t).is_some(),
            None => false,
        }
    }

    /// List non-expired pending claims (no tokens).
    pub fn list(&self) -> Vec<PendingView> {
        let mut map = self.by_token.lock().expect("pending secret registry lock");
        Self::purge_expired_locked(&mut map);
        map.values()
            .map(|e| PendingView {
                id: e.id.clone(),
                name: e.name.clone(),
                expires_at_unix: e.expires_at_unix,
            })
            .collect()
    }

    fn purge_expired_locked(map: &mut HashMap<String, PendingEntry>) {
        let now = Instant::now();
        map.retain(|_, e| e.expires_at > now);
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn system_unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::{FakeSecretStore, SecretValue};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn pending_create_portal_claim_set() {
        let reg = PendingSecretRegistry::new(Duration::from_secs(60));
        let store = FakeSecretStore::new();
        let created = reg.request("OPENROUTER_API_KEY").unwrap();
        assert!(!format!("{created:?}").contains(&created.claim_token));

        let view = reg.peek(&created.claim_token).unwrap();
        assert_eq!(view.name, "OPENROUTER_API_KEY");
        assert_eq!(view.id, created.id);

        reg.submit(&created.claim_token, "sk-live-VALUE-never-in-chat", &store)
            .unwrap();
        assert_eq!(
            store.get("OPENROUTER_API_KEY").unwrap().unwrap().expose(),
            "sk-live-VALUE-never-in-chat"
        );
        // One-shot: second submit fails.
        assert!(reg.submit(&created.claim_token, "other", &store).is_err());
        assert!(reg.peek(&created.claim_token).is_err());
    }

    #[test]
    fn pending_expires() {
        let reg = PendingSecretRegistry::new(Duration::from_millis(30));
        let created = reg.request("SHORT_TTL_SECRET").unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(reg.peek(&created.claim_token).is_err());
        let store = FakeSecretStore::new();
        assert!(reg
            .submit(&created.claim_token, "too-late", &store)
            .is_err());
        assert!(!store.exists("SHORT_TTL_SECRET").unwrap());
    }

    #[test]
    fn cancel_clears_pending() {
        let reg = PendingSecretRegistry::default();
        let created = reg.request("TO_CANCEL").unwrap();
        assert!(reg.cancel(&created.claim_token));
        assert!(reg.peek(&created.claim_token).is_err());
        assert!(!reg.cancel(&created.claim_token));

        let created2 = reg.request("TO_CANCEL_BY_ID").unwrap();
        assert!(reg.cancel_by_id(&created2.id));
        assert!(reg.peek(&created2.claim_token).is_err());
    }

    #[test]
    fn submit_restores_pending_when_set_fails() {
        struct FailStore;
        impl SecretStore for FailStore {
            fn get(&self, _name: &str) -> Result<Option<SecretValue>> {
                Ok(None)
            }
            fn set(&self, _name: &str, _value: &str) -> Result<()> {
                bail!("simulated store failure");
            }
            fn delete(&self, _name: &str) -> Result<()> {
                Ok(())
            }
            fn exists(&self, _name: &str) -> Result<bool> {
                Ok(false)
            }
        }
        let reg = PendingSecretRegistry::new(Duration::from_secs(60));
        let created = reg.request("FAIL_SET").unwrap();
        let err = reg
            .submit(&created.claim_token, "value-should-not-stick", &FailStore)
            .unwrap_err();
        assert!(err.to_string().contains("simulated") || err.to_string().contains("restored"));
        // Token still valid — peek works; nothing stored.
        let view = reg.peek(&created.claim_token).unwrap();
        assert_eq!(view.name, "FAIL_SET");
        let ok_store = FakeSecretStore::new();
        reg.submit(&created.claim_token, "after-restore-VALUE", &ok_store)
            .unwrap();
        assert_eq!(
            ok_store.get("FAIL_SET").unwrap().unwrap().expose(),
            "after-restore-VALUE"
        );
    }
}
