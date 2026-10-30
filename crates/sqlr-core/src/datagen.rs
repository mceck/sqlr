//! Test-data generation (spec §8.6): per-column generators and a deterministic, seedable
//! row generator. Pure: the engine feeds it foreign-key pools and writes what it makes
//! through the transfer pipeline.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::schema::ConstraintDef;
use crate::table::{TableColumn, TableRef};
use crate::value::Value;

/// Why test data could not be generated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatagenError {
    /// A unique column's domain has fewer values than the requested rows.
    #[error(
        "Could not generate {rows} distinct values for column {column}: its domain is \
         too small for that many rows"
    )]
    DomainTooSmall {
        /// The number of rows requested.
        rows: u64,
        /// The unique column.
        column: String,
    },
    /// A foreign-key column refers to a table whose pool of values is empty.
    #[error("Column {column} references {table}.{key}, which has no rows to pick from")]
    EmptyForeignKeyPool {
        /// The generated column.
        column: String,
        /// The referenced table.
        table: String,
        /// The referenced column.
        key: String,
    },
    /// A sequence or date range cannot produce another value without overflowing.
    #[error("Column {column} cannot generate another value: its range is exhausted")]
    RangeExhausted {
        /// The generated column.
        column: String,
    },
}

/// How the values of one generated column are produced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Generator {
    /// Consecutive numbers.
    Sequence {
        /// The first value.
        start: i64,
        /// The increment between rows.
        step: i64,
    },
    /// Random whole numbers in a range.
    IntRange {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
    },
    /// Random decimal numbers in a range.
    FloatRange {
        /// The lowest value.
        min: f64,
        /// The highest value.
        max: f64,
        /// Digits kept after the decimal point.
        decimals: u32,
    },
    /// Random exact decimals.
    Decimal {
        /// Total number of digits.
        precision: u32,
        /// Digits after the decimal point.
        scale: u32,
    },
    /// Random booleans.
    Bool {
        /// Share of rows that are true.
        true_ratio: f32,
    },
    /// Days since 1970-01-01.
    DateRange {
        /// First day of the range.
        from: i64,
        /// Last day of the range.
        to: i64,
    },
    /// Seconds since 1970-01-01 00:00:00 UTC.
    TimestampRange {
        /// First instant of the range.
        from: i64,
        /// Last instant of the range.
        to: i64,
    },
    /// A random UUID.
    Uuid,
    /// A first name.
    FirstName,
    /// A last name.
    LastName,
    /// A full name.
    FullName,
    /// Built from the row's name columns when it has any.
    Email,
    /// A phone number.
    Phone,
    /// A city name.
    City,
    /// A country name.
    Country,
    /// A company name.
    Company,
    /// Lorem ipsum text.
    Lorem {
        /// Number of words.
        words: u32,
    },
    /// `#` a digit, `?` a letter, `*` a letter or digit, anything else as written.
    Pattern(String),
    /// One value picked at random from a list.
    OneOf(Vec<String>),
    /// The same text in every row.
    Constant(String),
    /// A key of another table, picked from the keys read from it.
    ForeignKey {
        /// The table the keys come from.
        table: TableRef,
        /// The key column of that table.
        column: String,
    },
    /// Left to the database: the column is not written.
    Default,
}

/// How one column of a generated table gets its values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnGen {
    /// The column name.
    pub column: String,
    /// The generator for its values.
    pub generator: Generator,
    /// Share of rows that get NULL instead. Not applied to a unique column; the dialog only
    /// offers it for nullable columns.
    pub null_ratio: f32,
    /// Values must not repeat (primary and unique keys).
    pub unique: bool,
}

/// What to generate: which table, how many rows and how each column is filled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataGenSpec {
    /// The table to fill.
    pub table: TableRef,
    /// Number of rows to generate.
    pub rows: u64,
    /// Seed of the random generator: the same seed gives the same rows.
    pub seed: u64,
    /// One entry per column to write.
    pub columns: Vec<ColumnGen>,
}

