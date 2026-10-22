//! Cancellation: `hdbconnect_async` has no cancel API, so a second connection of the same
//! user runs `ALTER SYSTEM CANCEL SESSION '<connection id>'` on a second connection.

use async_trait::async_trait;
use hdbconnect_async::Connection as HdbConnection;
use sqlr_driver_sdk::{CancelHandle, DriverError};

use crate::connect::Params;
use crate::errors;

/// The statement that cancels the running statement of session `id`.
pub fn cancel_statement(id: i64) -> String {
    format!("ALTER SYSTEM CANCEL SESSION '{id}'")
}

pub(crate) struct HanaCancel {
    params: Params,
    session: i64,
}

impl HanaCancel {
    pub(crate) fn new(params: Params, session: i64) -> Self {
        Self { params, session }
    }
}

#[async_trait]
impl CancelHandle for HanaCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let side = HdbConnection::new(self.params.params.clone())
            .await
            .map_err(errors::map)?;
        side.exec(cancel_statement(self.session))
            .await
            .map_err(errors::map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_statement_for_connection_id() {
        assert_eq!(
            cancel_statement(300_123),
            "ALTER SYSTEM CANCEL SESSION '300123'"
        );
        // The id is a number: nothing a user typed reaches the statement.
        assert_eq!(cancel_statement(-1), "ALTER SYSTEM CANCEL SESSION '-1'");
    }
}
