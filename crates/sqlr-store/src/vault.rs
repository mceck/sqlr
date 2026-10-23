//! One keyring key per configuration directory, cached for the store's lifetime. Every
//! caller must run these blocking operations off the UI thread.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fs2::FileExt;
use parking_lot::Mutex;
use ring::{aead, digest};
use zeroize::{Zeroize, Zeroizing};

use crate::{McpTokenStore, SecretStore, StoreError};

const HEADER: &[u8] = b"SQLRVAULT\x01";
const NONCE_LEN: usize = 12;
const KEY_ID: &str = "sqlr:vault:v1";

type Data = BTreeMap<String, BTreeMap<String, StoredSecret>>;

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSecret {
    // None is a tombstone: do not consult a stale legacy keychain item again.
    value: Option<String>,
    // Persist before deleting the original so an interrupted migration can resume.
    legacy_pending: bool,
}

impl Drop for StoredSecret {
    fn drop(&mut self) {
        if let Some(value) = &mut self.value {
            value.zeroize();
        }
    }
}

enum Operation<'a> {
    Get,
    Set(&'a str),
    Delete,
}

/// Authenticated encrypted credentials in `<dir>/secrets.vault`. Construction performs no
/// I/O; the first operation unlocks the keyring. Share one `Arc` across all consumers.
/// No Debug implementation: the cached key and plaintext must never reach logs.
pub struct VaultSecretStore {
    dir: PathBuf,
    keyring: Arc<dyn SecretStore>,
    key: Mutex<Option<Zeroizing<[u8; 32]>>>,
}

impl VaultSecretStore {
    /// `keyring` supplies the master key and the old per-connection entries for migration.
    pub fn new(dir: &Path, keyring: Arc<dyn SecretStore>) -> Self {
        Self {
            dir: dir.to_path_buf(),
            keyring,
            key: Mutex::new(None),
        }
    }

    fn legacy_get(&self, id: &str, field: &str) -> Result<Option<String>, StoreError> {
        // Connection ids/driver field names are user-controlled. They must never reach
        // the reserved keyring accounts holding this or another directory's master key.
        if id == KEY_ID {
            Ok(None)
        } else {
            self.keyring.get(id, field)
        }
    }

    fn legacy_delete(&self, id: &str, field: &str) -> Result<(), StoreError> {
        if id == KEY_ID {
            Ok(())
        } else {
            self.keyring.delete(id, field)
        }
    }

    fn access(
        &self,
        id: &str,
        field: &str,
        op: Operation<'_>,
        legacy_get: impl FnOnce() -> Result<Option<String>, StoreError>,
        legacy_delete: impl FnOnce() -> Result<(), StoreError>,
    ) -> Result<Option<String>, StoreError> {
        let reading = matches!(&op, Operation::Get);
        let mut cached_key = self.key.lock();
        std::fs::create_dir_all(&self.dir)?;
        let lock = lock_file(&self.dir.join("secrets.vault.lock"))?;
        // The OS lock serializes other processes and independently constructed stores.
        lock.lock_exclusive()?;
        let path = self.dir.join("secrets.vault");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if cached_key.is_none() {
            let dir = std::fs::canonicalize(&self.dir)?;
            let account = STANDARD.encode(digest::digest(
                &digest::SHA256,
                dir.as_os_str().as_encoded_bytes(),
            ));
            let key = match self.keyring.get(KEY_ID, &account)? {
                Some(encoded) => {
                    let encoded = Zeroizing::new(encoded);
                    let decoded = Zeroizing::new(
                        STANDARD
                            .decode(encoded.as_bytes())
                            .map_err(|_| invalid_key())?,
                    );
                    let mut key = Zeroizing::new([0; 32]);
                    if decoded.len() != key.len() {
                        return Err(invalid_key());
                    }
                    key.copy_from_slice(&decoded);
                    key
                }
                None if bytes.is_some() => {
                    return Err(StoreError::Secret(
                        "The credential vault key is missing".into(),
                    ));
                }
                None => {
                    let mut key = Zeroizing::new([0; 32]);
                    random(key.as_mut())?;
                    let encoded = Zeroizing::new(STANDARD.encode(key.as_ref()));
                    self.keyring.set(KEY_ID, &account, &encoded)?;
                    key
                }
            };
            *cached_key = Some(key);
        }
        let key = cached_key.as_ref().expect("key loaded above");
        let mut data = match bytes {
            Some(bytes) => decrypt(key, bytes)?,
            None => Data::default(),
        };
        let existing = data.get(id).and_then(|fields| fields.get(field));
        let replacement = match op {
            Operation::Get if existing.is_some() => None,
            Operation::Get => {
                let value = legacy_get()?;
                let legacy_pending = value.is_some();
                Some(StoredSecret {
                    value,
                    legacy_pending,
                })
            }
            Operation::Set(value) => Some(StoredSecret {
                value: Some(value.into()),
                legacy_pending: existing.is_none_or(|s| s.legacy_pending),
            }),
            Operation::Delete => Some(StoredSecret {
                value: None,
                legacy_pending: existing.is_none_or(|s| s.legacy_pending),
            }),
        };
        if let Some(replacement) = replacement {
            data.entry(id.into())
                .or_default()
                .insert(field.into(), replacement);
            persist(&self.dir, key, &data)?;
        }
        if data[id][field].legacy_pending {
            // The encrypted value/tombstone is already durable. On failure keep the
            // pending flag, the original keychain entry and a retryable error.
            legacy_delete()?;
            data.get_mut(id)
                .unwrap()
                .get_mut(field)
                .unwrap()
                .legacy_pending = false;
            persist(&self.dir, key, &data)?;
        }
        Ok(if reading {
            data[id][field].value.clone()
        } else {
            None
        })
        // Dropping the file releases the OS lock, also on any earlier error path.
    }
}

