//! Display formats for cell values (§4.3): dates, numbers, booleans, NULL, binary and JSON.
//! The grid and the value panel apply them; exports do too when the user turned that on.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::chunk::{Cell, ColumnMeta, DisplayHint, hex_bytes};
use crate::settings_model::{SettingValue, setting_defs};

/// Bytes a grid cell shows before "…"; exports are not bounded.
const GRID_BYTES: usize = 32;
/// JSON up to this size is re-serialized on one line.
const COMPACT_JSON_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boolean {
    TrueFalse,
    OneZero,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binary {
    Hex,
    Base64,
    Utf8,
}

/// The formats in force for one connection, built from the resolved settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DataFormats {
    pub null_text: String,
    pub date: String,
    pub time: String,
    pub timestamp: String,
    pub timestamptz: String,
    /// The grouping separator of whole numbers; empty for none.
    grouping: &'static str,
    /// Decimals of floats; `None` shows the value as it is.
    decimals: Option<usize>,
    boolean: Boolean,
    binary: Binary,
    pretty_json: bool,
    /// Exports write the formatted text.
    pub use_for_export: bool,
}

impl Default for DataFormats {
    fn default() -> Self {
        Self::from_values(|_| None)
    }
}

impl DataFormats {
    /// The formats a settings lookup describes; ids it does not answer keep their defaults.
    pub fn from_values(get: impl Fn(&str) -> Option<SettingValue>) -> Self {
        let value = |id: &str| {
            get(id).or_else(|| {
                setting_defs()
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.default.clone())
            })
        };
        let text = |id: &str| {
            value(id)
                .map(|v| v.as_str().to_string())
                .unwrap_or_default()
        };
        let decimals = value("formats.decimals").map_or(-1, |v| v.as_i64());
        Self {
            null_text: text("grid.null_text"),
            date: text("formats.date"),
            time: text("formats.time"),
            timestamp: text("formats.timestamp"),
            timestamptz: text("formats.timestamptz"),
            grouping: match text("formats.number_grouping").as_str() {
                "comma" => ",",
                "space" => " ",
                "underscore" => "_",
                _ => "",
            },
            decimals: usize::try_from(decimals).ok(),
            boolean: match text("formats.boolean").as_str() {
                "one_zero" => Boolean::OneZero,
                "check" => Boolean::Check,
                _ => Boolean::TrueFalse,
            },
            binary: match text("formats.binary").as_str() {
                "base64" => Binary::Base64,
                "utf8" => Binary::Utf8,
                _ => Binary::Hex,
            },
            pretty_json: text("formats.json") == "pretty",
            use_for_export: value("formats.use_for_export").is_some_and(|v| v.as_bool()),
        }
    }

    /// `true` when every cell shows exactly as the server sent it (the grid then keeps its
    /// own fast path).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A number that changes whenever the formats do, for caches keyed by the layout.
    pub fn revision(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{self:?}").hash(&mut h);
        h.finish()
    }

    /// The cell as the grid shows it. Long binary values are cut.
    pub fn format<'a>(&self, meta: &ColumnMeta, cell: Cell<'a>) -> Cow<'a, str> {
        self.format_with(meta, cell, Some(GRID_BYTES))
    }

    /// The cell as an export writes it when [`Self::use_for_export`] is on: no cuts.
    pub fn format_export<'a>(&self, meta: &ColumnMeta, cell: Cell<'a>) -> Cow<'a, str> {
        self.format_with(meta, cell, None)
    }

    fn format_with<'a>(
        &self,
        meta: &ColumnMeta,
        cell: Cell<'a>,
        max_bytes: Option<usize>,
    ) -> Cow<'a, str> {
        match cell {
            Cell::Null => Cow::Owned(self.null_text.clone()),
            Cell::Bool(b) => Cow::Borrowed(match (self.boolean, b) {
                (Boolean::TrueFalse, true) => "true",
                (Boolean::TrueFalse, false) => "false",
                (Boolean::OneZero, true) => "1",
                (Boolean::OneZero, false) => "0",
                (Boolean::Check, true) => "✓",
                (Boolean::Check, false) => "✗",
            }),
            Cell::Int(i) => Cow::Owned(group(&i.to_string(), self.grouping)),
            Cell::Float(f) => {
                let raw = match self.decimals {
                    Some(n) if f.is_finite() => format!("{f:.n$}"),
                    _ => f.to_string(),
                };
                Cow::Owned(group(&raw, self.grouping))
            }
            Cell::Bytes(b) => Cow::Owned(self.bytes(b, max_bytes)),
            Cell::Text(s) => match meta.hint {
                DisplayHint::Temporal => self.temporal(meta, s),
                DisplayHint::Decimal => Cow::Owned(group(s, self.grouping)),
                DisplayHint::Json if !self.pretty_json => match compact(s) {
                    Some(c) => Cow::Owned(c),
                    None => Cow::Borrowed(s),
                },
                _ => Cow::Borrowed(s),
            },
        }
    }

    fn bytes(&self, b: &[u8], max: Option<usize>) -> String {
        let shown = &b[..max.map_or(b.len(), |m| b.len().min(m))];
        let cut = shown.len() < b.len();
        let mut out = match self.binary {
            Binary::Hex => hex_bytes(shown),
            Binary::Base64 => base64(shown),
            Binary::Utf8 => match std::str::from_utf8(shown) {
                Ok(s) => s.to_string(),
                Err(_) => hex_bytes(shown),
            },
        };
        if cut {
            let unit = if b.len() == 1 { "byte" } else { "bytes" };
            out.push_str(&format!("… ({} {unit})", b.len()));
        }
        out
    }

    fn temporal<'a>(&self, meta: &ColumnMeta, s: &'a str) -> Cow<'a, str> {
        let t = meta.type_name.to_lowercase();
        let (pattern, kind) = if t.contains("timestamptz")
            || t.contains("with time zone")
            || t.contains("timestamp with")
        {
            (&self.timestamptz, Temporal::Zoned)
        } else if t.contains("timestamp") || t.contains("datetime") {
            (&self.timestamp, Temporal::DateTime)
        } else if t.contains("date") {
            (&self.date, Temporal::Date)
        } else if t.contains("time") {
            (&self.time, Temporal::Time)
        } else {
            return Cow::Borrowed(s);
        };
        if pattern.is_empty() {
            return Cow::Borrowed(s);
        }
        match kind.reformat(s.trim(), pattern) {
            Some(text) => Cow::Owned(text),
            None => Cow::Borrowed(s),
        }
    }
}

