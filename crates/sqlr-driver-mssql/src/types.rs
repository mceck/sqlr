//! SQL Server values: TDS data to sqlr cells, and bound parameters the other way.

use std::borrow::Cow;

use sqlr_core::{ColumnKind, ColumnMeta, DisplayHint, Value};
use tiberius::numeric::Numeric;
use tiberius::time::{Date, DateTime, DateTime2, DateTimeOffset, SmallDateTime, Time};
use tiberius::{Column, ColumnData, ColumnType};

/// The column a TDS result describes.
pub(crate) fn column_meta(column: &Column) -> ColumnMeta {
    let (type_name, kind, hint) = match column.column_type() {
        ColumnType::Bit | ColumnType::Bitn => ("bit", ColumnKind::Bool, DisplayHint::Plain),
        ColumnType::Int1 => ("tinyint", ColumnKind::Int, DisplayHint::Number),
        ColumnType::Int2 => ("smallint", ColumnKind::Int, DisplayHint::Number),
        ColumnType::Int4 => ("int", ColumnKind::Int, DisplayHint::Number),
        ColumnType::Int8 => ("bigint", ColumnKind::Int, DisplayHint::Number),
        ColumnType::Intn => ("int", ColumnKind::Int, DisplayHint::Number),
        ColumnType::Float4 => ("real", ColumnKind::Float, DisplayHint::Number),
        ColumnType::Float8 | ColumnType::Floatn => {
            ("float", ColumnKind::Float, DisplayHint::Number)
        }
        ColumnType::Money | ColumnType::Money4 => ("money", ColumnKind::Text, DisplayHint::Decimal),
        ColumnType::Decimaln => ("decimal", ColumnKind::Text, DisplayHint::Decimal),
        ColumnType::Numericn => ("numeric", ColumnKind::Text, DisplayHint::Decimal),
        ColumnType::Guid => ("uniqueidentifier", ColumnKind::Text, DisplayHint::Uuid),
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => {
            ("varbinary", ColumnKind::Bytes, DisplayHint::Binary)
        }
        ColumnType::BigVarChar => ("varchar", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::BigChar => ("char", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::NVarchar => ("nvarchar", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::NChar => ("nchar", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::Text => ("text", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::NText => ("ntext", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::Xml => ("xml", ColumnKind::Text, DisplayHint::Xml),
        ColumnType::SSVariant => ("sql_variant", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::Udt => ("udt", ColumnKind::Text, DisplayHint::Plain),
        ColumnType::Datetime => ("datetime", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Datetime4 => ("smalldatetime", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Datetimen => ("datetime", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Datetime2 => ("datetime2", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::DatetimeOffsetn => ("datetimeoffset", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Daten => ("date", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Timen => ("time", ColumnKind::Text, DisplayHint::Temporal),
        ColumnType::Null => ("null", ColumnKind::Text, DisplayHint::Plain),
    };
    ColumnMeta::new(column.name(), type_name, kind, hint)
}

/// A TDS value as an owned sqlr value. Dates, decimals, UUIDs and XML travel as text.
pub(crate) fn decode(data: &ColumnData<'_>) -> Value {
    match data {
        ColumnData::U8(Some(v)) => Value::Int(i64::from(*v)),
        ColumnData::I16(Some(v)) => Value::Int(i64::from(*v)),
        ColumnData::I32(Some(v)) => Value::Int(i64::from(*v)),
        ColumnData::I64(Some(v)) => Value::Int(*v),
        ColumnData::F32(Some(v)) => Value::Float(f64::from(*v)),
        ColumnData::F64(Some(v)) => Value::Float(*v),
        ColumnData::Bit(Some(v)) => Value::Bool(*v),
        ColumnData::String(Some(v)) => Value::Text(v.to_string()),
        ColumnData::Guid(Some(v)) => Value::Text(v.to_string()),
        ColumnData::Binary(Some(v)) => Value::Bytes(v.to_vec()),
        ColumnData::Numeric(Some(v)) => Value::Text(numeric_text(*v)),
        ColumnData::Xml(Some(v)) => Value::Text(v.to_string()),
        ColumnData::DateTime(Some(v)) => Value::Text(datetime_text(*v)),
        ColumnData::SmallDateTime(Some(v)) => Value::Text(smalldatetime_text(*v)),
        ColumnData::Time(Some(v)) => Value::Text(time_text(*v)),
        ColumnData::Date(Some(v)) => Value::Text(date_text(*v)),
        ColumnData::DateTime2(Some(v)) => Value::Text(datetime2_text(*v)),
        ColumnData::DateTimeOffset(Some(v)) => Value::Text(datetimeoffset_text(*v)),
        _ => Value::Null,
    }
}

/// A bound parameter as the TDS value the server gets.
pub(crate) fn encode(value: &Value) -> ColumnData<'static> {
    match value {
        Value::Null => ColumnData::String(None),
        Value::Bool(v) => ColumnData::Bit(Some(*v)),
        Value::Int(v) => ColumnData::I64(Some(*v)),
        Value::Float(v) => ColumnData::F64(Some(*v)),
        Value::Text(v) => ColumnData::String(Some(Cow::Owned(v.clone()))),
        Value::Bytes(v) => ColumnData::Binary(Some(Cow::Owned(v.clone()))),
    }
}

/// `numeric` with its exact scale: `12.340` keeps its trailing zero.
pub(crate) fn numeric_text(number: Numeric) -> String {
    let scale = number.scale() as usize;
    let value = number.value();
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if scale == 0 {
        out.push_str(&digits);
        return out;
    }
    if digits.len() <= scale {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', scale - digits.len()));
        out.push_str(&digits);
    } else {
        let point = digits.len() - scale;
        out.push_str(&digits[..point]);
        out.push('.');
        out.push_str(&digits[point..]);
    }
    out
}

fn date_parts(day: u32) -> (i64, u32, u32) {
    // `date` counts days from 0001-01-01; 1970-01-01 is 719162 days later.
    civil_from_days(i64::from(day) - 719_162)
}

fn date_text(date: Date) -> String {
    let (y, m, d) = date_parts(date.days());
    format!("{y:04}-{m:02}-{d:02}")
}

/// `datetime`/`smalldatetime` count days from 1900-01-01; 1970-01-01 is 25567 later.
fn date_from_1900(days: i64) -> (i64, u32, u32) {
    civil_from_days(days - 25_567)
}

fn datetime_text(datetime: DateTime) -> String {
    let (y, m, d) = date_from_1900(i64::from(datetime.days()));
    let fragments = u64::from(datetime.seconds_fragments());
    let seconds = fragments / 300;
    let millis = (fragments % 300) * 1000 / 300;
    format!(
        "{y:04}-{m:02}-{d:02} {}",
        clock_text(seconds, millis as u32, 3)
    )
}

fn smalldatetime_text(datetime: SmallDateTime) -> String {
    let (y, m, d) = date_from_1900(i64::from(datetime.days()));
    let seconds = u64::from(datetime.seconds_fragments()) / 300;
    format!("{y:04}-{m:02}-{d:02} {}", clock_text(seconds, 0, 0))
}

fn clock_text(seconds: u64, fraction: u32, fraction_digits: u32) -> String {
    let seconds = seconds % 86_400;
    let (h, min, sec) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if fraction_digits == 0 {
        format!("{h:02}:{min:02}:{sec:02}")
    } else {
        format!(
            "{h:02}:{min:02}:{sec:02}.{fraction:0width$}",
            width = fraction_digits as usize
        )
    }
}

fn time_text(time: Time) -> String {
    let scale = time.scale().min(9) as u32;
    let increments = time.increments();
    let units_per_second = 10u64.pow(scale);
    let seconds = increments / units_per_second;
    let fraction = (increments % units_per_second) as u32;
    clock_text(seconds, fraction, scale)
}

fn datetime2_text(datetime: DateTime2) -> String {
    let (y, m, d) = date_parts(datetime.date().days());
    let time = time_text(datetime.time());
    format!("{y:04}-{m:02}-{d:02} {time}")
}

fn datetimeoffset_text(datetime: DateTimeOffset) -> String {
    let (y, m, d) = date_parts(datetime.datetime2().date().days());
    let time = time_text(datetime.datetime2().time());
    let offset = datetime.offset();
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = i32::from(offset).abs();
    format!(
        "{y:04}-{m:02}-{d:02} {time}{sign}{:02}:{:02}",
        offset / 60,
        offset % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_keeps_scale_and_sign() {
        assert_eq!(numeric_text(Numeric::new_with_scale(12340, 3)), "12.340");
        assert_eq!(numeric_text(Numeric::new_with_scale(-5, 2)), "-0.05");
        assert_eq!(numeric_text(Numeric::new_with_scale(7, 0)), "7");
    }

    #[test]
    fn dates_use_the_sql_server_epochs() {
        assert_eq!(date_text(Date::new(0)), "0001-01-01");
        assert_eq!(date_text(Date::new(719_162)), "1970-01-01");
        assert_eq!(date_text(Date::new(738_885)), "2024-01-01");
        assert_eq!(
            datetime_text(DateTime::new(0, 0)),
            "1900-01-01 00:00:00.000"
        );
        assert_eq!(
            datetime_text(DateTime::new(45_290, 300)),
            "2024-01-01 00:00:01.000"
        );
        assert_eq!(time_text(Time::new(36_615_000_000, 7)), "01:01:01.5000000");
    }
}
