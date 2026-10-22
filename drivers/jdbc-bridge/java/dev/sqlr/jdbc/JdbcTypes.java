package dev.sqlr.jdbc;

import java.math.BigDecimal;
import java.sql.Array;
import java.sql.Blob;
import java.sql.Clob;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.SQLXML;
import java.sql.Types;
import java.time.OffsetDateTime;
import java.time.OffsetTime;
import java.util.regex.Pattern;

/**
 * JDBC types as chunk columns and JDBC values as cells. Values arrive in the right kind or as
 * text, never as the {@code toString()} of a driver object ({@code org.vendor.Type@1a2b3c}).
 */
final class JdbcTypes {
    private JdbcTypes() {}

    /** Cells larger than this are cut, with the cut shown in the text. */
    static final int MAX_LOB = 16 * 1024 * 1024;

    private static final Pattern OBJECT_TO_STRING = Pattern.compile("^[\\w.$]+@[0-9a-f]+$");

    /** Types a driver reports as binary or "other" but that mean text (UUID, JSON, intervals…). */
    private static boolean textual(String typeName) {
        if (typeName == null) return false;
        String n = typeName.toLowerCase();
        return n.equals("uuid") || n.equals("json") || n.equals("jsonb") || n.startsWith("interval")
                || n.equals("xml") || n.equals("citext") || n.equals("inet") || n.equals("cidr")
                || n.equals("enum") || n.equals("hstore");
    }

    static int kindOf(int type, String typeName) {
        if (textual(typeName)) return ChunkWriter.TEXT;
        switch (type) {
            case Types.BIT:
            case Types.BOOLEAN:
                return ChunkWriter.BOOL;
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT:
                return ChunkWriter.INT;
            case Types.FLOAT:
            case Types.REAL:
            case Types.DOUBLE:
                return ChunkWriter.FLOAT;
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB:
                return ChunkWriter.BYTES;
            default:
                return ChunkWriter.TEXT;
        }
    }

    static int hintOf(int type, String typeName) {
        switch (type) {
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT:
            case Types.FLOAT:
            case Types.REAL:
            case Types.DOUBLE:
                return ChunkWriter.NUMBER;
            case Types.DECIMAL:
            case Types.NUMERIC:
                return ChunkWriter.DECIMAL;
            case Types.DATE:
            case Types.TIME:
            case Types.TIMESTAMP:
            case Types.TIME_WITH_TIMEZONE:
            case Types.TIMESTAMP_WITH_TIMEZONE:
                return ChunkWriter.TEMPORAL;
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB:
                return ChunkWriter.BINARY;
            case Types.SQLXML:
                return ChunkWriter.XML;
            case Types.ARRAY:
                return ChunkWriter.ARRAY;
            default:
                String n = typeName == null ? "" : typeName.toLowerCase();
                if (n.equals("json") || n.equals("jsonb")) return ChunkWriter.JSON;
                if (n.equals("uuid")) return ChunkWriter.UUID;
                if (n.startsWith("interval")) return ChunkWriter.INTERVAL;
                if (n.contains("geometry") || n.contains("geography")) return ChunkWriter.GEOMETRY;
                return ChunkWriter.PLAIN;
        }
    }

    /** Column {@code col} (1-based) of the current row as a cell of its kind. */
    static Object read(ResultSet rs, int col, int type, String typeName) throws SQLException {
        if (textual(typeName)) {
            String text = rs.getString(col);
            if (rs.wasNull()) return null;
            return OBJECT_TO_STRING.matcher(text).matches() ? "‹unsupported " + typeName + "›" : text;
        }
        Object cell = value(rs, col, type, typeName);
        if (rs.wasNull()) return null;
        return cell;
    }

