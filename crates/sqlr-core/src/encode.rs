//! Text encoders and decoders for copy, export and import: CSV, TSV, JSON, JSON Lines,
//! SQL `INSERT` and Markdown.
//!
//! Text formats keep NULL and the empty string apart (CSV/TSV quote an empty string that
//! would read as the NULL text; see [`CsvOptions::null_text`] for the full convention). Bytes are written as `0x…` hex everywhere but SQL, so they
//! come back as text.

use std::collections::HashMap;

use crate::chunk::{Cell, ColumnKind, ColumnMeta, DisplayHint, hex_bytes};
use crate::driver::Dialect;
use crate::formats::DataFormats;
use crate::value::Value;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CsvOptions {
    pub delimiter: char,
    pub quote: char,
    /// The first line names the columns.
    pub header: bool,
    /// How NULL is written (the NULL sentinel).
    ///
    /// A sentinel that a CSV field can hold bare (no delimiter, quote or line break) is
    /// written bare, and a text equal to it is quoted: on reading, only a bare field equal
    /// to the sentinel is NULL. A sentinel that needs quoting is written quoted, so it never
    /// splits a record; on reading, any field equal to it is NULL, and a text equal to such
    /// a sentinel reads back as NULL too (CSV has no second quoting to tell them apart).
    pub null_text: String,
    /// `\r\n` line endings instead of `\n`.
    pub crlf: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: ',',
            quote: '"',
            header: true,
            null_text: String::new(),
            crlf: false,
        }
    }
}

impl CsvOptions {
    /// Whether `text` must be quoted to stay one field.
    pub(crate) fn needs_quotes(&self, text: &str) -> bool {
        text.chars()
            .any(|c| c == self.delimiter || c == self.quote || c == '\n' || c == '\r')
    }

