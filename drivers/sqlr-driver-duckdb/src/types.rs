use duckdb::{
    Statement,
    core::{LogicalTypeHandle, LogicalTypeId},
    types::{TimeUnit, ValueRef},
};
use sqlr_driver_sdk::{ColumnKind, ColumnMeta, DisplayHint, DriverError, ErrorKind, Value};
use std::sync::Arc;

pub(crate) fn bind(value: &Value) -> duckdb::types::Value {
    match value {
        Value::Null => duckdb::types::Value::Null,
        Value::Bool(v) => duckdb::types::Value::Boolean(*v),
        Value::Int(v) => duckdb::types::Value::BigInt(*v),
        Value::Float(v) => duckdb::types::Value::Double(*v),
        Value::Text(v) => duckdb::types::Value::Text(v.clone()),
        Value::Bytes(v) => duckdb::types::Value::Blob(v.clone()),
    }
}
pub(crate) fn columns(stmt: &Statement<'_>) -> Arc<[ColumnMeta]> {
    let schema = stmt.schema();
    (0..stmt.column_count())
        .map(|i| {
            let ty = stmt.column_logical_type(i);
            let field = schema.field(i);
            let id = if field
                .metadata()
                .get("ARROW:extension:name")
                .is_some_and(|name| name == "arrow.bool8")
            {
                LogicalTypeId::Boolean
            } else if matches!(field.data_type(), duckdb::arrow::datatypes::DataType::Null) {
                LogicalTypeId::SqlNull
            } else {
                ty.id()
            };
            use LogicalTypeId::*;
            let (kind, hint) = match id {
                Boolean => (ColumnKind::Bool, DisplayHint::Plain),
                Tinyint | Smallint | Integer | Bigint | UTinyint | USmallint | UInteger => {
                    (ColumnKind::Int, DisplayHint::Number)
                }
                Float | Double => (ColumnKind::Float, DisplayHint::Number),
                Hugeint | UHugeint | UBigint => (ColumnKind::Text, DisplayHint::Number),
                Decimal => (ColumnKind::Text, DisplayHint::Decimal),
                Date | Time | TimeNs | TimeTZ | Timestamp | TimestampS | TimestampMs
                | TimestampNs | TimestampTZ => (ColumnKind::Text, DisplayHint::Temporal),
                Blob => (ColumnKind::Bytes, DisplayHint::Binary),
                Geometry => (ColumnKind::Bytes, DisplayHint::Geometry),
                Interval => (ColumnKind::Text, DisplayHint::Interval),
                Uuid => (ColumnKind::Text, DisplayHint::Uuid),
                // Invalid is used by DuckDB for bare parameter-derived expressions.
                Invalid => (ColumnKind::Mixed, DisplayHint::Plain),
                _ => (ColumnKind::Text, DisplayHint::Plain),
            };
            ColumnMeta::new(
                stmt.column_name(i).cloned().unwrap_or_default(),
                match id {
                    Boolean => "BOOLEAN".into(),
                    SqlNull => "NULL".into(),
                    _ => type_name(&ty),
                },
                kind,
                hint,
            )
        })
        .collect::<Vec<_>>()
        .into()
}
fn type_name(ty: &LogicalTypeHandle) -> String {
    if ty.id() == LogicalTypeId::Decimal {
        format!("DECIMAL({},{})", ty.decimal_width(), ty.decimal_scale())
    } else {
        match ty.id() {
            LogicalTypeId::TimeTZ => "TIMETZ".into(),
            LogicalTypeId::TimeNs => "TIME_NS".into(),
            LogicalTypeId::TimestampTZ => "TIMESTAMPTZ".into(),
            LogicalTypeId::TimestampS => "TIMESTAMP_S".into(),
            LogicalTypeId::TimestampMs => "TIMESTAMP_MS".into(),
            LogicalTypeId::TimestampNs => "TIMESTAMP_NS".into(),
            _ => format!("{:?}", ty.id()).to_ascii_uppercase(),
        }
    }
}
pub(crate) fn decode(value: ValueRef<'_>, column: &ColumnMeta) -> Result<Value, DriverError> {
    use ValueRef::*;
    Ok(match value {
        Null => Value::Null,
        Boolean(v) => Value::Bool(v),
        TinyInt(v) if column.kind == ColumnKind::Bool => Value::Bool(v != 0),
        TinyInt(v) => Value::Int(v.into()),
        SmallInt(v) => Value::Int(v.into()),
        Int(v) => Value::Int(v.into()),
        BigInt(v) => Value::Int(v),
        UTinyInt(v) => Value::Int(v.into()),
        USmallInt(v) => Value::Int(v.into()),
        UInt(v) => Value::Int(v.into()),
        HugeInt(v) => Value::Text(v.to_string()),
        UHugeInt(v) => Value::Text(v.to_string()),
        UBigInt(v) => Value::Text(v.to_string()),
        Float(v) => Value::Float(v.into()),
        Double(v) => Value::Float(v),
        Decimal(v) => Value::Text(v.to_string()),
        Text(v) => Value::Text(String::from_utf8_lossy(v).into_owned()),
        Blob(bytes) if column.type_name == "HUGEINT" => {
            Value::Text(i128::from_ne_bytes(fixed(bytes)?).to_string())
        }
        Blob(bytes) if column.type_name == "UHUGEINT" => {
            Value::Text(u128::from_ne_bytes(fixed(bytes)?).to_string())
        }
        Blob(bytes) if column.type_name == "UUID" => {
            let bytes: [u8; 16] = fixed(bytes)?;
            let hex = sqlr_driver_sdk::hex_bytes(&bytes);
            let hex = &hex[2..];
            Value::Text(format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ))
        }
        Blob(bytes) if column.type_name == "TIMETZ" => {
            let bits = u64::from_ne_bytes(fixed(bytes)?);
            // SAFETY: the stable C API only interprets DuckDB's packed local time and UTC
            // offset: a pure conversion of a plain value with no pointers involved.
            let value =
                unsafe { duckdb::ffi::duckdb_from_time_tz(duckdb::ffi::duckdb_time_tz { bits }) };
            let nanos = (i128::from(value.time.hour) * 3600
                + i128::from(value.time.min) * 60
                + i128::from(value.time.sec))
                * 1_000_000_000
                + i128::from(value.time.micros) * 1000;
            Value::Text(format!("{}{}", time(nanos), offset(value.offset)))
        }
        Blob(v) | Geometry(v) => Value::Bytes(v.to_vec()),
        Date32(v) => Value::Text(date(v)),
        Time64(unit, v) => Value::Text(time(nanoseconds(unit, v))),
        Timestamp(_, i64::MAX) => Value::Text("infinity".into()),
        Timestamp(_, value) if value == -i64::MAX => Value::Text("-infinity".into()),
        Timestamp(unit, v) => {
            let ns = nanoseconds(unit, v);
            let days = ns.div_euclid(86_400_000_000_000);
            let day = i32::try_from(days).map_err(|_| {
                DriverError::new(
                    ErrorKind::Unsupported,
                    "timestamp is outside the supported calendar range",
                )
            })?;
            Value::Text(format!(
                "{} {}{}",
                date(day),
                time(ns.rem_euclid(86_400_000_000_000)),
                if column.type_name == "TIMESTAMPTZ" {
                    "+00:00"
                } else {
                    ""
                },
            ))
        }
        Interval {
            months,
            days,
            nanos,
        } => Value::Text(format!("{months} months {days} days {nanos} nanoseconds")),
        Enum(values, index) => {
            let owned: duckdb::types::Value = Enum(values, index).into();
            if let duckdb::types::Value::Enum(v) = owned {
                Value::Text(v)
            } else {
                return Err(unsupported());
            }
        }
        List(duckdb::types::ListType::Regular(array), index) => display(array, index)?,
        List(duckdb::types::ListType::Large(array), index) => display(array, index)?,
        Struct(array, index) => display(array, index)?,
        Array(array, index) => display(array, index)?,
        Map(array, index) => display(array, index)?,
        Union(array, index) => display(array.as_ref(), index)?,
        _ => return Err(unsupported()),
    })
}
fn unsupported() -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        "this DuckDB result type is not supported; cast the column to VARCHAR",
    )
}

