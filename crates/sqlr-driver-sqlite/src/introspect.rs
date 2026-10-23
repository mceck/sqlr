//! Object definitions parsed from the `CREATE` statements SQLite keeps in `sqlite_schema`.
//! SQLite has no catalog of constraints, collations or generated expressions: the
//! statement text is the definition. Column constraints are read as table constraints.

use rusqlite::{Connection, OptionalExtension};
use sqlr_core::{
    ColumnDef, ConstraintDef, DependentDef, Dialect, DriverError, ErrorKind, FkAction, Identity,
    IndexColumn, IndexDef, ObjectDef, ObjectKind, ObjectRef, TableDef, TableRef, TriggerDef,
    ViewDef,
};

use crate::dialect::SqliteDialect;
use crate::map_error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A bare word or a quoted identifier.
    Word,
    Quoted,
    Str,
    Num,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

/// Splits `sql` into tokens, skipping whitespace and comments.
fn tokenize(sql: &str) -> Vec<Tok> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let quoted_end = |i: usize, close: u8| {
        // Past the closing quote; a doubled quote is part of the text.
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == close {
                if close != b']' && b.get(j + 1) == Some(&close) {
                    j += 2;
                    continue;
                }
                return j + 1;
            }
            j += 1;
        }
        b.len()
    };
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
        } else if c == b'\'' {
            let end = quoted_end(i, b'\'');
            out.push(Tok {
                kind: Kind::Str,
                start: i,
                end,
            });
            i = end;
        } else if matches!(c, b'x' | b'X') && b.get(i + 1) == Some(&b'\'') {
            let end = quoted_end(i + 1, b'\'');
            out.push(Tok {
                kind: Kind::Str,
                start: i,
                end,
            });
            i = end;
        } else if matches!(c, b'"' | b'`' | b'[') {
            let close = if c == b'[' { b']' } else { c };
            let end = quoted_end(i, close);
            out.push(Tok {
                kind: Kind::Quoted,
                start: i,
                end,
            });
            i = end;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let mut j = i + 1;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric()
                    || b[j] == b'.'
                    || (matches!(b[j], b'+' | b'-') && matches!(b[j - 1], b'e' | b'E')))
            {
                j += 1;
            }
            out.push(Tok {
                kind: Kind::Num,
                start: i,
                end: j,
            });
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let mut j = i + 1;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] >= 0x80)
            {
                j += 1;
            }
            out.push(Tok {
                kind: Kind::Word,
                start: i,
                end: j,
            });
            i = j;
        } else {
            // One character, or a UTF-8 sequence that is not a word (never in practice).
            let len = sql[i..].chars().next().map_or(1, char::len_utf8);
            out.push(Tok {
                kind: Kind::Punct,
                start: i,
                end: i + len,
            });
            i += len;
        }
    }
    out
}

/// A cursor over the tokens of one statement.
struct Parser<'a> {
    sql: &'a str,
    toks: Vec<Tok>,
    pos: usize,
}

