//! Fuzz `.pgman/pgman.toml` parsing. The file is intended for git,
//! so it can come from anywhere — a bad commit shouldn't crash
//! pgman at startup.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pgman::project::parse(s);
    }
});
