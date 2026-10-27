//! The dialect over the wire: data, not code (spec §6.2). A driver describes its quoting,
//! keywords, placeholders and transaction statements in a [`DialectSpec`]; the client
//! builds a [`Dialect`] from it. What the spec cannot say falls back to the ANSI defaults
//! with the declared quote character.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sqlr_core::{Dialect, Node, NodeKind, NodePath, SqlSyntax};

/// How a row limit is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimitStyle {
    /// `… LIMIT n`
    Limit,
    /// `… FETCH FIRST n ROWS ONLY`
    FetchFirst,
    /// `SELECT TOP n * …`
    Top,
    /// No limit clause.
    None,
}

/// How the placeholder of a bound parameter is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaceholderStyle {
    /// `?`
    Question,
    /// `$1`, `$2`, …
    Dollar,
    /// `:1`, `:2`, …
    Colon,
}

/// The lexical rules of a dialect on the wire: the same data as [`SqlSyntax`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyntaxSpec {
    /// The keywords.
    pub keywords: Vec<String>,
    /// The reserved words, which identifiers must be quoted to use.
    pub reserved: Vec<String>,
    /// The function names.
    pub functions: Vec<String>,
    /// `$tag$ … $tag$` strings (PostgreSQL).
    pub dollar_quotes: bool,
    /// Nested block comments (PostgreSQL).
    pub nested_comments: bool,
    /// `E'…'` strings with backslash escapes (PostgreSQL).
    pub escape_strings: bool,
    /// The text that starts a line comment.
    pub line_comment: String,
    /// The line comment needs whitespace after it (MySQL `-- `).
    pub line_comment_needs_space: bool,
    /// `#` starts a line comment (MySQL).
    pub hash_comments: bool,
    /// Identifier quotes as `(open, close)`; the first pair quotes names.
    pub ident_quotes: Vec<(u8, u8)>,
    /// `"…"` is a string literal, not an identifier (MySQL).
    pub double_quote_strings: bool,
    /// Every string literal takes backslash escapes (MySQL).
    pub backslash_escapes: bool,
    /// The tree-sitter grammar id.
    pub grammar: String,
}

/// Everything a client needs to build a [`Dialect`] for a driver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DialectSpec {
    /// The lexical rules.
    pub syntax: SyntaxSpec,
    /// The schemas searched for an unqualified name, in order.
    pub default_schemas: Vec<String>,
    /// Databases and schemas are the same level (MySQL).
    pub database_is_schema: bool,
    /// How placeholders of bound parameters are written.
    pub placeholder: PlaceholderStyle,
    /// How a row limit is written.
    pub limit: LimitStyle,
    /// Which segments of a node's path name the relation in `SELECT * FROM …`, in order;
    /// empty means every segment.
    pub select_segments: Vec<usize>,
    /// The relation is named by the node's own name, not by its path.
    #[serde(default)]
    pub select_uses_name: bool,
    /// The statement that starts a transaction.
    pub begin_sql: String,
    /// The statement that turns read-only mode on, if the server has one.
    pub read_only_on: Option<String>,
    /// The statement that turns read-only mode off, if the server has one.
    pub read_only_off: Option<String>,
    /// The server commits an open transaction before running DDL.
    pub ddl_commits_transaction: bool,
    /// A failed statement aborts the open transaction until it is rolled back.
    pub error_aborts_transaction: bool,
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

