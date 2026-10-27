//! Pending edits of a result (§7.7): changed cells, inserted rows and deleted rows, and the
//! changeset they make. Rows are numbered like the grid's source rows: loaded rows by
//! their index, inserted rows from [`INSERTED_ROW_BASE`] up, so their numbers stay the same
//! while more rows load.

use std::collections::{BTreeMap, BTreeSet};

use sqlr_core::{
    Cell, Change, Changeset, ColumnKind, ColumnMeta, RowBuffer, TableInfo, TableRef, Value,
};

/// Why a result's rows cannot be edited, or an edit is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The result has columns that come from no table column.
    #[error("computed columns")]
    ComputedColumns,
    /// The result mixes columns of several tables.
    #[error("columns from several tables")]
    SeveralTables,
    /// The table has no primary or unique key.
    #[error("no primary or unique key on {table}")]
    NoRowKey {
        /// The table name.
        table: String,
    },
    /// The result does not hold the table's key columns.
    #[error("the key of {table} is not in the result")]
    KeyNotInResult {
        /// The table name.
        table: String,
    },
    /// The result as a whole cannot be edited.
    #[error("The result cannot be edited")]
    ResultNotEditable,
    /// The column is generated or not part of the table.
    #[error("This column cannot be edited")]
    ColumnNotEditable,
    /// NULL for a `NOT NULL` column.
    #[error("The column is NOT NULL")]
    NotNull,
    /// The inserted row does not exist.
    #[error("No such row")]
    NoSuchRow,
    /// The row is marked for deletion.
    #[error("The row is deleted")]
    RowDeleted,
    /// A paste into a result that omits required columns.
    #[error("Open all table columns before pasting; this result omits required columns: {columns}")]
    MissingRequired {
        /// The omitted columns, comma separated.
        columns: String,
    },
    /// A row's key holds NULL or is not loaded.
    #[error("row {row} has no key value")]
    NoKeyValue {
        /// The 1-based row number.
        row: usize,
    },
}

/// Text typed into a cell that is not a value of the column's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    /// A whole number was expected.
    #[error("Not an integer")]
    NotInteger,
    /// A number was expected.
    #[error("Not a number")]
    NotNumber,
    /// A boolean was expected.
    #[error("Not a boolean (true or false)")]
    NotBoolean,
    /// Hex bytes (`0x…`) were expected.
    #[error("Not hex bytes (0x…)")]
    NotHexBytes,
}

