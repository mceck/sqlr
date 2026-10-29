//! Context-aware completion: what the caret is completing ([`caret_context`]) and the
//! matching items from the statement scope, the catalog, functions and keywords
//! ([`candidates`]). It never blocks: a catalog piece that isn't loaded becomes a
//! [`CatalogNeed`] for the caller to request.

use sqlr_core::{
    CatalogNeed, CatalogRelation, LexState, RelationKind, SchemaCatalog, SqlSyntax, Token,
    TokenKind,
};

use crate::editor::scope::{RelationRef, StatementScope};

/// At most this many items are offered.
pub const MAX_ITEMS: usize = 200;
/// Callers clip the text before the caret to at most this many bytes, at a line start.
pub const CONTEXT_CLIP_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub enum ContextKind {
    /// In a string, a quoted identifier or a comment.
    Off,
    Relation {
        schema: Option<String>,
    },
    Member {
        qualifier: String,
        quoted: bool,
    },
    General,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaretContext {
    pub prefix: String,
    pub kind: ContextKind,
}

/// Display and sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
    Column,
    Cte,
    Table,
    View,
    Schema,
    Function,
    Keyword,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub insert: String,
    pub kind: ItemKind,
    pub detail: String,
    /// Chars to move the caret back after inserting.
    pub caret_back: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidates {
    pub items: Vec<CompletionItem>,
    pub needs: Vec<CatalogNeed>,
}

/// The catalog as completion sees it.
pub trait CompletionSource {
    /// False when there is no live connection: never emit needs.
    fn available(&self) -> bool;
    /// `None` = not loaded.
    fn schemas(&self) -> Option<&[String]>;
    /// Exact name; `None` = not loaded.
    fn schema(&self, name: &str) -> Option<&SchemaCatalog>;
    fn default_schemas(&self) -> &[String];
}

/// No catalog: unavailable, nothing loaded.
pub struct NoCatalog;

impl CompletionSource for NoCatalog {
    fn available(&self) -> bool {
        false
    }

    fn schemas(&self) -> Option<&[String]> {
        None
    }

    fn schema(&self, _name: &str) -> Option<&SchemaCatalog> {
        None
    }

    fn default_schemas(&self) -> &[String] {
        &[]
    }
}

/// A relation name follows these keywords.
const RELATION_KEYWORDS: [&str; 5] = ["FROM", "JOIN", "UPDATE", "INTO", "TABLE"];
/// The keywords that start a clause, for deciding what a `,` continues.
const CLAUSE_KEYWORDS: [&str; 10] = [
    "SELECT", "FROM", "WHERE", "GROUP", "ORDER", "HAVING", "ON", "SET", "VALUES", "LIMIT",
];

/// What the caret at the end of `before_caret` completes; `before_caret` starts at a line
/// start in state `start`.
pub fn caret_context(syntax: &SqlSyntax, before_caret: &str, start: LexState) -> CaretContext {
    analyze_context(syntax, before_caret, start).0
}

/// Context plus whether a relation here can receive an automatic alias.
pub(crate) fn analyze_context(
    syntax: &SqlSyntax,
    before_caret: &str,
    start: LexState,
) -> (CaretContext, bool) {
    let off = CaretContext {
        prefix: String::new(),
        kind: ContextKind::Off,
    };
    let mut tokens = Vec::new();
    let end = syntax.lex(before_caret, start, &mut tokens);
    if end.state != LexState::Normal {
        return (off, false);
    }
    let text = |t: &Token| &before_caret[t.start..t.end];
    if let Some(t) = tokens.last()
        && t.kind == TokenKind::Comment
        && t.end == before_caret.len()
        && !syntax.line_comment.is_empty()
        && text(t).starts_with(syntax.line_comment)
    {
        // A line comment runs to the caret.
        return (off, false);
    }
    tokens.retain(|t| t.kind != TokenKind::Comment);
    let prefix = match tokens.last() {
        Some(t)
            if t.end == before_caret.len()
                && matches!(
                    t.kind,
                    TokenKind::Ident | TokenKind::Keyword | TokenKind::Function
                ) =>
        {
            let t = tokens.pop().expect("checked above");
            text(&t).to_string()
        }
        _ => String::new(),
    };
    let is_keyword = |t: &Token, list: &[&str]| {
        t.kind == TokenKind::Keyword && list.iter().any(|k| text(t).eq_ignore_ascii_case(k))
    };
    let kind = match tokens.as_slice() {
        [.., q, dot]
            if dot.kind == TokenKind::Punct
                && text(dot) == "."
                && matches!(
                    q.kind,
                    TokenKind::Ident | TokenKind::QuotedIdent | TokenKind::Keyword
                ) =>
        {
            let (qualifier, quoted) = syntax.unquote(text(q));
            ContextKind::Member { qualifier, quoted }
        }
        [.., last] if is_keyword(last, &RELATION_KEYWORDS) => {
            ContextKind::Relation { schema: None }
        }
        [rest @ .., last] if text(last) == "," && clause_is_from(rest, &text, &is_keyword) => {
            ContextKind::Relation { schema: None }
        }
        _ => ContextKind::General,
    };
    // Strip schema qualifiers to find the keyword introducing this relation.
    let mut relation_tokens = tokens.as_slice();
    while let [rest @ .., q, dot] = relation_tokens {
        if text(dot) != "."
            || !matches!(
                q.kind,
                TokenKind::Ident | TokenKind::QuotedIdent | TokenKind::Keyword
            )
        {
            break;
        }
        relation_tokens = rest;
    }
    let alias = match relation_tokens {
        [.., last] if is_keyword(last, &["FROM", "JOIN"]) => true,
        [rest @ .., last] if text(last) == "," => clause_is_from(rest, &text, &is_keyword),
        _ => false,
    };
    (CaretContext { prefix, kind }, alias)
}

/// Whether the nearest clause keyword before the end of `tokens`, at paren depth 0, is FROM.
fn clause_is_from<'a>(
    tokens: &[Token],
    text: &impl Fn(&Token) -> &'a str,
    is_keyword: &impl Fn(&Token, &[&str]) -> bool,
) -> bool {
    let mut depth = 0usize;
    for t in tokens.iter().rev() {
        match text(t) {
            ")" if t.kind == TokenKind::Punct => depth += 1,
            "(" if t.kind == TokenKind::Punct => match depth.checked_sub(1) {
                Some(d) => depth = d,
                // The `,` is inside parentheses opened here.
                None => return false,
            },
            w if depth == 0 && is_keyword(t, &CLAUSE_KEYWORDS) => {
                return w.eq_ignore_ascii_case("FROM");
            }
            _ => {}
        }
    }
    false
}

