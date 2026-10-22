package dev.sqlr.jdbc;

import java.io.BufferedInputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The sqlr JDBC bridge: a sidecar driver (docs/driver-protocol.md) that serves any database
 * with a JDBC driver. stdout carries protocol frames only; everything else goes to stderr.
 * Arguments are {@code key=value} pairs describing the database (see {@link Preset}).
 */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        // Nothing but frames may reach the real stdout.
        OutputStream stdout = new FileOutputStream(FileDescriptor.out);
        System.setOut(System.err);
        InputStream stdin = new BufferedInputStream(new FileInputStream0());
        Bridge bridge = new Bridge(Preset.parse(args), stdout);
        ExecutorService pool = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r, "sqlr-jdbc-request");
            t.setDaemon(true);
            return t;
        });
        try {
            while (true) {
                Protocol.Frame frame = Protocol.read(stdin);
                if (frame == null) break;
                if (frame.type != Protocol.CONTROL) continue;
                final long id = frame.id;
                final Object message;
                try {
                    message = MsgPack.read(frame.payload);
                } catch (IllegalArgumentException e) {
                    Protocol.write(stdout, Protocol.CONTROL, id, MsgPack.write(MsgPack.variant("Error",
                            MsgPack.map("kind", "Protocol", "message", "malformed request: " + e.getMessage(), "position", null, "hint", null))));
                    continue;
                }
                if ("Shutdown".equals(message)) {
                    Protocol.write(stdout, Protocol.CONTROL, id, MsgPack.write("Ok"));
                    break;
                }
                pool.execute(() -> {
                    try {
                        bridge.handle(id, message);
                    } catch (IOException e) {
                        System.err.println("cannot answer: " + e);
                        System.exit(1);
                    }
                });
            }
        } finally {
            bridge.closeAll();
        }
        System.exit(0);
    }

    /** stdin as an unbuffered stream. */
    private static final class FileInputStream0 extends InputStream {
        private final InputStream in = new java.io.FileInputStream(FileDescriptor.in);

        @Override public int read() throws IOException { return in.read(); }
        @Override public int read(byte[] b, int off, int len) throws IOException { return in.read(b, off, len); }
    }
}
