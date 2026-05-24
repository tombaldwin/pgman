//! Fuzz the Postgres / RDS server-log reconstructor. Same exposure
//! shape as the Hibernate parser — pasted-in, externally-sourced.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pgman::query::pglog::parse(s);
    }
});