    private static Object value(ResultSet rs, int col, int type, String typeName) throws SQLException {
        switch (type) {
            case Types.BIT:
            case Types.BOOLEAN:
                return rs.getBoolean(col);
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT:
                return rs.getLong(col);
            case Types.FLOAT:
            case Types.REAL:
            case Types.DOUBLE:
                return rs.getDouble(col);
            case Types.DECIMAL:
            case Types.NUMERIC: {
                BigDecimal d = rs.getBigDecimal(col);
                return d == null ? null : d.toPlainString();
            }
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
                return rs.getBytes(col);
            case Types.BLOB: {
                Blob blob = rs.getBlob(col);
                if (blob == null) return null;
                try {
                    long n = Math.min(blob.length(), MAX_LOB);
                    return blob.getBytes(1, (int) n);
                } finally {
                    blob.free();
                }
            }
            case Types.CLOB:
            case Types.NCLOB: {
                Clob clob = rs.getClob(col);
                if (clob == null) return null;
                try {
                    long n = Math.min(clob.length(), MAX_LOB);
                    String text = clob.getSubString(1, (int) n);
                    return clob.length() > n ? text + "… (truncated at " + MAX_LOB + " characters)" : text;
                } finally {
                    clob.free();
                }
            }
            case Types.SQLXML: {
                SQLXML xml = rs.getSQLXML(col);
                return xml == null ? null : xml.getString();
            }
            case Types.TIMESTAMP_WITH_TIMEZONE: {
                OffsetDateTime t = rs.getObject(col, OffsetDateTime.class);
                return t == null ? null : t.toString();
            }
            case Types.TIME_WITH_TIMEZONE: {
                OffsetTime t = rs.getObject(col, OffsetTime.class);
                return t == null ? null : t.toString();
            }
            case Types.DATE: {
                java.sql.Date d = rs.getDate(col);
                return d == null ? null : d.toLocalDate().toString();
            }
            case Types.TIME: {
                java.sql.Time t = rs.getTime(col);
                return t == null ? null : t.toLocalTime().toString();
            }
            case Types.TIMESTAMP: {
                java.sql.Timestamp t = rs.getTimestamp(col);
                return t == null ? null : t.toLocalDateTime().toString();
            }
            case Types.ARRAY: {
                Array array = rs.getArray(col);
                if (array == null) return null;
                try {
                    return formatArray(array.getArray());
                } finally {
                    array.free();
                }
            }
            default:
                return other(rs, col, typeName);
        }
    }

    /** Strings, numbers, booleans and arrays are shown; any other driver object is not guessed at. */
    private static Object other(ResultSet rs, int col, String typeName) throws SQLException {
        Object o = rs.getObject(col);
        if (o == null) return null;
        if (o instanceof String) return o;
        if (o instanceof byte[]) return o;
        if (o instanceof Boolean) return o.toString();
        if (o instanceof Number || o instanceof java.util.UUID) return o.toString();
        if (o instanceof java.time.temporal.TemporalAccessor) return o.toString();
        if (o instanceof java.sql.Array) {
            java.sql.Array a = (java.sql.Array) o;
            try {
                return formatArray(a.getArray());
            } finally {
                a.free();
            }
        }
        String text = rs.getString(col);
        if (text == null || OBJECT_TO_STRING.matcher(text).matches()) {
            return "‹unsupported " + (typeName == null || typeName.isEmpty() ? o.getClass().getSimpleName() : typeName) + "›";
        }
        return text;
    }

    /** {@code [1, 2, NULL, 'x']}-style text for any Java array of values. */
    static String formatArray(Object array) {
        if (array == null) return "NULL";
        if (!array.getClass().isArray()) return element(array);
        StringBuilder sb = new StringBuilder("[");
        int n = java.lang.reflect.Array.getLength(array);
        for (int i = 0; i < n; i++) {
            if (i > 0) sb.append(", ");
            Object e = java.lang.reflect.Array.get(array, i);
            sb.append(e != null && e.getClass().isArray() ? formatArray(e) : element(e));
        }
        return sb.append(']').toString();
    }

    private static String element(Object e) {
        if (e == null) return "NULL";
        if (e instanceof String) return "'" + ((String) e).replace("'", "''") + "'";
        if (e instanceof byte[]) {
            StringBuilder sb = new StringBuilder("0x");
            for (byte b : (byte[]) e) sb.append(String.format("%02x", b));
            return sb.toString();
        }
        String s = e.toString();
        return OBJECT_TO_STRING.matcher(s).matches() ? "‹unsupported " + e.getClass().getSimpleName() + "›" : s;
    }
}
