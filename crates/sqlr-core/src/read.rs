//! Streamed decoding of CSV, TSV, JSON (an array of objects) and JSON Lines: rows are
//! handed over one at a time while the input is read, so a large file is never held in
//! memory (import, file preview).

use std::collections::HashMap;
use std::fmt;
use std::io::BufRead;

use serde::Deserialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::encode::{CsvOptions, TextFormat};
use crate::value::Value;

/// Why a text file could not be read back into rows.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The input is not valid UTF-8.
    #[error("The file is not UTF-8 text")]
    NotUtf8,
    /// The input could not be read.
    #[error("{0}")]
    Io(std::io::Error),
    /// The format is write-only.
    #[error("This format cannot be read back")]
    WriteOnlyFormat,
    /// A quoted CSV field is never closed.
    #[error("Unclosed quote starting on line {line}")]
    UnclosedQuote {
        /// The line the quoted field starts on.
        line: usize,
    },
    /// A closing quote is followed by something other than a delimiter or line end.
    #[error("Unexpected \"{found}\" after a closing quote on line {line}")]
    AfterClosingQuote {
        /// The unexpected character.
        found: char,
        /// The line of the record.
        line: usize,
    },
    /// A record has more fields than the header names.
    #[error("Line {line} has {found} fields, expected {expected}")]
    TooManyFields {
        /// The line the record starts on.
        line: usize,
        /// The number of fields found.
        found: usize,
        /// The number of columns.
        expected: usize,
    },
    /// A JSON Lines line is not a JSON object.
    #[error("Line {line}: {source}")]
    JsonLine {
        /// The line.
        line: u64,
        /// The parser's complaint.
        source: serde_json::Error,
    },
    /// The input is not a JSON array of objects.
    #[error("Expected a JSON array of objects: {0}")]
    JsonArray(serde_json::Error),
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::InvalidData {
            Self::NotUtf8
        } else {
            Self::Io(e)
        }
    }
}

/// Reads `reader` in `format`, calling `on_row(columns, line, row)` for each row: the
/// column names known so far, the line the row starts on (1-based; for a JSON array, the
/// object's position in it) and its values in column order. A row of objects may be
/// shorter than `columns` (keys first seen later are appended to `columns`). `on_row`
/// returns `false` to stop early. Returns the column names at the end.
///
/// CSV and TSV values are text or NULL; JSON keeps numbers and booleans. A leading
/// byte-order mark is skipped.
pub fn read_rows<R: BufRead>(
    format: &TextFormat,
    mut reader: R,
    on_row: impl FnMut(&[String], u64, Vec<Value>) -> bool,
) -> Result<Vec<String>, ReadError> {
    skip_bom(&mut reader)?;
    match format {
        TextFormat::Csv(o) => read_delimited(o, reader, on_row),
        TextFormat::Tsv => read_delimited(&CsvOptions::tsv(), reader, on_row),
        TextFormat::JsonLines => read_json_lines(reader, on_row),
        TextFormat::Json => read_json_array(reader, on_row),
        TextFormat::SqlInsert { .. } | TextFormat::Markdown => Err(ReadError::WriteOnlyFormat),
    }
}

fn skip_bom<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    if reader.fill_buf()?.starts_with(b"\xEF\xBB\xBF") {
        reader.consume(3);
    }
    Ok(())
}

/// Chars of a buffered reader, read a line at a time.
struct Chars<R> {
    reader: R,
    line: String,
    pos: usize,
    eof: bool,
    error: Option<ReadError>,
}

impl<R: BufRead> Chars<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            line: String::new(),
            pos: 0,
            eof: false,
            error: None,
        }
    }

    fn fill(&mut self) -> bool {
        if self.pos < self.line.len() {
            return true;
        }
        if self.eof {
            return false;
        }
        self.line.clear();
        self.pos = 0;
        match self.reader.read_line(&mut self.line) {
            Ok(0) => {
                self.eof = true;
                false
            }
            Ok(_) => true,
            Err(e) => {
                self.error = Some(e.into());
                self.eof = true;
                false
            }
        }
    }

    fn peek(&mut self) -> Option<char> {
        if !self.fill() {
            return None;
        }
        self.line[self.pos..].chars().next()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
}