fn syntax(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

impl<'a> Parser<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            toks: tokenize(sql),
            pos: 0,
        }
    }

    fn text(&self, t: Tok) -> &'a str {
        &self.sql[t.start..t.end]
    }

    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// True when the next token is the keyword `word` (bare, any case).
    fn is(&self, word: &str) -> bool {
        self.peek()
            .is_some_and(|t| t.kind == Kind::Word && self.text(t).eq_ignore_ascii_case(word))
    }

    fn is_punct(&self, c: &str) -> bool {
        self.peek()
            .is_some_and(|t| t.kind == Kind::Punct && self.text(t) == c)
    }

    fn eat(&mut self, word: &str) -> bool {
        let yes = self.is(word);
        if yes {
            self.pos += 1;
        }
        yes
    }

    fn eat_punct(&mut self, c: &str) -> bool {
        let yes = self.is_punct(c);
        if yes {
            self.pos += 1;
        }
        yes
    }

    fn expect(&mut self, word: &str) -> Result<(), DriverError> {
        if self.eat(word) {
            Ok(())
        } else {
            Err(syntax(format!("expected {word} in: {}", self.sql)))
        }
    }

    fn expect_punct(&mut self, c: &str) -> Result<(), DriverError> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            Err(syntax(format!("expected `{c}` in: {}", self.sql)))
        }
    }

    fn next(&mut self) -> Result<Tok, DriverError> {
        let t = self
            .peek()
            .ok_or_else(|| syntax(format!("unexpected end of: {}", self.sql)))?;
        self.pos += 1;
        Ok(t)
    }

    /// An identifier: a bare word, a quoted name or a string.
    fn name(&mut self) -> Result<String, DriverError> {
        let t = self.next()?;
        let text = self.text(t);
        Ok(match t.kind {
            Kind::Word => text.to_string(),
            Kind::Quoted | Kind::Str => {
                let inner = &text[1..text.len() - 1];
                match text.as_bytes()[0] {
                    b'"' => inner.replace("\"\"", "\""),
                    b'`' => inner.replace("``", "`"),
                    b'\'' => inner.replace("''", "'"),
                    _ => inner.to_string(),
                }
            }
            _ => return Err(syntax(format!("expected a name, found `{text}`"))),
        })
    }

    /// `[schema .] name`, keeping the name.
    fn qualified_name(&mut self) -> Result<String, DriverError> {
        let first = self.name()?;
        if self.eat_punct(".") {
            self.name()
        } else {
            Ok(first)
        }
    }

    /// Skips a balanced group starting at `(`; returns the source between the parentheses.
    fn group(&mut self) -> Result<&'a str, DriverError> {
        let open = self.next()?;
        if self.text(open) != "(" {
            return Err(syntax(format!("expected `(` in: {}", self.sql)));
        }
        let mut depth = 1;
        while depth > 0 {
            let t = self.next()?;
            match (t.kind, self.text(t)) {
                (Kind::Punct, "(") => depth += 1,
                (Kind::Punct, ")") => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                return Ok(self.sql[open.end..t.start].trim());
            }
        }
        unreachable!()
    }

    /// Source from token `from` to the token before the cursor.
    fn slice(&self, from: usize) -> &'a str {
        if from >= self.pos {
            return "";
        }
        &self.sql[self.toks[from].start..self.toks[self.pos - 1].end]
    }

    /// Advances to the next top-level `,` or `)` (not consumed), skipping groups.
    fn skip_to_separator(&mut self) -> Result<(), DriverError> {
        while !self.at_end() && !self.is_punct(",") && !self.is_punct(")") {
            if self.is_punct("(") {
                self.group()?;
            } else {
                self.pos += 1;
            }
        }
        Ok(())
    }

    /// Names in a parenthesized list (`(a, "b" COLLATE x DESC)`), ignoring order and
    /// collation.
    fn name_list(&mut self) -> Result<Vec<String>, DriverError> {
        self.expect_punct("(")?;
        let mut names = Vec::new();
        loop {
            names.push(self.name()?);
            while !self.is_punct(",") && !self.is_punct(")") {
                self.next()?;
            }
            if self.eat_punct(")") {
                return Ok(names);
            }
            self.expect_punct(",")?;
        }
    }

    /// True when the token after the next one is the keyword `word`.
    fn second_is(&self, word: &str) -> bool {
        self.toks
            .get(self.pos + 1)
            .is_some_and(|t| t.kind == Kind::Word && self.text(*t).eq_ignore_ascii_case(word))
    }

    /// `ON CONFLICT <resolution>` after a key or NOT NULL.
    fn skip_conflict(&mut self) {
        if self.is("ON") && self.second_is("CONFLICT") {
            self.pos += 3;
        }
    }

    fn fk_action(&mut self) -> Result<FkAction, DriverError> {
        let start = self.pos;
        // `SET NULL`, `SET DEFAULT` and `NO ACTION` are two words.
        let two_words = self.is("SET") || self.is("NO");
        self.next()?;
        if two_words {
            self.next()?;
        }
        let text = self.slice(start);
        FkAction::parse(text).ok_or_else(|| syntax(format!("unknown action `{text}`")))
    }

    /// `REFERENCES table [(cols)] [ON DELETE|UPDATE action | MATCH name]* [deferrable]`.
    fn references(
        &mut self,
        name: Option<String>,
        columns: Vec<String>,
        schema: &str,
    ) -> Result<ConstraintDef, DriverError> {
        self.expect("REFERENCES")?;
        let target = self.name()?;
        let target_columns = if self.is_punct("(") {
            self.name_list()?
        } else {
            Vec::new()
        };
        let (mut on_update, mut on_delete) = (FkAction::NoAction, FkAction::NoAction);
        let (mut deferrable, mut initially_deferred) = (false, false);
        loop {
            if self.eat("ON") {
                if self.eat("DELETE") {
                    on_delete = self.fk_action()?;
                } else {
                    self.expect("UPDATE")?;
                    on_update = self.fk_action()?;
                }
            } else if self.eat("MATCH") {
                self.next()?;
            } else if self.is("DEFERRABLE") || (self.is("NOT") && self.second_is("DEFERRABLE")) {
                let not = self.eat("NOT");
                self.expect("DEFERRABLE")?;
                let mut deferred = false;
                if self.eat("INITIALLY") {
                    deferred = self.eat("DEFERRED");
                    if !deferred {
                        self.expect("IMMEDIATE")?;
                    }
                }
                deferrable = !not;
                initially_deferred = !not && deferred;
            } else {
                break;
            }
        }
        Ok(ConstraintDef::ForeignKey {
            name,
            columns,
            target: table_ref(schema, &target),
            target_columns,
            on_update,
            on_delete,
            deferrable,
            initially_deferred,
        })
    }
}

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.to_string()),
        name: name.to_string(),
    }
}

