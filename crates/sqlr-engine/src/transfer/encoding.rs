//! Opening text files in the encoding they were written in: UTF-8 as is, UTF-16 by its
//! byte-order mark, anything else (Latin-1, Windows-1252…) by the option's label, decoded
//! on the fly so memory stays bounded.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use encoding_rs::{Decoder, Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// `path` as UTF-8 text. `encoding` is a label (`utf-8`, `utf-16`, `latin1`,
/// `windows-1252`…); `None` detects a UTF-16 byte-order mark and otherwise reads UTF-8.
pub(super) fn open_text(
    path: &Path,
    encoding: Option<&str>,
) -> io::Result<Box<dyn BufRead + Send>> {
    let mut file = BufReader::new(File::open(path)?);
    let encoding = match encoding.map(str::trim).filter(|e| !e.is_empty()) {
        Some(label) => Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Unknown text encoding \"{label}\""),
            )
        })?,
        None => {
            let head = file.fill_buf()?;
            if head.starts_with(&[0xFF, 0xFE]) {
                UTF_16LE
            } else if head.starts_with(&[0xFE, 0xFF]) {
                UTF_16BE
            } else {
                UTF_8
            }
        }
    };
    if encoding == UTF_8 {
        // The readers skip a UTF-8 byte-order mark themselves.
        return Ok(Box::new(file));
    }
    Ok(Box::new(BufReader::new(Decoding {
        inner: file,
        decoder: encoding.new_decoder_with_bom_removal(),
        out: Vec::new(),
        pos: 0,
        done: false,
    })))
}

/// A reader of the UTF-8 form of an encoded stream.
struct Decoding<R> {
    inner: R,
    decoder: Decoder,
    out: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<R: BufRead> Read for Decoding<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.out.len() {
            if self.done {
                return Ok(0);
            }
            let src = self.inner.fill_buf()?;
            let last = src.is_empty();
            let mut text = String::with_capacity(
                self.decoder
                    .max_utf8_buffer_length(src.len())
                    .unwrap_or(src.len() * 4 + 16),
            );
            let (_, read, _) = self.decoder.decode_to_string(src, &mut text, last);
            self.inner.consume(read);
            self.done = last;
            self.out = text.into_bytes();
            self.pos = 0;
        }
        let n = buf.len().min(self.out.len() - self.pos);
        buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(bytes: &[u8], encoding: Option<&str>) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, bytes).unwrap();
        let mut text = String::new();
        open_text(&path, encoding)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    #[test]
    fn text_is_decoded_by_label_or_bom() {
        // Latin-1 by label.
        assert_eq!(read_all(b"caf\xe9", Some("latin1")), "café");
        // UTF-16 with a BOM, both byte orders, without any label.
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("é;ß\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(read_all(&le, None), "é;ß\n");
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("é;ß\n".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(read_all(&be, None), "é;ß\n");
        // UTF-8 stays as it is.
        assert_eq!(read_all("naïve".as_bytes(), None), "naïve");
        // An unknown label is an error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        assert!(open_text(&path, Some("klingon")).is_err());
    }
}
