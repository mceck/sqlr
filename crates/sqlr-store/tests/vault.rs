use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlr_store::{
    McpTokenStore, MemoryMcpTokenStore, MemorySecretStore, SecretStore, StoreError,
    VaultMcpTokenStore, VaultSecretStore,
};

#[derive(Default)]
struct Keyring {
    inner: MemorySecretStore,
    reads: Mutex<Vec<(String, String)>>,
    writes: Mutex<Vec<(String, String)>>,
    locked: AtomicBool,
    refuse_delete: AtomicBool,
    block_write: Mutex<Option<std::path::PathBuf>>,
    refuse_set: AtomicBool,
}

impl SecretStore for Keyring {
    fn get(&self, id: &str, field: &str) -> Result<Option<String>, StoreError> {
        self.reads.lock().push((id.into(), field.into()));
        if self.locked.load(Ordering::SeqCst) {
            return Err(StoreError::Secret("access denied".into()));
        }
        if id == "old"
            && let Some(path) = self.block_write.lock().take()
        {
            std::fs::create_dir(path).unwrap();
        }
        self.inner.get(id, field)
    }

    fn set(&self, id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        if self.refuse_set.load(Ordering::SeqCst) {
            return Err(StoreError::Secret("key save denied".into()));
        }
        self.writes.lock().push((id.into(), field.into()));
        self.inner.set(id, field, value)
    }

    fn delete(&self, id: &str, field: &str) -> Result<(), StoreError> {
        if self.refuse_delete.load(Ordering::SeqCst) {
            return Err(StoreError::Secret("cleanup denied".into()));
        }
        self.inner.delete(id, field)
    }
}

fn vault(dir: &tempfile::TempDir, keyring: &Arc<Keyring>) -> VaultSecretStore {
    VaultSecretStore::new(dir.path(), keyring.clone())
}

#[test]
fn connections_share_one_lazy_keyring_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    let store = vault(&dir, &keyring);
    assert!(keyring.reads.lock().is_empty());
    store.set("one", "password", "first-secret").unwrap();
    store.set("two", "password", "second-secret").unwrap();
    store.set("two", "ssh_password", "ssh-secret").unwrap();
    assert_eq!(
        store.get("one", "password").unwrap().as_deref(),
        Some("first-secret")
    );
    assert_eq!(
        store.get("two", "password").unwrap().as_deref(),
        Some("second-secret")
    );
    assert_eq!(
        store.get("two", "ssh_password").unwrap().as_deref(),
        Some("ssh-secret")
    );
    assert_eq!(keyring.reads.lock().len(), 1);
    assert_eq!(keyring.writes.lock().len(), 1);
    drop(store);

    keyring.reads.lock().clear();
    let restarted = vault(&dir, &keyring);
    assert_eq!(
        restarted.get("one", "password").unwrap().as_deref(),
        Some("first-secret")
    );
    assert_eq!(
        restarted.get("two", "password").unwrap().as_deref(),
        Some("second-secret")
    );
    assert_eq!(keyring.reads.lock().len(), 1);
}

#[test]
fn archive_contains_no_plaintext_and_changes_nonce_on_each_write() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    let store = vault(&dir, &keyring);
    store
        .set("conn", "password", "secret-do-not-expose")
        .unwrap();
    let before = std::fs::read(dir.path().join("secrets.vault")).unwrap();
    assert!(!before.windows(20).any(|w| w == b"secret-do-not-expose"));
    store
        .set("conn", "password", "secret-do-not-expose")
        .unwrap();
    let after = std::fs::read(dir.path().join("secrets.vault")).unwrap();
    assert_ne!(before, after);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("secrets.vault"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn legacy_secrets_migrate_once_and_are_removed_after_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring
        .inner
        .set("old", "password", "legacy-password")
        .unwrap();
    let store = vault(&dir, &keyring);
    assert_eq!(
        store.get("old", "password").unwrap().as_deref(),
        Some("legacy-password")
    );
    assert_eq!(keyring.inner.get("old", "password").unwrap(), None);
    let count = keyring.reads.lock().len();
    assert_eq!(
        store.get("old", "password").unwrap().as_deref(),
        Some("legacy-password")
    );
    assert_eq!(keyring.reads.lock().len(), count);
    drop(store);
    assert_eq!(
        vault(&dir, &keyring)
            .get("old", "password")
            .unwrap()
            .as_deref(),
        Some("legacy-password")
    );
}