    /// Tab-separated, quoted like CSV where needed (what spreadsheets paste).
    pub fn tsv() -> Self {
        Self {
            delimiter: '\t',
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TextFormat {
    Csv(CsvOptions),
    Tsv,
    /// An array of objects.
    Json,
    /// One object per line.
    JsonLines,
    /// One `INSERT` per row into `table` (quoted as one identifier).
    SqlInsert {
        table: String,
    },
    Markdown,
}

/// `text` with the whitespace between JSON tokens removed; `None` when it is not JSON.
/// Strings, numbers and every other token are copied as written, so `1.10` and numbers
/// beyond `f64` survive.
pub fn compact_json(text: &str) -> Option<String> {
    reformat_json(text, None)
}

/// A JSON syntax error, as the char offset of the offending token and the parser's message
/// (`expected value at line 1 column 1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// The error position, as a char offset into the text.
    pub at: usize,
    /// The parser's message, with its line and column.
    pub message: String,
}

/// The first syntax error in `text`; `None` when it is valid JSON.
pub fn json_error(text: &str) -> Option<JsonError> {
    let err = serde_json::from_str::<serde::de::IgnoredAny>(text).err()?;
    Some(JsonError {
        at: char_offset(text, err.line(), err.column()),
        message: err.to_string(),
    })
}

/// The char offset of a 1-based serde_json position. Its columns count bytes, not chars.
fn char_offset(text: &str, line: usize, column: usize) -> usize {
    let mut line_start = 0;
    for _ in 1..line {
        match text[line_start..].find('\n') {
            Some(n) => line_start += n + 1,
            None => return text.chars().count(),
        }
    }
    let mut byte = (line_start + column.saturating_sub(1)).min(text.len());
    while byte > line_start && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    text[..byte].chars().count()
}

/// `text` indented by two spaces per level (like `serde_json`'s pretty printer), with every
/// token copied as written; `None` when it is not JSON.
pub fn pretty_json(text: &str) -> Option<String> {
    reformat_json(text, Some("  "))
}

fn reformat_json(text: &str, indent: Option<&str>) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let newline = |out: &mut String, depth: usize| {
        if let Some(unit) = indent {
            out.push('\n');
            for _ in 0..depth {
                out.push_str(unit);
            }
        }
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('}' | ']')) {
                    out.push(chars.next().unwrap_or(']'));
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => {
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
            }
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

fn ansi_string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Writes rows in one [`TextFormat`]: `header`, then `row` per row, then `footer`.
pub struct Encoder<'d> {
    format: TextFormat,
    columns: Vec<(ColumnKind, DisplayHint)>,
    /// Column names; JSON keys made unique.
    names: Vec<String>,
    /// `INSERT INTO t (a, b) VALUES (` for `SqlInsert`.
    insert_prefix: String,
    /// Writes SQL literals; ANSI forms without one.
    dialect: Option<&'d dyn Dialect>,
    rows: usize,
    /// Display formats applied to the text formats (CSV, TSV, Markdown); SQL and JSON keep
    /// the raw values, where the types matter.
    formats: Option<(DataFormats, Vec<ColumnMeta>)>,
    /// Prefix text cells that a spreadsheet would interpret as a formula.
    spreadsheet_safe: bool,
}

fn quote_ansi(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `name`, `name_2`, `name_3`… so JSON objects never repeat a key.
fn unique_names(columns: &[ColumnMeta]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut names = Vec::with_capacity(columns.len());
    for c in columns {
        let mut name = c.name.clone();
        let mut n = 1;
        while seen.contains_key(&name) {
            n += 1;
            name = format!("{}_{n}", c.name);
        }
        seen.insert(name.clone(), 1);
        names.push(name);
    }
    names
}

fn is_numeric(kind: ColumnKind, hint: DisplayHint) -> bool {
    matches!(kind, ColumnKind::Int | ColumnKind::Float)
        || matches!(hint, DisplayHint::Number | DisplayHint::Decimal)
}

fn csv_options(format: &TextFormat) -> Option<CsvOptions> {
    match format {
        TextFormat::Csv(o) => Some(o.clone()),
        TextFormat::Tsv => Some(CsvOptions::tsv()),
        _ => None,
    }
}

impl<'d> Encoder<'d> {
    pub fn new(
        format: TextFormat,
        columns: &[ColumnMeta],
        dialect: Option<&'d dyn Dialect>,
    ) -> Self {
        let quote = |ident: &str| match dialect {
            Some(d) => d.quote_ident(ident),
            None => quote_ansi(ident),
        };
        let insert_prefix = match &format {
            TextFormat::SqlInsert { table } => {
                let cols: Vec<String> = columns.iter().map(|c| quote(&c.name)).collect();
                format!(
                    "INSERT INTO {} ({}) VALUES (",
                    quote(table),
                    cols.join(", ")
                )
            }
            _ => String::new(),
        };
        let names = match format {
            TextFormat::Json | TextFormat::JsonLines => unique_names(columns),
            _ => columns.iter().map(|c| c.name.clone()).collect(),
        };
        Self {
            format,
            columns: columns.iter().map(|c| (c.kind, c.hint)).collect(),
            names,
            insert_prefix,
            dialect,
            rows: 0,
            formats: None,
            spreadsheet_safe: false,
        }
    }

    /// Neutralizes text that a spreadsheet would treat as a formula. Used for exports and
    /// clipboard copies, where the file may be opened in Excel or LibreOffice; the generic
    /// encoder stays round-trip exact for import/export tests.
    pub fn with_spreadsheet_safe(mut self) -> Self {
        self.spreadsheet_safe = true;
        self
    }

    /// Writes cells the way the grid shows them (CSV, TSV and Markdown only; NULL stays
    /// empty). `columns` are the columns given to [`Self::new`].
    pub fn with_formats(mut self, formats: DataFormats, columns: &[ColumnMeta]) -> Self {
        let text = matches!(
            self.format,
            TextFormat::Csv(_) | TextFormat::Tsv | TextFormat::Markdown
        );
        if text && !formats.is_default() {
            self.formats = Some((formats, columns.to_vec()));
        }
        self
    }

    pub fn header(&self, out: &mut String) {
        if let Some(o) = csv_options(&self.format) {
            if o.header {
                for (i, name) in self.names.iter().enumerate() {
                    if i > 0 {
                        out.push(o.delimiter);
                    }
                    csv_field_safe(&o, Some(name), out, self.spreadsheet_safe);
                }
                out.push_str(if o.crlf { "\r\n" } else { "\n" });
            }
            return;
        }
        match &self.format {
            TextFormat::Json => out.push('['),
            TextFormat::Markdown => {
                out.push('|');
                for name in &self.names {
                    out.push(' ');
                    markdown_text(name, out);
                    out.push_str(" |");
                }
                out.push_str("\n|");
                for &(kind, hint) in &self.columns {
                    out.push_str(if is_numeric(kind, hint) {
                        " ---: |"
                    } else {
                        " --- |"
                    });
                }
                out.push('\n');
            }
            _ => {}
        }
    }

    pub fn row(&mut self, cells: &[Cell<'_>], out: &mut String) {
        let Some((formats, columns)) = &self.formats else {
            return self.encode_row(cells, out);
        };
        let shown: Vec<Option<String>> = cells
            .iter()
            .zip(columns)
            .map(|(cell, meta)| match cell {
                Cell::Null => None,
                cell => Some(formats.format_export(meta, *cell).into_owned()),
            })
            .collect();
        let cells: Vec<Cell<'_>> = shown
            .iter()
            .map(|s| s.as_deref().map_or(Cell::Null, Cell::Text))
            .collect();
        self.encode_row(&cells, out);
    }

    fn encode_row(&mut self, cells: &[Cell<'_>], out: &mut String) {
        if let Some(o) = csv_options(&self.format) {
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    out.push(o.delimiter);
                }
                match cell {
                    Cell::Null => csv_field(&o, None, out),
                    Cell::Text(t) => csv_field_safe(&o, Some(t), out, self.spreadsheet_safe),
                    other => csv_field(&o, Some(&plain_text(*other)), out),
                }
            }
            out.push_str(if o.crlf { "\r\n" } else { "\n" });
            self.rows += 1;
            return;
        }
        match &self.format {
            TextFormat::Json => {
                if self.rows > 0 {
                    out.push(',');
                }
                out.push_str("\n  ");
                self.json_object(cells, out);
            }
            TextFormat::JsonLines => {
                self.json_object(cells, out);
                out.push('\n');
            }
            TextFormat::SqlInsert { .. } => {
                out.push_str(&self.insert_prefix);
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.sql_literal(*cell, out);
                }
                out.push_str(");\n");
            }
            TextFormat::Markdown => {
                out.push('|');
                for cell in cells {
                    out.push(' ');
                    match cell {
                        Cell::Text(t) => markdown_text(t, out),
                        other => markdown_text(&plain_text(*other), out),
                    }
                    out.push_str(" |");
                }
                out.push('\n');
            }
            TextFormat::Csv(_) | TextFormat::Tsv => unreachable!("handled above"),
        }
        self.rows += 1;
    }

    pub fn footer(&self, out: &mut String) {
        if self.format == TextFormat::Json {
            out.push_str("\n]\n");
        }
    }

    fn json_object(&self, cells: &[Cell<'_>], out: &mut String) {
        out.push('{');
        for (i, (cell, name)) in cells.iter().zip(&self.names).enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_string(name, out);
            out.push(':');
            let hint = self.columns.get(i).map_or(DisplayHint::Plain, |c| c.1);
            match cell {
                Cell::Null => out.push_str("null"),
                Cell::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Cell::Int(v) => out.push_str(&v.to_string()),
                Cell::Float(f) => match serde_json::Number::from_f64(*f) {
                    Some(n) => out.push_str(&n.to_string()),
                    None => json_string(&f.to_string(), out),
                },
                Cell::Text(t) => {
                    let embedded = (hint == DisplayHint::Json)
                        .then(|| compact_json(t))
                        .flatten();
                    match embedded {
                        Some(json) => out.push_str(&json),
                        None => json_string(t, out),
                    }
                }
                Cell::Bytes(b) => json_string(&hex_bytes(b), out),
            }
        }
        out.push('}');
    }

    fn string_literal(&self, text: &str) -> String {
        match self.dialect {
            Some(d) => d.string_literal(text),
            None => ansi_string_literal(text),
        }
    }

    fn sql_literal(&self, cell: Cell<'_>, out: &mut String) {
        match cell {
            Cell::Null => out.push_str("NULL"),
            Cell::Bool(b) => out.push_str(match self.dialect {
                Some(d) => d.bool_literal(b),
                None => {
                    if b {
                        "TRUE"
                    } else {
                        "FALSE"
                    }
                }
            }),
            Cell::Int(v) => out.push_str(&v.to_string()),
            Cell::Float(f) if f.is_finite() => out.push_str(&f.to_string()),
            Cell::Float(f) => {
                let text = if f.is_nan() {
                    "NaN"
                } else if f > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                out.push_str(&self.string_literal(text));
            }
            Cell::Text(t) => out.push_str(&self.string_literal(t)),
            Cell::Bytes(b) => out.push_str(&match self.dialect {
                Some(d) => d.bytes_literal(b),
                None => format!("X'{}'", &hex_bytes(b)[2..]),
            }),
        }
    }
}

/// A non-text, non-NULL cell as text.
fn plain_text(cell: Cell<'_>) -> String {
    match cell {
        Cell::Float(f) if f.is_nan() => "NaN".into(),
        other => other.to_string(),
    }
}

/// Writes one text field, optionally neutralizing a leading character that a spreadsheet
/// would evaluate as a formula. Quoting alone does not stop `=`, `+`, `-` or `@` formulas;
/// a leading apostrophe makes the cell text in Excel and LibreOffice.
fn csv_field_safe(o: &CsvOptions, text: Option<&str>, out: &mut String, safe: bool) {
    let Some(text) = text else {
        csv_field(o, None, out);
        return;
    };
    if safe
        && !text.starts_with('\'')
        && text
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r' | '\n'))
    {
        let neutralized = format!("'{text}");
        csv_field(o, Some(&neutralized), out);
    } else {
        csv_field(o, Some(text), out);
    }
}

/// Writes one field; `None` is NULL (see [`CsvOptions::null_text`]).
fn csv_field(o: &CsvOptions, text: Option<&str>, out: &mut String) {
    let needs_quotes = match text {
        None => o.needs_quotes(&o.null_text),
        Some(text) => text == o.null_text || o.needs_quotes(text),
    };
    let text = text.unwrap_or(&o.null_text);
    if !needs_quotes {
        out.push_str(text);
        return;
    }
    out.push(o.quote);
    for c in text.chars() {
        if c == o.quote {
            out.push(c);
        }
        out.push(c);
    }
    out.push(o.quote);
}

fn markdown_text(text: &str, out: &mut String) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("<br>");
            }
            '\n' => out.push_str("<br>"),
            c => out.push(c),
        }
    }
}

