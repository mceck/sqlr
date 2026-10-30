//! Where a transfer reads and writes (spec §8.5, including NoSQL endpoints).
//!
//! Tables export and import through shared jobs; this supports document and
//! key-value endpoints: collections and indices as sources and targets, Redis keys as a
//! source. Documents meeting a tabular side are flattened by the Table rule
//! ([`crate::doc_rows`]); rows meeting a collection become flat documents.

use std::path::PathBuf;

use crate::document::DocTarget;
use crate::encode::TextFormat;
use crate::schema::{ColumnDef, TableDef};
use crate::table::TableRef;

/// How rows are written into a table or collection.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportOptions {
    /// Rows per batch (default 500).
    pub batch: usize,
    /// Commit after each batch instead of once at the end.
    pub commit_per_batch: bool,
    /// Skip rows that fail (conversion or insert) and report them, instead of stopping at
    /// the first one.
    pub skip_bad_rows: bool,
    /// Delete every row of the table first, in the import's transaction.
    pub truncate_first: bool,
    /// Insert-or-update by these target columns instead of plain inserts.
    pub upsert_key: Option<Vec<String>>,
    /// Switch off foreign keys (and unique checks where the server has them) for the load;
    /// restored when it ends, also on failure or cancel.
    pub disable_constraints: bool,
    /// Where rejected rows are written, as CSV with their line and the reason.
    pub rejected_file: Option<PathBuf>,
    /// With `skip_bad_rows`: stop after this many rejected rows.
    pub max_rejected: Option<u64>,
    /// The text encoding of a file source (`utf-8`, `utf-16`, `latin1`); `None` detects a
    /// byte-order mark and otherwise reads UTF-8.
    pub encoding: Option<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            batch: 500,
            commit_per_batch: false,
            skip_bad_rows: false,
            truncate_first: false,
            upsert_key: None,
            disable_constraints: false,
            rejected_file: None,
            max_rejected: None,
            encoding: None,
        }
    }
}

/// A transfer: `mapping` pairs source column indexes with target columns or fields
/// (`target: None` skips one); empty, every source column goes to the column or field of
/// its own name.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransferSpec {
    /// Where rows are read from.
    pub source: Endpoint,
    /// Where rows are written to.
    pub target: Endpoint,
    /// Source columns paired with target columns.
    pub mapping: Vec<ColumnMapping>,
    /// Batching and error handling of the load.
    pub options: TransferOptions,
}

/// The options of a transfer; the import's options, extended for file transfers.
pub type TransferOptions = ImportOptions;

/// What happens to a value on its way to the target column.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Transform {
    /// Strips leading and trailing whitespace.
    Trim,
    /// Converts to upper case.
    Upper,
    /// Converts to lower case.
    Lower,
    /// An empty (or all-blank) text becomes NULL.
    NullIfEmpty,
    /// Every row gets this text, whatever the source holds.
    Constant(String),
}

/// One source column's destination: a target column (`None` skips the column), the type it
/// is converted to when it differs from the target column's own, and a transform.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnMapping {
    /// Index of the source column.
    pub source: usize,
    /// Target column or field; `None` skips the source column.
    pub target: Option<String>,
    /// Type the value is converted to, when it differs from the target column's.
    pub target_type: Option<String>,
    /// Transform applied on the way.
    pub transform: Option<Transform>,
}

impl ColumnMapping {
    /// A mapping of source column `source` to `target`, with no conversion or transform.
    pub fn new(source: usize, target: Option<String>) -> Self {
        Self {
            source,
            target,
            target_type: None,
            transform: None,
        }
    }
}

impl From<(usize, Option<String>)> for ColumnMapping {
    fn from((source, target): (usize, Option<String>)) -> Self {
        Self::new(source, target)
    }
}