#[derive(Clone, Copy)]
enum Temporal {
    Date,
    Time,
    DateTime,
    Zoned,
}

impl Temporal {
    /// `None` when `s` does not parse or the pattern is invalid: the caller shows `s`.
    fn reformat(self, s: &str, pattern: &str) -> Option<String> {
        use std::fmt::Write;
        let mut out = String::new();
        match self {
            Self::Date => write!(
                out,
                "{}",
                jiff::civil::Date::from_str(s).ok()?.strftime(pattern)
            ),
            Self::Time => write!(
                out,
                "{}",
                jiff::civil::Time::from_str(s).ok()?.strftime(pattern)
            ),
            Self::DateTime => {
                write!(
                    out,
                    "{}",
                    jiff::civil::DateTime::from_str(s).ok()?.strftime(pattern)
                )
            }
            Self::Zoned => {
                write!(
                    out,
                    "{}",
                    jiff::Timestamp::from_str(s).ok()?.strftime(pattern)
                )
            }
        }
        .ok()?;
        Some(out)
    }
}

fn compact(s: &str) -> Option<String> {
    (s.len() <= COMPACT_JSON_BYTES && s.contains(['\n', '\r']))
        .then(|| crate::encode::compact_json(s))
        .flatten()
}

/// Inserts `sep` between the thousands of the integer part of `s` (`-1234.50` → `-1,234.50`).
/// Text that is not a plain number is returned as it is.
fn group(s: &str, sep: &str) -> String {
    if sep.is_empty() {
        return s.to_string();
    }
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", s),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    if int.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || frac.is_some_and(|f| !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + int.len() / 3 * sep.len());
    out.push_str(sign);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ColumnKind;

    fn meta(type_name: &str, kind: ColumnKind, hint: DisplayHint) -> ColumnMeta {
        ColumnMeta::new("c", type_name, kind, hint)
    }

    fn formats(pairs: &[(&str, SettingValue)]) -> DataFormats {
        DataFormats::from_values(|id| pairs.iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()))
    }

    fn text(s: &str) -> SettingValue {
        SettingValue::Text(s.into())
    }

    fn choice(s: &str) -> SettingValue {
        SettingValue::Choice(s.into())
    }

    #[test]
    fn defaults_change_nothing() {
        let f = DataFormats::default();
        assert!(f.is_default());
        let ts = meta("timestamp", ColumnKind::Text, DisplayHint::Temporal);
        assert_eq!(
            f.format(&ts, Cell::Text("2024-03-05 10:11:12")),
            "2024-03-05 10:11:12"
        );
        let int = meta("int", ColumnKind::Int, DisplayHint::Number);
        assert_eq!(f.format(&int, Cell::Int(1234567)), "1234567");
        assert_eq!(f.format(&int, Cell::Null), "NULL");
        assert_eq!(f.format(&int, Cell::Float(1.5)), "1.5");
        let b = meta("bytea", ColumnKind::Bytes, DisplayHint::Binary);
        assert_eq!(f.format(&b, Cell::Bytes(&[1, 2])), "0x0102");
    }

    #[test]
    fn formats_per_type() {
        let f = formats(&[
            ("formats.timestamp", text("%d/%m/%Y %H:%M")),
            ("formats.date", text("%d.%m.%Y")),
            ("formats.time", text("%H:%M")),
            ("formats.timestamptz", text("%Y-%m-%d %H:%M %:z")),
            ("formats.number_grouping", choice("comma")),
            ("formats.decimals", SettingValue::Int(2)),
            ("formats.boolean", choice("check")),
            ("formats.binary", choice("base64")),
            ("grid.null_text", text("∅")),
        ]);
        let temporal = |t: &str| meta(t, ColumnKind::Text, DisplayHint::Temporal);
        assert_eq!(
            f.format(&temporal("timestamp"), Cell::Text("2024-03-05 10:11:12")),
            "05/03/2024 10:11"
        );
        assert_eq!(
            f.format(&temporal("datetime"), Cell::Text("2024-03-05T10:11:12.5")),
            "05/03/2024 10:11"
        );
        assert_eq!(
            f.format(&temporal("date"), Cell::Text("2024-03-05")),
            "05.03.2024"
        );
        assert_eq!(f.format(&temporal("time"), Cell::Text("10:11:12")), "10:11");
        assert_eq!(
            f.format(
                &temporal("timestamptz"),
                Cell::Text("2024-03-05 10:11:12+02")
            ),
            "2024-03-05 08:11 +00:00"
        );
        // A value that does not parse, or a type the formats do not know, stays as it is.
        assert_eq!(
            f.format(&temporal("date"), Cell::Text("infinity")),
            "infinity"
        );
        assert_eq!(
            f.format(&temporal("interval"), Cell::Text("1 day")),
            "1 day"
        );

        let int = meta("int", ColumnKind::Int, DisplayHint::Number);
        assert_eq!(f.format(&int, Cell::Int(1234567)), "1,234,567");
        assert_eq!(f.format(&int, Cell::Int(-999)), "-999");
        assert_eq!(f.format(&int, Cell::Int(-1000)), "-1,000");
        assert_eq!(f.format(&int, Cell::Float(1234.5)), "1,234.50");
        let dec = meta("numeric", ColumnKind::Text, DisplayHint::Decimal);
        assert_eq!(f.format(&dec, Cell::Text("1234567.8900")), "1,234,567.8900");
        assert_eq!(f.format(&dec, Cell::Text("NaN")), "NaN");
        let boolean = meta("bool", ColumnKind::Bool, DisplayHint::Plain);
        assert_eq!(f.format(&boolean, Cell::Bool(true)), "✓");
        assert_eq!(f.format(&boolean, Cell::Bool(false)), "✗");
        assert_eq!(f.format(&boolean, Cell::Null), "∅");
        let bytes = meta("bytea", ColumnKind::Bytes, DisplayHint::Binary);
        assert_eq!(f.format(&bytes, Cell::Bytes(b"Man")), "TWFu");
        assert_eq!(f.format(&bytes, Cell::Bytes(b"Ma")), "TWE=");
        assert_eq!(f.format(&bytes, Cell::Bytes(b"M")), "TQ==");
        assert_eq!(f.format(&bytes, Cell::Bytes(b"")), "");
    }

    #[test]
    fn boolean_and_binary_variants() {
        let b = meta("bool", ColumnKind::Bool, DisplayHint::Plain);
        let one = formats(&[("formats.boolean", choice("one_zero"))]);
        assert_eq!(one.format(&b, Cell::Bool(true)), "1");
        assert_eq!(one.format(&b, Cell::Bool(false)), "0");
        let bytes = meta("bytea", ColumnKind::Bytes, DisplayHint::Binary);
        let utf8 = formats(&[("formats.binary", choice("utf8"))]);
        assert_eq!(
            utf8.format(&bytes, Cell::Bytes("héllo".as_bytes())),
            "héllo"
        );
        assert_eq!(utf8.format(&bytes, Cell::Bytes(&[0xff, 0xfe])), "0xfffe");
    }

    #[test]
    fn long_binary_is_cut_in_the_grid_but_not_in_exports() {
        let f = DataFormats::default();
        let bytes = meta("bytea", ColumnKind::Bytes, DisplayHint::Binary);
        let blob = vec![7u8; 100];
        let grid = f.format(&bytes, Cell::Bytes(&blob));
        assert!(grid.ends_with("… (100 bytes)"), "{grid}");
        assert_eq!(f.format_export(&bytes, Cell::Bytes(&blob)).len(), 2 + 200);
    }

    #[test]
    fn json_compacts_unless_pretty() {
        let json = meta("jsonb", ColumnKind::Text, DisplayHint::Json);
        let raw = "{\n  \"a\": 1\n}";
        assert_eq!(
            DataFormats::default().format(&json, Cell::Text(raw)),
            "{\"a\":1}"
        );
        let pretty = formats(&[("formats.json", choice("pretty"))]);
        assert_eq!(pretty.format(&json, Cell::Text(raw)), raw);
    }

    #[test]
    fn revision_follows_the_formats() {
        let a = DataFormats::default();
        let b = formats(&[("formats.boolean", choice("check"))]);
        assert_ne!(a.revision(), b.revision());
        assert_eq!(a.revision(), DataFormats::default().revision());
        assert!(!b.is_default());
    }
}