impl SyntaxSpec {
    /// The spec that describes `s`.
    pub fn from_syntax(s: &SqlSyntax) -> Self {
        let owned = |list: &[&'static str]| list.iter().map(|w| w.to_string()).collect();
        Self {
            keywords: owned(&s.keywords),
            reserved: owned(&s.reserved),
            functions: owned(&s.functions),
            dollar_quotes: s.dollar_quotes,
            nested_comments: s.nested_comments,
            escape_strings: s.escape_strings,
            line_comment: s.line_comment.to_string(),
            line_comment_needs_space: s.line_comment_needs_space,
            hash_comments: s.hash_comments,
            ident_quotes: s.ident_quotes.to_vec(),
            double_quote_strings: s.double_quote_strings,
            backslash_escapes: s.backslash_escapes,
            grammar: s.grammar.to_string(),
        }
    }

    /// The syntax; strings are leaked once per distinct spec (a handful per run).
    pub fn to_syntax(&self) -> Arc<SqlSyntax> {
        static CACHE: LazyLock<Mutex<HashMap<String, Arc<SqlSyntax>>>> =
            LazyLock::new(Mutex::default);
        let key = format!("{self:?}");
        if let Some(found) = CACHE.lock().get(&key) {
            return found.clone();
        }
        let list = |words: &[String]| -> Vec<&'static str> {
            let mut v: Vec<&'static str> = words.iter().map(|w| leak(w)).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let quotes: &'static [(u8, u8)] = Box::leak(self.ident_quotes.clone().into_boxed_slice());
        let syntax = Arc::new(SqlSyntax {
            keywords: list(&self.keywords),
            reserved: list(&self.reserved),
            functions: list(&self.functions),
            dollar_quotes: self.dollar_quotes,
            nested_comments: self.nested_comments,
            escape_strings: self.escape_strings,
            line_comment: leak(&self.line_comment),
            line_comment_needs_space: self.line_comment_needs_space,
            hash_comments: self.hash_comments,
            ident_quotes: quotes,
            double_quote_strings: self.double_quote_strings,
            backslash_escapes: self.backslash_escapes,
            grammar: leak(&self.grammar),
        });
        CACHE.lock().insert(key, syntax.clone());
        syntax
    }
}

impl DialectSpec {
    /// Describes `dialect` as data. What the trait cannot report (limit syntax, which path
    /// segments name a relation) is read off a probe node.
    pub fn from_dialect(dialect: &dyn Dialect) -> Self {
        let placeholder = match (
            dialect.placeholder(1).as_str(),
            dialect.placeholder(2).as_str(),
        ) {
            ("$1", "$2") => PlaceholderStyle::Dollar,
            (":1", ":2") => PlaceholderStyle::Colon,
            _ => PlaceholderStyle::Question,
        };
        let probe = Node {
            path: NodePath(vec!["p0aaa".into(), "p1bbb".into(), "p2ccc".into()]),
            name: "pnname".into(),
            kind: NodeKind::Table,
            has_children: true,
            detail: None,
        };
        let (limit, select_segments, select_uses_name) = match dialect.select_all(&probe, Some(7)) {
            Some(sql) => {
                let limit = if sql.contains("LIMIT 7") {
                    LimitStyle::Limit
                } else if sql.contains("FETCH FIRST 7") {
                    LimitStyle::FetchFirst
                } else if sql.contains("TOP 7") {
                    LimitStyle::Top
                } else {
                    LimitStyle::None
                };
                let at = |needle: &str| sql.find(needle);
                let mut segments: Vec<(usize, usize)> = ["p0aaa", "p1bbb", "p2ccc"]
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| at(n).map(|pos| (pos, i)))
                    .collect();
                segments.sort_unstable();
                let indices: Vec<usize> = segments.into_iter().map(|(_, i)| i).collect();
                let uses_name = indices.is_empty() && sql.contains("pnname");
                (
                    limit,
                    if indices.len() == 3 {
                        Vec::new()
                    } else {
                        indices
                    },
                    uses_name,
                )
            }
            None => (LimitStyle::Limit, Vec::new(), false),
        };
        Self {
            syntax: SyntaxSpec::from_syntax(&dialect.syntax()),
            default_schemas: dialect.default_schemas(),
            database_is_schema: dialect.database_is_schema(),
            placeholder,
            limit,
            select_segments,
            select_uses_name,
            begin_sql: dialect.begin_sql().to_string(),
            read_only_on: dialect.read_only_sql(true),
            read_only_off: dialect.read_only_sql(false),
            ddl_commits_transaction: dialect.ddl_commits_transaction(),
            error_aborts_transaction: dialect.error_aborts_transaction(),
        }
    }
}

/// A [`Dialect`] built from a driver's [`DialectSpec`].
pub struct SidecarDialect {
    spec: DialectSpec,
    syntax: Arc<SqlSyntax>,
    begin: &'static str,
}

impl SidecarDialect {
    /// Builds the dialect from `spec`.
    pub fn new(spec: DialectSpec) -> Self {
        let syntax = spec.syntax.to_syntax();
        let begin = leak(&spec.begin_sql);
        Self {
            spec,
            syntax,
            begin,
        }
    }
}

impl Dialect for SidecarDialect {
    fn quote_ident(&self, ident: &str) -> String {
        let (open, close) = self
            .syntax
            .ident_quotes
            .first()
            .map_or(('"', '"'), |&(o, c)| (o as char, c as char));
        format!(
            "{open}{}{close}",
            ident.replace(close, &format!("{close}{close}"))
        )
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let segments = node.path.segments();
        let named: Vec<&String> = if self.spec.select_uses_name {
            vec![&node.name]
        } else if self.spec.select_segments.is_empty() {
            segments.iter().collect()
        } else {
            self.spec
                .select_segments
                .iter()
                .filter_map(|&i| segments.get(i))
                .collect()
        };
        let name = named
            .iter()
            .map(|s| self.quote_ident(s))
            .collect::<Vec<_>>()
            .join(".");
        Some(match (limit, self.spec.limit) {
            (Some(n), LimitStyle::Limit) => format!("SELECT * FROM {name} LIMIT {n}"),
            (Some(n), LimitStyle::FetchFirst) => {
                format!("SELECT * FROM {name} FETCH FIRST {n} ROWS ONLY")
            }
            (Some(n), LimitStyle::Top) => format!("SELECT TOP {n} * FROM {name}"),
            _ => format!("SELECT * FROM {name}"),
        })
    }

    fn syntax(&self) -> Arc<SqlSyntax> {
        self.syntax.clone()
    }

    fn default_schemas(&self) -> Vec<String> {
        self.spec.default_schemas.clone()
    }

    fn database_is_schema(&self) -> bool {
        self.spec.database_is_schema
    }

    fn placeholder(&self, index: usize) -> String {
        match self.spec.placeholder {
            PlaceholderStyle::Question => "?".to_string(),
            PlaceholderStyle::Dollar => format!("${index}"),
            PlaceholderStyle::Colon => format!(":{index}"),
        }
    }

    fn begin_sql(&self) -> &'static str {
        self.begin
    }

    fn read_only_sql(&self, on: bool) -> Option<String> {
        if on {
            self.spec.read_only_on.clone()
        } else {
            self.spec.read_only_off.clone()
        }
    }

    fn ddl_commits_transaction(&self) -> bool {
        self.spec.ddl_commits_transaction
    }

    fn error_aborts_transaction(&self) -> bool {
        self.spec.error_aborts_transaction
    }
}