#[test]
fn interrupted_legacy_cleanup_can_be_retried_without_losing_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring
        .inner
        .set("old", "password", "legacy-password")
        .unwrap();
    keyring.refuse_delete.store(true, Ordering::SeqCst);
    let store = vault(&dir, &keyring);
    assert!(store.get("old", "password").is_err());
    assert_eq!(
        keyring.inner.get("old", "password").unwrap().as_deref(),
        Some("legacy-password")
    );
    drop(store);
    keyring.refuse_delete.store(false, Ordering::SeqCst);
    assert_eq!(
        vault(&dir, &keyring)
            .get("old", "password")
            .unwrap()
            .as_deref(),
        Some("legacy-password")
    );
    assert_eq!(keyring.inner.get("old", "password").unwrap(), None);
}

#[test]
fn deletes_and_overwrites_do_not_resurrect_legacy_values() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring.inner.set("old", "password", "old-value").unwrap();
    let store = vault(&dir, &keyring);
    store.set("old", "password", "replacement").unwrap();
    assert_eq!(keyring.inner.get("old", "password").unwrap(), None);
    store.delete("old", "password").unwrap();
    drop(store);
    // A stale entry from an older app version must not override a tombstone.
    keyring.inner.set("old", "password", "old-value").unwrap();
    assert_eq!(vault(&dir, &keyring).get("old", "password").unwrap(), None);
}

#[test]
fn deleting_an_unmigrated_secret_removes_the_legacy_item() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring.inner.set("old", "password", "old-value").unwrap();
    let store = vault(&dir, &keyring);
    store.delete("old", "password").unwrap();
    store.delete("old", "password").unwrap();
    assert_eq!(keyring.inner.get("old", "password").unwrap(), None);
    assert_eq!(store.get("old", "password").unwrap(), None);
}

#[test]
fn absent_fields_are_not_repeatedly_looked_up_in_the_keyring() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    let store = vault(&dir, &keyring);
    assert_eq!(store.get("conn", "ssh_password").unwrap(), None);
    let count = keyring.reads.lock().len();
    assert_eq!(store.get("conn", "ssh_password").unwrap(), None);
    assert_eq!(keyring.reads.lock().len(), count);
    drop(store);
    keyring.reads.lock().clear();
    assert_eq!(
        vault(&dir, &keyring).get("conn", "ssh_password").unwrap(),
        None
    );
    assert_eq!(keyring.reads.lock().len(), 1);
}

#[test]
fn denied_unlock_is_retryable_and_does_not_change_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&dir, &keyring)
        .set("conn", "password", "saved")
        .unwrap();
    let path = dir.path().join("secrets.vault");
    let before = std::fs::read(&path).unwrap();
    keyring.locked.store(true, Ordering::SeqCst);
    let store = vault(&dir, &keyring);
    assert!(store.set("conn", "password", "replacement").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    keyring.locked.store(false, Ordering::SeqCst);
    assert_eq!(
        store.get("conn", "password").unwrap().as_deref(),
        Some("saved")
    );
}

#[test]
fn corrupted_archives_are_never_overwritten_or_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&dir, &keyring)
        .set("conn", "password", "saved")
        .unwrap();
    let path = dir.path().join("secrets.vault");
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    keyring.inner.set("conn", "password", "legacy").unwrap();
    let store = vault(&dir, &keyring);
    assert!(store.get("conn", "password").is_err());
    assert!(store.set("conn", "password", "replacement").is_err());
    assert!(store.delete("conn", "password").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        keyring.inner.get("conn", "password").unwrap().as_deref(),
        Some("legacy")
    );
}

#[test]
fn a_missing_or_invalid_master_key_never_replaces_an_existing_archive() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&dir, &keyring)
        .set("conn", "password", "saved")
        .unwrap();
    let path = dir.path().join("secrets.vault");
    let before = std::fs::read(&path).unwrap();
    let (id, field) = keyring.writes.lock()[0].clone();
    keyring.inner.delete(&id, &field).unwrap();
    assert!(
        vault(&dir, &keyring)
            .set("conn", "password", "new")
            .is_err()
    );
    assert_eq!(keyring.writes.lock().len(), 1);
    keyring
        .inner
        .set(&id, &field, "invalid-key-material")
        .unwrap();
    let error = vault(&dir, &keyring)
        .get("conn", "password")
        .unwrap_err()
        .to_string();
    assert!(!error.contains("invalid-key-material"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn failed_archive_writes_preserve_legacy_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring.inner.set("old", "password", "legacy").unwrap();
    // Make the destination unreplaceable after the initial read but before persistence.
    *keyring.block_write.lock() = Some(dir.path().join("secrets.vault"));
    assert!(vault(&dir, &keyring).get("old", "password").is_err());
    assert_eq!(
        keyring.inner.get("old", "password").unwrap().as_deref(),
        Some("legacy")
    );
    std::fs::remove_dir(dir.path().join("secrets.vault")).unwrap();
    assert_eq!(
        vault(&dir, &keyring)
            .get("old", "password")
            .unwrap()
            .as_deref(),
        Some("legacy")
    );
}

#[test]
fn a_master_key_that_cannot_be_saved_never_creates_a_vault() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    keyring.inner.set("old", "password", "legacy").unwrap();
    keyring.refuse_set.store(true, Ordering::SeqCst);
    let store = vault(&dir, &keyring);
    assert!(store.get("old", "password").is_err());
    assert!(!dir.path().join("secrets.vault").exists());
    assert_eq!(
        keyring.inner.get("old", "password").unwrap().as_deref(),
        Some("legacy")
    );
    keyring.refuse_set.store(false, Ordering::SeqCst);
    assert_eq!(
        store.get("old", "password").unwrap().as_deref(),
        Some("legacy")
    );
}

