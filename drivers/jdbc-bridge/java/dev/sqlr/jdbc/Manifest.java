package dev.sqlr.jdbc;

import java.sql.Connection;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

/** The driver manifest (the connection form) and the dialect, as protocol data. */
final class Manifest {
    private Manifest() {}

    private static Map<String, Object> field(String key, String label, Object ty, String def, boolean required, boolean secret, String group) {
        return MsgPack.map("key", key, "label", label, "ty", ty, "default", def, "required", required,
                "secret", secret, "group", group);
    }

    static Map<String, Object> of(Preset p) {
        List<Object> fields = new ArrayList<>();
        for (String key : p.fields) {
            switch (key.trim()) {
                case "host": fields.add(field("host", "Host", "Text", "localhost", true, false, "Main")); break;
                case "port": fields.add(field("port", "Port", "Port", p.defaultPort > 0 ? String.valueOf(p.defaultPort) : null, false, false, "Main")); break;
                case "database": fields.add(field("database", "Database", "Text", null, false, false, "Main")); break;
                case "path": fields.add(field("path", "Database file or URL part", MsgPack.variant("File", MsgPack.map("save", true, "extensions", new ArrayList<Object>())), null, true, false, "Main")); break;
                case "jdbc_url": fields.add(field("jdbc_url", "JDBC URL", "Text", null, true, false, "Main")); break;
                case "driver_class": fields.add(field("driver_class", "Driver class", "Text", null, true, false, "Main")); break;
                case "jar": fields.add(field("jar", "Driver jar", MsgPack.variant("File", MsgPack.map("save", false, "extensions", new ArrayList<Object>(Arrays.asList("jar")))), null, false, false, "Main")); break;
                case "extra_jars": fields.add(field("extra_jars", "More jars (separated by ;)", "Text", null, false, false, "Advanced")); break;
                case "user": fields.add(field("user", "User", "Text", null, false, false, "Main")); break;
                case "password": fields.add(field("password", "Password", "Password", null, false, true, "Main")); break;
                default: break;
            }
        }
        Map<String, Object> caps = MsgPack.map("transactions", false, "server_cancel", true, "editable_results", true,
                "explain", false, "multi_resultset", false, "tabular_query", false, "schemas", true, "ssh", false, "dump", false);
        return MsgPack.map("id", p.id, "name", p.name, "family", "Relational", "capabilities", caps, "fields", fields,
                "dialect", "ansi", "url_schemes", new ArrayList<Object>(), "default_port", p.defaultPort > 0 ? (Object) (long) p.defaultPort : null,
                "editing", null);
    }

    private static final String[] ANSI = {"ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "BETWEEN", "BY", "CASE", "CAST",
            "CHECK", "COLUMN", "COMMIT", "CONSTRAINT", "CREATE", "CROSS", "DEFAULT", "DELETE", "DESC", "DISTINCT", "DROP",
            "ELSE", "END", "EXCEPT", "EXISTS", "FALSE", "FETCH", "FOREIGN", "FROM", "FULL", "GRANT", "GROUP", "HAVING", "IN",
            "INDEX", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "LEFT", "LIKE", "LIMIT", "NOT", "NULL",
            "OFFSET", "ON", "OR", "ORDER", "OUTER", "PRIMARY", "REFERENCES", "RIGHT", "ROLLBACK", "SELECT", "SET", "TABLE",
            "THEN", "TRUE", "UNION", "UNIQUE", "UPDATE", "USING", "VALUES", "VIEW", "WHEN", "WHERE", "WITH"};

    static Map<String, Object> dialect(Preset p, Connection c) {
        TreeSet<String> keywords = new TreeSet<>(Arrays.asList(ANSI));
        try {
            String extra = c.getMetaData().getSQLKeywords();
            if (extra != null) for (String k : extra.split(",")) if (!k.trim().isEmpty()) keywords.add(k.trim().toUpperCase());
        } catch (SQLException ignored) {
            // the ANSI words alone
        }
        char open = p.quote.isEmpty() ? '"' : p.quote.charAt(0);
        char close = open == '[' ? ']' : open;
        List<Object> quotes = new ArrayList<>();
        quotes.add(new ArrayList<Object>(Arrays.asList((long) open, (long) close)));
        Map<String, Object> syntax = MsgPack.map(
                "keywords", new ArrayList<Object>(keywords), "reserved", new ArrayList<Object>(keywords),
                "functions", new ArrayList<Object>(Arrays.asList("abs", "avg", "cast", "coalesce", "count", "lower", "max", "min", "nullif", "round", "sum", "upper")),
                "dollar_quotes", false, "nested_comments", false, "escape_strings", false,
                "line_comment", "--", "line_comment_needs_space", false, "hash_comments", false,
                "ident_quotes", quotes, "double_quote_strings", false, "backslash_escapes", false, "grammar", "sequel");
        String limit;
        switch (p.limit) {
            case "FETCH_FIRST": limit = "FetchFirst"; break;
            case "TOP": limit = "Top"; break;
            case "NONE": limit = "None"; break;
            default: limit = "Limit";
        }
        return MsgPack.map("syntax", syntax, "default_schemas", new ArrayList<Object>(), "database_is_schema", p.catalogAsSchema,
                "placeholder", "Question", "limit", limit,
                "select_segments", new ArrayList<Object>(Arrays.asList(0L, 2L)), "select_uses_name", false,
                "begin_sql", "BEGIN", "read_only_on", null, "read_only_off", null,
                "ddl_commits_transaction", false, "error_aborts_transaction", false);
    }
}
