//! Parquet files: rows read as values and written as typed columns, through Parquet's own
//! record and column APIs (no Arrow). Reading streams row by row; writing buffers one row
//! group (64 K rows) at a time.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use parquet::basic::{Compression, LogicalType, Repetition, Type as Physical, ZstdLevel};
use parquet::data_type::{BoolType, ByteArray, ByteArrayType, DoubleType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::writer::SerializedFileWriter;
use parquet::record::Field;
use parquet::schema::types::Type;
use sqlr_core::{ColumnKind, Value};

use super::error::TransferFileError;

fn open(path: &Path) -> Result<SerializedFileReader<File>, TransferFileError> {
    let file = File::open(path).map_err(|source| TransferFileError::File {
        path: path.to_path_buf(),
        source,
    })?;
    SerializedFileReader::new(file).map_err(|source| TransferFileError::Parquet {
        path: path.to_path_buf(),
        source,
    })
}

/// The top-level column names of a Parquet file.
pub(super) fn column_names(path: &Path) -> Result<Vec<String>, TransferFileError> {
    let reader = open(path)?;
    Ok(reader
        .metadata()
        .file_metadata()
        .schema()
        .get_fields()
        .iter()
        .map(|f| f.name().to_string())
        .collect())
}

/// Calls `on_row(line, row)` for every row (line = 1-based row number), until it returns
/// `false`.
pub(super) fn read_rows(
    path: &Path,
    mut on_row: impl FnMut(u64, Vec<Value>) -> bool,
) -> Result<(), TransferFileError> {
    let reader = open(path)?;
    let rows = reader
        .get_row_iter(None)
        .map_err(|source| TransferFileError::Parquet {
            path: path.to_path_buf(),
            source,
        })?;
    for (i, row) in rows.enumerate() {
        let row = row.map_err(|source| TransferFileError::ParquetRow { row: i + 1, source })?;
        let values = row.get_column_iter().map(|(_, f)| field_value(f)).collect();
        if !on_row(i as u64 + 1, values) {
            break;
        }
    }
    Ok(())
}

fn field_value(field: &Field) -> Value {
    match field {
        Field::Null => Value::Null,
        Field::Bool(b) => Value::Bool(*b),
        Field::Byte(v) => Value::Int(i64::from(*v)),
        Field::Short(v) => Value::Int(i64::from(*v)),
        Field::Int(v) => Value::Int(i64::from(*v)),
        Field::Long(v) => Value::Int(*v),
        Field::UByte(v) => Value::Int(i64::from(*v)),
        Field::UShort(v) => Value::Int(i64::from(*v)),
        Field::UInt(v) => Value::Int(i64::from(*v)),
        Field::ULong(v) => match i64::try_from(*v) {
            Ok(v) => Value::Int(v),
            Err(_) => Value::Text(v.to_string()),
        },
        Field::Float16(v) => Value::Float(f64::from(*v)),
        Field::Float(v) => Value::Float(f64::from(*v)),
        Field::Double(v) => Value::Float(*v),
        Field::Str(s) => Value::Text(s.clone()),
        Field::Bytes(b) => Value::Bytes(b.data().to_vec()),
        // Exact decimals, dates and timestamps keep their text form.
        Field::Decimal(_)
        | Field::Date(_)
        | Field::TimestampMillis(_)
        | Field::TimestampMicros(_) => Value::Text(field.to_string()),
        nested => Value::Text(nested.to_string()),
    }
}

/// The Parquet type a result column is written as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Bool,
    Int,
    Float,
    Text,
    Bytes,
}

impl Kind {
    /// Mixed columns (SQLite's dynamic types) are written as text.
    pub fn of(kind: ColumnKind) -> Self {
        match kind {
            ColumnKind::Bool => Kind::Bool,
            ColumnKind::Int => Kind::Int,
            ColumnKind::Float => Kind::Float,
            ColumnKind::Bytes => Kind::Bytes,
            ColumnKind::Text | ColumnKind::Mixed => Kind::Text,
        }
    }
}

const ROW_GROUP: usize = 65_536;

/// Writes rows to a Parquet file, one row group per [`ROW_GROUP`] rows.
pub(super) struct ParquetWriter {
    writer: SerializedFileWriter<File>,
    names: Vec<String>,
    kinds: Vec<Kind>,
    buffer: Vec<Vec<Value>>,
}

