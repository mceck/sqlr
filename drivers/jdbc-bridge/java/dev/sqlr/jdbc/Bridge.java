package dev.sqlr.jdbc;

import java.io.IOException;
import java.io.OutputStream;
import java.io.File;
import java.net.URL;
import java.net.URLClassLoader;
import java.sql.Connection;
import java.sql.Driver;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Properties;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

/** The driver side of the sidecar protocol over JDBC. One instance per process. */
final class Bridge {
    static final String PROTOCOL = "1.0.0";
    /** A chunk is sent once its cells weigh this much: well under the 64 MiB frame limit. */
    private static final long CHUNK_BUDGET = 24L * 1024 * 1024;

    private final Preset preset;
    private final OutputStream out;

    private final Map<Long, Session> sessions = new ConcurrentHashMap<>();
    private final Map<Long, Stream> streams = new ConcurrentHashMap<>();
    private final AtomicLong nextStream = new AtomicLong(1);

    Bridge(Preset preset, OutputStream out) {
        this.preset = preset;
        this.out = out;
    }

    private static final class Session {
        final Connection conn;
        final Meta meta;
        volatile Statement running;

        Session(Connection conn, Meta meta) {
            this.conn = conn;
            this.meta = meta;
        }
    }

    private static final class Stream {
        final long session;
        final Statement statement;
        final ResultSet rs;
        final List<ChunkWriter.Column> columns;
        final int[] types;
        final String[] typeNames;
        volatile boolean done;

        Stream(long session, Statement statement, ResultSet rs, List<ChunkWriter.Column> columns, int[] types, String[] typeNames) {
            this.session = session;
            this.statement = statement;
            this.rs = rs;
            this.columns = columns;
            this.types = types;
            this.typeNames = typeNames;
        }
    }

    /** A protocol-level failure carrying the {@code DriverError} to send. */
    static final class Failure extends Exception {
        private static final long serialVersionUID = 1L;
        final String kind;
        final Integer position;

        Failure(String kind, String message) {
            this(kind, message, null);
        }

        Failure(String kind, String message, Integer position) {
            super(message);
            this.kind = kind;
            this.position = position;
        }
    }

    // ----- one request -----

    /** Handles a control message; sends the answer (and chunk frames) itself. */
    void handle(long id, Object message) throws IOException {
        Object answer;
        try {
            answer = dispatch(id, message);
        } catch (Failure f) {
            answer = error(f.kind, f.getMessage(), f.position);
        } catch (SQLException e) {
            answer = error(kindOf(e), clean(e), null);
        } catch (RuntimeException e) {
            answer = error("Other", e.getClass().getSimpleName() + ": " + e.getMessage(), null);
        } catch (LinkageError e) {
            answer = error("Other", "the JDBC driver cannot load: " + e, null);
        }
        Protocol.write(out, Protocol.CONTROL, id, MsgPack.write(answer));
    }

    private static Object error(String kind, String message, Integer position) {
        return MsgPack.variant("Error", MsgPack.map("kind", kind, "message", message, "position", position, "hint", null));
    }

    static String clean(SQLException e) {
        String m = e.getMessage();
        return m == null || m.isEmpty() ? e.getClass().getSimpleName() : m;
    }

