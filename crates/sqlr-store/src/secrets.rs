use std::collections::HashMap;

use parking_lot::Mutex;

use crate::StoreError;

pub trait SecretStore: Send + Sync {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError>;
    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError>;
    /// Succeeds when the secret is absent.
    fn delete(&self, conn_id: &str, field: &str) -> Result<(), StoreError>;
}

/// One store shared by the connection repo (writes) and the engine (reads on connect).
impl<T: SecretStore + ?Sized> SecretStore for std::sync::Arc<T> {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError> {
        (**self).get(conn_id, field)
    }
    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        (**self).set(conn_id, field, value)
    }
    fn delete(&self, conn_id: &str, field: &str) -> Result<(), StoreError> {
        (**self).delete(conn_id, field)
    }
}

/// OS keychain backend: service `sqlr`, user `<conn_id>/<field>`. Used by the vault for
/// its master key and migration of old per-connection entries; app consumers share a
/// `VaultSecretStore` instead of accessing this backend directly.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(conn_id: &str, field: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new("sqlr", &format!("{conn_id}/{field}")).map_err(secret_err)
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
fn secret_err(e: keyring::Error) -> StoreError {
    StoreError::Secret(e.to_string())
}

impl SecretStore for KeyringStore {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError> {
        match Self::entry(conn_id, field)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(secret_err(e)),
        }
    }

    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        Self::entry(conn_id, field)?
            .set_password(value)
            .map_err(secret_err)
    }

    fn delete(&self, conn_id: &str, field: &str) -> Result<(), StoreError> {
        match Self::entry(conn_id, field)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(secret_err(e)),
        }
    }
}

/// In-memory store for tests and keychain-less environments.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<(String, String), String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .map
            .lock()
            .get(&(conn_id.to_owned(), field.to_owned()))
            .cloned())
    }

    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        self.map
            .lock()
            .insert((conn_id.to_owned(), field.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, conn_id: &str, field: &str) -> Result<(), StoreError> {
        self.map
            .lock()
            .remove(&(conn_id.to_owned(), field.to_owned()));
        Ok(())
    }
}
