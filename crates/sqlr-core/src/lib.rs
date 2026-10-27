//! Domain types and the columnar result format shared by all sqlr crates.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![deny(missing_docs)]

pub mod admin;
pub mod catalog;
pub mod changeset;
pub mod chunk;
pub mod classify;
pub mod clipboard;
pub mod compare;
pub mod config;
pub mod datagen;
pub mod doc;
pub mod doc_admin;
pub mod doc_rows;
pub mod docchunk;
pub mod document;
pub mod driver;
mod driver_properties;
pub use driver_properties::{driver_property_fields, validate_driver_properties};
pub mod encode;
pub mod er;
pub mod error;
pub mod es_request;
pub mod format_sql;
pub mod formats;
pub use format_sql::{Case, FormatOptions, Indent, format_sql};
pub mod generate;
pub mod keymap;
pub mod known;
pub mod kv;
pub mod lang;
pub mod library;
pub mod manifest;
pub mod mongo_shell;
pub mod node;
pub mod panic_scope;
pub mod plan;
pub mod profile;
pub mod read;
pub mod rows;
pub mod schema;
pub mod schema_change;
pub mod settings_model;
pub mod syntax;
pub mod table;
pub mod task;
pub mod tools;
mod transfer;
pub mod url;
pub mod value;
pub mod wire;

pub use admin::*;
pub use catalog::*;
pub use changeset::*;
pub use chunk::*;
pub use classify::*;
pub use clipboard::*;
pub use compare::{DiffKind, ObjectDiff, SchemaSnapshot, compare, migration_script, normalize};
pub use config::*;
pub use datagen::{
    ColumnGen, DataGenSpec, DatagenError, Generator, RowGenerator, default_generator,
};
pub use doc::*;
pub use doc_admin::*;
pub use doc_rows::*;
pub use docchunk::*;
pub use document::*;
pub use driver::*;
pub use encode::*;
pub use er::{
    ErColors, ErColumn, ErEdge, ErLayout, ErModel, ErTable, layout as er_layout,
    to_svg as er_to_svg,
};
pub use error::*;
pub use es_request::*;
pub use formats::DataFormats;
pub use generate::*;
pub use keymap::{ActionId, Chord, GROUPS as ACTION_GROUPS, Keymap, Mods, PRESETS, Scope};
pub use known::*;
pub use kv::*;
pub use lang::*;
pub use library::*;
pub use manifest::*;
pub use mongo_shell::*;
pub use node::*;
pub use plan::{
    ExplainParser, ExplainSpec, PlanInsights, PlanIssue, PlanIssueLevel, PlanNode, PlanRole,
    PlanTree, explain_check, parse_mysql_json, parse_mysql_tree, parse_pg_json, parse_sqlite_rows,
};
pub use profile::*;
pub use read::{ReadError, read_rows};
pub use rows::*;
pub use schema::*;
pub use schema_change::*;
pub use settings_model::*;
pub use syntax::*;
pub use table::*;
pub use task::*;
pub use tools::*;
pub use transfer::*;
pub use url::*;
pub use value::*;
pub use wire::*;