/// Why a typed or pasted value cannot be put in a cell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CellEditError {
    /// The text is not a value of the column's kind.
    #[error(transparent)]
    Input(#[from] InputError),
    /// The edit buffer refuses the cell.
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// The row number of the first inserted row; inserted rows count up from it, far above any
/// loaded row.
pub const INSERTED_ROW_BASE: usize = 1 << 48;

/// Whether a grid row number stands for an inserted row.
pub fn is_inserted_row(row: usize) -> bool {
    row >= INSERTED_ROW_BASE
}

/// What happened to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Clean,
    Edited,
    Inserted,
    Deleted,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditBuffer {
    /// The result columns of the row key, in key order.
    key_cols: Vec<usize>,
    /// Per result column: it may be written (read from the table, not computed).
    writable: Vec<bool>,
    /// Per result column: the table column is `NOT NULL`.
    not_null: Vec<bool>,
    automatic: Vec<bool>,
    defaults: Vec<bool>,
    missing_required: Vec<String>,
    /// New values of loaded rows, by row then column.
    updates: BTreeMap<usize, BTreeMap<usize, Value>>,
    /// Inserted rows by their row number, in insertion order; `None` leaves the column to
    /// its default.
    inserted: Vec<(usize, Vec<Option<Value>>)>,
    /// The row number of the next inserted row.
    next_inserted: usize,
    deleted: BTreeSet<usize>,
    /// Bumps on every change.
    revision: u64,
}

impl EditBuffer {
    pub fn new(key_cols: Vec<usize>, writable: Vec<bool>, not_null: Vec<bool>) -> Self {
        let automatic = (0..writable.len()).map(|c| key_cols.contains(&c)).collect();
        let defaults = vec![false; writable.len()];
        Self {
            automatic,
            defaults,
            key_cols,
            writable,
            not_null,
            ..Self::default()
        }
    }

    /// The edit buffer for a result read from `info`'s table, or why its rows cannot be
    /// edited: `computed columns`, `columns from several tables`, `no primary or unique key
    /// on <table>`, `the key of <table> is not in the result`.
    pub fn for_table(columns: &[ColumnMeta], info: &TableInfo) -> Result<Self, EditError> {
        let origins: Vec<_> = columns.iter().map(|c| c.origin.as_ref()).collect();
        let Some(first) = origins.iter().flatten().next() else {
            return Err(EditError::ComputedColumns);
        };
        if origins.iter().flatten().any(|o| o.table != first.table) {
            return Err(EditError::SeveralTables);
        }
        let table = &info.table.name;
        let Some(key) = info.row_key() else {
            return Err(EditError::NoRowKey {
                table: table.clone(),
            });
        };
        let key_cols = key
            .iter()
            .map(|name| {
                origins
                    .iter()
                    .position(|o| o.is_some_and(|o| &o.column == name))
            })
            .collect::<Option<Vec<usize>>>()
            .ok_or_else(|| EditError::KeyNotInResult {
                table: table.clone(),
            })?;
        let table_column = |c: usize| origins[c].and_then(|o| info.column(&o.column));
        let writable = (0..columns.len())
            .map(|c| table_column(c).is_some_and(|t| !t.generated))
            .collect();
        let not_null = (0..columns.len())
            .map(|c| table_column(c).is_some_and(|t| !t.nullable))
            .collect();
        let mut edits = Self::new(key_cols, writable, not_null);
        edits.automatic = (0..columns.len())
            .map(|c| {
                table_column(c).is_some_and(|t| {
                    t.automatic || (info.primary_key.contains(&t.name) && t.default.is_some())
                })
            })
            .collect();
        edits.defaults = (0..columns.len())
            .map(|c| {
                table_column(c).is_some_and(|t| t.default.is_some() || t.generated || t.automatic)
            })
            .collect();
        edits.missing_required = info
            .columns
            .iter()
            .filter(|t| !t.nullable && !t.generated && !t.automatic && t.default.is_none())
            .filter(|t| {
                !origins
                    .iter()
                    .flatten()
                    .any(|origin| origin.column == t.name)
            })
            .map(|t| t.name.clone())
            .collect();
        Ok(edits)
    }

    /// Changed rows, inserted rows and deleted rows.
    pub fn pending(&self) -> usize {
        let updated = self
            .updates
            .keys()
            .filter(|r| !self.deleted.contains(r))
            .count();
        updated + self.inserted.len() + self.deleted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending() == 0
    }

    /// Bumps on every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn inserted_rows(&self) -> usize {
        self.inserted.len()
    }

    /// The row numbers of the inserted rows, in insertion order.
    pub fn inserted_row_numbers(&self) -> Vec<usize> {
        self.inserted.iter().map(|(row, _)| *row).collect()
    }

    pub(super) fn inserted_number_at(&self, index: usize) -> Option<usize> {
        self.inserted.get(index).map(|(row, _)| *row)
    }

    fn inserted_cells(&self, row: usize) -> Option<&Vec<Option<Value>>> {
        self.inserted
            .binary_search_by_key(&row, |(r, _)| *r)
            .ok()
            .map(|index| &self.inserted[index].1)
    }

    pub fn writable(&self, col: usize) -> bool {
        self.writable.get(col).copied().unwrap_or(false)
    }

    pub fn not_null(&self, col: usize) -> bool {
        self.not_null.get(col).copied().unwrap_or(false)
    }

    pub fn automatic(&self, col: usize) -> bool {
        self.automatic.get(col).copied().unwrap_or(false)
    }

    pub fn has_default(&self, col: usize) -> bool {
        self.defaults.get(col).copied().unwrap_or(false) || self.automatic(col)
    }

    pub(super) fn check_insert_columns(&self) -> Result<(), EditError> {
        if self.missing_required.is_empty() {
            Ok(())
        } else {
            Err(EditError::MissingRequired {
                columns: self.missing_required.join(", "),
            })
        }
    }

    pub(super) fn content_eq(&self, other: &Self) -> bool {
        self.updates == other.updates
            && self.inserted == other.inserted
            && self.deleted == other.deleted
    }

    pub(super) fn history_bytes(&self) -> usize {
        let size = |v: &Value| {
            std::mem::size_of::<Value>()
                + match v {
                    Value::Text(t) => t.len(),
                    Value::Bytes(b) => b.len(),
                    _ => 0,
                }
        };
        self.updates
            .values()
            .flat_map(|r| r.values())
            .map(&size)
            .sum::<usize>()
            + self
                .inserted
                .iter()
                .flat_map(|(_, r)| r.iter().flatten())
                .map(&size)
                .sum::<usize>()
            + self.deleted.len() * 32
            + self.updates.len() * 64
            + self.inserted.len() * 64
    }

    pub(super) fn clipboard_overrides(&self) -> BTreeMap<(usize, usize), Value> {
        let mut values: BTreeMap<_, _> = self
            .updates
            .iter()
            .flat_map(|(&r, cells)| cells.iter().map(move |(&c, v)| ((r, c), v.clone())))
            .collect();
        for (r, cells) in &self.inserted {
            for (c, value) in cells.iter().enumerate() {
                values.insert((*r, c), value.clone().unwrap_or(Value::Null));
            }
        }
        values
    }

    pub(super) fn revert_cell(&mut self, row: usize, col: usize) {
        if is_inserted_row(row) {
            if let Ok(index) = self.inserted.binary_search_by_key(&row, |(r, _)| *r)
                && let Some(cell) = self.inserted[index].1.get_mut(col)
            {
                *cell = None;
            }
        } else if let Some(cells) = self.updates.get_mut(&row) {
            cells.remove(&col);
            if cells.is_empty() {
                self.updates.remove(&row);
            }
        }
        self.revision += 1;
    }

    pub fn row_state(&self, row: usize) -> RowState {
        if is_inserted_row(row) {
            RowState::Inserted
        } else if self.deleted.contains(&row) {
            RowState::Deleted
        } else if self.updates.contains_key(&row) {
            RowState::Edited
        } else {
            RowState::Clean
        }
    }

    /// The cell of a loaded row holds a new value.
    pub fn is_edited(&self, row: usize, col: usize) -> bool {
        self.updates.get(&row).is_some_and(|r| r.contains_key(&col))
    }

    /// The cell's current value: the new one, else the loaded one. `None` for a cell of an
    /// inserted row left to its default, or out of range.
    pub fn value(&self, rows: &RowBuffer, row: usize, col: usize) -> Option<Value> {
        if is_inserted_row(row) {
            return self.inserted_cells(row)?.get(col)?.clone();
        }
        if let Some(v) = self.updates.get(&row).and_then(|r| r.get(&col)) {
            return Some(v.clone());
        }
        rows.cell(row, col).map(Value::from_cell)
    }

    /// The displayed value without cloning potentially large text or binary cells.
    pub(super) fn cell<'a>(
        &'a self,
        rows: &'a RowBuffer,
        row: usize,
        col: usize,
    ) -> Option<Cell<'a>> {
        if is_inserted_row(row) {
            return self
                .inserted_cells(row)?
                .get(col)?
                .as_ref()
                .map(Value::as_cell);
        }
        self.updates
            .get(&row)
            .and_then(|r| r.get(&col))
            .map(Value::as_cell)
            .or_else(|| rows.cell(row, col))
    }

    /// Sets a cell; refused on a column that cannot be written, NULL on a `NOT NULL` column
    /// and a deleted row. Setting a loaded cell back to its loaded value drops the edit.
    pub fn set(
        &mut self,
        rows: &RowBuffer,
        row: usize,
        col: usize,
        value: Value,
    ) -> Result<(), EditError> {
        if !self.writable(col) {
            return Err(EditError::ColumnNotEditable);
        }
        if value.is_null() && self.not_null(col) {
            return Err(EditError::NotNull);
        }
        if is_inserted_row(row) {
            let width = self.writable.len();
            let cells = self
                .inserted
                .binary_search_by_key(&row, |(r, _)| *r)
                .ok()
                .and_then(|index| self.inserted.get_mut(index))
                .map(|(_, cells)| cells)
                .ok_or(EditError::NoSuchRow)?;
            cells.resize(width, None);
            cells[col] = Some(value);
        } else {
            if self.deleted.contains(&row) {
                return Err(EditError::RowDeleted);
            }
            let original = rows.cell(row, col).map(Value::from_cell);
            let edits = self.updates.entry(row).or_default();
            if original.as_ref() == Some(&value) {
                edits.remove(&col);
                if edits.is_empty() {
                    self.updates.remove(&row);
                }
            } else {
                edits.insert(col, value);
            }
        }
        self.revision += 1;
        Ok(())
    }

    /// Appends a row of defaults; returns its row number.
    pub fn add_row(&mut self) -> usize {
        let cells = vec![None; self.writable.len()];
        self.push_inserted(cells)
    }

    /// Appends a copy of `row`, leaving automatic columns to their defaults; returns the
    /// new row's number. Manually assigned keys are copied like other writable values.
    pub fn duplicate(&mut self, rows: &RowBuffer, row: usize) -> usize {
        let cells = (0..self.writable.len())
            .map(|c| {
                (self.writable(c) && !self.automatic(c))
                    .then(|| self.value(rows, row, c))
                    .flatten()
            })
            .collect();
        self.push_inserted(cells)
    }

    fn push_inserted(&mut self, cells: Vec<Option<Value>>) -> usize {
        let row = INSERTED_ROW_BASE + self.next_inserted;
        self.next_inserted += 1;
        self.inserted.push((row, cells));
        self.revision += 1;
        row
    }

    /// Marks a loaded row deleted (dropping its edits), or forgets an inserted one.
    pub fn delete(&mut self, row: usize) {
        if is_inserted_row(row) {
            self.inserted.retain(|(r, _)| *r != row);
        } else {
            self.updates.remove(&row);
            self.deleted.insert(row);
        }
        self.revision += 1;
    }

    /// Drops every pending change of `row` (an inserted row goes away).
    pub fn revert(&mut self, row: usize) {
        if is_inserted_row(row) {
            self.delete(row);
            return;
        }
        self.updates.remove(&row);
        self.deleted.remove(&row);
        self.revision += 1;
    }

    pub fn clear(&mut self) {
        self.updates.clear();
        self.inserted.clear();
        self.deleted.clear();
        self.revision += 1;
    }

    /// The pending changes as DML input: deletes, then updates, then inserts (so a deleted
    /// key can be inserted again). Columns are named after the table columns they were
    /// read from.
    pub fn changeset(&self, rows: &RowBuffer, table: TableRef) -> Result<Changeset, EditError> {
        let columns = rows.columns();
        let name = |c: usize| {
            columns[c]
                .origin
                .as_ref()
                .map_or(columns[c].name.clone(), |o| o.column.clone())
        };
        let key_of = |row: usize| -> Result<Vec<Value>, EditError> {
            self.key_cols
                .iter()
                .map(|&c| match rows.cell(row, c).map(Value::from_cell) {
                    Some(Value::Null) | None => Err(EditError::NoKeyValue { row: row + 1 }),
                    Some(v) => Ok(v),
                })
                .collect()
        };
        let mut changes = Vec::new();
        for &row in &self.deleted {
            changes.push(Change::Delete { key: key_of(row)? });
        }
        for (&row, cells) in &self.updates {
            if self.deleted.contains(&row) {
                continue;
            }
            let key = key_of(row)?;
            for (&col, value) in cells {
                changes.push(Change::Update {
                    key: key.clone(),
                    column: name(col),
                    value: value.clone(),
                });
            }
        }
        for (_, cells) in &self.inserted {
            let values = cells
                .iter()
                .enumerate()
                .filter_map(|(c, v)| Some((name(c), v.clone()?)))
                .collect();
            changes.push(Change::Insert { values });
        }
        Ok(Changeset {
            table,
            key_columns: self.key_cols.iter().map(|&c| name(c)).collect(),
            changes,
        })
    }
}

