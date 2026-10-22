package dev.sqlr.jdbc;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** What the launcher tells the bridge about the database it serves: {@code key=value} arguments. */
final class Preset {
    final String id;
    final String name;
    /** The JDBC driver class; empty for the generic bridge, which asks the user. */
    final String driverClass;
    /** URL with {@code {host}}, {@code {port}}, {@code {database}} and {@code {path}}; empty for generic. */
    final String urlTemplate;
    final int defaultPort;
    final List<String> fields;
    final String quote;
    /** LIMIT, FETCH_FIRST, TOP or NONE. */
    final String limit;
    /** The database has catalogs but no schemas (MySQL-like): catalogs are shown as schemas. */
    final boolean catalogAsSchema;

    Preset(Map<String, String> a) {
        id = a.getOrDefault("id", "jdbc");
        name = a.getOrDefault("name", "JDBC");
        driverClass = a.getOrDefault("class", "");
        urlTemplate = a.getOrDefault("url", "");
        defaultPort = a.containsKey("port") ? Integer.parseInt(a.get("port")) : 0;
        fields = new ArrayList<>(Arrays.asList(a.getOrDefault("fields", "jdbc_url,driver_class,jar,extra_jars,user,password").split(",")));
        quote = a.getOrDefault("quote", "\"");
        limit = a.getOrDefault("limit", "LIMIT");
        catalogAsSchema = "true".equals(a.get("catalog_as_schema"));
    }

    static Preset parse(String[] args) {
        Map<String, String> m = new LinkedHashMap<>();
        for (String arg : args) {
            int eq = arg.indexOf('=');
            if (eq > 0) m.put(arg.substring(0, eq), arg.substring(eq + 1));
        }
        return new Preset(m);
    }
}
