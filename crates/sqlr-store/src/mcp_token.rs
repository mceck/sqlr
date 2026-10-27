//! The MCP bearer token's storage interface and legacy OS keychain backend. The app uses
//! `VaultMcpTokenStore` to share its credential vault and migrate the old keychain item.

use parking_lot::Mutex;

use crate::StoreError;

/// The keychain service that holds the MCP token. Separate from connection secrets so
/// rotating it never touches a connection.
pub const MCP_TOKEN_SERVICE: &str = "sqlr-mcp";
pub const MCP_TOKEN_ACCOUNT: &str = "token";

pub trait McpTokenStore: Send + Sync {
    /// The stored token, or `None` when none was generated yet.
    fn get(&self) -> Result<Option<String>, StoreError>;
    fn set(&self, token: &str) -> Result<(), StoreError>;
    /// Succeeds when the token is absent.
    fn delete(&self) -> Result<(), StoreError>;
}

impl<T: McpTokenStore + ?Sized> McpTokenStore for std::sync::Arc<T> {
    fn get(&self) -> Result<Option<String>, StoreError> {
        (**self).get()
    }
    fn set(&self, token: &str) -> Result<(), StoreError> {
        (**self).set(token)
    }
    fn delete(&self) -> Result<(), StoreError> {
        (**self).delete()
    }
}

/// OS keychain: service [`MCP_TOKEN_SERVICE`], account [`MCP_TOKEN_ACCOUNT`].
pub struct KeyringMcpTokenStore;

impl KeyringMcpTokenStore {
    fn entry() -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(MCP_TOKEN_SERVICE, MCP_TOKEN_ACCOUNT).map_err(secret_err)
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
fn secret_err(e: keyring::Error) -> StoreError {
    StoreError::Secret(e.to_string())
}

impl McpTokenStore for KeyringMcpTokenStore {
    fn get(&self) -> Result<Option<String>, StoreError> {
        match Self::entry()?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(secret_err(e)),
        }
    }

    fn set(&self, token: &str) -> Result<(), StoreError> {
        Self::entry()?.set_password(token).map_err(secret_err)
    }

    fn delete(&self) -> Result<(), StoreError> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(secret_err(e)),
        }
    }
}

/// In-memory token store for tests and keychain-less environments.
#[derive(Default)]
pub struct MemoryMcpTokenStore {
    token: Mutex<Option<String>>,
}

impl MemoryMcpTokenStore {
    pub fn with_token(token: &str) -> Self {
        Self {
            token: Mutex::new(Some(token.to_string())),
        }
    }
}

impl McpTokenStore for MemoryMcpTokenStore {
    fn get(&self) -> Result<Option<String>, StoreError> {
        Ok(self.token.lock().clone())
    }

    fn set(&self, token: &str) -> Result<(), StoreError> {
        *self.token.lock() = Some(token.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<(), StoreError> {
        *self.token.lock() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_token_round_trips() {
        let store = MemoryMcpTokenStore::default();
        assert_eq!(store.get().unwrap(), None);
        store.set("abc").unwrap();
        assert_eq!(store.get().unwrap().as_deref(), Some("abc"));
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
        store.delete().unwrap();
    }
}