/// The items for `ctx`, filtered by its prefix, and the catalog pieces still missing.
pub fn candidates(
    ctx: &CaretContext,
    scope: &StatementScope,
    syntax: &SqlSyntax,
    source: &dyn CompletionSource,
) -> Candidates {
    let mut b = Builder {
        syntax,
        source,
        prefix: ctx.prefix.to_lowercase(),
        matches: Vec::new(),
        needs: Vec::new(),
    };
    match &ctx.kind {
        ContextKind::Off => return Candidates::default(),
        ContextKind::Relation { schema: None } => {
            for cte in &scope.ctes {
                b.identifier(cte, ItemKind::Cte, || "cte".into());
            }
            for d in source.default_schemas() {
                b.relations_of(d);
            }
            match source.schemas() {
                Some(list) => {
                    for s in list {
                        b.identifier(s, ItemKind::Schema, || "schema".into());
                    }
                }
                None => b.need(CatalogNeed::Schemas),
            }
        }
        ContextKind::Relation { schema: Some(s) } => b.schema_relations(s, false),
        ContextKind::Member { qualifier, quoted } => {
            match resolve(scope, qualifier, *quoted) {
                Some(r) if !is_cte(scope, r) => {
                    if let Some(rel) = b.lookup(r) {
                        for col in &rel.columns {
                            b.identifier(&col.name, ItemKind::Column, || col.type_name.clone());
                        }
                    }
                }
                // CTE columns aren't completed.
                Some(_) => {}
                None if is_cte_name(scope, qualifier, *quoted) => {}
                None => b.schema_relations(qualifier, *quoted),
            }
        }
        ContextKind::General => {
            let mut seen: Vec<&str> = Vec::new();
            for r in &scope.relations {
                if is_cte(scope, r) {
                    continue;
                }
                let Some(rel) = b.lookup(r) else { continue };
                for col in &rel.columns {
                    if !seen.contains(&col.name.as_str()) {
                        seen.push(&col.name);
                        b.identifier(&col.name, ItemKind::Column, || {
                            format!("{}.{}", rel.name, col.type_name)
                        });
                    }
                }
            }
            for f in &syntax.functions {
                b.push(f, || CompletionItem {
                    label: (*f).to_string(),
                    insert: format!("{f}()"),
                    kind: ItemKind::Function,
                    detail: "function".into(),
                    caret_back: 1,
                });
            }
            for k in &syntax.keywords {
                b.push(k, || CompletionItem {
                    label: (*k).to_string(),
                    insert: k.to_ascii_uppercase(),
                    kind: ItemKind::Keyword,
                    detail: "keyword".into(),
                    caret_back: 0,
                });
            }
        }
    }
    b.finish()
}