const FIRST: [&str; 20] = [
    "Ada", "Alan", "Grace", "Linus", "Margaret", "Dennis", "Barbara", "Ken", "Edsger", "Radia",
    "Donald", "Frances", "Tim", "Hedy", "Guido", "Anita", "Bjarne", "Sophie", "Yukihiro", "Ines",
];
const LAST: [&str; 20] = [
    "Lovelace",
    "Turing",
    "Hopper",
    "Torvalds",
    "Hamilton",
    "Ritchie",
    "Liskov",
    "Thompson",
    "Dijkstra",
    "Perlman",
    "Knuth",
    "Allen",
    "Berners",
    "Lamarr",
    "Rossum",
    "Borg",
    "Stroustrup",
    "Wilson",
    "Matsumoto",
    "Rossi",
];
const CITIES: [&str; 12] = [
    "Rome", "Milan", "Turin", "Naples", "Florence", "Bologna", "Berlin", "Lisbon", "Madrid",
    "Paris", "Vienna", "Dublin",
];
const COUNTRIES: [&str; 10] = [
    "Italy", "Germany", "France", "Spain", "Portugal", "Austria", "Ireland", "Greece", "Poland",
    "Sweden",
];
const COMPANIES: [&str; 10] = [
    "Acme",
    "Globex",
    "Initech",
    "Umbrella",
    "Hooli",
    "Stark",
    "Wayne",
    "Wonka",
    "Cyberdyne",
    "Tyrell",
];
const WORDS: [&str; 24] = [
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "minim",
    "veniam",
    "quis",
    "nostrud",
];