/// The variants describe transfer endpoints (`Table`, `Query`, `File`) plus the
/// NoSQL ones; File transfers support `NewTable` and other file formats.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Endpoint {
    /// A file: read (CSV, TSV, JSON, JSON Lines) or written (every export format).
    File {
        /// The file.
        path: PathBuf,
        /// Its text format.
        format: TextFormat,
    },
    /// An Excel workbook's sheet (`None`: the first), read or written (`.xlsx`).
    Workbook {
        /// The `.xlsx` file.
        path: PathBuf,
        /// The sheet; `None` takes the first.
        sheet: Option<String>,
        /// The first row holds the column names.
        header: bool,
    },
    /// Rows made up by the test-data generator: a source only. Its foreign-key pools are
    /// read from the target's connection.
    Generator(crate::datagen::DataGenSpec),
    /// A Parquet file, read or written.
    Parquet {
        /// The `.parquet` file.
        path: PathBuf,
    },
    /// A table of a connection: a source (all its rows) or a target (it must exist).
    Table {
        /// The connection's id.
        conn: String,
        /// The table.
        table: TableRef,
    },
    /// A table the transfer creates, from the source's columns with their types mapped to
    /// the target's dialect; `ddl` replaces the generated `CREATE TABLE` when the user
    /// edited it.
    NewTable {
        /// The target connection's id.
        conn: String,
        /// The table to create.
        table: TableRef,
        /// The edited `CREATE TABLE` statement, replacing the generated one.
        ddl: Option<String>,
    },
    /// The rows of a statement that only reads, on the connection's explorer session (on
    /// `database` when given): a source only.
    Query {
        /// The connection's id.
        conn: String,
        /// Database to run it on, when the connection has several.
        database: Option<String>,
        /// The statement, which only reads.
        sql: String,
    },
    /// A collection (MongoDB) or an index (Elasticsearch). As a source, `query` (a statement
    /// in the connection's language) picks the documents; `None` reads them all.
    Collection {
        /// The connection's id.
        conn: String,
        /// The collection or index.
        target: DocTarget,
        /// A statement in the connection's language that picks the documents.
        query: Option<String>,
    },
    /// The keys of a Redis database matching `pattern` (`SCAN … MATCH`), a source only: one
    /// row per key with its name, type, TTL in milliseconds and value as JSON. The pattern
    /// is bytes, as key names are: a binary key's escaped name matches only that key.
    RedisKeys {
        /// The connection's id.
        conn: String,
        /// The logical database.
        db: u32,
        /// The `MATCH` pattern, as bytes.
        pattern: Vec<u8>,
    },
}

impl Endpoint {
    /// The connection the endpoint is on; `None` for a file.
    pub fn conn(&self) -> Option<&str> {
        match self {
            Endpoint::File { .. }
            | Endpoint::Workbook { .. }
            | Endpoint::Parquet { .. }
            | Endpoint::Generator(_) => None,
            Endpoint::Table { conn, .. }
            | Endpoint::NewTable { conn, .. }
            | Endpoint::Query { conn, .. }
            | Endpoint::Collection { conn, .. }
            | Endpoint::RedisKeys { conn, .. } => Some(conn),
        }
    }
}

/// One mapping per source column: to the target column of the same name (compared ignoring
/// case), or skipped when there is none.
pub fn auto_mapping(source: &[String], target: &[String]) -> Vec<ColumnMapping> {
    source
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let found = target
                .iter()
                .find(|t| t.eq_ignore_ascii_case(name))
                .cloned();
            ColumnMapping::new(i, found)
        })
        .collect()
}

/// The definition of the table a transfer creates from source columns `(name, type)` of
/// dialect `from`, their types mapped to dialect `to`, plus the mapping warnings.
pub fn new_table_def(
    table: &TableRef,
    from: &str,
    to: &str,
    columns: &[(String, String)],
) -> (TableDef, Vec<String>) {
    let mut warnings = Vec::new();
    let mut def = TableDef::new(TableRef {
        database: None,
        ..table.clone()
    });
    for (name, type_name) in columns {
        let (mapped, warning) = map_type(from, type_name, to);
        if let Some(warning) = warning {
            warnings.push(format!("Column {name}: {warning}"));
        }
        def.columns.push(ColumnDef {
            name: name.clone(),
            type_name: mapped,
            nullable: true,
            ..ColumnDef::default()
        });
    }
    (def, warnings)
}

/// The value classes a column type is mapped through between dialects.
#[derive(Clone, Debug, PartialEq)]
enum TypeClass {
    Int2,
    Int4,
    Int8,
    Numeric(Option<(u32, u32)>),
    Float4,
    Float8,
    Bool,
    Text,
    Varchar(Option<u32>),
    Char(Option<u32>),
    Bytes,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Uuid,
    Json,
    Array,
    Enum,
    Unknown,
}

