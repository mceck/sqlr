package dev.sqlr.jdbc;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** The explorer tree, autocomplete's catalog and table descriptions, from {@code DatabaseMetaData}. */
final class Meta {
    private final Connection conn;
    private final Preset preset;

    Meta(Connection conn, Preset preset) {
        this.conn = conn;
        this.preset = preset;
    }

    private static final String[][] FOLDERS = {
            {"tables", "Tables"}, {"views", "Views"}, {"procedures", "Procedures"},
            {"functions", "Functions"}, {"sequences", "Sequences"}};

    private static Map<String, Object> node(List<String> path, String name, String kind, boolean children, String detail) {
        return MsgPack.map("path", new ArrayList<Object>(path), "name", name, "kind", kind,
                "has_children", children, "detail", detail);
    }

    /** The current schema (or catalog) the connection is in, if the driver says. */
    String currentSchema() {
        try {
            String s = preset.catalogAsSchema ? conn.getCatalog() : conn.getSchema();
            return s;
        } catch (SQLException | RuntimeException e) {
            return null;
        }
    }

    private String escape(String pattern) throws SQLException {
        String esc = conn.getMetaData().getSearchStringEscape();
        if (esc == null || esc.isEmpty()) return pattern;
        return pattern.replace(esc, esc + esc).replace("%", esc + "%").replace("_", esc + "_");
    }

    private ResultSet tables(String schema, String name, String... types) throws SQLException {
        DatabaseMetaData md = conn.getMetaData();
        String s = escape(schema);
        String n = name == null ? "%" : escape(name);
        return preset.catalogAsSchema ? md.getTables(s, null, n, types) : md.getTables(null, s, n, types);
    }

    List<Object> browse(List<String> path) throws SQLException {
        DatabaseMetaData md = conn.getMetaData();
        List<Object> out = new ArrayList<>();
        if (path.isEmpty()) {
            try (ResultSet rs = preset.catalogAsSchema ? md.getCatalogs() : md.getSchemas()) {
                while (rs.next()) {
                    String name = rs.getString(1);
                    out.add(node(List.of(name), name, "Schema", true, null));
                }
            }
            return out;
        }
        String schema = path.get(0);
        if (path.size() == 1) {
            for (String[] f : FOLDERS) out.add(node(List.of(schema, f[0]), f[1], "Folder", true, null));
            return out;
        }
        String folder = path.get(1);
        if (path.size() == 2) {
            switch (folder) {
                case "tables": relations(out, schema, "Table", "TABLE", "BASE TABLE", "SYSTEM TABLE", "GLOBAL TEMPORARY", "LOCAL TEMPORARY"); break;
                case "views": relations(out, schema, "View", "VIEW", "SYSTEM VIEW"); break;
                case "procedures":
                    try (ResultSet rs = preset.catalogAsSchema ? md.getProcedures(escape(schema), null, "%") : md.getProcedures(null, escape(schema), "%")) {
                        while (rs.next()) out.add(node(List.of(schema, folder, rs.getString("PROCEDURE_NAME")), rs.getString("PROCEDURE_NAME"), "Procedure", false, null));
                    }
                    break;
                case "functions":
                    try (ResultSet rs = preset.catalogAsSchema ? md.getFunctions(escape(schema), null, "%") : md.getFunctions(null, escape(schema), "%")) {
                        while (rs.next()) out.add(node(List.of(schema, folder, rs.getString("FUNCTION_NAME")), rs.getString("FUNCTION_NAME"), "Function", false, null));
                    }
                    break;
                case "sequences": relations(out, schema, "Sequence", "SEQUENCE"); break;
                default: break;
            }
            return out;
        }
        if (path.size() == 3 && (folder.equals("tables") || folder.equals("views"))) {
            String table = path.get(2);
            try (ResultSet rs = preset.catalogAsSchema ? md.getColumns(escape(schema), null, escape(table), "%") : md.getColumns(null, escape(schema), escape(table), "%")) {
                while (rs.next()) {
                    String column = rs.getString("COLUMN_NAME");
                    out.add(node(List.of(schema, folder, table, column), column, "Column", false, rs.getString("TYPE_NAME")));
                }
            }
        }
        return out;
    }

    private void relations(List<Object> out, String schema, String kind, String... types) throws SQLException {
        String folder = kind.equals("View") ? "views" : kind.equals("Sequence") ? "sequences" : "tables";
        try (ResultSet rs = tables(schema, null, types)) {
            while (rs.next()) {
                String name = rs.getString("TABLE_NAME");
                boolean opens = !kind.equals("Sequence");
                out.add(node(List.of(schema, folder, name), name, kind, opens, null));
            }
        }
    }

