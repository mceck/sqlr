//! Static description of a driver: identity, capabilities and connection form.

use crate::schema::ObjectKind;

/// The shape of a driver's data and operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Family {
    /// Tables and SQL (the `RelationalConn` trait).
    Relational,
    /// Collections of documents (the `DocumentConn` trait).
    Document,
    /// Keys and typed values (the `KeyValueConn` trait).
    KeyValue,
}

/// The optional features a driver declares; the UI offers an action only when the driver declares it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// Explicit transactions (manual commit mode).
    pub transactions: bool,
    /// Running statements can be cancelled on the server.
    pub server_cancel: bool,
    /// Result grids can be edited through a changeset.
    pub editable_results: bool,
    /// `EXPLAIN` plans.
    pub explain: bool,
    /// One statement can return several result sets.
    pub multi_resultset: bool,
    /// Queries return tabular results.
    pub tabular_query: bool,
    /// The driver has schemas between database and table.
    pub schemas: bool,
    /// Connections can go through an SSH tunnel.
    pub ssh: bool,
    /// Databases dump and restore through native tools (`pg_dump`, `mysqldump`, …).
    pub dump: bool,
}

/// The kind of input a connection form field takes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FieldType {
    /// A single-line text.
    Text,
    /// A port number.
    Port,
    /// A masked secret, kept in the credential vault.
    Password,
    /// A checkbox.
    Bool,
    /// A file path picked with a native dialog; `save` asks for a new file.
    File {
        /// With `save`, picks a new file rather than an existing one.
        save: bool,
        /// File extensions the dialog offers.
        extensions: Vec<String>,
    },
    /// One of `(value, label)`.
    Choice(Vec<(String, String)>),
    /// A non-negative whole number.
    Number,
}

/// One field of a driver's connection form.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldSpec {
    /// Key under which the value is stored in the connection.
    pub key: String,
    /// Label shown next to the input.
    pub label: String,
    /// The kind of input.
    pub ty: FieldType,
    /// Value the field starts with.
    pub default: Option<String>,
    /// The form refuses to connect without a value.
    pub required: bool,
    /// The value is a secret: stored in the credential vault, never in `connections.toml`.
    pub secret: bool,
    /// Name of the form section that holds the field.
    pub group: String,
    /// Accept arbitrary non-secret session properties in this group.
    #[serde(default)]
    pub free_form: bool,
}

/// The description a driver publishes of itself: identity, family, capabilities, dialect and connection form.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DriverManifest {
    /// Driver id (`postgres`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// The driver's family.
    pub family: Family,
    /// The capabilities the driver declares.
    pub capabilities: Capabilities,
    /// The connection form fields.
    pub fields: Vec<FieldSpec>,
    /// Name of the dialect that quotes identifiers and generates SQL for the driver.
    pub dialect: String,
    /// URL schemes the connection form accepts in its URL box (`postgresql`, `mysql`, …).
    pub url_schemes: Vec<String>,
    /// Port the form starts with.
    pub default_port: Option<u16>,
    /// Object editing support; `None` hides object editors.
    pub editing: Option<ObjectEditing>,
}

/// What object editors may do with a driver's objects (spec §5.3): menu entries and
/// sub-tabs are shown only for the kinds listed.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObjectEditing {
    /// Kinds that can be created.
    pub create: Vec<ObjectKind>,
    /// Kinds that can be altered.
    pub alter: Vec<ObjectKind>,
    /// Kinds that can be dropped.
    pub drop: Vec<ObjectKind>,
    /// Kinds that can be renamed.
    pub rename: Vec<ObjectKind>,
    /// DDL runs inside a transaction (all or nothing).
    pub transactional_ddl: bool,
    /// Column type suggestions for the column editor.
    pub column_types: Vec<String>,
    /// Objects and columns take comments.
    pub comments: bool,
    /// A table's columns can be reordered (MySQL `AFTER`, a SQLite rebuild).
    pub column_order: bool,
}

impl DriverManifest {
    /// The query language the driver's editor tabs use.
    pub fn language(&self) -> crate::lang::QueryLanguage {
        crate::lang::QueryLanguage::for_dialect(&self.dialect)
    }
}