#[test]
fn a_valid_but_wrong_key_cannot_read_or_replace_credentials() {
    use base64::Engine as _;

    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&dir, &keyring)
        .set("conn", "password", "saved")
        .unwrap();
    let path = dir.path().join("secrets.vault");
    let before = std::fs::read(&path).unwrap();
    let (id, field) = keyring.writes.lock()[0].clone();
    let wrong_key = base64::engine::general_purpose::STANDARD.encode([0; 32]);
    keyring.inner.set(&id, &field, &wrong_key).unwrap();
    let store = vault(&dir, &keyring);
    assert!(store.get("conn", "password").is_err());
    assert!(store.set("conn", "password", "new").is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn canonical_directory_aliases_use_the_same_master_key() {
    let dir = tempfile::tempdir().unwrap();
    let alias_dir = tempfile::tempdir().unwrap();
    let alias = alias_dir.path().join("alias");
    std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&dir, &keyring)
        .set("conn", "password", "saved")
        .unwrap();
    let alias_store = VaultSecretStore::new(&alias, keyring.clone());
    assert_eq!(
        alias_store.get("conn", "password").unwrap().as_deref(),
        Some("saved")
    );
    assert_eq!(keyring.writes.lock().len(), 1);
}

#[test]
fn concurrent_store_instances_do_not_lose_writes_or_race_key_creation() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    std::thread::scope(|scope| {
        for n in 0..8 {
            let store = vault(&dir, &keyring);
            scope.spawn(move || {
                store
                    .set(&format!("conn-{n}"), "password", &format!("secret-{n}"))
                    .unwrap()
            });
        }
    });
    let store = vault(&dir, &keyring);
    for n in 0..8 {
        assert_eq!(
            store.get(&format!("conn-{n}"), "password").unwrap(),
            Some(format!("secret-{n}"))
        );
    }
    assert_eq!(keyring.writes.lock().len(), 1);
}

#[test]
fn different_configuration_directories_have_separate_keys() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    vault(&first, &keyring)
        .set("conn", "password", "first")
        .unwrap();
    vault(&second, &keyring)
        .set("conn", "password", "second")
        .unwrap();
    {
        let writes = keyring.writes.lock();
        assert_ne!(writes[0], writes[1]);
    }
    assert_eq!(
        vault(&first, &keyring)
            .get("conn", "password")
            .unwrap()
            .as_deref(),
        Some("first")
    );
}

#[test]
fn connection_identifiers_cannot_read_or_delete_the_master_key() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    let store = vault(&dir, &keyring);
    store.set("conn", "password", "saved").unwrap();
    let (key_id, field) = keyring.writes.lock()[0].clone();
    assert!(store.get(&key_id, &field).unwrap().is_none());
    store.set(&key_id, &field, "connection-password").unwrap();
    store.delete(&key_id, &field).unwrap();
    drop(store);
    assert_eq!(
        vault(&dir, &keyring)
            .get("conn", "password")
            .unwrap()
            .as_deref(),
        Some("saved")
    );
}

#[test]
fn mcp_tokens_use_the_same_unlocked_vault_and_migrate_the_old_token() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(Keyring::default());
    let store = Arc::new(vault(&dir, &keyring));
    store.set("conn", "password", "password").unwrap();
    let legacy = Arc::new(MemoryMcpTokenStore::default());
    legacy.set("legacy-token").unwrap();
    let tokens = VaultMcpTokenStore::new(store, legacy.clone());
    assert_eq!(tokens.get().unwrap().as_deref(), Some("legacy-token"));
    assert_eq!(legacy.get().unwrap(), None);
    tokens.set("replacement").unwrap();
    assert_eq!(tokens.get().unwrap().as_deref(), Some("replacement"));
    tokens.delete().unwrap();
    assert_eq!(tokens.get().unwrap(), None);
    assert_eq!(keyring.reads.lock().len(), 1);
}