/// Words that end a column's type and start its constraints.
const COLUMN_CONSTRAINTS: [&str; 11] = [
    "CONSTRAINT",
    "PRIMARY",
    "NOT",
    "NULL",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "COLLATE",
    "REFERENCES",
    "GENERATED",
    "AS",
];

/// Parses `CREATE TABLE` into a definition (indexes and triggers come separately).
pub(crate) fn parse_table(sql: &str, schema: &str) -> Result<TableDef, DriverError> {
    let mut p = Parser::new(sql);
    p.expect("CREATE")?;
    if p.is("VIRTUAL") {
        return Err(DriverError::new(
            ErrorKind::Unsupported,
            "Virtual tables cannot be edited: their columns come from the module",
        ));
    }
    if !p.eat("TEMP") {
        p.eat("TEMPORARY");
    }
    p.expect("TABLE")?;
    if p.eat("IF") {
        p.expect("NOT")?;
        p.expect("EXISTS")?;
    }
    let name = p.qualified_name()?;
    let mut def = TableDef::new(table_ref(schema, &name));
    let mut primary: Option<ConstraintDef> = None;
    let mut others: Vec<ConstraintDef> = Vec::new();
    p.expect_punct("(")?;
    loop {
        let mut constraint_name: Option<String> = None;
        if p.eat("CONSTRAINT") {
            constraint_name = Some(p.name()?);
        }
        if p.is("PRIMARY") || p.is("UNIQUE") || p.is("CHECK") || p.is("FOREIGN") {
            // A table constraint.
            if p.eat("PRIMARY") {
                p.expect("KEY")?;
                let columns = p.name_list()?;
                p.skip_conflict();
                primary = Some(ConstraintDef::PrimaryKey {
                    name: constraint_name,
                    columns,
                });
            } else if p.eat("UNIQUE") {
                let columns = p.name_list()?;
                p.skip_conflict();
                others.push(ConstraintDef::Unique {
                    name: constraint_name,
                    columns,
                });
            } else if p.eat("CHECK") {
                let expression = p.group()?.to_string();
                others.push(ConstraintDef::Check {
                    name: constraint_name,
                    expression,
                });
            } else {
                p.expect("FOREIGN")?;
                p.expect("KEY")?;
                let columns = p.name_list()?;
                others.push(p.references(constraint_name, columns, schema)?);
            }
        } else {
            let column = parse_column(&mut p, schema, &mut primary, &mut others)?;
            def.columns.push(column);
        }
        p.skip_to_separator()?;
        if p.eat_punct(")") {
            break;
        }
        p.expect_punct(",")?;
    }
    while !p.at_end() {
        if p.eat("WITHOUT") {
            p.expect("ROWID")?;
            def.options.push(("without_rowid".into(), "true".into()));
        } else if p.eat("STRICT") {
            def.options.push(("strict".into(), "true".into()));
        } else {
            p.next()?;
        }
    }
    def.constraints = primary.into_iter().chain(others).collect();
    Ok(def)
}

