//! Static description of a driver: identity, capabilities and connection form.

use crate::schema::ObjectKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Family {
    Relational,
    Document,
    KeyValue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    pub transactions: bool,
    pub server_cancel: bool,
    pub editable_results: bool,
    pub explain: bool,
    pub multi_resultset: bool,
    pub tabular_query: bool,
    pub schemas: bool,
    pub ssh: bool,
    /// Databases dump and restore through native tools (`pg_dump`, `mysqldump`, …).
    pub dump: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FieldType {
    Text,
    Port,
    Password,
    Bool,
    /// A file path picked with a native dialog; `save` asks for a new file.
    File {
        save: bool,
        extensions: Vec<String>,
    },
    /// One of `(value, label)`.
    Choice(Vec<(String, String)>),
    /// A non-negative whole number.
    Number,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    pub ty: FieldType,
    pub default: Option<String>,
    pub required: bool,
    pub secret: bool,
    pub group: String,
    /// Accept arbitrary non-secret session properties in this group.
    #[serde(default)]
    pub free_form: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DriverManifest {
    pub id: String,
    pub name: String,
    pub family: Family,
    pub capabilities: Capabilities,
    pub fields: Vec<FieldSpec>,
    pub dialect: String,
    /// URL schemes the connection form accepts in its URL box (`postgresql`, `mysql`, …).
    pub url_schemes: Vec<String>,
    pub default_port: Option<u16>,
    /// Object editing support; `None` hides object editors.
    pub editing: Option<ObjectEditing>,
}

/// What object editors may do with a driver's objects (spec §5.3): menu entries and
/// sub-tabs are shown only for the kinds listed.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObjectEditing {
    pub create: Vec<ObjectKind>,
    pub alter: Vec<ObjectKind>,
    pub drop: Vec<ObjectKind>,
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