fn json_string(text: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(text).expect("strings always serialize"));
}

/// Decoded rows, each with the line (1-based) it starts on.
pub type NumberedRows = Vec<(u64, Vec<Value>)>;

/// Reads CSV, TSV, JSON (an array of objects) or JSON Lines into column names and rows.
/// CSV and TSV values are text or NULL; JSON keeps numbers and booleans.
pub fn decode(format: &TextFormat, text: &str) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let (names, rows) = decode_numbered(format, text)?;
    Ok((names, rows.into_iter().map(|(_, row)| row).collect()))
}

/// Like [`decode`], with each row's line in `text` (1-based; for a JSON array, the object's
/// position in it), to name rows in error reports. Rows of objects are padded with NULL to
/// the union of their keys. See [`crate::read_rows`] for the streamed form.
pub fn decode_numbered(
    format: &TextFormat,
    text: &str,
) -> Result<(Vec<String>, NumberedRows), String> {
    let mut rows = Vec::new();
    let names = crate::read::read_rows(format, text.as_bytes(), |_, line, row| {
        rows.push((line, row));
        true
    })?;
    for (_, row) in &mut rows {
        row.resize(names.len(), Value::Null);
    }
    Ok((names, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ColumnKind, DisplayHint};
    use crate::value::Value;

    fn columns() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta::new("id", "int8", ColumnKind::Int, DisplayHint::Number),
            ColumnMeta::new("v", "text", ColumnKind::Text, DisplayHint::Plain),
            ColumnMeta::new("b", "bytea", ColumnKind::Bytes, DisplayHint::Binary),
        ]
    }

    fn awkward() -> Vec<Option<String>> {
        let mut v: Vec<Option<String>> = [
            "a,b",
            "\"q\"",
            "tab\there",
            "line\nbreak",
            "cr\r\nlf",
            "|pipe|",
            "😀",
            "",
            " padded ",
            "\\N",
            "semi;colon",
        ]
        .iter()
        .map(|s| Some(s.to_string()))
        .collect();
        v.push(None);
        v.push(Some("x".repeat(1 << 20)));
        v
    }

    fn rows() -> Vec<Vec<Value>> {
        awkward()
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                vec![
                    Value::Int(i as i64),
                    text.map_or(Value::Null, Value::Text),
                    if i % 2 == 0 {
                        Value::Bytes(vec![1, 2, 0xff])
                    } else {
                        Value::Null
                    },
                ]
            })
            .collect()
    }

    fn encode(format: TextFormat, columns: &[ColumnMeta], rows: &[Vec<Value>]) -> String {
        let mut encoder = Encoder::new(format, columns, None);
        let mut out = String::new();
        encoder.header(&mut out);
        for row in rows {
            let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
            encoder.row(&cells, &mut out);
        }
        encoder.footer(&mut out);
        out
    }

    #[test]
    fn export_uses_display_formats_only_for_text_formats() {
        use crate::settings_model::SettingValue;
        let columns = [
            ColumnMeta::new("n", "int8", ColumnKind::Int, DisplayHint::Number),
            ColumnMeta::new("b", "bool", ColumnKind::Bool, DisplayHint::Plain),
        ];
        let formats = DataFormats::from_values(|id| match id {
            "formats.number_grouping" => Some(SettingValue::Choice("comma".into())),
            "formats.boolean" => Some(SettingValue::Choice("check".into())),
            _ => None,
        });
        let run = |format: TextFormat, formats: Option<&DataFormats>| {
            let mut encoder = Encoder::new(format, &columns, None);
            if let Some(f) = formats {
                encoder = encoder.with_formats(f.clone(), &columns);
            }
            let mut out = String::new();
            encoder.row(&[Cell::Int(1234567), Cell::Bool(true)], &mut out);
            encoder.row(&[Cell::Null, Cell::Bool(false)], &mut out);
            out
        };
        let csv = || TextFormat::Csv(CsvOptions::default());
        assert_eq!(run(csv(), None), "1234567,true\n,false\n");
        assert_eq!(run(csv(), Some(&formats)), "\"1,234,567\",✓\n,✗\n");
        // Formats that change nothing leave the output alone.
        assert_eq!(run(csv(), Some(&DataFormats::default())), run(csv(), None));
        // SQL and JSON keep the raw values: their types matter.
        for format in [TextFormat::Json, TextFormat::JsonLines] {
            assert_eq!(run(format.clone(), Some(&formats)), run(format, None));
        }
        let sql = TextFormat::SqlInsert { table: "t".into() };
        assert_eq!(run(sql.clone(), Some(&formats)), run(sql, None));
    }

    /// What a text format gives back: everything but NULL becomes text.
    fn as_text(rows: &[Vec<Value>]) -> Vec<Vec<Value>> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|v| match v {
                        Value::Null => Value::Null,
                        Value::Text(t) => Value::Text(t.clone()),
                        other => Value::Text(other.display().into_owned()),
                    })
                    .collect()
            })
            .collect()
    }

    /// What JSON gives back: scalars keep their type, bytes become hex text.
    fn as_json(rows: &[Vec<Value>]) -> Vec<Vec<Value>> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|v| match v {
                        Value::Bytes(_) => Value::Text(v.display().into_owned()),
                        other => other.clone(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn encoders_round_trip_awkward_text() {
        let cols = columns();
        let rows = rows();
        let semicolon = CsvOptions {
            delimiter: ';',
            quote: '\'',
            header: true,
            null_text: "\\N".into(),
            crlf: true,
        };
        let text_formats = [
            TextFormat::Csv(CsvOptions::default()),
            TextFormat::Csv(semicolon),
            TextFormat::Tsv,
        ];
        for format in text_formats {
            let text = encode(format.clone(), &cols, &rows);
            let (names, decoded) = decode(&format, &text).unwrap();
            assert_eq!(names, ["id", "v", "b"], "{format:?}");
            assert_eq!(decoded.len(), rows.len(), "{format:?}");
            assert!(decoded == as_text(&rows), "{format:?} round trip differs");
        }
        for format in [TextFormat::Json, TextFormat::JsonLines] {
            let text = encode(format.clone(), &cols, &rows);
            let (names, decoded) = decode(&format, &text).unwrap();
            assert_eq!(names, ["id", "v", "b"], "{format:?}");
            assert!(decoded == as_json(&rows), "{format:?} round trip differs");
        }
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        let cols = &columns()[..2];
        let rows = vec![
            vec![Value::Int(1), Value::Text("plain".into())],
            vec![Value::Int(2), Value::Text(String::new())],
            vec![Value::Int(3), Value::Null],
            vec![Value::Int(4), Value::Text("a\"b".into())],
        ];
        assert_eq!(
            encode(TextFormat::Csv(CsvOptions::default()), cols, &rows),
            "id,v\n1,plain\n2,\"\"\n3,\n4,\"a\"\"b\"\n"
        );
        assert_eq!(
            encode(TextFormat::Tsv, cols, &rows[..1]),
            "id\tv\n1\tplain\n"
        );
    }

    #[test]
    fn spreadsheet_safe_csv_neutralizes_formula_starts() {
        let cols = [
            ColumnMeta::new("=head", "text", ColumnKind::Text, DisplayHint::Plain),
            ColumnMeta::new("n", "int", ColumnKind::Int, DisplayHint::Number),
        ];
        let rows = vec![
            vec![Value::Text("=1+1".into()), Value::Int(-5)],
            vec![Value::Text("+cmd".into()), Value::Int(2)],
            vec![Value::Text("-text".into()), Value::Int(3)],
            vec![Value::Text("@cell".into()), Value::Int(4)],
            vec![Value::Text("'safe".into()), Value::Int(5)],
        ];
        let mut encoder = Encoder::new(TextFormat::Csv(CsvOptions::default()), &cols, None)
            .with_spreadsheet_safe();
        let mut out = String::new();
        encoder.header(&mut out);
        for row in &rows {
            let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
            encoder.row(&cells, &mut out);
        }
        assert_eq!(
            out,
            "'=head,n\n'=1+1,-5\n'+cmd,2\n'-text,3\n'@cell,4\n'safe,5\n"
        );
        // The generic encoder stays round-trip exact.
        assert_eq!(
            encode(TextFormat::Csv(CsvOptions::default()), &cols, &rows[..1]),
            "=head,n\n=1+1,-5\n"
        );
    }

    #[test]
    fn csv_null_text_with_a_line_break_does_not_add_rows() {
        let cols = vec![ColumnMeta::new(
            "v",
            "text",
            ColumnKind::Text,
            DisplayHint::Plain,
        )];
        let rows = vec![
            vec![Value::Text("one".into())],
            vec![Value::Null],
            vec![Value::Text("two".into())],
        ];
        let format = TextFormat::Csv(CsvOptions {
            null_text: "N/A\nN/A".into(),
            ..CsvOptions::default()
        });
        let text = encode(format.clone(), &cols, &rows);
        let (_, decoded) = decode(&format, &text).unwrap_or_else(|e| panic!("{e}\n{text:?}"));
        assert_eq!(decoded, rows, "{text:?}");
    }

    /// Every NULL sentinel round-trips NULL, the empty string, texts that contain the
    /// sentinel and awkward texts; a sentinel CSV can hold bare also keeps a text equal to it.
    #[test]
    fn csv_null_sentinels_round_trip() {
        let cols = vec![
            ColumnMeta::new("v", "text", ColumnKind::Text, DisplayHint::Plain),
            ColumnMeta::new("w", "text", ColumnKind::Text, DisplayHint::Plain),
        ];
        for (null_text, bare) in [
            ("", true),
            ("\\N", true),
            ("a,b", false),
            ("say \"x\"", false),
            ("\"", false),
            ("N/A\nN/A", false),
            ("cr\r", false),
        ] {
            let mut texts = vec![
                String::new(),
                format!("<{null_text}>"),
                "plain".to_string(),
                "a,b\n\"q\"".to_string(),
            ];
            if bare {
                texts.push(null_text.to_string());
            }
            let mut rows = vec![vec![Value::Null, Value::Null]];
            for t in texts {
                rows.push(vec![Value::Text(t.clone()), Value::Null]);
                rows.push(vec![Value::Null, Value::Text(t)]);
            }
            for header in [true, false] {
                let format = TextFormat::Csv(CsvOptions {
                    null_text: null_text.into(),
                    header,
                    ..CsvOptions::default()
                });
                let text = encode(format.clone(), &cols, &rows);
                let (_, decoded) =
                    decode(&format, &text).unwrap_or_else(|e| panic!("{e}\n{text:?}"));
                assert_eq!(decoded, rows, "{null_text:?}: {text:?}");
            }
        }
    }

    /// A sentinel that needs quoting is written quoted, and a text equal to it cannot be
    /// told apart: it reads back as NULL (the documented `null_text` convention).
    #[test]
    fn csv_text_equal_to_a_quoted_sentinel_reads_as_null() {
        let cols = vec![ColumnMeta::new(
            "v",
            "text",
            ColumnKind::Text,
            DisplayHint::Plain,
        )];
        let format = TextFormat::Csv(CsvOptions {
            null_text: "a,b".into(),
            ..CsvOptions::default()
        });
        let rows = vec![vec![Value::Null], vec![Value::Text("a,b".into())]];
        let text = encode(format.clone(), &cols, &rows);
        assert_eq!(text, "v\n\"a,b\"\n\"a,b\"\n");
        let (_, decoded) = decode(&format, &text).unwrap();
        assert_eq!(decoded, [vec![Value::Null], vec![Value::Null]]);
    }

    fn small_rows() -> Vec<Vec<Value>> {
        vec![
            vec![
                Value::Int(1),
                Value::Text("it's".into()),
                Value::Bytes(vec![1, 2]),
            ],
            vec![Value::Int(2), Value::Null, Value::Null],
            vec![Value::Int(3), Value::Text("a|b\nc".into()), Value::Null],
        ]
    }

    #[test]
    fn sql_insert_quotes_identifiers_and_literals() {
        let format = TextFormat::SqlInsert {
            table: "my \"table\"".into(),
        };
        assert_eq!(
            encode(format, &columns(), &small_rows()),
            "INSERT INTO \"my \"\"table\"\"\" (\"id\", \"v\", \"b\") VALUES (1, 'it''s', X'0102');\n\
             INSERT INTO \"my \"\"table\"\"\" (\"id\", \"v\", \"b\") VALUES (2, NULL, NULL);\n\
             INSERT INTO \"my \"\"table\"\"\" (\"id\", \"v\", \"b\") VALUES (3, 'a|b\nc', NULL);\n"
        );
        let cols = vec![
            ColumnMeta::new("f", "float8", ColumnKind::Float, DisplayHint::Plain),
            ColumnMeta::new("ok", "bool", ColumnKind::Bool, DisplayHint::Plain),
        ];
        let rows = vec![vec![Value::Float(f64::NAN), Value::Bool(true)]];
        assert_eq!(
            encode(TextFormat::SqlInsert { table: "t".into() }, &cols, &rows),
            "INSERT INTO \"t\" (\"f\", \"ok\") VALUES ('NaN', TRUE);\n"
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        assert_eq!(
            encode(TextFormat::Markdown, &columns(), &small_rows()),
            "| id | v | b |\n\
             | ---: | --- | --- |\n\
             | 1 | it's | 0x0102 |\n\
             | 2 | NULL | NULL |\n\
             | 3 | a\\|b<br>c | NULL |\n"
        );
    }

    #[test]
    fn mixed_column_encodes_each_cell_by_its_type() {
        let cols = vec![ColumnMeta::new(
            "m",
            "",
            ColumnKind::Mixed,
            DisplayHint::Plain,
        )];
        let rows = vec![
            vec![Value::Int(1)],
            vec![Value::Text("abc".into())],
            vec![Value::Float(1.5)],
            vec![Value::Null],
        ];
        let text = encode(TextFormat::JsonLines, &cols, &rows);
        assert_eq!(
            text,
            "{\"m\":1}\n{\"m\":\"abc\"}\n{\"m\":1.5}\n{\"m\":null}\n"
        );
        assert_eq!(decode(&TextFormat::JsonLines, &text).unwrap().1, rows);
    }

    #[test]
    fn json_reformatting_keeps_every_token() {
        let raw = "{\"n\": 123456789012345678901234, \"d\": 1.10,\n \"s\": \"a : b, {c} \\\" [\", \"e\": [ ], \"o\": {}}";
        assert_eq!(
            compact_json(raw).unwrap(),
            "{\"n\":123456789012345678901234,\"d\":1.10,\"s\":\"a : b, {c} \\\" [\",\"e\":[],\"o\":{}}"
        );
        assert_eq!(
            pretty_json("{\"a\":[1,{\"b\":1.10}],\"e\":[],\"s\":\"x,y\"}").unwrap(),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": 1.10\n    }\n  ],\n  \"e\": [],\n  \"s\": \"x,y\"\n}"
        );
        assert_eq!(compact_json("{oops"), None);
        assert_eq!(pretty_json("[1,"), None);
        // Export embeds the column's JSON with every number as written.
        let cols = vec![ColumnMeta::new(
            "j",
            "jsonb",
            ColumnKind::Text,
            DisplayHint::Json,
        )];
        let rows = vec![vec![Value::Text(raw.into())]];
        let text = encode(TextFormat::JsonLines, &cols, &rows);
        assert!(
            text.contains("123456789012345678901234") && text.contains("1.10"),
            "{text}"
        );
    }

    struct ELiterals;

    impl Dialect for ELiterals {
        fn quote_ident(&self, ident: &str) -> String {
            format!("[{ident}]")
        }
        fn select_all(&self, _node: &crate::node::Node, _limit: Option<usize>) -> Option<String> {
            None
        }
        fn string_literal(&self, text: &str) -> String {
            format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
        }
        fn bytes_literal(&self, bytes: &[u8]) -> String {
            format!("{}::bytea", bytes.len())
        }
        fn bool_literal(&self, value: bool) -> &'static str {
            if value { "1" } else { "0" }
        }
    }

    #[test]
    fn sql_insert_writes_booleans_the_way_the_dialect_does() {
        let cols = [ColumnMeta::new(
            "ok",
            "bool",
            ColumnKind::Bool,
            DisplayHint::Plain,
        )];
        let mut encoder = Encoder::new(
            TextFormat::SqlInsert { table: "t".into() },
            &cols,
            Some(&ELiterals),
        );
        let mut out = String::new();
        encoder.row(&[Cell::Bool(true)], &mut out);
        assert_eq!(out, "INSERT INTO [t] ([ok]) VALUES (1);\n");
    }

    #[test]
    fn sql_insert_asks_the_dialect_for_each_literal() {
        let cols = &columns()[1..];
        let row = [Value::Text("a\\b'c".into()), Value::Bytes(vec![1, 2, 3])];
        let mut encoder = Encoder::new(
            TextFormat::SqlInsert { table: "t".into() },
            cols,
            Some(&ELiterals),
        );
        let mut out = String::new();
        let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
        encoder.row(&cells, &mut out);
        assert_eq!(
            out,
            "INSERT INTO [t] ([v], [b]) VALUES (E'a\\\\b''c', 3::bytea);\n"
        );
    }

    #[test]
    fn markdown_escapes_backslashes_before_pipes() {
        let cols = &columns()[1..2];
        let rows = vec![vec![Value::Text("a\\|b".into())]];
        assert!(encode(TextFormat::Markdown, cols, &rows).ends_with("| a\\\\\\|b |\n"));
    }

    #[test]
    fn json_objects_union_keys_in_first_seen_order() {
        let text = r#"[{"b":1,"a":"x"},{"c":null,"a":"y","d":[1,{"z":2}]}]"#;
        let (names, rows) = decode(&TextFormat::Json, text).unwrap();
        assert_eq!(names, ["b", "a", "c", "d"]);
        assert_eq!(
            rows,
            vec![
                vec![
                    Value::Int(1),
                    Value::Text("x".into()),
                    Value::Null,
                    Value::Null
                ],
                vec![
                    Value::Null,
                    Value::Text("y".into()),
                    Value::Null,
                    Value::Text(r#"[1,{"z":2}]"#.into())
                ],
            ]
        );
        let (names, rows) =
            decode(&TextFormat::JsonLines, "{\"a\":1.5}\n\n{\"a\":true}\n").unwrap();
        assert_eq!(names, ["a"]);
        assert_eq!(rows, vec![vec![Value::Float(1.5)], vec![Value::Bool(true)]]);
    }

    #[test]
    fn malformed_input_is_an_error() {
        let csv = TextFormat::Csv(CsvOptions::default());
        assert!(decode(&csv, "a,b\n\"open,1\n").is_err());
        assert!(decode(&TextFormat::Json, "{\"a\":1}").is_err());
        assert!(decode(&TextFormat::JsonLines, "[1]").is_err());
        assert!(decode(&TextFormat::Markdown, "| a |").is_err());
    }

    #[test]
    fn decode_numbered_reports_record_lines() {
        let csv = "a,b\n1,\"x\ny\"\n\n2,z\r\n3,w";
        let (names, rows) = decode_numbered(&TextFormat::Csv(CsvOptions::default()), csv).unwrap();
        assert_eq!(names, ["a", "b"]);
        let lines: Vec<u64> = rows.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 5, 6]);
        assert_eq!(rows[0].1[1], Value::Text("x\ny".into()));
        let headless = CsvOptions {
            header: false,
            ..CsvOptions::default()
        };
        let (_, rows) = decode_numbered(&TextFormat::Csv(headless), "1\n2").unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [1, 2]);
        let (_, rows) =
            decode_numbered(&TextFormat::JsonLines, "{\"a\":1}\n\n{\"a\":2}\n").unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [1, 3]);
        let (_, rows) = decode_numbered(&TextFormat::Json, "[{\"a\":1},{\"a\":2}]").unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [1, 2]);
    }
}
