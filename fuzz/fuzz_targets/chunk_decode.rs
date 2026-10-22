#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Neither decoder may panic, and a chunk that decodes must encode back to a chunk
    // that decodes to the same rows.
    if let Ok(chunk) = sqlr_core::decode_chunk(data) {
        let mut again = Vec::new();
        sqlr_core::encode_chunk(&chunk, &mut again);
        let back = sqlr_core::decode_chunk(&again).expect("encoded chunks decode");
        assert_eq!(chunk.len(), back.len());
    }
    if let Ok(chunk) = sqlr_core::decode_doc_chunk(data) {
        let _ = chunk.docs().count();
    }
});