/// A small xorshift64* generator: the same seed gives the same numbers on every platform.
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift.
        Self(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let span = (hi as i128 - lo as i128 + 1) as u128;
        (lo as i128 + (u128::from(self.next()) % span) as i128) as i64
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

/// Days since 1970-01-01 as `YYYY-MM-DD`; `None` when the day count overflows.
fn date_text(days: i64) -> Option<String> {
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe
        .checked_add(era.checked_mul(400)?)?
        .checked_add(i64::from(m <= 2))?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn timestamp_text(seconds: i64) -> Option<String> {
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    Some(format!(
        "{} {:02}:{:02}:{:02}",
        date_text(days)?,
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    ))
}

fn slug(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Whether a type name is an integer type, without matching `interval` or `point`.
fn is_integer_type(ty: &str) -> bool {
    let base = ty.split(['(', ' ', '[']).next().unwrap_or(ty);
    matches!(
        base,
        "int"
            | "integer"
            | "int2"
            | "int4"
            | "int8"
            | "bigint"
            | "smallint"
            | "tinyint"
            | "mediumint"
            | "serial"
            | "bigserial"
            | "smallserial"
            | "serial2"
            | "serial4"
            | "serial8"
    )
}

/// The generator that suits `column` by its name and type; a foreign key picks from its
/// target's keys, a column the database fills itself is left to it.
pub fn default_generator(column: &TableColumn, fk: Option<&ConstraintDef>) -> Generator {
    if let Some(ConstraintDef::ForeignKey {
        target,
        target_columns,
        columns,
        ..
    }) = fk
    {
        let position = columns.iter().position(|c| *c == column.name).unwrap_or(0);
        return Generator::ForeignKey {
            table: target.clone(),
            column: target_columns
                .get(position)
                .cloned()
                .unwrap_or_else(|| "id".into()),
        };
    }
    let defaulted = column.default.as_deref().is_some_and(|d| {
        let d = d.to_ascii_lowercase();
        d.contains("nextval") || d.contains("auto_increment") || d.contains("identity")
    }) || column.generated
        || column.automatic;
    if defaulted {
        return Generator::Default;
    }
    let name = column.name.to_lowercase();
    let ty = column.type_name.to_lowercase();
    let has = |s: &str| name.contains(s);
    let is_text = ty.contains("char") || ty.contains("text") || ty.contains("string");
    if has("email") {
        return Generator::Email;
    }
    if has("first_name") || has("firstname") || name == "given_name" {
        return Generator::FirstName;
    }
    if has("last_name") || has("lastname") || has("surname") || name == "family_name" {
        return Generator::LastName;
    }
    if has("phone") || has("mobile") {
        return Generator::Phone;
    }
    if is_text {
        if name == "name" || name == "full_name" || name == "fullname" {
            return Generator::FullName;
        }
        if has("city") {
            return Generator::City;
        }
        if has("country") {
            return Generator::Country;
        }
        if has("company") || has("organization") {
            return Generator::Company;
        }
    }
    if ty.contains("uuid") || has("uuid") {
        return Generator::Uuid;
    }
    let now = 1_750_000_000;
    if ty.contains("timestamp") || ty.contains("datetime") {
        return Generator::TimestampRange {
            from: now - 3 * 365 * 86_400,
            to: now,
        };
    }
    if ty == "date" {
        return Generator::DateRange {
            from: now / 86_400 - 3 * 365,
            to: now / 86_400,
        };
    }
    if ty.contains("bool") || ty == "tinyint(1)" || ty == "bit" {
        return Generator::Bool { true_ratio: 0.5 };
    }
    if is_integer_type(&ty) {
        return if name == "id" {
            Generator::Sequence { start: 1, step: 1 }
        } else {
            Generator::IntRange {
                min: 1,
                max: 100_000,
            }
        };
    }
    if ty.contains("numeric") || ty.contains("decimal") || ty.contains("money") {
        return Generator::Decimal {
            precision: 10,
            scale: 2,
        };
    }
    if ty.contains("float") || ty.contains("double") || ty.contains("real") {
        return Generator::FloatRange {
            min: 0.0,
            max: 1000.0,
            decimals: 2,
        };
    }
    Generator::Lorem { words: 3 }
}

/// Generates rows of a [`DataGenSpec`]: the same spec (seed included) gives the same rows.
pub struct RowGenerator {
    spec: DataGenSpec,
    pools: BTreeMap<String, Vec<Value>>,
    rng: Rng,
    made: u64,
    seen: Vec<HashSet<String>>,
}

/// Attempts to find a value that is not a repeat before giving up.
const UNIQUE_RETRIES: usize = 200;

impl RowGenerator {
    /// `fk_pools`: the keys of each referenced table, by `table.column`
    /// ([`RowGenerator::pool_key`]).
    pub fn new(spec: &DataGenSpec, fk_pools: BTreeMap<String, Vec<Value>>) -> Self {
        Self {
            rng: Rng::new(spec.seed),
            seen: vec![HashSet::new(); spec.columns.len()],
            spec: spec.clone(),
            pools: fk_pools,
            made: 0,
        }
    }

    /// How a foreign key's pool is named in the map given to [`RowGenerator::new`].
    pub fn pool_key(table: &TableRef, column: &str) -> String {
        format!(
            "{}.{}.{column}",
            table.schema.clone().unwrap_or_default(),
            table.name
        )
    }

    /// Values remembered to keep unique columns unique (none for a generator that cannot
    /// repeat by construction).
    pub fn remembered(&self) -> usize {
        self.seen.iter().map(HashSet::len).sum()
    }

    /// Rows made so far.
    pub fn made(&self) -> u64 {
        self.made
    }

    /// The next `n` rows (fewer at the end of the spec's row count): one value per column
    /// of the spec, `Default` columns as NULL (the caller leaves them out of the write).
    pub fn next_batch(&mut self, n: usize) -> Result<Vec<Vec<Value>>, DatagenError> {
        let remaining = self.spec.rows.saturating_sub(self.made);
        let count = (n as u64).min(remaining) as usize;
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            rows.push(self.row()?);
        }
        Ok(rows)
    }

    fn row(&mut self) -> Result<Vec<Value>, DatagenError> {
        let mut first = String::new();
        let mut last = String::new();
        let mut row = Vec::with_capacity(self.spec.columns.len());
        for i in 0..self.spec.columns.len() {
            let (generator, null_ratio, unique, name) = {
                let c = &self.spec.columns[i];
                (
                    c.generator.clone(),
                    c.null_ratio,
                    c.unique,
                    c.column.clone(),
                )
            };
            if matches!(generator, Generator::Default) {
                row.push(Value::Null);
                continue;
            }
            if !unique && null_ratio > 0.0 && (self.rng.unit() as f32) < null_ratio {
                row.push(Value::Null);
                continue;
            }
            let mut value = Value::Null;
            // A sequence with a step, or a UUID, cannot repeat: nothing to remember (a
            // remembered value per row would cost memory in proportion to the row count).
            let by_construction = matches!(&generator, Generator::Uuid)
                || matches!(&generator, Generator::Sequence { step, .. } if *step != 0);
            let unique = unique && !by_construction;
            let attempts = if unique { UNIQUE_RETRIES } else { 1 };
            for attempt in 0..attempts {
                value = self.value(&generator, &first, &last, name.as_str())?;
                if !unique {
                    break;
                }
                let key = value.display().into_owned();
                if self.seen[i].insert(key) {
                    break;
                }
                if attempt + 1 == attempts {
                    return Err(DatagenError::DomainTooSmall {
                        rows: self.spec.rows,
                        column: name,
                    });
                }
            }
            match (&generator, &value) {
                (Generator::FirstName, Value::Text(t)) => first = t.clone(),
                (Generator::LastName, Value::Text(t)) => last = t.clone(),
                (Generator::FullName, Value::Text(t)) => {
                    let mut parts = t.split_whitespace();
                    first = parts.next().unwrap_or("").to_string();
                    last = parts.next().unwrap_or("").to_string();
                }
                _ => {}
            }
            row.push(value);
        }
        self.made += 1;
        Ok(row)
    }

    fn value(
        &mut self,
        generator: &Generator,
        first: &str,
        last: &str,
        column: &str,
    ) -> Result<Value, DatagenError> {
        let r = &mut self.rng;
        Ok(match generator {
            Generator::Sequence { start, step } => {
                let exhausted = || DatagenError::RangeExhausted {
                    column: column.to_string(),
                };
                let made = i64::try_from(self.made).map_err(|_| exhausted())?;
                let value = step
                    .checked_mul(made)
                    .and_then(|offset| start.checked_add(offset))
                    .ok_or_else(exhausted)?;
                Value::Int(value)
            }
            Generator::IntRange { min, max } => Value::Int(r.range(*min, *max)),
            Generator::FloatRange { min, max, decimals } => {
                let f = min + (max - min) * r.unit();
                let scale = 10f64.powi(*decimals as i32);
                Value::Float((f * scale).round() / scale)
            }
            Generator::Decimal { precision, scale } => {
                let int_digits = precision.saturating_sub(*scale).clamp(1, 15);
                let int = r.range(0, 10i64.pow(int_digits) - 1);
                if *scale == 0 {
                    Value::Text(int.to_string())
                } else {
                    let frac = r.range(0, 10i64.pow((*scale).min(15)) - 1);
                    Value::Text(format!("{int}.{frac:0width$}", width = *scale as usize))
                }
            }
            Generator::Bool { true_ratio } => Value::Bool((r.unit() as f32) < *true_ratio),
            Generator::DateRange { from, to } => {
                Value::Text(date_text(r.range(*from, *to)).ok_or_else(|| {
                    DatagenError::RangeExhausted {
                        column: column.to_string(),
                    }
                })?)
            }
            Generator::TimestampRange { from, to } => {
                Value::Text(timestamp_text(r.range(*from, *to)).ok_or_else(|| {
                    DatagenError::RangeExhausted {
                        column: column.to_string(),
                    }
                })?)
            }
            Generator::Uuid => {
                let (a, b) = (r.next(), r.next());
                Value::Text(format!(
                    "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
                    (a >> 32) as u32,
                    (a >> 16) as u16,
                    a as u16 & 0xfff,
                    0x8000 | (b >> 48) as u16 & 0x3fff,
                    b & 0xffff_ffff_ffff
                ))
            }
            Generator::FirstName => Value::Text(r.pick(&FIRST).to_string()),
            Generator::LastName => Value::Text(r.pick(&LAST).to_string()),
            Generator::FullName => Value::Text(format!("{} {}", r.pick(&FIRST), r.pick(&LAST))),
            Generator::Email => {
                let (f, l) = if first.is_empty() {
                    (r.pick(&FIRST).to_string(), r.pick(&LAST).to_string())
                } else {
                    (first.to_string(), last.to_string())
                };
                let n = r.range(1, 9999);
                Value::Text(format!("{}.{}{n}@example.com", slug(&f), slug(&l)))
            }
            Generator::Phone => Value::Text(format!(
                "+39 3{:02} {:07}",
                r.range(10, 99),
                r.range(0, 9_999_999)
            )),
            Generator::City => Value::Text(r.pick(&CITIES).to_string()),
            Generator::Country => Value::Text(r.pick(&COUNTRIES).to_string()),
            Generator::Company => Value::Text(format!(
                "{} {}",
                r.pick(&COMPANIES),
                r.pick(&["Ltd", "Inc", "SRL", "GmbH"])
            )),
            Generator::Lorem { words } => {
                let text: Vec<&str> = (0..(*words).max(1)).map(|_| r.pick(&WORDS)).collect();
                Value::Text(text.join(" "))
            }
            Generator::Pattern(p) => Value::Text(
                p.chars()
                    .map(|c| match c {
                        '#' => char::from(b'0' + r.range(0, 9) as u8),
                        '?' => char::from(b'a' + r.range(0, 25) as u8),
                        '*' => {
                            let k = r.range(0, 35) as u8;
                            char::from(if k < 10 { b'0' + k } else { b'a' + k - 10 })
                        }
                        other => other,
                    })
                    .collect(),
            ),
            Generator::OneOf(options) => {
                if options.is_empty() {
                    Value::Null
                } else {
                    Value::Text(options[r.range(0, options.len() as i64 - 1) as usize].clone())
                }
            }
            Generator::Constant(text) => Value::Text(text.clone()),
            Generator::ForeignKey { table, column: key } => {
                let pool = self.pools.get(&Self::pool_key(table, key));
                match pool.filter(|p| !p.is_empty()) {
                    Some(pool) => pool[(r.next() % pool.len() as u64) as usize].clone(),
                    None => {
                        return Err(DatagenError::EmptyForeignKeyPool {
                            column: column.to_string(),
                            table: table.name.clone(),
                            key: key.clone(),
                        });
                    }
                }
            }
            Generator::Default => Value::Null,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, default: Option<&str>) -> TableColumn {
        TableColumn {
            automatic: false,
            name: name.into(),
            type_name: ty.into(),
            nullable: true,
            default: default.map(String::from),
            generated: false,
        }
    }

    fn spec(columns: Vec<ColumnGen>, rows: u64, seed: u64) -> DataGenSpec {
        DataGenSpec {
            table: TableRef {
                database: None,
                schema: Some("s".into()),
                name: "t".into(),
            },
            rows,
            seed,
            columns,
        }
    }

    fn colgen(column: &str, generator: Generator) -> ColumnGen {
        ColumnGen {
            column: column.into(),
            generator,
            null_ratio: 0.0,
            unique: false,
        }
    }

    #[test]
    fn same_seed_same_rows() {
        let columns = vec![
            colgen(
                "n",
                Generator::IntRange {
                    min: 0,
                    max: 1_000_000,
                },
            ),
            colgen("who", Generator::FullName),
            colgen("mail", Generator::Email),
            colgen(
                "at",
                Generator::TimestampRange {
                    from: 0,
                    to: 1_700_000_000,
                },
            ),
            colgen("u", Generator::Uuid),
        ];
        let make = |seed| {
            RowGenerator::new(&spec(columns.clone(), 50, seed), BTreeMap::new())
                .next_batch(50)
                .unwrap()
        };
        assert_eq!(make(7), make(7));
        assert_ne!(make(7), make(8));
        assert_eq!(make(7).len(), 50);
        // The email follows the row's name.
        let rows = make(7);
        let (Value::Text(who), Value::Text(mail)) = (&rows[0][1], &rows[0][2]) else {
            panic!("text values");
        };
        let first = who.split_whitespace().next().unwrap().to_lowercase();
        assert!(mail.starts_with(&first), "{who} → {mail}");
        // Batches continue where the last stopped and stop at the row count.
        let mut g = RowGenerator::new(&spec(columns.clone(), 25, 7), BTreeMap::new());
        assert_eq!(g.next_batch(20).unwrap().len(), 20);
        assert_eq!(g.next_batch(20).unwrap().len(), 5);
        assert!(g.next_batch(20).unwrap().is_empty());
    }

    #[test]
    fn defaults_by_name_and_type() {
        let g = |n: &str, t: &str, d: Option<&str>| default_generator(&col(n, t, d), None);
        assert_eq!(g("email", "text", None), Generator::Email);
        assert_eq!(g("first_name", "varchar(20)", None), Generator::FirstName);
        assert_eq!(g("name", "text", None), Generator::FullName);
        assert!(matches!(
            g("created_at", "timestamptz", None),
            Generator::TimestampRange { .. }
        ));
        assert_eq!(
            g("id", "integer", Some("nextval('t_id_seq'::regclass)")),
            Generator::Default
        );
        assert_eq!(
            g("id", "integer", None),
            Generator::Sequence { start: 1, step: 1 }
        );
        assert!(matches!(
            g("qty", "bigint", None),
            Generator::IntRange { .. }
        ));
        assert!(matches!(
            g("small", "int4", None),
            Generator::IntRange { .. }
        ));
        // `interval` and `point` contain "int" but are not integers.
        assert!(matches!(
            g("duration", "interval", None),
            Generator::Lorem { .. }
        ));
        assert!(matches!(g("where", "point", None), Generator::Lorem { .. }));
        assert!(matches!(
            g("active", "boolean", None),
            Generator::Bool { .. }
        ));
        assert!(matches!(
            g("price", "numeric(10,2)", None),
            Generator::Decimal { .. }
        ));
        assert_eq!(g("ref", "uuid", None), Generator::Uuid);
        assert!(matches!(g("notes", "text", None), Generator::Lorem { .. }));
        let fk = ConstraintDef::ForeignKey {
            name: None,
            columns: vec!["user_id".into()],
            target: TableRef {
                database: None,
                schema: Some("s".into()),
                name: "users".into(),
            },
            target_columns: vec!["id".into()],
            on_update: Default::default(),
            on_delete: Default::default(),
            deferrable: false,
            initially_deferred: false,
        };
        assert!(matches!(
            default_generator(&col("user_id", "integer", None), Some(&fk)),
            Generator::ForeignKey { column, .. } if column == "id"
        ));
    }

    #[test]
    fn extreme_ranges_report_an_error_instead_of_panicking() {
        let mut g = RowGenerator::new(
            &spec(
                vec![colgen(
                    "n",
                    Generator::Sequence {
                        start: i64::MAX,
                        step: 1,
                    },
                )],
                2,
                1,
            ),
            BTreeMap::new(),
        );
        assert!(g.next_batch(1).is_ok());
        assert!(matches!(
            g.next_batch(1),
            Err(DatagenError::RangeExhausted { .. })
        ));

        let mut g = RowGenerator::new(
            &spec(
                vec![colgen(
                    "d",
                    Generator::DateRange {
                        from: i64::MAX,
                        to: i64::MAX,
                    },
                )],
                1,
                1,
            ),
            BTreeMap::new(),
        );
        assert!(matches!(
            g.next_batch(1),
            Err(DatagenError::RangeExhausted { .. })
        ));
    }

    #[test]
    fn pattern_generator() {
        let rows = RowGenerator::new(
            &spec(
                vec![colgen("code", Generator::Pattern("AB-##-??-*".into()))],
                20,
                3,
            ),
            BTreeMap::new(),
        )
        .next_batch(20)
        .unwrap();
        for row in rows {
            let Value::Text(code) = &row[0] else {
                panic!("text");
            };
            let c: Vec<char> = code.chars().collect();
            assert_eq!(c.len(), 10, "{code}");
            assert_eq!(&code[..3], "AB-");
            assert!(c[3].is_ascii_digit() && c[4].is_ascii_digit(), "{code}");
            assert_eq!(c[5], '-');
            assert!(
                c[6].is_ascii_lowercase() && c[7].is_ascii_lowercase(),
                "{code}"
            );
            assert_eq!(c[8], '-', "{code}");
            assert!(c[9].is_ascii_alphanumeric(), "{code}");
        }
    }

    #[test]
    fn unique_retries_then_errors() {
        // 30 distinct values from a domain of 50 work; 30 from a domain of 5 cannot.
        let unique = |generator| ColumnGen {
            unique: true,
            ..colgen("k", generator)
        };
        let ok = RowGenerator::new(
            &spec(vec![unique(Generator::IntRange { min: 1, max: 50 })], 30, 1),
            BTreeMap::new(),
        )
        .next_batch(30)
        .unwrap();
        let distinct: HashSet<String> = ok.iter().map(|r| r[0].display().into_owned()).collect();
        assert_eq!(distinct.len(), 30);
        let err = RowGenerator::new(
            &spec(vec![unique(Generator::IntRange { min: 1, max: 5 })], 30, 1),
            BTreeMap::new(),
        )
        .next_batch(30)
        .unwrap_err();
        assert!(err.to_string().contains("domain is too small"), "{err}");
        assert_eq!(
            err.to_string(),
            "Could not generate 30 distinct values for column k: its domain is too small for that many rows"
        );
    }

    #[test]
    fn unique_by_construction_costs_no_memory() {
        let unique = |generator| ColumnGen {
            unique: true,
            ..colgen("k", generator)
        };
        let mut g = RowGenerator::new(
            &spec(
                vec![
                    unique(Generator::Sequence { start: 1, step: 1 }),
                    unique(Generator::Uuid),
                ],
                5000,
                9,
            ),
            BTreeMap::new(),
        );
        assert_eq!(g.next_batch(5000).unwrap().len(), 5000);
        assert_eq!(g.remembered(), 0);
        // A random range has to remember what it gave out.
        let mut g = RowGenerator::new(
            &spec(
                vec![unique(Generator::IntRange {
                    min: 1,
                    max: 1_000_000,
                })],
                100,
                9,
            ),
            BTreeMap::new(),
        );
        g.next_batch(100).unwrap();
        assert_eq!(g.remembered(), 100);
    }

    #[test]
    fn null_ratio_and_foreign_pools() {
        let table = TableRef {
            database: None,
            schema: Some("s".into()),
            name: "users".into(),
        };
        let mut pools = BTreeMap::new();
        pools.insert(
            RowGenerator::pool_key(&table, "id"),
            (1..=3).map(Value::Int).collect(),
        );
        let columns = vec![
            ColumnGen {
                null_ratio: 0.5,
                ..colgen("n", Generator::IntRange { min: 1, max: 9 })
            },
            colgen(
                "user_id",
                Generator::ForeignKey {
                    table,
                    column: "id".into(),
                },
            ),
        ];
        let rows = RowGenerator::new(&spec(columns.clone(), 2000, 5), pools)
            .next_batch(2000)
            .unwrap();
        let nulls = rows.iter().filter(|r| r[0].is_null()).count();
        assert!((800..1200).contains(&nulls), "{nulls} of 2000 near half");
        assert!(rows.iter().all(|r| matches!(&r[1], Value::Int(1..=3))));
        // An empty referenced table is a clear error.
        let err = RowGenerator::new(&spec(columns, 5, 5), BTreeMap::new())
            .next_batch(5)
            .unwrap_err();
        assert!(err.to_string().contains("no rows to pick from"), "{err}");
    }
}