/// `text` typed into a cell of `meta`'s column, as a value of its kind.
pub fn parse_input(meta: &ColumnMeta, text: &str) -> Result<Value, InputError> {
    let t = text.trim();
    match meta.kind {
        ColumnKind::Int => t
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| InputError::NotInteger),
        ColumnKind::Float => t
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| InputError::NotNumber),
        ColumnKind::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" => Ok(Value::Bool(false)),
            _ => Err(InputError::NotBoolean),
        },
        ColumnKind::Bytes => parse_hex(text)
            .map(Value::Bytes)
            .ok_or(InputError::NotHexBytes),
        ColumnKind::Text | ColumnKind::Mixed => Ok(Value::Text(text.to_string())),
    }
}

/// `0x` followed by hex digits (whitespace between them is ignored) as bytes.
pub fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let digits: String = text
        .trim()
        .strip_prefix("0x")?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
        .collect()
}

/// A value as the inline editor shows it: NULL as empty text, bytes as `0x…`.
pub(crate) fn edit_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.display().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlr_core::{Cell, ChunkBuilder, ColumnOrigin, DisplayHint, TableColumn};

    use super::*;

    fn table() -> TableRef {
        TableRef {
            database: None,
            schema: Some("s".into()),
            name: "orders".into(),
        }
    }

    fn col(name: &str, kind: ColumnKind, origin: Option<&str>) -> ColumnMeta {
        ColumnMeta::new(format!("r_{name}"), "t", kind, DisplayHint::Plain).with_origin(origin.map(
            |c| ColumnOrigin {
                table: table(),
                column: c.into(),
            },
        ))
    }

    fn info(primary_key: &[&str]) -> TableInfo {
        let c = |name: &str, nullable, generated| TableColumn {
            automatic: name == "id",
            name: name.into(),
            type_name: "t".into(),
            nullable,
            default: None,
            generated,
        };
        TableInfo {
            table: table(),
            columns: vec![
                c("id", false, false),
                c("note", true, false),
                c("g", true, true),
            ],
            primary_key: primary_key.iter().map(|s| s.to_string()).collect(),
            unique_keys: vec![],
        }
    }

    fn rows() -> RowBuffer {
        let cols: Arc<[ColumnMeta]> = Arc::from(vec![
            col("id", ColumnKind::Int, Some("id")),
            col("note", ColumnKind::Text, Some("note")),
            col("g", ColumnKind::Int, Some("g")),
            col("x", ColumnKind::Int, None),
        ]);
        let mut buf = RowBuffer::new(cols.clone());
        let mut b = ChunkBuilder::new(cols);
        for i in 1..=3 {
            b.push_row(&[Cell::Int(i), Cell::Text("n"), Cell::Int(0), Cell::Int(9)])
                .unwrap();
        }
        buf.push_chunk(b.finish());
        buf
    }

    #[test]
    fn editability_reasons() {
        let r = rows();
        let edits = EditBuffer::for_table(r.columns(), &info(&["id"])).unwrap();
        assert_eq!(edits.writable, [true, true, false, false]);
        assert_eq!(edits.not_null, [true, false, false, false]);
        assert_eq!(
            EditBuffer::for_table(r.columns(), &info(&[]))
                .unwrap_err()
                .to_string(),
            "no primary or unique key on orders"
        );
        assert_eq!(
            EditBuffer::for_table(&r.columns()[1..], &info(&["id"]))
                .unwrap_err()
                .to_string(),
            "the key of orders is not in the result"
        );
        assert_eq!(
            EditBuffer::for_table(&r.columns()[3..], &info(&["id"]))
                .unwrap_err()
                .to_string(),
            "computed columns"
        );
        let mut other = col("y", ColumnKind::Int, Some("y"));
        other.origin.as_mut().unwrap().table.name = "items".into();
        let mixed = vec![r.columns()[0].clone(), other];
        assert_eq!(
            EditBuffer::for_table(&mixed, &info(&["id"]))
                .unwrap_err()
                .to_string(),
            "columns from several tables"
        );
    }

    #[test]
    fn manually_assigned_primary_keys_are_copied_when_duplicating() {
        let r = rows();
        let mut info = info(&["id"]);
        info.columns[0].automatic = false;
        let mut edits = EditBuffer::for_table(r.columns(), &info).unwrap();
        let row = edits.duplicate(&r, 0);
        assert_eq!(edits.value(&r, row, 0), Some(Value::Int(1)));
    }

    #[test]
    fn primary_keys_with_server_defaults_are_regenerated() {
        let r = rows();
        let mut info = info(&["id"]);
        info.columns[0].automatic = false;
        info.columns[0].default = Some("100".into());
        let mut edits = EditBuffer::for_table(r.columns(), &info).unwrap();
        assert!(edits.automatic(0));
        let row = edits.duplicate(&r, 0);
        assert_eq!(edits.value(&r, row, 0), None);
    }

    #[test]
    fn edits_make_a_changeset() {
        let r = rows();
        let mut e = EditBuffer::for_table(r.columns(), &info(&["id"])).unwrap();
        e.set(&r, 0, 1, Value::Text("a".into())).unwrap();
        e.set(&r, 0, 0, Value::Int(10)).unwrap();
        assert!(e.set(&r, 1, 0, Value::Null).is_err(), "NOT NULL");
        assert!(e.set(&r, 1, 2, Value::Int(1)).is_err(), "generated");
        e.set(&r, 1, 1, Value::Text("b".into())).unwrap();
        e.set(&r, 1, 1, Value::Text("n".into())).unwrap();
        assert_eq!(e.row_state(1), RowState::Clean, "back to the loaded value");
        e.delete(2);
        let new = e.add_row();
        assert_eq!(new, INSERTED_ROW_BASE);
        e.set(&r, new, 1, Value::Text("c".into())).unwrap();
        let dup = e.duplicate(&r, 0);
        assert_eq!(e.value(&r, dup, 1), Some(Value::Text("a".into())));
        assert_eq!(e.value(&r, dup, 0), None, "the key keeps its default");
        assert_eq!(e.pending(), 4);
        let set = e.changeset(&r, table()).unwrap();
        assert_eq!(set.key_columns, ["id"]);
        assert_eq!(
            set.changes,
            vec![
                Change::Delete {
                    key: vec![Value::Int(3)]
                },
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "id".into(),
                    value: Value::Int(10)
                },
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "note".into(),
                    value: Value::Text("a".into())
                },
                Change::Insert {
                    values: vec![("note".into(), Value::Text("c".into()))]
                },
                Change::Insert {
                    values: vec![("note".into(), Value::Text("a".into()))]
                },
            ]
        );
        e.revert(2);
        e.revert(INSERTED_ROW_BASE);
        assert_eq!(e.pending(), 2);
        assert_eq!(e.inserted_rows(), 1);
    }

    #[test]
    fn typed_input() {
        let int = ColumnMeta::new("i", "int", ColumnKind::Int, DisplayHint::Number);
        assert_eq!(parse_input(&int, " 42 "), Ok(Value::Int(42)));
        assert_eq!(
            parse_input(&int, "4x").unwrap_err().to_string(),
            "Not an integer"
        );
        let bytes = ColumnMeta::new("b", "bytea", ColumnKind::Bytes, DisplayHint::Binary);
        assert_eq!(
            parse_input(&bytes, "0x01 ff"),
            Ok(Value::Bytes(vec![1, 255]))
        );
        assert!(parse_input(&bytes, "01").is_err());
        let b = ColumnMeta::new("b", "bool", ColumnKind::Bool, DisplayHint::Plain);
        assert_eq!(parse_input(&b, "FALSE"), Ok(Value::Bool(false)));
        let t = ColumnMeta::new("t", "text", ColumnKind::Text, DisplayHint::Plain);
        assert_eq!(parse_input(&t, " a "), Ok(Value::Text(" a ".into())));
    }
}