/// The `(a, b)` or `(a)` parameters after a type's name.
fn type_params(type_name: &str) -> Vec<u32> {
    let Some(open) = type_name.find('(') else {
        return Vec::new();
    };
    let Some(close) = type_name[open..].find(')') else {
        return Vec::new();
    };
    type_name[open + 1..open + close]
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect()
}

fn classify_type(dialect: &str, type_name: &str) -> TypeClass {
    let lower = type_name.trim().to_ascii_lowercase();
    if lower.ends_with("[]") || (dialect == "postgres" && lower.starts_with('_')) {
        return TypeClass::Array;
    }
    let params = type_params(&lower);
    // MySQL's unsigned integers reach past their signed twin's range; `zerofill` implies it.
    let unsigned = lower.contains(" unsigned") || lower.contains(" zerofill");
    // The name without parameters or a trailing `unsigned`/`zerofill`.
    let base = lower
        .split('(')
        .next()
        .unwrap_or("")
        .replace(" unsigned", "")
        .replace(" zerofill", "")
        .replace(" without time zone", "")
        .replace(" with time zone", "");
    let base = base.trim();
    // `time(3) without time zone` keeps its tail after the parameters.
    let tail = lower
        .rsplit(')')
        .next()
        .filter(|_| lower.contains(')'))
        .unwrap_or("");
    let with_tz = lower.contains("with time zone") || tail.contains("with time zone");
    match base {
        "smallint" if unsigned => TypeClass::Int4,
        "smallint" | "int2" | "smallserial" | "serial2" => TypeClass::Int2,
        "int" | "integer" | "int4" | "serial" | "serial4" if unsigned => TypeClass::Int8,
        "int" | "integer" | "int4" | "serial" | "serial4" | "mediumint" => TypeClass::Int4,
        "bigint" | "int8" | "bigserial" | "serial8" if unsigned => {
            TypeClass::Numeric(Some((20, 0)))
        }
        "bigint" | "int8" | "bigserial" | "serial8" => TypeClass::Int8,
        "tinyint" if params.first() == Some(&1) => TypeClass::Bool,
        "tinyint" => TypeClass::Int2,
        "numeric" | "decimal" | "dec" | "number" => TypeClass::Numeric(match params.as_slice() {
            [p, s] => Some((*p, *s)),
            [p] => Some((*p, 0)),
            _ => None,
        }),
        "real" | "float4" => TypeClass::Float4,
        "float" if dialect == "mysql" => TypeClass::Float4,
        "float" | "double" | "double precision" | "float8" => TypeClass::Float8,
        "bool" | "boolean" => TypeClass::Bool,
        "text" | "tinytext" | "mediumtext" | "longtext" | "clob" | "string" | "name" => {
            TypeClass::Text
        }
        "varchar" | "character varying" | "nvarchar" | "varchar2" | "nvarchar2" => {
            TypeClass::Varchar(params.first().copied())
        }
        "char" | "character" | "bpchar" | "nchar" => TypeClass::Char(params.first().copied()),
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" => {
            TypeClass::Bytes
        }
        "date" => TypeClass::Date,
        "time" | "timetz" => TypeClass::Time,
        "timestamp" | "datetime" if with_tz => TypeClass::TimestampTz,
        "timestamp" | "datetime" | "datetime2" => TypeClass::Timestamp,
        "timestamptz" => TypeClass::TimestampTz,
        "interval" => TypeClass::Interval,
        "uuid" => TypeClass::Uuid,
        "json" | "jsonb" => TypeClass::Json,
        "enum" | "set" => TypeClass::Enum,
        _ => TypeClass::Unknown,
    }
}

