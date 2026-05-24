//! Fuzz the syntax-highlighter tokenizer. Runs on every keystroke
//! in the editor; a panic on hostile input would crash the TUI
//! mid-typing. We also check the span-coverage invariant the
//! renderer relies on (no gaps, char-boundary aligned).

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let spans = pgman::query::highlight::tokenize(s);
        let mut next = 0;
        for sp in &spans {
            assert!(sp.start == next, "gap or overlap");
            assert!(sp.start <= sp.end);
            assert!(s.is_char_boundary(sp.start));
            assert!(s.is_char_boundary(sp.end));
            next = sp.end;
        }
        assert_eq!(next, s.len(), "spans don't cover whole buffer");
    }
});