/// Accumulates the items matching the prefix, and the needs.
struct Builder<'a> {
    syntax: &'a SqlSyntax,
    source: &'a dyn CompletionSource,
    /// Lowercased.
    prefix: String,
    /// `(0 = prefix match / 1 = substring match, item)`.
    matches: Vec<(u8, CompletionItem)>,
    needs: Vec<CatalogNeed>,
}

impl<'a> Builder<'a> {
    fn need(&mut self, need: CatalogNeed) {
        if self.source.available() && !self.needs.contains(&need) {
            self.needs.push(need);
        }
    }

    /// Adds the item `make` builds if `label` matches the prefix; nothing is built otherwise.
    fn push(&mut self, label: &str, make: impl FnOnce() -> CompletionItem) {
        if let Some(class) = match_class(label, &self.prefix) {
            self.matches.push((class, make()));
        }
    }

    /// An identifier item, inserted quoted if needed.
    fn identifier(&mut self, name: &str, kind: ItemKind, detail: impl FnOnce() -> String) {
        let syntax = self.syntax;
        self.push(name, || CompletionItem {
            label: name.to_string(),
            insert: syntax.quote_if_needed(name).into_owned(),
            kind,
            detail: detail(),
            caret_back: 0,
        });
    }

    /// The tables and views of the schema written as `name` (with `quoted` as written):
    /// nothing if the loaded schema list has no such schema.
    fn schema_relations(&mut self, name: &str, quoted: bool) {
        match self.source.schemas() {
            Some(list) => {
                if let Some(exact) = find_name(list.iter().map(String::as_str), name, quoted) {
                    self.relations_of(exact);
                }
            }
            None => {
                // Not known to be a schema yet: ask for the list, and use it if loaded anyway.
                self.need(CatalogNeed::Schemas);
                if let Some(cat) = self.source.schema(name) {
                    self.add_relations(cat);
                }
            }
        }
    }

    /// The tables and views of the schema named exactly `schema`.
    fn relations_of(&mut self, schema: &str) {
        match self.source.schema(schema) {
            Some(cat) => self.add_relations(cat),
            None => self.need(CatalogNeed::Schema(schema.to_string())),
        }
    }

    fn add_relations(&mut self, cat: &SchemaCatalog) {
        for rel in &cat.relations {
            let kind = match rel.kind {
                RelationKind::Table => ItemKind::Table,
                RelationKind::View | RelationKind::MaterializedView => ItemKind::View,
            };
            self.identifier(&rel.name, kind, || cat.schema.clone());
        }
    }

