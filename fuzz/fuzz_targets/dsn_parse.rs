//! Fuzz `Dsn::parse`. The function takes a `postgres://` connection
//! string from CLI / project config / IntelliJ XML / Spring YAML —
//! all sources we don't control. Any panic here is a real bug.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pgman::conn::Dsn::parse(s);
    }
});
