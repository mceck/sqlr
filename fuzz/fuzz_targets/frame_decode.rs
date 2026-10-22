#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sqlr_sidecar::frame::decode_frame(data);
    // Control messages from a driver are untrusted too.
    let _ = sqlr_sidecar::messages::decode::<sqlr_sidecar::Response>(data);
    let _ = sqlr_sidecar::messages::decode::<sqlr_sidecar::Request>(data);
});