    /** Tables, views and their columns of {@code schema}. */
    Map<String, Object> catalog(String schema) throws SQLException {
        DatabaseMetaData md = conn.getMetaData();
        Map<String, Map<String, Object>> relations = new LinkedHashMap<>();
        try (ResultSet rs = tables(schema, null, "TABLE", "BASE TABLE", "VIEW", "SYSTEM TABLE", "SYSTEM VIEW")) {
            while (rs.next()) {
                String name = rs.getString("TABLE_NAME");
                String type = rs.getString("TABLE_TYPE");
                boolean view = type != null && type.contains("VIEW");
                relations.put(name, MsgPack.map("name", name, "kind", view ? "View" : "Table", "columns", new ArrayList<Object>()));
            }
        }
        try (ResultSet rs = preset.catalogAsSchema ? md.getColumns(escape(schema), null, "%", "%") : md.getColumns(null, escape(schema), "%", "%")) {
            while (rs.next()) {
                Map<String, Object> relation = relations.get(rs.getString("TABLE_NAME"));
                if (relation == null) continue;
                @SuppressWarnings("unchecked")
                List<Object> columns = (List<Object>) relation.get("columns");
                columns.add(MsgPack.map("name", rs.getString("COLUMN_NAME"), "type_name", rs.getString("TYPE_NAME")));
            }
        }
        List<Object> sorted = new ArrayList<>(relations.values());
        sorted.sort((a, b) -> ((String) ((Map<?, ?>) a).get("name")).compareTo((String) ((Map<?, ?>) b).get("name")));
        return MsgPack.map("schema", schema, "relations", sorted);
    }

    /** Columns and keys of a table, for editing results. */
    Map<String, Object> describe(String schemaIn, String table) throws SQLException {
        DatabaseMetaData md = conn.getMetaData();
        String schema = schemaIn != null ? schemaIn : currentSchema();
        List<Object> columns = new ArrayList<>();
        String cat = preset.catalogAsSchema ? escape(schema) : null;
        String sch = preset.catalogAsSchema ? null : (schema == null ? null : escape(schema));
        try (ResultSet rs = md.getColumns(cat, sch, escape(table), "%")) {
            while (rs.next()) {
                String generated = "";
                try {
                    generated = rs.getString("IS_GENERATEDCOLUMN");
                } catch (SQLException ignored) {
                    // not reported
                }
                columns.add(MsgPack.map(
                        "name", rs.getString("COLUMN_NAME"),
                        "type_name", rs.getString("TYPE_NAME"),
                        "nullable", rs.getInt("NULLABLE") != DatabaseMetaData.columnNoNulls,
                        "default", rs.getString("COLUMN_DEF"),
                        "generated", "YES".equalsIgnoreCase(generated)));
            }
        }
        List<String[]> pk = new ArrayList<>();
        try (ResultSet rs = preset.catalogAsSchema ? md.getPrimaryKeys(cat, null, table) : md.getPrimaryKeys(null, schema, table)) {
            while (rs.next()) pk.add(new String[]{String.format("%08d", rs.getInt("KEY_SEQ")), rs.getString("COLUMN_NAME")});
        }
        pk.sort((a, b) -> a[0].compareTo(b[0]));
        List<Object> primary = new ArrayList<>();
        for (String[] k : pk) primary.add(k[1]);
        Map<String, List<Object>> unique = new LinkedHashMap<>();
        try (ResultSet rs = preset.catalogAsSchema ? md.getIndexInfo(cat, null, table, true, false) : md.getIndexInfo(null, schema, table, true, false)) {
            while (rs.next()) {
                String index = rs.getString("INDEX_NAME");
                String column = rs.getString("COLUMN_NAME");
                if (index == null || column == null) continue;
                unique.computeIfAbsent(index, k -> new ArrayList<>()).add(column);
            }
        }
        List<Object> uniqueKeys = new ArrayList<>();
        for (List<Object> cols : unique.values()) {
            if (!cols.equals(primary)) uniqueKeys.add(cols);
        }
        return MsgPack.map(
                "table", MsgPack.map("database", null, "schema", schema, "name", table),
                "columns", columns, "primary_key", primary, "unique_keys", uniqueKeys);
    }
}