fn parse_column(
    p: &mut Parser,
    schema: &str,
    primary: &mut Option<ConstraintDef>,
    others: &mut Vec<ConstraintDef>,
) -> Result<ColumnDef, DriverError> {
    let name = p.name()?;
    let type_start = p.pos;
    while !p.at_end()
        && !p.is_punct(",")
        && !p.is_punct(")")
        && !COLUMN_CONSTRAINTS.iter().any(|w| p.is(w))
    {
        if p.is_punct("(") {
            p.group()?;
        } else {
            p.pos += 1;
        }
    }
    let mut column = ColumnDef {
        type_name: p.slice(type_start).to_string(),
        name,
        nullable: true,
        ..Default::default()
    };
    let mut constraint_name: Option<String> = None;
    loop {
        if p.eat("CONSTRAINT") {
            constraint_name = Some(p.name()?);
            continue;
        }
        let name = constraint_name.take();
        if p.eat("PRIMARY") {
            p.expect("KEY")?;
            if !p.eat("ASC") {
                p.eat("DESC");
            }
            p.skip_conflict();
            if p.eat("AUTOINCREMENT") {
                column.identity = Some(Identity::AutoIncrement);
            }
            *primary = Some(ConstraintDef::PrimaryKey {
                name,
                columns: vec![column.name.clone()],
            });
        } else if p.eat("NOT") {
            p.expect("NULL")?;
            p.skip_conflict();
            column.nullable = false;
        } else if p.eat("NULL") {
            p.skip_conflict();
        } else if p.eat("UNIQUE") {
            p.skip_conflict();
            others.push(ConstraintDef::Unique {
                name,
                columns: vec![column.name.clone()],
            });
        } else if p.eat("CHECK") {
            others.push(ConstraintDef::Check {
                name,
                expression: p.group()?.to_string(),
            });
        } else if p.eat("DEFAULT") {
            let start = p.pos;
            if p.is_punct("(") {
                p.group()?;
            } else {
                if p.is_punct("-") || p.is_punct("+") {
                    p.pos += 1;
                }
                p.next()?;
            }
            column.default = Some(p.slice(start).to_string());
        } else if p.eat("COLLATE") {
            column.collation = Some(p.name()?);
        } else if p.is("REFERENCES") {
            others.push(p.references(name, vec![column.name.clone()], schema)?);
        } else if p.is("GENERATED") || p.is("AS") {
            if p.eat("GENERATED") {
                p.expect("ALWAYS")?;
            }
            p.expect("AS")?;
            column.generated = Some(p.group()?.to_string());
            column.generated_stored = p.eat("STORED");
            if !column.generated_stored {
                p.eat("VIRTUAL");
            }
        } else {
            return Ok(column);
        }
    }
}