impl ParquetWriter {
    pub fn create(
        path: &Path,
        names: Vec<String>,
        kinds: Vec<Kind>,
    ) -> Result<Self, TransferFileError> {
        let fields = names
            .iter()
            .zip(&kinds)
            .map(|(name, kind)| {
                let (physical, logical) = match kind {
                    Kind::Bool => (Physical::BOOLEAN, None),
                    Kind::Int => (Physical::INT64, None),
                    Kind::Float => (Physical::DOUBLE, None),
                    Kind::Text => (Physical::BYTE_ARRAY, Some(LogicalType::String)),
                    Kind::Bytes => (Physical::BYTE_ARRAY, None),
                };
                Type::primitive_type_builder(name, physical)
                    .with_repetition(Repetition::OPTIONAL)
                    .with_logical_type(logical)
                    .build()
                    .map(Arc::new)
                    .map_err(|source| TransferFileError::ParquetColumn {
                        column: name.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let schema = Type::group_type_builder("schema")
            .with_fields(fields)
            .build()?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .build();
        let file = File::create(path).map_err(|source| TransferFileError::Create {
            path: path.to_path_buf(),
            source,
        })?;
        let writer = SerializedFileWriter::new(file, Arc::new(schema), Arc::new(props))?;
        Ok(Self {
            writer,
            names,
            kinds,
            buffer: Vec::new(),
        })
    }

    pub fn write_row(&mut self, row: Vec<Value>) -> Result<(), TransferFileError> {
        self.buffer.push(row);
        if self.buffer.len() >= ROW_GROUP {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), TransferFileError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.buffer);
        let mut group = self.writer.next_row_group()?;
        for (i, kind) in self.kinds.iter().enumerate() {
            let name = &self.names[i];
            let cell = |row: &Vec<Value>| row.get(i).unwrap_or(&Value::Null).clone();
            let mut column = group
                .next_column()?
                .ok_or(TransferFileError::FewerColumns)?;
            let definition: Vec<i16> = rows.iter().map(|r| i16::from(!cell(r).is_null())).collect();
            let bad = |v: &Value, what: &'static str| {
                let shown: String = v.display().chars().take(40).collect();
                TransferFileError::ColumnValue {
                    column: name.clone(),
                    shown,
                    what,
                }
            };
            match kind {
                Kind::Bool => {
                    let mut values = Vec::new();
                    for r in &rows {
                        match cell(r) {
                            Value::Null => {}
                            Value::Bool(b) => values.push(b),
                            Value::Int(n) if n == 0 || n == 1 => values.push(n == 1),
                            other => return Err(bad(&other, "a boolean")),
                        }
                    }
                    column
                        .typed::<BoolType>()
                        .write_batch(&values, Some(&definition), None)
                }
                Kind::Int => {
                    let mut values = Vec::new();
                    for r in &rows {
                        match cell(r) {
                            Value::Null => {}
                            Value::Int(n) => values.push(n),
                            Value::Bool(b) => values.push(i64::from(b)),
                            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
                                values.push(f as i64)
                            }
                            Value::Text(t) => match t.trim().parse() {
                                Ok(n) => values.push(n),
                                Err(_) => return Err(bad(&Value::Text(t), "an integer")),
                            },
                            other => return Err(bad(&other, "an integer")),
                        }
                    }
                    column
                        .typed::<Int64Type>()
                        .write_batch(&values, Some(&definition), None)
                }
                Kind::Float => {
                    let mut values = Vec::new();
                    for r in &rows {
                        match cell(r) {
                            Value::Null => {}
                            Value::Float(f) => values.push(f),
                            Value::Int(n) => values.push(n as f64),
                            Value::Text(t) => match t.trim().parse() {
                                Ok(f) => values.push(f),
                                Err(_) => return Err(bad(&Value::Text(t), "a number")),
                            },
                            other => return Err(bad(&other, "a number")),
                        }
                    }
                    column
                        .typed::<DoubleType>()
                        .write_batch(&values, Some(&definition), None)
                }
                Kind::Text | Kind::Bytes => {
                    let mut values = Vec::new();
                    for r in &rows {
                        match cell(r) {
                            Value::Null => {}
                            Value::Bytes(b) if *kind == Kind::Bytes => {
                                values.push(ByteArray::from(b))
                            }
                            other => values
                                .push(ByteArray::from(other.display().into_owned().into_bytes())),
                        }
                    }
                    column
                        .typed::<ByteArrayType>()
                        .write_batch(&values, Some(&definition), None)
                }
            }
            .map_err(|source| TransferFileError::ParquetColumn {
                column: name.clone(),
                source,
            })?;
            column.close()?;
        }
        group.close()?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), TransferFileError> {
        self.flush()?;
        self.writer.close()?;
        Ok(())
    }
}