    /// The catalog entry of `r`: in its own schema, or in the default schemas in order.
    fn lookup(&mut self, r: &RelationRef) -> Option<&'a CatalogRelation> {
        let source = self.source;
        let search = |schema: &str, this: &mut Self| match source.schema(schema) {
            Some(cat) => {
                let names = cat.relations.iter().map(|x| x.name.as_str());
                let name = find_name(names, &r.name, r.quoted)?;
                cat.relations.iter().find(|x| x.name == name)
            }
            None => {
                this.need(CatalogNeed::Schema(schema.to_string()));
                None
            }
        };
        match &r.schema {
            Some(s) => {
                let exact = match source.schemas() {
                    Some(list) => find_name(list.iter().map(String::as_str), s, false)?,
                    None => s.as_str(),
                };
                search(exact, self)
            }
            None => source
                .default_schemas()
                .iter()
                .find_map(|d| search(d, self)),
        }
    }

    fn finish(mut self) -> Candidates {
        self.matches.sort_by(|(ca, a), (cb, b)| {
            (ca, a.kind, a.label.len(), &a.label).cmp(&(cb, b.kind, b.label.len(), &b.label))
        });
        self.matches.truncate(MAX_ITEMS);
        Candidates {
            items: self.matches.into_iter().map(|(_, item)| item).collect(),
            needs: self.needs,
        }
    }
}

/// Initials of word segments, with a suffix to avoid names already in the statement.
pub(crate) fn relation_alias(name: &str, scope: &StatementScope, syntax: &SqlSyntax) -> String {
    let mut base = String::new();
    let mut boundary = true;
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            boundary = true;
            previous_lower = false;
            continue;
        }
        if (boundary || (previous_lower && c.is_uppercase())) && c.is_alphabetic() {
            base.extend(c.to_lowercase());
        }
        boundary = false;
        previous_lower = c.is_lowercase();
    }
    if base.is_empty() {
        base.push('t');
    }
    let used = |alias: &str| {
        scope.ctes.iter().any(|n| n.eq_ignore_ascii_case(alias))
            || scope.relations.iter().any(|r| {
                r.name.eq_ignore_ascii_case(alias)
                    || r.alias
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(alias))
            })
            || syntax
                .reserved
                .iter()
                .any(|k| k.eq_ignore_ascii_case(alias))
            || name.eq_ignore_ascii_case(alias)
    };
    let mut alias = base.clone();
    let mut suffix = 2;
    while used(&alias) {
        alias = format!("{base}{suffix}");
        suffix += 1;
    }
    syntax.quote_if_needed(&alias).into_owned()
}

/// `Some(0)` for a case-insensitive prefix match, `Some(1)` for a substring match.
fn match_class(label: &str, prefix_lower: &str) -> Option<u8> {
    if prefix_lower.is_empty() {
        return Some(0);
    }
    if label.is_ascii() && prefix_lower.is_ascii() {
        let (l, p) = (label.as_bytes(), prefix_lower.as_bytes());
        if l.len() < p.len() {
            return None;
        }
        if l[..p.len()].eq_ignore_ascii_case(p) {
            return Some(0);
        }
        return l
            .windows(p.len())
            .any(|w| w.eq_ignore_ascii_case(p))
            .then_some(1);
    }
    let lower = label.to_lowercase();
    if lower.starts_with(prefix_lower) {
        Some(0)
    } else {
        lower.contains(prefix_lower).then_some(1)
    }
}

/// The first of `names` equal to `name`, exactly, or (unquoted) ignoring ASCII case.
fn find_name<'n>(
    names: impl Iterator<Item = &'n str> + Clone,
    name: &str,
    quoted: bool,
) -> Option<&'n str> {
    names.clone().find(|n| *n == name).or_else(|| {
        (!quoted)
            .then(|| names.into_iter().find(|n| n.eq_ignore_ascii_case(name)))
            .flatten()
    })
}

