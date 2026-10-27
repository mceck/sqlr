//! Framing: hostile bytes from a driver's stdout never panic or allocate the claimed size.

use sqlr_sidecar::frame::{
    CHUNK, CONTROL, FrameError, MAX_FRAME, decode_frame, encode_frame, read_frame,
};

fn frame(ty: u8, id: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_frame(ty, id, payload, &mut out).unwrap();
    out
}

#[test]
fn frames_round_trip() {
    let bytes = frame(CHUNK, 0xdead_beef_0102, b"payload");
    let (f, used) = decode_frame(&bytes).unwrap();
    assert_eq!(
        (f.ty, f.id, f.payload.as_slice()),
        (CHUNK, 0xdead_beef_0102, &b"payload"[..])
    );
    assert_eq!(used, bytes.len());
    let (empty, _) = decode_frame(&frame(CONTROL, 0, b"")).unwrap();
    assert!(empty.payload.is_empty());
}

#[test]
fn frame_decoder_rejects_malformed_input() {
    let good = frame(CONTROL, 7, b"abc");
    for len in 0..good.len() {
        assert!(decode_frame(&good[..len]).is_err(), "prefix {len}");
    }
    // Length shorter than the header.
    assert!(matches!(
        decode_frame(&[0, 0, 0, 3, 1, 0, 0]),
        Err(FrameError::TooShort(3))
    ));
    // Length past the limit, with nothing behind it.
    let huge = ((MAX_FRAME + 1) as u32).to_be_bytes();
    assert!(matches!(decode_frame(&huge), Err(FrameError::TooLarge(_))));
    let max = (u32::MAX).to_be_bytes();
    assert!(matches!(decode_frame(&max), Err(FrameError::TooLarge(_))));
    // Unknown frame type.
    let mut unknown = good;
    unknown[4] = 0x7f;
    assert!(matches!(
        decode_frame(&unknown),
        Err(FrameError::UnknownType(0x7f))
    ));
    // The encoder refuses what the decoder would.
    let mut out = Vec::new();
    assert!(matches!(
        encode_frame(CHUNK, 1, &vec![0u8; MAX_FRAME], &mut out),
        Err(FrameError::TooLarge(_))
    ));
}

#[tokio::test]
async fn reader_never_allocates_the_claimed_length() {
    // A header claiming 60 MiB, then the pipe closes: the reader fails after reading what
    // arrived (a few bytes), not after reserving 60 MiB.
    let mut bytes = ((60 * 1024 * 1024) as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(&[CONTROL, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 3]);
    let mut reader = &bytes[..];
    assert!(matches!(
        read_frame(&mut reader).await,
        Err(FrameError::Truncated)
    ));
    // Clean EOF between frames is not an error; EOF inside a header is.
    let mut empty = &[][..];
    assert!(read_frame(&mut empty).await.unwrap().is_none());
    let mut partial = &[0u8, 0][..];
    assert!(matches!(
        read_frame(&mut partial).await,
        Err(FrameError::Truncated)
    ));
    // Several frames in a row.
    let mut two = frame(CONTROL, 1, b"a");
    two.extend(frame(CHUNK, 2, b"bb"));
    let mut reader = &two[..];
    assert_eq!(read_frame(&mut reader).await.unwrap().unwrap().id, 1);
    assert_eq!(
        read_frame(&mut reader).await.unwrap().unwrap().payload,
        b"bb"
    );
    assert!(read_frame(&mut reader).await.unwrap().is_none());
}
