//! The one thing the launcher says itself: when the bridge cannot start (no Java), it
//! answers the handshake with an error carrying the reason, so the user reads it in the
//! Driver Manager or the connection dialog instead of "driver exited unexpectedly".

use std::io::{Read, Write};

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    match b.len() {
        n if n < 32 => out.push(0xa0 | n as u8),
        n if n < 256 => out.extend([0xd9, n as u8]),
        n if n < 65536 => out.extend([0xda, (n >> 8) as u8, n as u8]),
        n => {
            out.push(0xdb);
            out.extend((n as u32).to_be_bytes());
        }
    }
    out.extend(b);
}

/// MessagePack of `{"Error": {"kind": "Connection", "message": …, "position": nil, "hint": nil}}`.
fn error_message(message: &str) -> Vec<u8> {
    let mut out = vec![0x81];
    put_str(&mut out, "Error");
    out.push(0x84);
    put_str(&mut out, "kind");
    put_str(&mut out, "Connection");
    put_str(&mut out, "message");
    put_str(&mut out, message);
    put_str(&mut out, "position");
    out.push(0xc0);
    put_str(&mut out, "hint");
    out.push(0xc0);
    out
}

/// Reads requests from `input` and answers each with `message` as an error, until the pipe
/// closes or a `Shutdown` arrives.
pub fn answer_with_error(
    mut input: impl Read,
    mut output: impl Write,
    message: &str,
) -> std::io::Result<()> {
    let payload = error_message(message);
    loop {
        let mut head = [0u8; 4];
        if input.read_exact(&mut head).is_err() {
            return Ok(());
        }
        let len = u32::from_be_bytes(head) as usize;
        if !(9..=64 * 1024 * 1024).contains(&len) {
            return Ok(());
        }
        let mut body = Vec::new();
        input.by_ref().take(len as u64).read_to_end(&mut body)?;
        if body.len() < len {
            return Ok(());
        }
        let shutdown = body[9..].starts_with(&[0xa8]) && &body[10..] == b"Shutdown";
        let mut frame = Vec::with_capacity(13 + payload.len());
        frame.extend(((9 + payload.len()) as u32).to_be_bytes());
        frame.push(0x01);
        frame.extend_from_slice(&body[1..9]); // the request's id
        frame.extend_from_slice(&payload);
        output.write_all(&frame)?;
        output.flush()?;
        if shutdown {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = ((9 + payload.len()) as u32).to_be_bytes().to_vec();
        out.push(0x01);
        out.extend(id.to_be_bytes());
        out.extend(payload);
        out
    }

    #[test]
    fn handshake_gets_the_reason() {
        let mut input = request(7, b"\x81\xa5Hello\x80");
        input.extend(request(8, b"\xa8Shutdown"));
        let mut output = Vec::new();
        answer_with_error(&input[..], &mut output, "no Java here").unwrap();
        // Two answers, with the requests' ids, each carrying the message.
        assert_eq!(&output[5..13], &7u64.to_be_bytes());
        let text = String::from_utf8_lossy(&output).to_string();
        assert_eq!(text.matches("no Java here").count(), 2, "{text:?}");
        assert!(text.contains("Connection"));
    }

    #[test]
    fn garbage_ends_quietly() {
        let mut output = Vec::new();
        answer_with_error(&b"\xff\xff\xff\xff"[..], &mut output, "x").unwrap();
        assert!(output.is_empty());
    }
}