/// True when `text` is one parenthesized group (`(a + b)`, not `(a) + (b)`).
fn is_one_group(text: &str) -> bool {
    let mut p = Parser::new(text);
    p.is_punct("(") && p.group().is_ok() && p.at_end()
}

/// Parses `CREATE [UNIQUE] INDEX … ON table (columns) [WHERE predicate]`.
pub(crate) fn parse_index(sql: &str) -> Result<IndexDef, DriverError> {
    let mut p = Parser::new(sql);
    p.expect("CREATE")?;
    let unique = p.eat("UNIQUE");
    p.expect("INDEX")?;
    if p.eat("IF") {
        p.expect("NOT")?;
        p.expect("EXISTS")?;
    }
    let name = p.qualified_name()?;
    p.expect("ON")?;
    p.name()?;
    p.expect_punct("(")?;
    let mut columns = Vec::new();
    loop {
        let start = p.pos;
        // The key's expression ends at `,`, `)`, or an ASC or DESC after it: as its first
        // token, `asc` and `desc` are column names.
        while !p.is_punct(",")
            && !p.is_punct(")")
            && !(p.pos > start && (p.is("ASC") || p.is("DESC")))
        {
            if p.is_punct("(") {
                p.group()?;
            } else {
                p.next()?;
            }
        }
        let text = p.slice(start);
        let single = p.pos == start + 1 && matches!(p.toks[start].kind, Kind::Word | Kind::Quoted);
        let expr = if single {
            Parser::new(text).name()?
        } else if is_one_group(text) {
            text.to_string()
        } else {
            format!("({text})")
        };
        let desc = p.eat("DESC");
        if !desc {
            p.eat("ASC");
        }
        columns.push(IndexColumn {
            expr,
            expression: !single,
            desc,
            prefix: None,
        });
        if p.eat_punct(")") {
            break;
        }
        p.expect_punct(",")?;
    }
    let predicate = if p.eat("WHERE") {
        let start = p.pos;
        p.pos = p.toks.len();
        Some(p.slice(start).to_string())
    } else {
        None
    };
    Ok(IndexDef {
        name,
        columns,
        unique,
        method: None,
        predicate,
        include: Vec::new(),
        definition: None,
    })
}

/// Parses `CREATE TRIGGER … BEGIN … END`.
pub(crate) fn parse_trigger(sql: &str, schema: &str) -> Result<TriggerDef, DriverError> {
    let mut p = Parser::new(sql);
    p.expect("CREATE")?;
    if !p.eat("TEMP") {
        p.eat("TEMPORARY");
    }
    p.expect("TRIGGER")?;
    if p.eat("IF") {
        p.expect("NOT")?;
        p.expect("EXISTS")?;
    }
    let name = p.qualified_name()?;
    let timing = if p.eat("BEFORE") {
        "BEFORE"
    } else if p.eat("AFTER") {
        "AFTER"
    } else if p.eat("INSTEAD") {
        p.expect("OF")?;
        "INSTEAD OF"
    } else {
        "BEFORE"
    };
    let start = p.pos;
    while !p.is("ON") {
        p.next()?;
    }
    let event = p.slice(start).to_string();
    p.expect("ON")?;
    let table = p.name()?;
    if p.eat("FOR") {
        p.expect("EACH")?;
        p.expect("ROW")?;
    }
    let condition = if p.eat("WHEN") {
        let start = p.pos;
        while !p.is("BEGIN") {
            if p.is_punct("(") {
                p.group()?;
            } else {
                p.next()?;
            }
        }
        Some(p.slice(start).to_string())
    } else {
        None
    };
    let begin = p.next()?;
    let end = p
        .toks
        .iter()
        .rev()
        .find(|t| t.kind == Kind::Word && p.text(**t).eq_ignore_ascii_case("END"))
        .ok_or_else(|| syntax(format!("no END in: {sql}")))?;
    Ok(TriggerDef {
        name,
        table: table_ref(schema, &table),
        timing: timing.to_string(),
        events: vec![event],
        for_each_row: true,
        condition,
        body: sql[begin.end..end.start].trim().to_string(),
    })
}