/// One parsed delimited field.
struct Field {
    text: String,
    quoted: bool,
}

/// The next record and the line it starts on; `None` at the end. Quoted fields may hold
/// delimiters, quotes (doubled) and line breaks; `\r\n`, `\n` and `\r` end a record.
fn next_record<R: BufRead>(
    chars: &mut Chars<R>,
    delimiter: char,
    quote: char,
    line: &mut usize,
) -> Result<Option<(usize, Vec<Field>)>, ReadError> {
    if chars.peek().is_none() {
        return match chars.error.take() {
            Some(e) => Err(e),
            None => Ok(None),
        };
    }
    let start_line = *line;
    let mut record = Vec::new();
    loop {
        let mut field = Field {
            text: String::new(),
            quoted: false,
        };
        if chars.peek() == Some(quote) {
            chars.next();
            field.quoted = true;
            let start = *line;
            loop {
                match chars.next() {
                    None => {
                        return Err(chars
                            .error
                            .take()
                            .unwrap_or(ReadError::UnclosedQuote { line: start }));
                    }
                    Some(c) if c == quote => {
                        if chars.peek() == Some(quote) {
                            chars.next();
                            field.text.push(quote);
                        } else {
                            break;
                        }
                    }
                    Some(c) => {
                        if c == '\n' {
                            *line += 1;
                        }
                        field.text.push(c);
                    }
                }
            }
        } else {
            while let Some(c) = chars.peek() {
                if c == delimiter || c == '\n' || c == '\r' {
                    break;
                }
                field.text.push(c);
                chars.next();
            }
        }
        record.push(field);
        match chars.next() {
            Some(c) if c == delimiter => {}
            Some('\r') => {
                if chars.peek() == Some('\n') {
                    chars.next();
                }
                *line += 1;
                return Ok(Some((start_line, record)));
            }
            Some('\n') => {
                *line += 1;
                return Ok(Some((start_line, record)));
            }
            None => {
                if let Some(e) = chars.error.take() {
                    return Err(e);
                }
                return Ok(Some((start_line, record)));
            }
            Some(c) => {
                return Err(ReadError::AfterClosingQuote {
                    found: c,
                    line: *line,
                });
            }
        }
    }
}

fn read_delimited<R: BufRead>(
    o: &CsvOptions,
    reader: R,
    mut on_row: impl FnMut(&[String], u64, Vec<Value>) -> bool,
) -> Result<Vec<String>, ReadError> {
    let mut chars = Chars::new(reader);
    let mut line = 1;
    let mut names: Vec<String> = Vec::new();
    if o.header {
        match next_record(&mut chars, o.delimiter, o.quote, &mut line)? {
            Some((_, fields)) => names = fields.into_iter().map(|f| f.text).collect(),
            None => return Ok(Vec::new()),
        }
    }
    // A sentinel that needs quoting is written quoted (see `CsvOptions::null_text`).
    let null_quoted = o.needs_quotes(&o.null_text);
    let mut width = names.len();
    while let Some((start, record)) = next_record(&mut chars, o.delimiter, o.quote, &mut line)? {
        let blank = record.len() == 1 && !record[0].quoted && record[0].text.is_empty();
        if blank && width != 1 {
            continue;
        }
        if width == 0 {
            width = record.len();
            names = (1..=width).map(|i| format!("column{i}")).collect();
        }
        // A short row is padded with NULLs; a long one has fields nothing names.
        if record.len() > width {
            return Err(ReadError::TooManyFields {
                line: start,
                found: record.len(),
                expected: width,
            });
        }
        let mut row: Vec<Value> = record
            .into_iter()
            .map(|f| {
                if f.text == o.null_text && (!f.quoted || null_quoted) {
                    Value::Null
                } else {
                    Value::Text(f.text)
                }
            })
            .collect();
        row.resize(width, Value::Null);
        if !on_row(&names, start as u64, row) {
            break;
        }
    }
    Ok(names)
}

