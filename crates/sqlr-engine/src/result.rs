//! Query results as the UI sees them.

use std::time::Duration;

use sqlr_core::{DriverError, RowBuffer};

use crate::tx::RunMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum QueryStatus {
    /// Executing or fetching a page.
    Running,
    /// Idle; `more` is true while the stream may still hold rows.
    Ready {
        more: bool,
    },
    Cancelled,
    Failed(DriverError),
}

pub struct QueryResult {
    pub sql: String,
    /// The server database the statement runs on, when the tab is bound to one.
    pub database: Option<String>,
    pub status: QueryStatus,
    pub rows: RowBuffer,
    /// Rows changed by a statement that returns no rows.
    pub affected: Option<u64>,
    /// From `execute` to the end of the most recent fetch.
    pub elapsed: Duration,
    /// Rows requested by the next `fetch_more` (and each `fetch_all` page).
    pub page_size: usize,
    /// The stream ended before it was exhausted because a later query on the session took it
    /// over or it was closed under this result (disconnect, lost connection). The status is
    /// then `Ready { more: false }` although the query may have had more rows.
    pub cut_short: bool,
    /// The statement's outcome, server notices and transaction notes, oldest first.
    pub messages: Vec<RunMessage>,
    // Drop the payload before releasing its reservation.
    pub(crate) memory: crate::result_memory::Reservation,
}