/// Parses `CREATE VIEW name [(columns)] AS query`.
pub(crate) fn parse_view(sql: &str, schema: &str) -> Result<ViewDef, DriverError> {
    let mut p = Parser::new(sql);
    p.expect("CREATE")?;
    if !p.eat("TEMP") {
        p.eat("TEMPORARY");
    }
    p.expect("VIEW")?;
    if p.eat("IF") {
        p.expect("NOT")?;
        p.expect("EXISTS")?;
    }
    let name = p.qualified_name()?;
    let columns = if p.is_punct("(") {
        p.name_list()?
    } else {
        Vec::new()
    };
    let as_ = p.next()?;
    if !p.text(as_).eq_ignore_ascii_case("AS") {
        return Err(syntax(format!("expected AS in: {sql}")));
    }
    Ok(ViewDef {
        view: table_ref(schema, &name),
        query: sql[as_.end..].trim().to_string(),
        materialized: false,
        columns,
        comment: None,
        options: Vec::new(),
        indexes: Vec::new(),
    })
}

/// The `sql` of `name` (of `ty`) in `schema`.
fn schema_sql(
    conn: &Connection,
    schema: &str,
    ty: &str,
    name: &str,
) -> Result<Option<String>, DriverError> {
    let q = SqliteDialect.quote_ident(schema);
    let sql: Option<Option<String>> = conn
        .query_row(
            &format!("SELECT sql FROM {q}.sqlite_schema WHERE type = ?1 AND name = ?2"),
            [ty, name],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_error)?;
    Ok(sql.flatten())
}

/// `sql` of the indexes (`ty = "index"`) or triggers of `table`, by name.
fn children_sql(
    conn: &Connection,
    schema: &str,
    ty: &str,
    table: &str,
) -> Result<Vec<String>, DriverError> {
    let q = SqliteDialect.quote_ident(schema);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT sql FROM {q}.sqlite_schema WHERE type = ?1 AND tbl_name = ?2 \
             AND sql IS NOT NULL ORDER BY name"
        ))
        .map_err(map_error)?;
    let rows = stmt
        .query_map([ty, table], |r| r.get(0))
        .map_err(map_error)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(map_error)?;
    Ok(rows)
}

fn not_found(what: &str, name: &str) -> DriverError {
    DriverError::new(ErrorKind::Other, format!("{what} \"{name}\" not found"))
}

pub(crate) fn load_object(conn: &Connection, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
    let schema = object.schema.as_deref().unwrap_or("main");
    let name = object.name.as_str();
    match object.kind {
        ObjectKind::Table => {
            let sql =
                schema_sql(conn, schema, "table", name)?.ok_or_else(|| not_found("table", name))?;
            let mut def = parse_table(&sql, schema)?;
            def.table.name = name.to_string();
            def.indexes = children_sql(conn, schema, "index", name)?
                .iter()
                .map(|s| parse_index(s))
                .collect::<Result<_, _>>()?;
            def.triggers = children_sql(conn, schema, "trigger", name)?
                .iter()
                .map(|s| parse_trigger(s, schema))
                .collect::<Result<_, _>>()?;
            def.dependents = dependents(conn, schema, name)?;
            Ok(ObjectDef::Table(def))
        }
        ObjectKind::View => {
            let sql =
                schema_sql(conn, schema, "view", name)?.ok_or_else(|| not_found("view", name))?;
            let mut view = parse_view(&sql, schema)?;
            view.view.name = name.to_string();
            if view.columns.is_empty() {
                view.columns = view_columns(conn, schema, name);
            }
            Ok(ObjectDef::View(view))
        }
        ObjectKind::Index => {
            let sql = schema_sql(conn, schema, "index", name)?.ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("index \"{name}\" not found, or it belongs to a constraint"),
                )
            })?;
            let table: String = conn
                .query_row(
                    &format!(
                        "SELECT tbl_name FROM {}.sqlite_schema WHERE type = 'index' AND name = ?1",
                        SqliteDialect.quote_ident(schema)
                    ),
                    [name],
                    |r| r.get(0),
                )
                .map_err(map_error)?;
            let mut index = parse_index(&sql)?;
            index.name = name.to_string();
            Ok(ObjectDef::Index {
                table: table_ref(schema, &table),
                index,
            })
        }
        ObjectKind::Trigger => {
            let sql = schema_sql(conn, schema, "trigger", name)?
                .ok_or_else(|| not_found("trigger", name))?;
            Ok(ObjectDef::Trigger(parse_trigger(&sql, schema)?))
        }
        other => Err(DriverError::new(
            ErrorKind::Unsupported,
            format!("SQLite has no {} objects", other.keyword().to_lowercase()),
        )),
    }
}

