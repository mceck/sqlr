use sqlr_core::{FormatOptions, LexState, SqlSyntax, TokenKind, format_sql};
fn tokens(syntax: &SqlSyntax, text: &str) -> Vec<(TokenKind, String)> {
    let mut ts = vec![];
    syntax.lex(text, LexState::Normal, &mut ts);
    ts.into_iter()
        .map(|t| {
            (
                t.kind,
                if t.kind == TokenKind::Keyword {
                    text[t.start..t.end].to_ascii_uppercase()
                } else {
                    text[t.start..t.end].to_string()
                },
            )
        })
        .collect()
}
#[test]
fn formatter_corpus() {
    assert_eq!(
        format_sql(
            &SqlSyntax::ansi(),
            "select a,b from t where a=1",
            &FormatOptions::default()
        ),
        "SELECT a, b\nFROM t\nWHERE a = 1"
    );
}
#[test]
fn formatter_preserves_tokens_corpus() {
    let mut syntax = (*SqlSyntax::ansi()).clone();
    syntax.dollar_quotes = true;
    syntax.hash_comments = true;
    for sql in [
        "select 'a  b',\"MiX\" from t -- comment\nwhere a!=2;",
        "select $$begin; end$$, $body$x\ny$body$;",
        "select 1 # hello\nfrom t",
        "select /*a*/foo/*b*/(x), a::int from t",
        "select 1e-2, :p, ? from t",
        "select ((((((((((((((((((((((((((((((((((((((((((((((((((1))))))))))))))))))))))))))))))))))))))))))))))))))",
    ] {
        let f = format_sql(&syntax, sql, &FormatOptions::default());
        assert_eq!(tokens(&syntax, sql), tokens(&syntax, &f), "{sql} => {f}");
        assert_eq!(
            format_sql(&syntax, &f, &FormatOptions::default()),
            f,
            "idempotent: {sql}"
        );
    }
}
#[test]
fn formatter_idempotent() {
    let sql = "select a,b from t where x in (select x from y) and a > 1;select 2";
    let s = SqlSyntax::ansi();
    let opts = FormatOptions::default();
    let f = format_sql(&s, sql, &opts);
    assert_eq!(format_sql(&s, &f, &opts), f);
}
#[test]
fn formatter_large_statement_time() {
    let text = format!("select '{}' from t", "x".repeat(1_000_000));
    let start = std::time::Instant::now();
    let out = format_sql(&SqlSyntax::ansi(), &text, &FormatOptions::default());
    assert!(out.contains(&"x".repeat(1_000_000)));
    if !cfg!(debug_assertions) {
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
    }
}

#[test]
fn formatter_preserves_semantically_significant_string_boundaries() {
    let syntax = SqlSyntax::ansi();
    let options = FormatOptions::default();
    let f = format_sql(&syntax, "select 'hello'\n'world'", &options);
    assert!(
        f.contains("'hello'\n'world'"),
        "PostgreSQL concatenates strings separated by a newline: {f}"
    );
    let f = format_sql(&syntax, "select 'hello'\n/* c */ 'world'", &options);
    assert!(
        f.contains("\n'world'"),
        "a comment between two literals must not remove their newline: {f}"
    );
    let f = format_sql(&syntax, "select U&'d\\0061t'", &options);
    assert!(
        f.contains("U&'d\\0061t'"),
        "Unicode string prefix must stay attached: {f}"
    );
}

#[test]
fn formatter_preserves_adjacent_dialect_lexemes() {
    let output = format_sql(
        &SqlSyntax::ansi(),
        "select 0xFF, ?1, @name, data ?| array['x'], U&\"d\\0061t\" from t",
        &FormatOptions::default(),
    );
    for lexeme in ["0xFF", "?1", "@name", "?|", "U&\"d\\0061t\""] {
        assert!(
            output.contains(lexeme),
            "lost adjacent dialect lexeme {lexeme}: {output}"
        );
    }
}
