package dev.sqlr.jdbc;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The part of MessagePack the sidecar protocol uses. Values are null, Boolean, Long, Double,
 * String, byte[], List and Map (string keys). Hostile input fails with an
 * IllegalArgumentException; no length is trusted before the buffer holds that many bytes.
 */
final class MsgPack {
    private MsgPack() {}

    private static final int MAX_DEPTH = 64;

    // ----- reading -----

    static Object read(byte[] bytes) {
        ByteBuffer in = ByteBuffer.wrap(bytes);
        Object value = read(in, 0);
        if (in.hasRemaining()) {
            throw new IllegalArgumentException("bytes after the message");
        }
        return value;
    }

    private static Object read(ByteBuffer in, int depth) {
        if (depth > MAX_DEPTH) {
            throw new IllegalArgumentException("message nests too deeply");
        }
        int b = u8(in);
        if (b < 0x80) return (long) b;
        if (b >= 0xe0) return (long) (byte) b;
        if (b >= 0xa0 && b <= 0xbf) return str(in, b & 0x1f);
        if (b >= 0x90 && b <= 0x9f) return array(in, b & 0x0f, depth);
        if (b >= 0x80 && b <= 0x8f) return map(in, b & 0x0f, depth);
        switch (b) {
            case 0xc0: return null;
            case 0xc2: return Boolean.FALSE;
            case 0xc3: return Boolean.TRUE;
            case 0xc4: return bin(in, u8(in));
            case 0xc5: return bin(in, u16(in));
            case 0xc6: return bin(in, u32(in));
            case 0xca: need(in, 4); return (double) in.getFloat();
            case 0xcb: need(in, 8); return in.getDouble();
            case 0xcc: return (long) u8(in);
            case 0xcd: return (long) u16(in);
            case 0xce: return (long) u32(in);
            case 0xcf: need(in, 8); return in.getLong();
            case 0xd0: need(in, 1); return (long) in.get();
            case 0xd1: need(in, 2); return (long) in.getShort();
            case 0xd2: need(in, 4); return (long) in.getInt();
            case 0xd3: need(in, 8); return in.getLong();
            case 0xd9: return str(in, u8(in));
            case 0xda: return str(in, u16(in));
            case 0xdb: return str(in, u32(in));
            case 0xdc: return array(in, u16(in), depth);
            case 0xdd: return array(in, u32(in), depth);
            case 0xde: return map(in, u16(in), depth);
            case 0xdf: return map(in, u32(in), depth);
            default: throw new IllegalArgumentException("unsupported MessagePack type 0x" + Integer.toHexString(b));
        }
    }

    private static void need(ByteBuffer in, long n) {
        if (n < 0 || n > in.remaining()) {
            throw new IllegalArgumentException("the message ends early");
        }
    }

    private static int u8(ByteBuffer in) { need(in, 1); return in.get() & 0xff; }
    private static int u16(ByteBuffer in) { need(in, 2); return in.getShort() & 0xffff; }
    private static int u32(ByteBuffer in) {
        need(in, 4);
        long n = in.getInt() & 0xffffffffL;
        if (n > Integer.MAX_VALUE) throw new IllegalArgumentException("length too large");
        return (int) n;
    }

    private static String str(ByteBuffer in, int n) {
        need(in, n);
        byte[] b = new byte[n];
        in.get(b);
        return new String(b, StandardCharsets.UTF_8);
    }

    private static byte[] bin(ByteBuffer in, int n) {
        need(in, n);
        byte[] b = new byte[n];
        in.get(b);
        return b;
    }

    private static List<Object> array(ByteBuffer in, int n, int depth) {
        // Every element takes at least one byte.
        need(in, n);
        List<Object> list = new ArrayList<>(Math.min(n, 1024));
        for (int i = 0; i < n; i++) list.add(read(in, depth + 1));
        return list;
    }

    private static Map<String, Object> map(ByteBuffer in, int n, int depth) {
        need(in, 2L * n);
        Map<String, Object> map = new LinkedHashMap<>();
        for (int i = 0; i < n; i++) {
            Object key = read(in, depth + 1);
            map.put(String.valueOf(key), read(in, depth + 1));
        }
        return map;
    }

    // ----- writing -----

    static byte[] write(Object value) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        write(out, value);
        return out.toByteArray();
    }

    @SuppressWarnings("unchecked")
    private static void write(ByteArrayOutputStream out, Object v) {
        if (v == null) {
            out.write(0xc0);
        } else if (v instanceof Boolean) {
            out.write((Boolean) v ? 0xc3 : 0xc2);
        } else if (v instanceof Integer || v instanceof Long || v instanceof Short || v instanceof Byte) {
            writeLong(out, ((Number) v).longValue());
        } else if (v instanceof Float || v instanceof Double) {
            out.write(0xcb);
            long bits = Double.doubleToRawLongBits(((Number) v).doubleValue());
            for (int i = 7; i >= 0; i--) out.write((int) (bits >>> (8 * i)) & 0xff);
        } else if (v instanceof String) {
            byte[] b = ((String) v).getBytes(StandardCharsets.UTF_8);
            if (b.length < 32) out.write(0xa0 | b.length);
            else if (b.length < 256) { out.write(0xd9); out.write(b.length); }
            else if (b.length < 65536) { out.write(0xda); out.write(b.length >> 8); out.write(b.length); }
            else { out.write(0xdb); writeInt(out, b.length); }
            out.write(b, 0, b.length);
        } else if (v instanceof byte[]) {
            byte[] b = (byte[]) v;
            out.write(0xc6);
            writeInt(out, b.length);
            out.write(b, 0, b.length);
        } else if (v instanceof List) {
            List<Object> l = (List<Object>) v;
            if (l.size() < 16) out.write(0x90 | l.size());
            else if (l.size() < 65536) { out.write(0xdc); out.write(l.size() >> 8); out.write(l.size()); }
            else { out.write(0xdd); writeInt(out, l.size()); }
            for (Object o : l) write(out, o);
        } else if (v instanceof Map) {
            Map<String, Object> m = (Map<String, Object>) v;
            if (m.size() < 16) out.write(0x80 | m.size());
            else { out.write(0xde); out.write(m.size() >> 8); out.write(m.size()); }
            for (Map.Entry<String, Object> e : m.entrySet()) {
                write(out, e.getKey());
                write(out, e.getValue());
            }
        } else {
            throw new IllegalArgumentException("cannot encode " + v.getClass());
        }
    }

    private static void writeLong(ByteArrayOutputStream out, long n) {
        if (n >= 0 && n < 128) { out.write((int) n); return; }
        if (n < 0 && n >= -32) { out.write((int) n & 0xff); return; }
        out.write(0xd3);
        for (int i = 7; i >= 0; i--) out.write((int) (n >>> (8 * i)) & 0xff);
    }

    private static void writeInt(ByteArrayOutputStream out, int n) {
        out.write(n >>> 24); out.write(n >>> 16); out.write(n >>> 8); out.write(n);
    }

    /** Tag-style helper: the one-entry map serde writes for a data-carrying enum variant. */
    static Map<String, Object> variant(String name, Object body) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put(name, body);
        return m;
    }

    static Map<String, Object> map(Object... kv) {
        Map<String, Object> m = new LinkedHashMap<>();
        for (int i = 0; i < kv.length; i += 2) m.put((String) kv[i], kv[i + 1]);
        return m;
    }
}