/// True when `sql` names `table`: one of its identifiers is the table's name (any case).
/// A column of the same name counts too, which only means one more object is recreated.
pub(crate) fn names_table(sql: &str, table: &str) -> bool {
    let p = Parser::new(sql);
    p.toks
        .iter()
        .filter(|t| matches!(t.kind, Kind::Word | Kind::Quoted))
        .any(|t| {
            Parser::new(p.text(*t))
                .name()
                .is_ok_and(|n| n.eq_ignore_ascii_case(table))
        })
}

/// Views, and triggers of other tables, that name `table`, in creation order.
fn dependents(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<DependentDef>, DriverError> {
    let q = SqliteDialect.quote_ident(schema);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT type, name, sql FROM {q}.sqlite_schema \
             WHERE (type = 'view' OR (type = 'trigger' AND tbl_name <> ?1)) \
               AND sql IS NOT NULL ORDER BY rowid"
        ))
        .map_err(map_error)?;
    let rows = stmt
        .query_map([table], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    Ok(rows
        .into_iter()
        .filter(|(_, name, sql)| !name.eq_ignore_ascii_case(table) && names_table(sql, table))
        .map(|(ty, name, sql)| DependentDef {
            kind: if ty == "view" {
                ObjectKind::View
            } else {
                ObjectKind::Trigger
            },
            name,
            sql,
        })
        .collect())
}

