//! Fuzz the Hibernate log reconstructor. Operators paste in
//! whatever their log shipping spit out; we shouldn't panic on
//! truncated / corrupt / hostile log slices.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pgman::query::hibernate::parse(s);
    }
});