/// A JSON object with its keys in document order.
pub(crate) struct OrderedObject(Vec<(String, serde_json::Value)>);

impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = OrderedObject;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedObject, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, serde_json::Value>()? {
                    entries.push(entry);
                }
                Ok(OrderedObject(entries))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}

fn json_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.is_u64()) {
            (Some(i), _) => Value::Int(i),
            (None, true) => Value::Text(n.to_string()),
            (None, false) => n.as_f64().map_or(Value::Null, Value::Float),
        },
        serde_json::Value::String(s) => Value::Text(s),
        other => Value::Text(other.to_string()),
    }
}

/// Turns objects into rows: the columns are the union of their keys in first-seen order,
/// and a key an object lacks is NULL.
struct ObjectRows<F> {
    names: Vec<String>,
    index: HashMap<String, usize>,
    on_row: F,
}

impl<F: FnMut(&[String], u64, Vec<Value>) -> bool> ObjectRows<F> {
    fn new(on_row: F) -> Self {
        Self {
            names: Vec::new(),
            index: HashMap::new(),
            on_row,
        }
    }

    /// Hands the object over; `false` once the caller wants no more.
    fn push(&mut self, line: u64, object: OrderedObject) -> bool {
        let mut cells = Vec::with_capacity(object.0.len());
        for (key, value) in object.0 {
            let i = match self.index.get(&key) {
                Some(&i) => i,
                None => {
                    self.names.push(key.clone());
                    self.index.insert(key, self.names.len() - 1);
                    self.names.len() - 1
                }
            };
            cells.push((i, json_value(value)));
        }
        let mut row = vec![Value::Null; self.names.len()];
        for (i, v) in cells {
            row[i] = v;
        }
        (self.on_row)(&self.names, line, row)
    }
}

fn read_json_lines<R: BufRead>(
    mut reader: R,
    on_row: impl FnMut(&[String], u64, Vec<Value>) -> bool,
) -> Result<Vec<String>, ReadError> {
    let mut rows = ObjectRows::new(on_row);
    let mut text = String::new();
    let mut line = 0u64;
    loop {
        text.clear();
        if reader.read_line(&mut text)? == 0 {
            break;
        }
        line += 1;
        if text.trim().is_empty() {
            continue;
        }
        let object = serde_json::from_str::<OrderedObject>(&text)
            .map_err(|source| ReadError::JsonLine { line, source })?;
        if !rows.push(line, object) {
            break;
        }
    }
    Ok(rows.names)
}

/// Streams the elements of a JSON array to [`ObjectRows`], flagging when it stopped early.
struct ArraySeed<'a, F> {
    rows: &'a mut ObjectRows<F>,
    stopped: &'a std::cell::Cell<bool>,
}

impl<'de, F: FnMut(&[String], u64, Vec<Value>) -> bool> DeserializeSeed<'de> for ArraySeed<'_, F> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, F: FnMut(&[String], u64, Vec<Value>) -> bool> Visitor<'de> for ArraySeed<'_, F> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON array of objects")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut n = 0;
        while let Some(object) = seq.next_element::<OrderedObject>()? {
            n += 1;
            if !self.rows.push(n, object) {
                self.stopped.set(true);
                return Ok(());
            }
        }
        Ok(())
    }
}