/// Decodes Arrow batches for results containing TIME_NS, which the native row
/// decoder does not handle. Other result sets keep the native decoding path.
pub(crate) fn decode_array(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    column: &ColumnMeta,
) -> Result<Value, DriverError> {
    use duckdb::arrow::datatypes::TimeUnit as ArrowTimeUnit;
    use duckdb::arrow::{array::*, datatypes::DataType};

    if array.is_null(row) {
        return Ok(Value::Null);
    }
    macro_rules! scalar {
        ($array:ty, $variant:ident) => {
            ValueRef::$variant(array_as::<$array>(array)?.value(row))
        };
    }
    macro_rules! temporal {
        ($array:ty, $variant:ident, $unit:ident) => {
            ValueRef::$variant(TimeUnit::$unit, array_as::<$array>(array)?.value(row))
        };
    }
    let value = match array.data_type() {
        DataType::Null => ValueRef::Null,
        DataType::Boolean => scalar!(BooleanArray, Boolean),
        DataType::Int8 => scalar!(Int8Array, TinyInt),
        DataType::Int16 => scalar!(Int16Array, SmallInt),
        DataType::Int32 => scalar!(Int32Array, Int),
        DataType::Int64 => scalar!(Int64Array, BigInt),
        DataType::UInt8 => scalar!(UInt8Array, UTinyInt),
        DataType::UInt16 => scalar!(UInt16Array, USmallInt),
        DataType::UInt32 => scalar!(UInt32Array, UInt),
        DataType::UInt64 => scalar!(UInt64Array, UBigInt),
        DataType::Float32 => scalar!(Float32Array, Float),
        DataType::Float64 => scalar!(Float64Array, Double),
        DataType::Binary => scalar!(BinaryArray, Blob),
        DataType::LargeBinary => scalar!(LargeBinaryArray, Blob),
        DataType::FixedSizeBinary(_) => scalar!(FixedSizeBinaryArray, Blob),
        DataType::Date32 => scalar!(Date32Array, Date32),
        DataType::Time64(ArrowTimeUnit::Microsecond) => {
            temporal!(Time64MicrosecondArray, Time64, Microsecond)
        }
        DataType::Time64(ArrowTimeUnit::Nanosecond) => {
            temporal!(Time64NanosecondArray, Time64, Nanosecond)
        }
        DataType::Timestamp(ArrowTimeUnit::Second, _) => {
            temporal!(TimestampSecondArray, Timestamp, Second)
        }
        DataType::Timestamp(ArrowTimeUnit::Millisecond, _) => {
            temporal!(TimestampMillisecondArray, Timestamp, Millisecond)
        }
        DataType::Timestamp(ArrowTimeUnit::Microsecond, _) => {
            temporal!(TimestampMicrosecondArray, Timestamp, Microsecond)
        }
        DataType::Timestamp(ArrowTimeUnit::Nanosecond, _) => {
            temporal!(TimestampNanosecondArray, Timestamp, Nanosecond)
        }
        DataType::Decimal128(_, _) if column.type_name == "HUGEINT" => {
            scalar!(Decimal128Array, HugeInt)
        }
        DataType::Decimal128(_, _) if column.type_name == "UHUGEINT" => {
            ValueRef::UHugeInt(array_as::<Decimal128Array>(array)?.value(row) as u128)
        }
        DataType::Interval(duckdb::arrow::datatypes::IntervalUnit::MonthDayNano) => {
            let interval = array_as::<IntervalMonthDayNanoArray>(array)?.value(row);
            ValueRef::Interval {
                months: interval.months,
                days: interval.days,
                nanos: interval.nanoseconds,
            }
        }
        _ => return display(array, row),
    };
    decode(value, column)
}

