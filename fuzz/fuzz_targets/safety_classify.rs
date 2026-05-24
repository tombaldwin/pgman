//! Fuzz the safety classifier. Any panic here means a SQL string
//! could potentially bypass the safety guard — high-stakes path.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pgman::safety::classify(s);
        let _ = pgman::safety::split_statements(s);
    }
});