    static String kindOf(SQLException e) {
        String state = e.getSQLState() == null ? "" : e.getSQLState();
        if (state.equals("57014") || state.equals("HY008") || state.equals("70100") || state.equals("JZ0KB")) return "Cancelled";
        if (state.startsWith("28")) return "Auth";
        if (state.startsWith("08")) return "Connection";
        if (state.startsWith("42")) return state.equals("42501") ? "Permission" : "Syntax";
        if (state.startsWith("0A")) return "Unsupported";
        return "Other";
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> body(Object message, String name) {
        return (Map<String, Object>) ((Map<String, Object>) message).get(name);
    }

    private static long num(Map<String, Object> m, String key) {
        Object v = m.get(key);
        if (!(v instanceof Long)) throw new IllegalArgumentException("missing number " + key);
        return (Long) v;
    }

    private static String text(Map<String, Object> m, String key) {
        Object v = m.get(key);
        return v instanceof String ? (String) v : null;
    }

    @SuppressWarnings("unchecked")
    private Object dispatch(long id, Object message) throws Failure, SQLException, IOException {
        if (message instanceof String) {
            if (message.equals("Shutdown")) return "Ok";
            throw new Failure("Protocol", "unknown request " + message);
        }
        if (!(message instanceof Map) || ((Map<?, ?>) message).size() != 1) {
            throw new Failure("Protocol", "a request is a one-entry map");
        }
        String name = (String) ((Map<?, ?>) message).keySet().iterator().next();
        Map<String, Object> b = body(message, name);
        switch (name) {
            case "Hello": return MsgPack.variant("Welcome", MsgPack.map("protocol", PROTOCOL, "manifest", Manifest.of(preset)));
            case "Connect": return connect(num(b, "conn"), b);
            case "Ping": return MsgPack.variant("Pong", serverInfo(session(b).conn));
            case "Browse": return MsgPack.variant("Nodes", session(b).meta.browse(strings(b.get("path"))));
            case "Execute": return execute(session(b), num(b, "conn"), text(b, "sql"), (List<Object>) b.get("params"));
            case "Fetch": return fetch(id, num(b, "stream"), (int) num(b, "max"));
            case "CloseStream": closeStream(num(b, "stream")); return "Ok";
            case "Cancel": cancel(session(b)); return "Ok";
            case "Describe": {
                Map<String, Object> table = (Map<String, Object>) b.get("table");
                return MsgPack.variant("Table", session(b).meta.describe(text(table, "schema"), text(table, "name")));
            }
            case "Catalog": return MsgPack.variant("Catalog", session(b).meta.catalog(text(b, "schema")));
            case "TakeNotices": return MsgPack.variant("Notices", new ArrayList<Object>());
            case "Disconnect": disconnect(num(b, "conn")); return "Ok";
            case "ConnectDatabase": throw new Failure("Unsupported", "this JDBC connection cannot open another database");
            default: throw new Failure("Protocol", "unknown request " + name);
        }
    }

    private Session session(Map<String, Object> b) throws Failure {
        Session s = sessions.get(num(b, "conn"));
        if (s == null) throw new Failure("Protocol", "no connection " + b.get("conn"));
        return s;
    }

    @SuppressWarnings("unchecked")
    private static List<String> strings(Object o) {
        List<String> out = new ArrayList<>();
        if (o instanceof List) for (Object e : (List<Object>) o) out.add(String.valueOf(e));
        return out;
    }

    private static Map<String, Object> serverInfo(Connection c) throws SQLException {
        java.sql.DatabaseMetaData md = c.getMetaData();
        c.isValid(5);
        return MsgPack.map("product", md.getDatabaseProductName(), "version", md.getDatabaseProductVersion());
    }

    // ----- connect -----

    @SuppressWarnings("unchecked")
    private Object connect(long conn, Map<String, Object> b) throws Failure, SQLException {
        Map<String, Object> config = (Map<String, Object>) b.get("config");
        Map<String, Object> values = (Map<String, Object>) config.get("values");
        Map<String, Object> secrets = (Map<String, Object>) b.get("secrets");
        String url = urlOf(values);
        String driverClass = preset.driverClass.isEmpty() ? str(values, "driver_class") : preset.driverClass;
        if (driverClass == null || driverClass.isEmpty()) throw new Failure("Other", "enter the JDBC driver class");
        Properties props = new Properties();
        String user = str(values, "user");
        if (user != null && !user.isEmpty()) props.setProperty("user", user);
        Object password = secrets == null ? null : secrets.get("password");
        if (password instanceof String && !((String) password).isEmpty()) props.setProperty("password", (String) password);

        Driver driver = load(driverClass, str(values, "jar"), str(values, "extra_jars"));
        if (!driver.acceptsURL(url)) {
            throw new Failure("Other", "the JDBC driver " + driverClass + " does not accept the URL " + url);
        }
        Connection c = driver.connect(url, props);
        if (c == null) throw new Failure("Connection", "the JDBC driver returned no connection for " + url);
        c.setAutoCommit(true);
        Session session = new Session(c, new Meta(c, preset));
        sessions.put(conn, session);
        String current = session.meta.currentSchema();
        return MsgPack.variant("Connected", MsgPack.map(
                "server_info", serverInfo(c),
                "dialect", Manifest.dialect(preset, c),
                "database_name", current,
                "schema_path", new ArrayList<Object>()));
    }

    private static String str(Map<String, Object> values, String key) {
        Object v = values.get(key);
        return v instanceof String && !((String) v).trim().isEmpty() ? ((String) v).trim() : null;
    }

    private String urlOf(Map<String, Object> values) throws Failure {
        if (preset.urlTemplate.isEmpty()) {
            String url = str(values, "jdbc_url");
            if (url == null) throw new Failure("Other", "enter the JDBC URL");
            return url;
        }
        String url = preset.urlTemplate;
        String port = str(values, "port");
        url = url.replace("{host}", orEmpty(str(values, "host")))
                .replace("{port}", port != null ? port : String.valueOf(preset.defaultPort))
                .replace("{database}", orEmpty(str(values, "database")))
                .replace("{path}", orEmpty(str(values, "path")));
        return url;
    }

    private static String orEmpty(String s) { return s == null ? "" : s; }

    /** The driver from the launcher's class path, or from jars the user named. */
    private static Driver load(String className, String jar, String extra) throws Failure {
        try {
            List<URL> urls = new ArrayList<>();
            if (jar != null) urls.add(new File(jar).toURI().toURL());
            if (extra != null) {
                for (String p : extra.split("[;:]")) if (!p.trim().isEmpty()) urls.add(new File(p.trim()).toURI().toURL());
            }
            ClassLoader loader = urls.isEmpty()
                    ? Bridge.class.getClassLoader()
                    : new URLClassLoader(urls.toArray(new URL[0]), Bridge.class.getClassLoader());
            return (Driver) Class.forName(className, true, loader).getDeclaredConstructor().newInstance();
        } catch (ClassNotFoundException e) {
            throw new Failure("Other", "the JDBC driver class " + className + " was not found: add its jar");
        } catch (ReflectiveOperationException | java.io.IOException | ClassCastException e) {
            throw new Failure("Other", "cannot load the JDBC driver " + className + ": " + e);
        }
    }

    private void disconnect(long conn) {
        Session s = sessions.remove(conn);
        List<Long> owned = new ArrayList<>();
        for (Map.Entry<Long, Stream> e : streams.entrySet()) if (e.getValue().session == conn) owned.add(e.getKey());
        for (Long id : owned) closeStream(id);
        if (s != null) {
            try { s.conn.close(); } catch (SQLException ignored) { }
        }
    }

    void closeAll() {
        for (Long conn : new ArrayList<>(sessions.keySet())) disconnect(conn);
    }

    // ----- statements -----

    private void cancel(Session s) throws SQLException {
        Statement st = s.running;
        if (st != null) st.cancel();
    }

    @SuppressWarnings("unchecked")
    private Object execute(Session s, long conn, String sql, List<Object> params) throws SQLException, Failure {
        if (params == null || params.isEmpty()) {
            String command = sql.trim();
            if (command.endsWith(";")) command = command.substring(0, command.length() - 1).trim();
            // BEGIN is not SQL on every JDBC database (for example Derby and Oracle).
            // End our explicit transactions by restoring the normal autocommit mode.
            switch (command.toUpperCase(Locale.ROOT)) {
                case "BEGIN":
                    s.conn.setAutoCommit(false);
                    return MsgPack.variant("Affected", 0L);
                case "COMMIT":
                    if (s.conn.getAutoCommit()) break;
                    s.conn.commit();
                    s.conn.setAutoCommit(true);
                    return MsgPack.variant("Affected", 0L);
                case "ROLLBACK":
                    if (s.conn.getAutoCommit()) break;
                    s.conn.rollback();
                    s.conn.setAutoCommit(true);
                    return MsgPack.variant("Affected", 0L);
                default: break;
            }
        }
        Statement st;
        boolean hasResult;
        if (params == null) {
            st = s.conn.createStatement();
            st.setFetchSize(500);
            s.running = st;
            try {
                hasResult = st.execute(sql);
            } finally {
                s.running = null;
            }
        } else {
            PreparedStatement ps = s.conn.prepareStatement(sql);
            ps.setFetchSize(500);
            for (int i = 0; i < params.size(); i++) bind(ps, i + 1, params.get(i));
            st = ps;
            s.running = st;
            try {
                hasResult = ps.execute();
            } finally {
                s.running = null;
            }
        }
        if (!hasResult) {
            long n = Math.max(st.getLargeUpdateCount(), 0);
            st.close();
            return MsgPack.variant("Affected", n);
        }
        ResultSet rs = st.getResultSet();
        ResultSetMetaData md = rs.getMetaData();
        int n = md.getColumnCount();
        List<ChunkWriter.Column> columns = new ArrayList<>();
        int[] types = new int[n];
        String[] typeNames = new String[n];
        for (int col = 1; col <= n; col++) {
            final int i = col;
            types[i - 1] = md.getColumnType(i);
            typeNames[i - 1] = md.getColumnTypeName(i);
            String table = safe(() -> md.getTableName(i));
            columns.add(new ChunkWriter.Column(md.getColumnLabel(i), typeNames[i - 1],
                    JdbcTypes.kindOf(types[i - 1], typeNames[i - 1]), JdbcTypes.hintOf(types[i - 1], typeNames[i - 1]),
                    safe(() -> md.getSchemaName(i)), table, safe(() -> md.getColumnName(i))));
        }
        long stream = nextStream.getAndIncrement();
        streams.put(stream, new Stream(conn, st, rs, columns, types, typeNames));
        return MsgPack.variant("Rows", MsgPack.map("stream", stream, "columns", ChunkWriter.meta(columns)));
    }

    private interface Getter { String get() throws SQLException; }

    private static String safe(Getter g) {
        try {
            String v = g.get();
            return v == null || v.isEmpty() ? null : v;
        } catch (SQLException | RuntimeException e) {
            return null;
        }
    }

    @SuppressWarnings("unchecked")
    private static void bind(PreparedStatement ps, int index, Object value) throws SQLException {
        if (value instanceof String) {
            ps.setNull(index, java.sql.Types.NULL);
            return;
        }
        if (!(value instanceof Map) || ((Map<?, ?>) value).size() != 1) {
            throw new SQLException("bad parameter " + index);
        }
        Map.Entry<String, Object> e = ((Map<String, Object>) value).entrySet().iterator().next();
        Object v = e.getValue();
        switch (e.getKey()) {
            case "Bool": ps.setBoolean(index, (Boolean) v); break;
            case "Int": ps.setLong(index, (Long) v); break;
            case "Float": ps.setDouble(index, (Double) v); break;
            case "Text": ps.setString(index, (String) v); break;
            case "Bytes": {
                List<Object> list = (List<Object>) v;
                byte[] bytes = new byte[list.size()];
                for (int i = 0; i < bytes.length; i++) bytes[i] = (byte) (long) (Long) list.get(i);
                ps.setBytes(index, bytes);
                break;
            }
            default: throw new SQLException("bad parameter " + index);
        }
    }

    private Object fetch(long id, long streamId, int max) throws Failure, SQLException, IOException {
        Stream stream = streams.get(streamId);
        if (stream == null) throw new Failure("Protocol", "no stream " + streamId);
        Session s = sessions.get(stream.session);
        ChunkWriter writer = new ChunkWriter(stream.columns);
        boolean exhausted = false;
        int n = stream.columns.size();
        int total = 0;
        if (s != null) s.running = stream.statement;
        try {
            while (total < Math.max(max, 1)) {
                if (stream.done || !stream.rs.next()) {
                    stream.done = true;
                    exhausted = true;
                    break;
                }
                Object[] cells = new Object[n];
                for (int c = 0; c < n; c++) cells[c] = JdbcTypes.read(stream.rs, c + 1, stream.types[c], stream.typeNames[c]);
                writer.addRow(cells);
                total++;
                // A frame holds 64 MiB: send a chunk whenever this one is big enough.
                if (writer.approxBytes() >= CHUNK_BUDGET) {
                    Protocol.write(out, Protocol.CHUNK, id, writer.finish());
                    writer = new ChunkWriter(stream.columns);
                }
            }
        } finally {
            if (s != null) s.running = null;
        }
        if (writer.rows() > 0) Protocol.write(out, Protocol.CHUNK, id, writer.finish());
        // Rows and the end in one call: report the rows now, the end on the next fetch.
        boolean finished = exhausted && total == 0;
        if (finished) closeStream(streamId);
        return MsgPack.variant("FetchDone", MsgPack.map("exhausted", finished));
    }

    private void closeStream(long streamId) {
        Stream stream = streams.remove(streamId);
        if (stream == null) return;
        try { stream.rs.close(); } catch (SQLException ignored) { }
        try { stream.statement.close(); } catch (SQLException ignored) { }
    }
}