fn array_as<T: duckdb::arrow::array::Array + 'static>(
    array: &dyn duckdb::arrow::array::Array,
) -> Result<&T, DriverError> {
    array
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| DriverError::new(ErrorKind::Protocol, "invalid DuckDB Arrow array type"))
}

fn nanoseconds(unit: TimeUnit, value: i64) -> i128 {
    i128::from(value)
        * match unit {
            TimeUnit::Second => 1_000_000_000,
            TimeUnit::Millisecond => 1_000_000,
            TimeUnit::Microsecond => 1_000,
            TimeUnit::Nanosecond => 1,
        }
}
fn date(days: i32) -> String {
    if days == i32::MAX {
        return "infinity".into();
    }
    if days == -i32::MAX {
        return "-infinity".into();
    }
    // SAFETY: DuckDB's calendar conversion is a pure function of a plain value and is
    // valid for its full date domain (infinities were handled above).
    let d = unsafe { duckdb::ffi::duckdb_from_date(duckdb::ffi::duckdb_date { days }) };
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}
fn time(nanos: i128) -> String {
    let seconds = nanos / 1_000_000_000;
    let fraction = nanos % 1_000_000_000;
    let mut text = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction != 0 {
        let digits = format!("{fraction:09}");
        text.push('.');
        text.push_str(digits.trim_end_matches('0'));
    }
    text
}

fn display(array: &dyn duckdb::arrow::array::Array, index: usize) -> Result<Value, DriverError> {
    duckdb::arrow::util::display::array_value_to_string(array, index)
        .map(Value::Text)
        .map_err(|error| DriverError::new(ErrorKind::Protocol, error.to_string()))
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], DriverError> {
    bytes
        .try_into()
        .map_err(|_| DriverError::new(ErrorKind::Protocol, "invalid DuckDB fixed-size value"))
}
fn offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    let mut text = format!("{sign}{:02}:{:02}", seconds / 3600, seconds / 60 % 60);
    if !seconds.is_multiple_of(60) {
        text.push_str(&format!(":{:02}", seconds % 60));
    }
    text
}