/// The relation `q` names: an alias first, then a relation name.
fn resolve<'s>(scope: &'s StatementScope, q: &str, quoted: bool) -> Option<&'s RelationRef> {
    let eq = |a: &str| {
        if quoted {
            a == q
        } else {
            a.eq_ignore_ascii_case(q)
        }
    };
    scope
        .relations
        .iter()
        .find(|r| r.alias.as_deref().is_some_and(eq))
        .or_else(|| scope.relations.iter().find(|r| eq(&r.name)))
}

fn is_cte_name(scope: &StatementScope, name: &str, quoted: bool) -> bool {
    scope.ctes.iter().any(|c| {
        if quoted {
            c == name
        } else {
            c.eq_ignore_ascii_case(name)
        }
    })
}

/// An unqualified relation named like a CTE of the statement.
fn is_cte(scope: &StatementScope, r: &RelationRef) -> bool {
    r.schema.is_none() && is_cte_name(scope, &r.name, r.quoted)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use sqlr_core::CatalogColumn;

    use super::*;

    #[derive(Default)]
    struct StubSource {
        available: bool,
        schemas: Option<Vec<String>>,
        catalogs: HashMap<String, SchemaCatalog>,
        defaults: Vec<String>,
    }

    impl CompletionSource for StubSource {
        fn available(&self) -> bool {
            self.available
        }

        fn schemas(&self) -> Option<&[String]> {
            self.schemas.as_deref()
        }

        fn schema(&self, name: &str) -> Option<&SchemaCatalog> {
            self.catalogs.get(name)
        }

        fn default_schemas(&self) -> &[String] {
            &self.defaults
        }
    }

    fn table(name: &str, columns: &[&str]) -> CatalogRelation {
        CatalogRelation {
            name: name.into(),
            kind: RelationKind::Table,
            columns: columns
                .iter()
                .map(|c| CatalogColumn {
                    name: (*c).into(),
                    type_name: "int4".into(),
                })
                .collect(),
        }
    }

    fn public_source(relations: Vec<CatalogRelation>) -> StubSource {
        let mut catalogs = HashMap::new();
        catalogs.insert(
            "public".to_string(),
            SchemaCatalog {
                schema: "public".into(),
                relations,
            },
        );
        StubSource {
            available: true,
            schemas: Some(vec!["public".into(), "sales".into()]),
            catalogs,
            defaults: vec!["public".into()],
        }
    }

    fn ctx(prefix: &str, kind: ContextKind) -> CaretContext {
        CaretContext {
            prefix: prefix.into(),
            kind,
        }
    }

    fn member(q: &str) -> ContextKind {
        ContextKind::Member {
            qualifier: q.into(),
            quoted: false,
        }
    }

    fn rel(schema: Option<&str>, name: &str, alias: Option<&str>) -> RelationRef {
        RelationRef {
            schema: schema.map(Into::into),
            name: name.into(),
            alias: alias.map(Into::into),
            quoted: false,
        }
    }

    fn scope(relations: Vec<RelationRef>) -> StatementScope {
        StatementScope {
            relations,
            ctes: Vec::new(),
        }
    }

    fn labels(c: &Candidates) -> Vec<&str> {
        c.items.iter().map(|i| i.label.as_str()).collect()
    }

    fn context_of(text: &str) -> CaretContext {
        caret_context(&SqlSyntax::ansi(), text, LexState::Normal)
    }

    #[test]
    fn contexts() {
        assert_eq!(
            context_of("SELECT * FROM us"),
            ctx("us", ContextKind::Relation { schema: None })
        );
        assert_eq!(context_of("SELECT u.na"), ctx("na", member("u")));
        assert_eq!(
            context_of("SELECT * FROM public."),
            ctx("", member("public"))
        );
        assert_eq!(
            context_of("SELECT * FROM a JOIN "),
            ctx("", ContextKind::Relation { schema: None })
        );
        assert_eq!(
            context_of("SELECT * FROM a, b"),
            ctx("b", ContextKind::Relation { schema: None })
        );
        assert_eq!(context_of("SELECT a, b"), ctx("b", ContextKind::General));
        assert_eq!(context_of("SELECT 'from x").kind, ContextKind::Off);
        assert_eq!(context_of("-- from x").kind, ContextKind::Off);
    }

    #[test]
    fn context_details() {
        assert_eq!(
            context_of(r#"SELECT "My T".c"#),
            ctx(
                "c",
                ContextKind::Member {
                    qualifier: "My T".into(),
                    quoted: true
                }
            )
        );
        // A `,` inside parentheses doesn't continue a FROM list.
        assert_eq!(
            context_of("SELECT * FROM t WHERE x IN (a, b").kind,
            ContextKind::General
        );
        assert_eq!(
            context_of("SELECT * FROM (SELECT 1 FROM x, y").kind,
            ContextKind::Relation { schema: None }
        );
        assert_eq!(context_of("SELECT \"ab").kind, ContextKind::Off);
        assert_eq!(context_of("SELECT /* x").kind, ContextKind::Off);
        assert_eq!(context_of("SELECT /* x */ a").kind, ContextKind::General);
        assert_eq!(
            caret_context(
                &SqlSyntax::ansi(),
                "' FROM x",
                LexState::Str { escapes: false }
            ),
            ctx("x", ContextKind::Relation { schema: None })
        );
        assert_eq!(
            caret_context(&SqlSyntax::ansi(), "UPDATE ", LexState::Normal).kind,
            ContextKind::Relation { schema: None }
        );
    }

    #[test]
    fn alias_columns() {
        let src = public_source(vec![table("orders", &["id", "total"])]);
        let sc = scope(vec![rel(Some("public"), "orders", Some("o"))]);
        let c = candidates(&ctx("t", member("o")), &sc, &SqlSyntax::ansi(), &src);
        assert_eq!(c.items[0].label, "total");
        assert_eq!(c.items[0].kind, ItemKind::Column);
        assert_eq!(c.items[0].detail, "int4");
        assert!(c.needs.is_empty());
        // Unquoted qualifiers match aliases case-insensitively.
        let c = candidates(&ctx("", member("O")), &sc, &SqlSyntax::ansi(), &src);
        assert_eq!(labels(&c), ["id", "total"]);
    }

    #[test]
    fn unqualified_table_uses_default_schemas() {
        let src = public_source(vec![table("users", &["id", "name"])]);
        let sc = scope(vec![rel(None, "users", Some("u"))]);
        let c = candidates(&ctx("", member("users")), &sc, &SqlSyntax::ansi(), &src);
        assert_eq!(labels(&c), ["id", "name"]);
        let c = candidates(
            &ctx("na", ContextKind::General),
            &sc,
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(c.items[0].label, "name");
        assert_eq!(c.items[0].detail, "users.int4");
        let c = candidates(
            &ctx("us", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(labels(&c), ["users"]);
        assert_eq!(c.items[0].kind, ItemKind::Table);
    }

    #[test]
    fn schema_qualifier_lists_tables() {
        let mut src = public_source(vec![table("orders", &[]), table("users", &[])]);
        src.catalogs.insert(
            "sales".into(),
            SchemaCatalog {
                schema: "sales".into(),
                relations: vec![table("invoices", &[])],
            },
        );
        let none = scope(Vec::new());
        let c = candidates(&ctx("", member("sales")), &none, &SqlSyntax::ansi(), &src);
        assert_eq!(labels(&c), ["invoices"]);
        let c = candidates(
            &ctx(
                "",
                ContextKind::Relation {
                    schema: Some("public".into()),
                },
            ),
            &none,
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(labels(&c), ["users", "orders"]);
        // Relation without a schema: tables of the defaults, then the schema names.
        let c = candidates(
            &ctx("", ContextKind::Relation { schema: None }),
            &StatementScope {
                relations: Vec::new(),
                ctes: vec!["recent".into()],
            },
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(labels(&c), ["recent", "users", "orders", "sales", "public"]);
    }

    #[test]
    fn no_catalog_offers_keywords_without_needs() {
        let c = candidates(
            &ctx("sel", ContextKind::General),
            &scope(vec![rel(None, "users", None)]),
            &SqlSyntax::ansi(),
            &NoCatalog,
        );
        assert_eq!(c.items[0].label, "SELECT");
        assert_eq!(c.items[0].insert, "SELECT");
        assert_eq!(c.items[0].kind, ItemKind::Keyword);
        assert!(c.needs.is_empty());
        let c = candidates(
            &ctx("", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &NoCatalog,
        );
        assert!(c.items.is_empty() && c.needs.is_empty());
    }

    #[test]
    fn missing_schema_is_requested_once() {
        let src = StubSource {
            available: true,
            schemas: Some(vec!["public".into()]),
            defaults: vec!["public".into()],
            ..StubSource::default()
        };
        let sc = scope(vec![
            rel(Some("public"), "orders", Some("o")),
            rel(None, "users", Some("u")),
        ]);
        let c = candidates(
            &ctx("", ContextKind::General),
            &sc,
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(c.needs, [CatalogNeed::Schema("public".into())]);
        let c = candidates(&ctx("", member("o")), &sc, &SqlSyntax::ansi(), &src);
        assert_eq!(c.needs, [CatalogNeed::Schema("public".into())]);
        assert!(c.items.is_empty());

        let unknown = StubSource {
            available: true,
            ..StubSource::default()
        };
        let c = candidates(
            &ctx("", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &unknown,
        );
        assert_eq!(c.needs, [CatalogNeed::Schemas]);
    }

    fn pg_like() -> Arc<SqlSyntax> {
        let ansi = SqlSyntax::ansi();
        let mut reserved = ansi.reserved.clone();
        reserved.push("USER");
        reserved.sort_unstable();
        Arc::new(SqlSyntax {
            reserved,
            dollar_quotes: true,
            nested_comments: true,
            escape_strings: true,
            ..(*ansi).clone()
        })
    }

    #[test]
    fn inserts_quoted_identifiers() {
        let src = public_source(vec![
            table("Order Items", &[]),
            table("user", &[]),
            table("line_items", &[]),
        ]);
        let c = candidates(
            &ctx("", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &pg_like(),
            &src,
        );
        let insert = |label: &str| {
            c.items
                .iter()
                .find(|i| i.label == label)
                .map(|i| i.insert.clone())
        };
        assert_eq!(insert("Order Items").as_deref(), Some("\"Order Items\""));
        assert_eq!(insert("user").as_deref(), Some("\"user\""));
        assert_eq!(insert("line_items").as_deref(), Some("line_items"));
    }

    #[test]
    fn prefix_before_substring_and_limit() {
        let mut rels: Vec<CatalogRelation> =
            (0..1000).map(|i| table(&format!("t{i:04}"), &[])).collect();
        rels.push(table("xt", &[]));
        let src = public_source(rels);
        let c = candidates(
            &ctx("t", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &src,
        );
        assert_eq!(c.items.len(), MAX_ITEMS);
        assert!(c.items.iter().all(|i| i.label.starts_with('t')));
        assert_eq!(c.items[0].label, "t0000");
        // Substring matches follow the prefix matches.
        let c = candidates(
            &ctx("T09", ContextKind::Relation { schema: None }),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &public_source(vec![table("at09", &[]), table("t0900", &[])]),
        );
        assert_eq!(labels(&c), ["t0900", "at09"]);
    }

    #[test]
    fn functions_put_caret_inside() {
        let c = candidates(
            &ctx("coal", ContextKind::General),
            &scope(Vec::new()),
            &SqlSyntax::ansi(),
            &NoCatalog,
        );
        assert_eq!(c.items[0].label, "coalesce");
        assert_eq!(c.items[0].insert, "coalesce()");
        assert_eq!(c.items[0].kind, ItemKind::Function);
        assert_eq!(c.items[0].caret_back, 1);
    }
}