/// `type_name` of `from_dialect` (`postgres`, `mysql`, `sqlite`) as the nearest type of
/// `to_dialect`, with a warning when the mapping loses something or the type is unknown
/// (it maps to text). The same dialect keeps the type as written.
pub fn map_type(from_dialect: &str, type_name: &str, to_dialect: &str) -> (String, Option<String>) {
    if from_dialect == to_dialect {
        return (type_name.to_string(), None);
    }
    let mut class = classify_type(from_dialect, type_name);
    // SQLite's INTEGER holds 64 bits whatever its declared width.
    if from_dialect == "sqlite" && matches!(class, TypeClass::Int2 | TypeClass::Int4) {
        class = TypeClass::Int8;
    }
    // …and its REAL is a double.
    if from_dialect == "sqlite" && class == TypeClass::Float4 {
        class = TypeClass::Float8;
    }
    let mut warning = None;
    let mapped = match to_dialect {
        "postgres" => match &class {
            TypeClass::Int2 => "smallint".into(),
            TypeClass::Int4 => "integer".into(),
            // SQLite's INTEGER is a 64-bit value.
            TypeClass::Int8 => "bigint".into(),
            TypeClass::Numeric(Some((p, s))) => format!("numeric({p},{s})"),
            TypeClass::Numeric(None) => "numeric".into(),
            TypeClass::Float4 => "real".into(),
            TypeClass::Float8 => "double precision".into(),
            TypeClass::Bool => "boolean".into(),
            TypeClass::Text | TypeClass::Enum => "text".into(),
            TypeClass::Varchar(Some(n)) => format!("varchar({n})"),
            TypeClass::Varchar(None) => "varchar".into(),
            TypeClass::Char(Some(n)) => format!("char({n})"),
            TypeClass::Char(None) => "char".into(),
            TypeClass::Bytes => "bytea".into(),
            TypeClass::Date => "date".into(),
            TypeClass::Time => "time".into(),
            TypeClass::Timestamp => "timestamp".into(),
            TypeClass::TimestampTz => "timestamptz".into(),
            TypeClass::Interval => "interval".into(),
            TypeClass::Uuid => "uuid".into(),
            TypeClass::Json => "jsonb".into(),
            TypeClass::Array => "jsonb".into(),
            TypeClass::Unknown => "text".into(),
        },
        "mysql" => match &class {
            TypeClass::Int2 => "smallint".into(),
            TypeClass::Int4 => "int".into(),
            TypeClass::Int8 => "bigint".into(),
            TypeClass::Numeric(Some((p, s))) => format!("decimal({p},{s})"),
            TypeClass::Numeric(None) => "decimal(38,10)".into(),
            TypeClass::Float4 => "float".into(),
            TypeClass::Float8 => "double".into(),
            TypeClass::Bool => "tinyint(1)".into(),
            TypeClass::Text | TypeClass::Unknown => "longtext".into(),
            TypeClass::Varchar(Some(n)) => format!("varchar({n})"),
            TypeClass::Varchar(None) => "longtext".into(),
            TypeClass::Char(Some(n)) => format!("char({n})"),
            TypeClass::Char(None) => "char(1)".into(),
            TypeClass::Bytes => "longblob".into(),
            TypeClass::Date => "date".into(),
            TypeClass::Time => "time(6)".into(),
            TypeClass::Timestamp => "datetime(6)".into(),
            TypeClass::TimestampTz => {
                warning = Some("The time zone is dropped: values are stored as UTC".to_string());
                "datetime(6)".into()
            }
            TypeClass::Interval => {
                warning = Some("MySQL has no interval type: stored as text".to_string());
                "varchar(64)".into()
            }
            TypeClass::Uuid => "char(36)".into(),
            TypeClass::Json | TypeClass::Array => "json".into(),
            TypeClass::Enum => "varchar(255)".into(),
        },
        "sqlite" => match &class {
            TypeClass::Int2 | TypeClass::Int4 | TypeClass::Int8 => "integer".into(),
            TypeClass::Numeric(Some((p, _))) if *p <= 15 => "numeric".into(),
            TypeClass::Numeric(_) => {
                warning = Some(
                    "SQLite stores numbers as 64-bit floats: the decimal stays text to remain exact"
                        .to_string(),
                );
                "text".into()
            }
            TypeClass::Float4 | TypeClass::Float8 => "real".into(),
            TypeClass::Bool => "boolean".into(),
            TypeClass::Bytes => "blob".into(),
            TypeClass::Interval => {
                warning = Some("SQLite has no interval type: stored as text".to_string());
                "text".into()
            }
            _ => "text".into(),
        },
        "sqlserver" => match &class {
            TypeClass::Int2 => "smallint".into(),
            TypeClass::Int4 => "int".into(),
            TypeClass::Int8 => "bigint".into(),
            TypeClass::Numeric(Some((p, s))) => format!("decimal({p},{s})"),
            TypeClass::Numeric(None) => "decimal(38,10)".into(),
            TypeClass::Float4 => "real".into(),
            TypeClass::Float8 => "float".into(),
            TypeClass::Bool => "bit".into(),
            TypeClass::Text => "nvarchar(max)".into(),
            TypeClass::Varchar(Some(n)) => format!("nvarchar({n})"),
            TypeClass::Varchar(None) => "nvarchar(max)".into(),
            TypeClass::Char(Some(n)) => format!("nchar({n})"),
            TypeClass::Char(None) => "nchar(1)".into(),
            TypeClass::Bytes => "varbinary(max)".into(),
            TypeClass::Date => "date".into(),
            TypeClass::Time => "time".into(),
            TypeClass::Timestamp => "datetime2".into(),
            TypeClass::TimestampTz => "datetimeoffset".into(),
            TypeClass::Interval => {
                warning = Some("SQL Server has no interval type: stored as text".to_string());
                "nvarchar(64)".into()
            }
            TypeClass::Uuid => "uniqueidentifier".into(),
            TypeClass::Json | TypeClass::Array => "nvarchar(max)".into(),
            TypeClass::Enum => "nvarchar(255)".into(),
            TypeClass::Unknown => "nvarchar(max)".into(),
        },
        "oracle" => match &class {
            TypeClass::Int2 | TypeClass::Int4 => "number(10)".into(),
            TypeClass::Int8 => "number(19)".into(),
            TypeClass::Numeric(Some((p, s))) => format!("number({p},{s})"),
            TypeClass::Numeric(None) => "number".into(),
            TypeClass::Float4 => "binary_float".into(),
            TypeClass::Float8 => "binary_double".into(),
            TypeClass::Bool => "number(1)".into(),
            TypeClass::Text => "clob".into(),
            TypeClass::Varchar(Some(n)) => format!("varchar2({n})"),
            TypeClass::Varchar(None) => "varchar2(4000)".into(),
            TypeClass::Char(Some(n)) => format!("char({n})"),
            TypeClass::Char(None) => "char(1)".into(),
            TypeClass::Bytes => "blob".into(),
            TypeClass::Date => "date".into(),
            TypeClass::Time => {
                warning = Some("Oracle has no time-only type: stored as text".to_string());
                "varchar2(32)".into()
            }
            TypeClass::Timestamp => "timestamp".into(),
            TypeClass::TimestampTz => "timestamp with time zone".into(),
            TypeClass::Interval => "interval day to second".into(),
            TypeClass::Uuid => "varchar2(36)".into(),
            TypeClass::Json | TypeClass::Array => "clob".into(),
            TypeClass::Enum => "varchar2(255)".into(),
            TypeClass::Unknown => "varchar2(4000)".into(),
        },
        "duckdb" => match &class {
            TypeClass::Int2 => "smallint".into(),
            TypeClass::Int4 => "integer".into(),
            TypeClass::Int8 => "bigint".into(),
            TypeClass::Numeric(Some((p, s))) => format!("decimal({p},{s})"),
            TypeClass::Numeric(None) => "decimal(38,10)".into(),
            TypeClass::Float4 => "real".into(),
            TypeClass::Float8 => "double".into(),
            TypeClass::Bool => "boolean".into(),
            TypeClass::Text => "varchar".into(),
            TypeClass::Varchar(Some(n)) => format!("varchar({n})"),
            TypeClass::Varchar(None) => "varchar".into(),
            TypeClass::Char(Some(n)) => format!("char({n})"),
            TypeClass::Char(None) => "char".into(),
            TypeClass::Bytes => "blob".into(),
            TypeClass::Date => "date".into(),
            TypeClass::Time => "time".into(),
            TypeClass::Timestamp => "timestamp".into(),
            TypeClass::TimestampTz => "timestamptz".into(),
            TypeClass::Interval => "interval".into(),
            TypeClass::Uuid => "uuid".into(),
            TypeClass::Json | TypeClass::Array => "json".into(),
            TypeClass::Enum => "varchar".into(),
            TypeClass::Unknown => "varchar".into(),
        },
        "clickhouse" => match &class {
            TypeClass::Int2 => "Int16".into(),
            TypeClass::Int4 => "Int32".into(),
            TypeClass::Int8 => "Int64".into(),
            TypeClass::Numeric(Some((p, s))) => format!("Decimal({p},{s})"),
            TypeClass::Numeric(None) => "Decimal(38,10)".into(),
            TypeClass::Float4 => "Float32".into(),
            TypeClass::Float8 => "Float64".into(),
            TypeClass::Bool => "Bool".into(),
            TypeClass::Text | TypeClass::Varchar(_) | TypeClass::Char(_) | TypeClass::Bytes => {
                "String".into()
            }
            TypeClass::Date => "Date".into(),
            TypeClass::Time => {
                warning = Some("ClickHouse has no time-only type: stored as text".to_string());
                "String".into()
            }
            TypeClass::Timestamp => "DateTime64(6)".into(),
            TypeClass::TimestampTz => "DateTime64(6, 'UTC')".into(),
            TypeClass::Interval => {
                warning = Some("ClickHouse has no interval type: stored as text".to_string());
                "String".into()
            }
            TypeClass::Uuid => "UUID".into(),
            TypeClass::Json | TypeClass::Array | TypeClass::Enum | TypeClass::Unknown => {
                "String".into()
            }
        },
        "snowflake" => match &class {
            TypeClass::Int2 => "NUMBER(5)".into(),
            TypeClass::Int4 => "NUMBER(10)".into(),
            TypeClass::Int8 => "NUMBER(19)".into(),
            TypeClass::Numeric(Some((p, s))) => format!("NUMBER({p},{s})"),
            TypeClass::Numeric(None) => "NUMBER(38,10)".into(),
            TypeClass::Float4 => "FLOAT".into(),
            TypeClass::Float8 => "DOUBLE".into(),
            TypeClass::Bool => "BOOLEAN".into(),
            TypeClass::Text => "VARCHAR".into(),
            TypeClass::Varchar(Some(n)) => format!("VARCHAR({n})"),
            TypeClass::Varchar(None) => "VARCHAR".into(),
            TypeClass::Char(Some(n)) => format!("CHAR({n})"),
            TypeClass::Char(None) => "CHAR".into(),
            TypeClass::Bytes => "BINARY".into(),
            TypeClass::Date => "DATE".into(),
            TypeClass::Time => "TIME".into(),
            TypeClass::Timestamp => "TIMESTAMP_NTZ".into(),
            TypeClass::TimestampTz => "TIMESTAMP_TZ".into(),
            TypeClass::Interval => {
                warning = Some("Snowflake has no interval type: stored as text".to_string());
                "VARCHAR".into()
            }
            TypeClass::Uuid => "VARCHAR(36)".into(),
            TypeClass::Json | TypeClass::Array => "VARIANT".into(),
            TypeClass::Enum => "VARCHAR".into(),
            TypeClass::Unknown => "VARCHAR".into(),
        },
        "hana" => match &class {
            TypeClass::Int2 => "SMALLINT".into(),
            TypeClass::Int4 => "INTEGER".into(),
            TypeClass::Int8 => "BIGINT".into(),
            TypeClass::Numeric(Some((p, s))) => format!("DECIMAL({p},{s})"),
            TypeClass::Numeric(None) => "DECIMAL(38,10)".into(),
            TypeClass::Float4 => "REAL".into(),
            TypeClass::Float8 => "DOUBLE".into(),
            TypeClass::Bool => "BOOLEAN".into(),
            TypeClass::Text => "NCLOB".into(),
            TypeClass::Varchar(Some(n)) => format!("NVARCHAR({n})"),
            TypeClass::Varchar(None) => "NVARCHAR(5000)".into(),
            TypeClass::Char(Some(n)) => format!("NCHAR({n})"),
            TypeClass::Char(None) => "NCHAR(1)".into(),
            TypeClass::Bytes => "BLOB".into(),
            TypeClass::Date => "DATE".into(),
            TypeClass::Time => "TIME".into(),
            TypeClass::Timestamp | TypeClass::TimestampTz => "TIMESTAMP".into(),
            TypeClass::Interval => {
                warning = Some("SAP HANA has no interval type: stored as text".to_string());
                "NVARCHAR(64)".into()
            }
            TypeClass::Uuid => "VARBINARY(16)".into(),
            TypeClass::Json | TypeClass::Array | TypeClass::Enum | TypeClass::Unknown => {
                "NCLOB".into()
            }
        },
        "jdbc" => match &class {
            TypeClass::Int2 => "SMALLINT".into(),
            TypeClass::Int4 => "INTEGER".into(),
            TypeClass::Int8 => "BIGINT".into(),
            TypeClass::Numeric(Some((p, s))) => format!("DECIMAL({p},{s})"),
            TypeClass::Numeric(None) => "DECIMAL(38,10)".into(),
            TypeClass::Float4 => "REAL".into(),
            TypeClass::Float8 => "DOUBLE PRECISION".into(),
            TypeClass::Bool => "BOOLEAN".into(),
            TypeClass::Text => "CLOB".into(),
            TypeClass::Varchar(Some(n)) => format!("VARCHAR({n})"),
            TypeClass::Varchar(None) => "VARCHAR(4000)".into(),
            TypeClass::Char(Some(n)) => format!("CHAR({n})"),
            TypeClass::Char(None) => "CHAR(1)".into(),
            TypeClass::Bytes => "BLOB".into(),
            TypeClass::Date => "DATE".into(),
            TypeClass::Time => "TIME".into(),
            TypeClass::Timestamp => "TIMESTAMP".into(),
            TypeClass::TimestampTz => "TIMESTAMP WITH TIME ZONE".into(),
            TypeClass::Interval => {
                warning = Some("The JDBC target has no interval type: stored as text".to_string());
                "VARCHAR(64)".into()
            }
            TypeClass::Uuid => "VARCHAR(36)".into(),
            TypeClass::Json | TypeClass::Array | TypeClass::Enum | TypeClass::Unknown => {
                "CLOB".into()
            }
        },
        other => {
            warning = Some(format!(
                "No type mapping to {other}: the type {type_name} is kept"
            ));
            type_name.trim().to_string()
        }
    };
    if class == TypeClass::Unknown {
        warning = Some(format!(
            "The type {} is unknown to {to_dialect}: mapped to {mapped}",
            type_name.trim()
        ));
    }
    (mapped, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(from: &str, ty: &str, to: &str) -> String {
        map_type(from, ty, to).0
    }

    #[test]
    fn auto_mapping_matches_names_ignoring_case_and_skips_the_rest() {
        let source = ["ID", "Name", "extra"].map(String::from);
        let target = ["name", "id", "other"].map(String::from);
        let mapping = auto_mapping(&source, &target);
        assert_eq!(mapping.len(), 3);
        assert_eq!(mapping[0].target.as_deref(), Some("id"));
        assert_eq!(mapping[1].target.as_deref(), Some("name"));
        assert_eq!(mapping[2].target, None, "no column of that name: skipped");
        assert_eq!(
            mapping.iter().map(|m| m.source).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_new_table_def_maps_the_types_and_collects_warnings() {
        let table = TableRef {
            database: Some("ignored".into()),
            schema: Some("public".into()),
            name: "copy".into(),
        };
        let columns = [
            ("id".to_string(), "int4".to_string()),
            ("seen".to_string(), "timestamptz".to_string()),
            ("blob".to_string(), "geometry".to_string()),
        ];
        let (def, warnings) = new_table_def(&table, "postgres", "mysql", &columns);
        assert_eq!(def.table.database, None);
        assert_eq!(def.table.name, "copy");
        let types: Vec<&str> = def.columns.iter().map(|c| c.type_name.as_str()).collect();
        assert_eq!(types, ["int", "datetime(6)", "longtext"]);
        assert!(def.columns.iter().all(|c| c.nullable));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with("Column seen:"));
    }

    #[test]
    fn unsigned_mysql_integers_map_to_a_wide_enough_type() {
        // The signed twin is too narrow: `INT UNSIGNED` reaches 4_294_967_295 and
        // `BIGINT UNSIGNED` 18_446_744_073_709_551_615.
        assert_eq!(map("mysql", "tinyint unsigned", "postgres"), "smallint");
        assert_eq!(map("mysql", "smallint unsigned", "postgres"), "integer");
        assert_eq!(map("mysql", "mediumint unsigned", "postgres"), "integer");
        assert_eq!(map("mysql", "int unsigned", "postgres"), "bigint");
        assert_eq!(map("mysql", "integer unsigned", "postgres"), "bigint");
        assert_eq!(map("mysql", "bigint unsigned", "postgres"), "numeric(20,0)");
        assert_eq!(map("mysql", "int unsigned", "sqlite"), "integer");
        let (ty, warning) = map_type("mysql", "bigint unsigned", "sqlite");
        assert_eq!(ty, "text");
        assert!(warning.unwrap().contains("exact"));
        assert_eq!(map("mysql", "int unsigned", "mysql"), "int unsigned");
    }

    #[test]
    fn map_type_table() {
        // PostgreSQL to MySQL.
        assert_eq!(map("postgres", "integer", "mysql"), "int");
        assert_eq!(map("postgres", "bigint", "mysql"), "bigint");
        assert_eq!(map("postgres", "numeric(12,3)", "mysql"), "decimal(12,3)");
        assert_eq!(map("postgres", "double precision", "mysql"), "double");
        assert_eq!(map("postgres", "boolean", "mysql"), "tinyint(1)");
        assert_eq!(map("postgres", "text", "mysql"), "longtext");
        assert_eq!(
            map("postgres", "character varying(40)", "mysql"),
            "varchar(40)"
        );
        assert_eq!(map("postgres", "bytea", "mysql"), "longblob");
        assert_eq!(
            map("postgres", "timestamp without time zone", "mysql"),
            "datetime(6)"
        );
        assert_eq!(map("postgres", "uuid", "mysql"), "char(36)");
        assert_eq!(map("postgres", "jsonb", "mysql"), "json");
        assert_eq!(map("postgres", "integer[]", "mysql"), "json");
        // MySQL to SQLite.
        let (ty, warning) = map_type("mysql", "bigint unsigned", "sqlite");
        assert_eq!(
            ty, "text",
            "an unsigned 64-bit value does not fit SQLite's INTEGER"
        );
        assert!(warning.unwrap().contains("exact"));
        assert_eq!(map("mysql", "decimal(10,2)", "sqlite"), "numeric");
        // SQLite keeps numbers as 64-bit floats: wide or unspecified decimals stay text.
        let (ty, warning) = map_type("mysql", "decimal(30,10)", "sqlite");
        assert_eq!(ty, "text");
        assert!(warning.unwrap().contains("exact"));
        assert_eq!(map("postgres", "numeric", "sqlite"), "text");
        assert_eq!(map("mysql", "tinyint(1)", "sqlite"), "boolean");
        assert_eq!(map("mysql", "varchar(255)", "sqlite"), "text");
        assert_eq!(map("mysql", "longblob", "sqlite"), "blob");
        assert_eq!(map("mysql", "datetime", "sqlite"), "text");
        assert_eq!(map("mysql", "json", "sqlite"), "text");
        assert_eq!(map("mysql", "enum('a','b')", "sqlite"), "text");
        // SQLite to PostgreSQL.
        assert_eq!(map("sqlite", "INTEGER", "postgres"), "bigint");
        assert_eq!(map("sqlite", "REAL", "postgres"), "double precision");
        assert_eq!(map("sqlite", "TEXT", "postgres"), "text");
        assert_eq!(map("sqlite", "BLOB", "postgres"), "bytea");
        assert_eq!(map("sqlite", "BOOLEAN", "postgres"), "boolean");
        assert_eq!(map("sqlite", "DATETIME", "postgres"), "timestamp");
        // The same dialect keeps the type as written.
        assert_eq!(map("postgres", "numeric(7,2)", "postgres"), "numeric(7,2)");
        assert_eq!(map("sqlite", "WEIRD", "sqlite"), "WEIRD");
        // Lossy and unknown mappings say so.
        let (ty, warning) = map_type("postgres", "timestamp with time zone", "mysql");
        assert_eq!(ty, "datetime(6)");
        assert!(warning.unwrap().contains("time zone"));
        let (ty, warning) = map_type("postgres", "interval", "mysql");
        assert_eq!(ty, "varchar(64)");
        assert!(warning.is_some());
        let (ty, warning) = map_type("postgres", "geometry", "mysql");
        assert_eq!(ty, "longtext");
        assert!(warning.unwrap().contains("geometry"));
        assert_eq!(map_type("postgres", "integer", "mysql").1, None);
        // SQL Server's `int` is 32-bit: a bigint must stay a bigint.
        assert_eq!(map("postgres", "bigint", "sqlserver"), "bigint");
        assert_eq!(map("postgres", "text", "sqlserver"), "nvarchar(max)");
        assert_eq!(
            map("postgres", "timestamptz", "sqlserver"),
            "datetimeoffset"
        );
        // Oracle has no `text`; varchar2 is the right string type.
        assert_eq!(map("postgres", "text", "oracle"), "clob");
        assert_eq!(map("postgres", "bigint", "oracle"), "number(19)");
        // An unknown target keeps the source type and says so instead of using SQLite types.
        let (ty, warning) = map_type("postgres", "bigint", "unknown-db");
        assert_eq!(ty, "bigint");
        assert!(warning.unwrap().contains("No type mapping"));
    }
}
