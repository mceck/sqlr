//! The failures of reading and writing transfer files and of mapping rows onto a table.

use std::io;
use std::path::PathBuf;

use sqlr_core::{DatagenError, DriverError, ReadError};

/// Why a transfer file (CSV, JSON, workbook, Parquet) or a row stream could not be read or
/// written.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TransferFileError {
    /// A text file could not be opened.
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The rows of a text file could not be decoded.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The test-data generator failed.
    #[error(transparent)]
    Datagen(#[from] DatagenError),
    /// The source of a row or document stream failed.
    #[error(transparent)]
    Source(#[from] DriverError),
    /// A Parquet file could not be opened or read.
    #[error("{}: {source}", path.display())]
    Parquet {
        path: PathBuf,
        #[source]
        source: parquet::errors::ParquetError,
    },
    /// A Parquet row could not be read.
    #[error("Row {row}: {source}")]
    ParquetRow {
        row: usize,
        #[source]
        source: parquet::errors::ParquetError,
    },
    /// A Parquet column could not be built or written.
    #[error("Column {column}: {source}")]
    ParquetColumn {
        column: String,
        #[source]
        source: parquet::errors::ParquetError,
    },
    /// Parquet refused to write.
    #[error("{0}")]
    ParquetWrite(#[from] parquet::errors::ParquetError),
    /// A file could not be created.
    #[error("{}: {source}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The result has more columns than the Parquet file.
    #[error("The file has fewer columns than the result")]
    FewerColumns,
    /// A value does not fit the Parquet column's type.
    #[error("Column {column}: \"{shown}\" is not {what}")]
    ColumnValue {
        column: String,
        shown: String,
        what: &'static str,
    },
    /// A workbook could not be opened.
    #[error("{}: {source}", path.display())]
    Workbook {
        path: PathBuf,
        #[source]
        source: calamine::XlsxError,
    },
    /// The workbook has no sheets.
    #[error("The workbook has no sheets")]
    NoSheets,
    /// A sheet could not be read.
    #[error("Sheet \"{name}\": {source}")]
    Sheet {
        name: String,
        #[source]
        source: calamine::XlsxError,
    },
    /// The workbook writer refused.
    #[error("{0}")]
    XlsxWrite(#[from] rust_xlsxwriter::XlsxError),
    /// More rows than an Excel sheet holds.
    #[error("An Excel sheet holds at most 1 048 576 rows")]
    SheetFull,
    /// A text cell longer than Excel's limit.
    #[error("Row {row}, column {column}: {chars} characters; an Excel cell holds at most 32 767")]
    CellTooLong {
        /// One-based row number in the sheet.
        row: u32,
        /// One-based column number in the sheet.
        column: u16,
        /// The value's character count.
        chars: usize,
    },
    /// The writer thread panicked or was cancelled.
    #[error("{0}")]
    Join(#[from] tokio::task::JoinError),
}

/// A value that cannot be converted for its target column.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("column {column}: \"{shown}\" is not {what}")]
pub(crate) struct ConvertError {
    /// The target column.
    pub column: String,
    /// The first 40 characters of the value.
    pub shown: String,
    /// The kind of value the column takes.
    pub what: &'static str,
}

/// Why an import's column mapping is unusable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MappingError {
    /// A mapped column is not in the table.
    #[error("{table} has no column \"{column}\"")]
    UnknownColumn { table: String, column: String },
    /// Two source columns map to one table column.
    #[error("Column \"{column}\" is mapped twice")]
    MappedTwice { column: String },
    /// No source column maps to a table column.
    #[error("No source column is mapped to a table column")]
    NothingMapped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stay_as_the_user_saw_them() {
        let open = TransferFileError::File {
            path: PathBuf::from("/tmp/a.csv"),
            source: io::Error::new(io::ErrorKind::NotFound, "missing"),
        };
        assert_eq!(open.to_string(), "/tmp/a.csv: missing");
        assert_eq!(
            TransferFileError::SheetFull.to_string(),
            "An Excel sheet holds at most 1 048 576 rows"
        );
        let value = TransferFileError::ColumnValue {
            column: "n".into(),
            shown: "x".into(),
            what: "a number",
        };
        assert_eq!(value.to_string(), "Column n: \"x\" is not a number");
        assert_eq!(
            MappingError::UnknownColumn {
                table: "t".into(),
                column: "c".into()
            }
            .to_string(),
            "t has no column \"c\""
        );
        assert_eq!(
            MappingError::NothingMapped.to_string(),
            "No source column is mapped to a table column"
        );
    }
}
