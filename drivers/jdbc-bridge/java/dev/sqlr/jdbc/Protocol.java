package dev.sqlr.jdbc;

import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/** Frames on stdin and stdout: u32 length, u8 type, u64 request id, payload (docs/driver-protocol.md). */
final class Protocol {
    private Protocol() {}

    static final int CONTROL = 0x01;
    static final int CHUNK = 0x02;
    static final int LOG = 0x04;
    static final int MAX_FRAME = 64 * 1024 * 1024;

    static final class Frame {
        final int type;
        final long id;
        final byte[] payload;

        Frame(int type, long id, byte[] payload) {
            this.type = type;
            this.id = id;
            this.payload = payload;
        }
    }

    /** The next frame, or null when the pipe closed between frames. */
    static Frame read(InputStream in) throws IOException {
        byte[] head = new byte[4];
        int first = in.read();
        if (first < 0) return null;
        head[0] = (byte) first;
        readFully(in, head, 1, 3);
        long length = ((head[0] & 0xffL) << 24) | ((head[1] & 0xff) << 16) | ((head[2] & 0xff) << 8) | (head[3] & 0xff);
        if (length < 9 || length > MAX_FRAME) {
            throw new IOException("bad frame length " + length);
        }
        // Read in pieces: a lying header costs nothing up front.
        ByteArrayOutputStream body = new ByteArrayOutputStream((int) Math.min(length, 65536));
        byte[] buf = new byte[65536];
        long left = length;
        while (left > 0) {
            int n = in.read(buf, 0, (int) Math.min(buf.length, left));
            if (n < 0) throw new EOFException("the pipe closed inside a frame");
            body.write(buf, 0, n);
            left -= n;
        }
        byte[] b = body.toByteArray();
        long id = 0;
        for (int i = 1; i < 9; i++) id = (id << 8) | (b[i] & 0xff);
        byte[] payload = new byte[b.length - 9];
        System.arraycopy(b, 9, payload, 0, payload.length);
        return new Frame(b[0] & 0xff, id, payload);
    }

    private static void readFully(InputStream in, byte[] b, int off, int len) throws IOException {
        while (len > 0) {
            int n = in.read(b, off, len);
            if (n < 0) throw new EOFException("the pipe closed inside a frame");
            off += n;
            len -= n;
        }
    }

    static synchronized void write(OutputStream out, int type, long id, byte[] payload) throws IOException {
        int length = 9 + payload.length;
        if (length > MAX_FRAME) throw new IOException("frame too large: " + length);
        byte[] head = new byte[13];
        head[0] = (byte) (length >>> 24);
        head[1] = (byte) (length >>> 16);
        head[2] = (byte) (length >>> 8);
        head[3] = (byte) length;
        head[4] = (byte) type;
        for (int i = 0; i < 8; i++) head[5 + i] = (byte) (id >>> (8 * (7 - i)));
        out.write(head);
        out.write(payload);
        out.flush();
    }
}