impl SecretStore for VaultSecretStore {
    fn get(&self, id: &str, field: &str) -> Result<Option<String>, StoreError> {
        self.access(
            &format!("connection:{id}"),
            field,
            Operation::Get,
            || self.legacy_get(id, field),
            || self.legacy_delete(id, field),
        )
    }

    fn set(&self, id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        self.access(
            &format!("connection:{id}"),
            field,
            Operation::Set(value),
            || self.legacy_get(id, field),
            || self.legacy_delete(id, field),
        )
        .map(|_| ())
    }

    fn delete(&self, id: &str, field: &str) -> Result<(), StoreError> {
        self.access(
            &format!("connection:{id}"),
            field,
            Operation::Delete,
            || self.legacy_get(id, field),
            || self.legacy_delete(id, field),
        )
        .map(|_| ())
    }
}

/// MCP tokens share the connection vault's cached key. The old dedicated keyring item is
/// read only during migration; rotating a token does not touch connection credentials.
pub struct VaultMcpTokenStore {
    vault: Arc<VaultSecretStore>,
    legacy: Arc<dyn McpTokenStore>,
}

impl VaultMcpTokenStore {
    pub fn new(vault: Arc<VaultSecretStore>, legacy: Arc<dyn McpTokenStore>) -> Self {
        Self { vault, legacy }
    }

    fn access(&self, op: Operation<'_>) -> Result<Option<String>, StoreError> {
        self.vault.access(
            "mcp",
            "token",
            op,
            || self.legacy.get(),
            || self.legacy.delete(),
        )
    }
}

impl McpTokenStore for VaultMcpTokenStore {
    fn get(&self) -> Result<Option<String>, StoreError> {
        self.access(Operation::Get)
    }

    fn set(&self, token: &str) -> Result<(), StoreError> {
        self.access(Operation::Set(token)).map(|_| ())
    }

    fn delete(&self) -> Result<(), StoreError> {
        self.access(Operation::Delete).map(|_| ())
    }
}

fn invalid_key() -> StoreError {
    StoreError::Secret("The credential vault key is invalid".into())
}

fn invalid_vault() -> StoreError {
    StoreError::Secret("The credential vault is invalid or cannot be decrypted".into())
}

fn random(bytes: &mut [u8]) -> Result<(), StoreError> {
    getrandom::fill(bytes)
        .map_err(|_| StoreError::Secret("Could not generate vault randomness".into()))
}

fn cipher(key: &[u8; 32]) -> Result<aead::LessSafeKey, StoreError> {
    aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| invalid_key())
}

fn decrypt(key: &[u8; 32], bytes: Vec<u8>) -> Result<Data, StoreError> {
    if !bytes.starts_with(HEADER) || bytes.len() < HEADER.len() + NONCE_LEN + aead::MAX_TAG_LEN {
        return Err(invalid_vault());
    }
    let nonce: [u8; NONCE_LEN] = bytes[HEADER.len()..HEADER.len() + NONCE_LEN]
        .try_into()
        .unwrap();
    let mut buffer = Zeroizing::new(bytes);
    let plaintext = cipher(key)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(HEADER),
            &mut buffer[HEADER.len() + NONCE_LEN..],
        )
        .map_err(|_| invalid_vault())?;
    // Serde error messages can echo plaintext: never return them.
    serde_json::from_slice(plaintext).map_err(|_| invalid_vault())
}

fn persist(dir: &Path, key: &[u8; 32], data: &Data) -> Result<(), StoreError> {
    let mut nonce = [0; NONCE_LEN];
    random(&mut nonce)?;
    let mut ciphertext = Zeroizing::new(serde_json::to_vec(data).map_err(|_| invalid_vault())?);
    cipher(key)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(HEADER),
            &mut *ciphertext,
        )
        .map_err(|_| invalid_vault())?;
    // NamedTempFile uses private permissions (0600 on Unix), including before the rename.
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(HEADER)?;
    file.write_all(&nonce)?;
    file.write_all(&ciphertext)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("secrets.vault"))
        .map_err(|e| StoreError::Io(e.error))?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn lock_file(path: &Path) -> Result<File, StoreError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
