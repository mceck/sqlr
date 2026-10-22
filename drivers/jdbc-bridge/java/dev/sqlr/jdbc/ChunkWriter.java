package dev.sqlr.jdbc;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.BitSet;
import java.util.List;

/** Encodes result rows in sqlr's chunk format (crates/sqlr-core/src/wire.rs). */
final class ChunkWriter {
    static final int BOOL = 0, INT = 1, FLOAT = 2, TEXT = 3, BYTES = 4, MIXED = 5;
    static final int PLAIN = 0, NUMBER = 1, DECIMAL = 2, TEMPORAL = 3, INTERVAL = 4, JSON = 5,
            XML = 6, UUID = 7, ARRAY = 8, GEOMETRY = 9, BINARY = 10;
    static final String[] KIND_NAMES = {"Bool", "Int", "Float", "Text", "Bytes", "Mixed"};
    static final String[] HINT_NAMES = {"Plain", "Number", "Decimal", "Temporal", "Interval", "Json",
            "Xml", "Uuid", "Array", "Geometry", "Binary"};

    static final class Column {
        final String name, typeName, schema, table, origin;
        final int kind, hint;

        Column(String name, String typeName, int kind, int hint, String schema, String table, String origin) {
            this.name = name;
            this.typeName = typeName;
            this.kind = kind;
            this.hint = hint;
            this.schema = schema;
            this.table = table;
            this.origin = origin;
        }
    }

    private final List<Column> columns;
    private final BitSet[] validity;
    private final ByteArrayOutputStream[] values;
    private int rows;

    ChunkWriter(List<Column> columns) {
        this.columns = columns;
        this.validity = new BitSet[columns.size()];
        this.values = new ByteArrayOutputStream[columns.size()];
        for (int i = 0; i < columns.size(); i++) {
            validity[i] = new BitSet();
            values[i] = new ByteArrayOutputStream();
        }
    }

    int rows() { return rows; }

    /** Bytes of cell data held so far (what the chunk will weigh, roughly). */
    long approxBytes() {
        long n = 0;
        for (ByteArrayOutputStream v : values) n += v.size();
        return n;
    }

    /** One row; each cell is null, Boolean, Long, Double, String or byte[] and matches its column's kind. */
    void addRow(Object[] cells) {
        for (int c = 0; c < cells.length; c++) {
            Object v = cells[c];
            if (v == null) continue;
            validity[c].set(rows);
            ByteArrayOutputStream out = values[c];
            switch (columns.get(c).kind) {
                case BOOL: out.write(((Boolean) v) ? 1 : 0); break;
                case INT: putLong(out, (Long) v); break;
                case FLOAT: putLong(out, Double.doubleToRawLongBits((Double) v)); break;
                case TEXT: putBytes(out, ((String) v).getBytes(StandardCharsets.UTF_8)); break;
                case BYTES: putBytes(out, (byte[]) v); break;
                default: putMixed(out, v);
            }
        }
        rows++;
    }

    byte[] finish() {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        out.write(1);
        varint(out, columns.size());
        for (Column c : columns) {
            putBytes(out, c.name.getBytes(StandardCharsets.UTF_8));
            putBytes(out, c.typeName.getBytes(StandardCharsets.UTF_8));
            out.write(c.kind);
            out.write(c.hint);
            if (c.table == null || c.table.isEmpty()) {
                out.write(0);
            } else {
                out.write(1);
                out.write(0); // no database
                optional(out, c.schema);
                putBytes(out, c.table.getBytes(StandardCharsets.UTF_8));
                putBytes(out, (c.origin == null ? c.name : c.origin).getBytes(StandardCharsets.UTF_8));
            }
        }
        varint(out, rows);
        for (int c = 0; c < columns.size(); c++) {
            byte[] bits = new byte[(rows + 7) / 8];
            for (int r = validity[c].nextSetBit(0); r >= 0; r = validity[c].nextSetBit(r + 1)) {
                bits[r / 8] |= (byte) (1 << (r % 8));
            }
            out.write(bits, 0, bits.length);
            varint(out, values[c].size());
            byte[] v = values[c].toByteArray();
            out.write(v, 0, v.length);
        }
        return out.toByteArray();
    }

    private static void optional(ByteArrayOutputStream out, String s) {
        if (s == null || s.isEmpty()) {
            out.write(0);
        } else {
            out.write(1);
            putBytes(out, s.getBytes(StandardCharsets.UTF_8));
        }
    }

    private static void putMixed(ByteArrayOutputStream out, Object v) {
        if (v instanceof Boolean) { out.write(1); out.write((Boolean) v ? 1 : 0); }
        else if (v instanceof Long) { out.write(2); putLong(out, (Long) v); }
        else if (v instanceof Double) { out.write(3); putLong(out, Double.doubleToRawLongBits((Double) v)); }
        else if (v instanceof byte[]) { out.write(5); putBytes(out, (byte[]) v); }
        else { out.write(4); putBytes(out, String.valueOf(v).getBytes(StandardCharsets.UTF_8)); }
    }

    private static void putLong(ByteArrayOutputStream out, long n) {
        byte[] b = ByteBuffer.allocate(8).order(java.nio.ByteOrder.LITTLE_ENDIAN).putLong(n).array();
        out.write(b, 0, 8);
    }

    private static void putBytes(ByteArrayOutputStream out, byte[] b) {
        varint(out, b.length);
        out.write(b, 0, b.length);
    }

    static void varint(ByteArrayOutputStream out, long n) {
        while (true) {
            int b = (int) (n & 0x7f);
            n >>>= 7;
            if (n == 0) { out.write(b); return; }
            out.write(b | 0x80);
        }
    }

    static List<Object> meta(List<Column> columns) {
        List<Object> list = new ArrayList<>();
        for (Column c : columns) {
            Object origin = null;
            if (c.table != null && !c.table.isEmpty()) {
                origin = MsgPack.map(
                        "table", MsgPack.map("database", null, "schema", c.schema == null || c.schema.isEmpty() ? null : c.schema, "name", c.table),
                        "column", c.origin == null ? c.name : c.origin);
            }
            list.add(MsgPack.map("name", c.name, "type_name", c.typeName, "kind", KIND_NAMES[c.kind],
                    "hint", HINT_NAMES[c.hint], "origin", origin));
        }
        return list;
    }
}