fn read_json_array<R: BufRead>(
    reader: R,
    on_row: impl FnMut(&[String], u64, Vec<Value>) -> bool,
) -> Result<Vec<String>, ReadError> {
    let mut rows = ObjectRows::new(on_row);
    let mut de = serde_json::Deserializer::from_reader(reader);
    let stopped = std::cell::Cell::new(false);
    let outcome = ArraySeed {
        rows: &mut rows,
        stopped: &stopped,
    }
    .deserialize(&mut de);
    match outcome {
        // Stopping early leaves the array unfinished, which serde reports as an error.
        _ if stopped.get() => {}
        Ok(()) => de.end().map_err(ReadError::JsonArray)?,
        Err(e) => return Err(ReadError::JsonArray(e)),
    }
    Ok(rows.names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(
        format: &TextFormat,
        text: &str,
        limit: usize,
    ) -> (Vec<String>, Vec<(u64, Vec<Value>)>) {
        let mut rows = Vec::new();
        let names = read_rows(format, text.as_bytes(), |_, line, row| {
            rows.push((line, row));
            rows.len() < limit
        })
        .unwrap();
        (names, rows)
    }

    #[test]
    fn a_short_row_is_padded_and_a_long_one_is_an_error() {
        let csv = "a,b,c\n1,2,3\n4,5\n6\n";
        let (_, rows) = collect(&TextFormat::Csv(CsvOptions::default()), csv, usize::MAX);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1].1,
            vec![
                Value::Text("4".into()),
                Value::Text("5".into()),
                Value::Null
            ]
        );
        assert_eq!(rows[2].1[1], Value::Null);
        let err = read_rows(
            &TextFormat::Csv(CsvOptions::default()),
            "a,b\n1,2,3\n".as_bytes(),
            |_, _, _| true,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "Line 2 has 3 fields, expected 2");
    }

    #[test]
    fn stops_early_and_reports_the_columns_so_far() {
        let csv = "a,b\n1,2\n3,4\n5,6\n";
        let (names, rows) = collect(&TextFormat::Csv(CsvOptions::default()), csv, 2);
        assert_eq!(names, ["a", "b"]);
        assert_eq!(rows.len(), 2);
        let json = "[{\"a\":1},{\"a\":2,\"b\":3},{\"c\":4}]";
        let (names, rows) = collect(&TextFormat::Json, json, 2);
        assert_eq!(names, ["a", "b"]);
        assert_eq!(rows[1], (2, vec![Value::Int(2), Value::Int(3)]));
        let (names, _) = collect(&TextFormat::Json, json, usize::MAX);
        assert_eq!(names, ["a", "b", "c"]);
        let (_, rows) = collect(&TextFormat::JsonLines, "{\"a\":1}\n{\"a\":2}\n", 1);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn reads_through_a_small_buffer() {
        // Records span the reader's buffer boundaries.
        let csv = format!("h\n\"{}\nx\"\n", "y".repeat(10_000));
        let reader = std::io::BufReader::with_capacity(7, csv.as_bytes());
        let mut rows = Vec::new();
        read_rows(
            &TextFormat::Csv(CsvOptions::default()),
            reader,
            |_, line, row| {
                rows.push((line, row));
                true
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 2);
        let json = "\u{feff}[{\"a\":\"è\"}]";
        let reader = std::io::BufReader::with_capacity(3, json.as_bytes());
        let mut n = 0;
        let names = read_rows(&TextFormat::Json, reader, |_, _, _| {
            n += 1;
            true
        })
        .unwrap();
        assert_eq!((names, n), (vec!["a".to_string()], 1));
    }

    #[test]
    fn reports_bad_input() {
        let fail = |format: &TextFormat, text: &[u8]| {
            read_rows(format, text, |_, _, _| true)
                .unwrap_err()
                .to_string()
        };
        let csv = TextFormat::Csv(CsvOptions::default());
        assert!(fail(&csv, b"a\n\"x").contains("Unclosed quote"));
        assert!(fail(&csv, b"a,b\n1,2,3\n").contains("Line 2 has 3 fields"));
        assert!(fail(&csv, b"a\n\xff\xfe\n").contains("UTF-8"));
        assert!(fail(&TextFormat::Json, b"{}").contains("JSON array"));
        assert!(fail(&TextFormat::Json, b"[{}] x").contains("JSON array"));
        assert!(fail(&TextFormat::JsonLines, b"{}\n[").starts_with("Line 2"));
    }
}
