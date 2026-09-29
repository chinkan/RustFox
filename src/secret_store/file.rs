//! AES-GCM encrypted file vault (fallback when OS keyring is unavailable).

use super::{validate_name, SecretStore, SecretValue};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const VAULT_VERSION: u32 = 1;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Serialize, Deserialize)]
struct VaultDisk {
    version: u32,
    /// Base64 of 12-byte nonce.
    nonce: String,
    /// Base64 of AES-GCM ciphertext over JSON `HashMap<String, String>`.
    ciphertext: String,
}

/// Encrypted-file `SecretStore`. Key material lives next to the vault file.
pub struct EncryptedFileSecretStore {
    vault_path: PathBuf,
    key: [u8; KEY_LEN],
    cache: Mutex<HashMap<String, String>>,
}

impl EncryptedFileSecretStore {
    /// Open (or create) a vault at `vault_path`. Master key: `<vault>.key`.
    pub fn open(vault_path: &Path) -> Result<Self> {
        let key_path = key_path_for(vault_path);
        if let Some(parent) = vault_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create secrets dir {}", parent.display()))?;
        }
        let key = load_or_create_key(&key_path)?;
        let cache = if vault_path.exists() {
            decrypt_vault(vault_path, &key)?
        } else {
            HashMap::new()
        };
        Ok(Self {
            vault_path: vault_path.to_path_buf(),
            key,
            cache: Mutex::new(cache),
        })
    }

    fn persist_locked(&self, map: &HashMap<String, String>) -> Result<()> {
        encrypt_and_write(&self.vault_path, &self.key, map)
    }
}

impl SecretStore for EncryptedFileSecretStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>> {
        validate_name(name)?;
        let map = self.cache.lock().expect("file secret store lock");
        Ok(map.get(name).map(|v| SecretValue::new(v.clone())))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        validate_name(name)?;
        let mut map = self.cache.lock().expect("file secret store lock");
        map.insert(name.to_string(), value.to_string());
        self.persist_locked(&map)?;
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let mut map = self.cache.lock().expect("file secret store lock");
        map.remove(name);
        self.persist_locked(&map)?;
        Ok(())
    }

    fn exists(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        let map = self.cache.lock().expect("file secret store lock");
        Ok(map.contains_key(name))
    }
}

fn key_path_for(vault_path: &Path) -> PathBuf {
    let mut p = vault_path.to_path_buf();
    let file_name = vault_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("vault");
    p.set_file_name(format!("{file_name}.key"));
    p
}

fn load_or_create_key(key_path: &Path) -> Result<[u8; KEY_LEN]> {
    if key_path.exists() {
        let bytes = fs::read(key_path)
            .with_context(|| format!("read secret vault key {}", key_path.display()))?;
        // Convert straight from the on-disk bytes; the key is never
        // materialised from a constant-initialised buffer.
        let key = <[u8; KEY_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
            anyhow!(
                "secret vault key at {} has wrong length {}",
                key_path.display(),
                bytes.len()
            )
        })?;
        return Ok(key);
    }
    // Draw the key directly from the OS CSPRNG instead of filling a
    // zero-initialised array, so no constant value is ever used as key material.
    let key: [u8; KEY_LEN] = rand::rng().random();
    write_private_file(key_path, &key)?;
    Ok(key)
}

fn write_private_file(path: &Path, data: &[u8]) -> Result<()> {
    let mut f = fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    f.write_all(data)
        .with_context(|| format!("write {}", path.display()))?;
    f.sync_all().ok();
    Ok(())
}

fn decrypt_vault(path: &Path, key: &[u8; KEY_LEN]) -> Result<HashMap<String, String>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read secret vault {}", path.display()))?;
    let disk: VaultDisk = serde_json::from_str(&raw)
        .with_context(|| format!("parse secret vault {}", path.display()))?;
    if disk.version != VAULT_VERSION {
        bail!("unsupported secret vault version {}", disk.version);
    }
    let nonce_bytes = decode_b64(&disk.nonce).context("decode vault nonce")?;
    if nonce_bytes.len() != NONCE_LEN {
        bail!("vault nonce length {}", nonce_bytes.len());
    }
    let ct = decode_b64(&disk.ciphertext).context("decode vault ciphertext")?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plain = cipher
        .decrypt(nonce, ct.as_ref())
        .map_err(|_| anyhow!("failed to decrypt secret vault (wrong key or corrupt file)"))?;
    let map: HashMap<String, String> =
        serde_json::from_slice(&plain).context("decode decrypted vault JSON")?;
    Ok(map)
}

fn encrypt_and_write(
    path: &Path,
    key: &[u8; KEY_LEN],
    map: &HashMap<String, String>,
) -> Result<()> {
    let plain = serde_json::to_vec(map).context("serialize secret map")?;
    // Fresh random nonce per encryption, drawn directly from the OS CSPRNG.
    let nonce_bytes: [u8; NONCE_LEN] = rand::rng().random();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plain.as_ref())
        .map_err(|_| anyhow!("failed to encrypt secret vault"))?;
    let disk = VaultDisk {
        version: VAULT_VERSION,
        nonce: encode_b64(&nonce_bytes),
        ciphertext: encode_b64(&ct),
    };
    let json = serde_json::to_vec_pretty(&disk).context("serialize vault disk")?;
    // Atomic-ish: write temp then rename.
    let tmp = path.with_extension("vault.tmp");
    write_private_file(&tmp, &json)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn encode_b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn decode_b64(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("base64 decode")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_backend_round_trip_persists() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        {
            let store = EncryptedFileSecretStore::open(&vault).unwrap();
            store
                .set("FILE_SECRET", "file-backend-VALUE-should-not-log")
                .unwrap();
            assert!(store.exists("FILE_SECRET").unwrap());
        }
        let store2 = EncryptedFileSecretStore::open(&vault).unwrap();
        let v = store2.get("FILE_SECRET").unwrap().unwrap();
        assert_eq!(v.expose(), "file-backend-VALUE-should-not-log");
        assert!(!format!("{v:?}").contains("file-backend-VALUE"));
        // Ciphertext file must not contain plaintext.
        let disk = fs::read_to_string(&vault).unwrap();
        assert!(!disk.contains("file-backend-VALUE-should-not-log"));
        store2.delete("FILE_SECRET").unwrap();
        assert!(!store2.exists("FILE_SECRET").unwrap());
    }
}