/// The columns a view yields; empty when the view cannot be resolved.
fn view_columns(conn: &Connection, schema: &str, name: &str) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare("SELECT name FROM pragma_table_info(?1, ?2) ORDER BY cid")
    else {
        return Vec::new();
    };
    stmt.query_map([name, schema], |r| r.get(0))
        .and_then(|rows| rows.collect::<Result<Vec<String>, _>>())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferrable_and_initially_deferred_are_distinct() {
        let def = parse_table(
            "CREATE TABLE t (a REFERENCES p DEFERRABLE, b REFERENCES p DEFERRABLE INITIALLY \
             DEFERRED, c REFERENCES p DEFERRABLE INITIALLY IMMEDIATE, d REFERENCES p NOT \
             DEFERRABLE INITIALLY DEFERRED, e REFERENCES p)",
            "main",
        )
        .unwrap();
        let flags: Vec<(bool, bool)> = def
            .constraints
            .iter()
            .map(|c| match c {
                ConstraintDef::ForeignKey {
                    deferrable,
                    initially_deferred,
                    ..
                } => (*deferrable, *initially_deferred),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            flags,
            [
                (true, false),
                (true, true),
                (true, false),
                (false, false),
                (false, false)
            ]
        );
    }

    #[test]
    fn parses_quoted_names_types_and_constraints() {
        let def = parse_table(
            "CREATE TABLE \"a \"\"b\"\" c\" ([x y] VARCHAR(10, 2) NOT NULL DEFAULT -1, \
             `z` CONSTRAINT k PRIMARY KEY DESC ON CONFLICT ABORT, w REFERENCES p ON DELETE SET NULL \
             NOT DEFERRABLE, UNIQUE (\"x y\" COLLATE NOCASE, z)) WITHOUT ROWID",
            "main",
        )
        .unwrap();
        assert_eq!(def.table.name, "a \"b\" c");
        assert_eq!(def.columns[0].name, "x y");
        assert_eq!(def.columns[0].type_name, "VARCHAR(10, 2)");
        assert_eq!(def.columns[0].default.as_deref(), Some("-1"));
        assert!(!def.columns[0].nullable);
        assert_eq!(def.columns[1].type_name, "");
        assert_eq!(def.columns[2].type_name, "");
        assert_eq!(
            def.constraints,
            [
                ConstraintDef::PrimaryKey {
                    name: Some("k".into()),
                    columns: vec!["z".into()]
                },
                ConstraintDef::ForeignKey {
                    name: None,
                    columns: vec!["w".into()],
                    target: table_ref("main", "p"),
                    target_columns: vec![],
                    on_update: FkAction::NoAction,
                    on_delete: FkAction::SetNull,
                    deferrable: false,
                    initially_deferred: false,
                },
                ConstraintDef::Unique {
                    name: None,
                    columns: vec!["x y".into(), "z".into()]
                },
            ]
        );
        assert_eq!(def.options, [("without_rowid".into(), "true".into())]);
    }

    #[test]
    fn parses_indexes_and_triggers() {
        let i = parse_index(
            "CREATE UNIQUE INDEX \"i\" ON t (a, lower(b) DESC, c COLLATE NOCASE) WHERE a > 0 AND b",
        )
        .unwrap();
        assert!(i.unique);
        let exprs: Vec<&str> = i.columns.iter().map(|c| c.expr.as_str()).collect();
        assert_eq!(exprs, ["a", "(lower(b))", "(c COLLATE NOCASE)"]);
        assert!(i.columns[1].desc);
        assert_eq!(i.predicate.as_deref(), Some("a > 0 AND b"));
        let t = parse_trigger(
            "CREATE TRIGGER x INSTEAD OF DELETE ON v BEGIN SELECT 'end'; END",
            "main",
        )
        .unwrap();
        assert_eq!(t.timing, "INSTEAD OF");
        assert_eq!(t.events, ["DELETE"]);
        assert_eq!(t.body, "SELECT 'end';");
        assert_eq!(t.condition, None);
    }

    #[test]
    fn parses_columns_named_like_index_direction_keywords() {
        // `asc` and `desc` are legal unquoted column names; only a keyword after an
        // expression is a direction.
        let i = parse_index("CREATE INDEX i ON t (asc, desc)").unwrap();
        let columns: Vec<(&str, bool)> = i
            .columns
            .iter()
            .map(|c| (c.expr.as_str(), c.desc))
            .collect();
        assert_eq!(columns, [("asc", false), ("desc", false)]);
    }

    #[test]
    fn direction_keywords_after_a_column_named_like_them_are_directions() {
        let i =
            parse_index("CREATE INDEX i ON t (asc DESC, b ASC, desc desc, (a + 1) DESC)").unwrap();
        let columns: Vec<(&str, bool)> = i
            .columns
            .iter()
            .map(|c| (c.expr.as_str(), c.desc))
            .collect();
        assert_eq!(
            columns,
            [
                ("asc", true),
                ("b", false),
                ("desc", true),
                ("(a + 1)", true)
            ]
        );
    }

    #[test]
    fn virtual_tables_are_reported_as_unsupported() {
        let err = parse_table("CREATE VIRTUAL TABLE docs USING fts5(body)", "main").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported, "{err:?}");
    }
}
