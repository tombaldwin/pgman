//! PostgreSQL's COPY text format, both ways: a row is fields separated by
//! tabs and ended by a newline, `\N` is NULL, and a backslash escapes
//! (`\t`, `\n`, `\\`, octal `\123`, hex `\x41`, and the rest). Values are
//! bytes, as the server sent them. Pure.

/// One row's fields; `None` is NULL.
pub type Row = Vec<Option<Vec<u8>>>;

/// One line (without its newline) as fields. Pure.
pub fn decode(line: &[u8]) -> Row {
    let mut fields = Vec::new();
    for raw in line.split(|b| *b == b'\t') {
        if raw == b"\\N" {
            fields.push(None);
            continue;
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            let b = raw[i];
            if b != b'\\' || i + 1 == raw.len() {
                out.push(b);
                i += 1;
                continue;
            }
            let c = raw[i + 1];
            i += 2;
            match c {
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'v' => out.push(11),
                b'0'..=b'7' => {
                    let mut v = u32::from(c - b'0');
                    for _ in 0..2 {
                        match raw.get(i) {
                            Some(d @ b'0'..=b'7') => {
                                v = v * 8 + u32::from(d - b'0');
                                i += 1;
                            }
                            _ => break,
                        }
                    }
                    out.push((v & 0xff) as u8);
                }
                b'x' if raw.get(i).is_some_and(u8::is_ascii_hexdigit) => {
                    let mut v = 0u32;
                    for _ in 0..2 {
                        match raw.get(i).and_then(|d| (*d as char).to_digit(16)) {
                            Some(d) => {
                                v = v * 16 + d;
                                i += 1;
                            }
                            None => break,
                        }
                    }
                    out.push(v as u8);
                }
                other => out.push(other),
            }
        }
        fields.push(Some(out));
    }
    fields
}

/// Fields as one line, with its newline. Pure.
pub fn encode(row: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, f) in row.iter().enumerate() {
        if n > 0 {
            out.push(b'\t');
        }
        match f {
            None => out.extend_from_slice(b"\\N"),
            Some(v) => {
                for &b in v {
                    match b {
                        b'\\' => out.extend_from_slice(b"\\\\"),
                        b'\t' => out.extend_from_slice(b"\\t"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        b'\r' => out.extend_from_slice(b"\\r"),
                        _ => out.push(b),
                    }
                }
            }
        }
    }
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<Vec<u8>> {
        Some(v.as_bytes().to_vec())
    }

    #[test]
    fn escapes_decode() {
        assert_eq!(
            decode(b"1\t\\N\ta\\tb\\nc\\\\d\t\\101\\x42\\q\t"),
            vec![s("1"), None, s("a\tb\nc\\d"), s("ABq"), s("")]
        );
        assert_eq!(
            decode(b"\\\\N"),
            vec![s("\\N")],
            "an escaped backslash isn't NULL"
        );
        assert_eq!(decode(b"\\x"), vec![s("x")]);
    }

    #[test]
    fn a_row_round_trips() {
        let row = vec![
            s("tab\there"),
            None,
            s("new\nline\r"),
            s("back\\slash"),
            s(""),
            s("ünï"),
        ];
        let line = encode(&row);
        assert_eq!(line.last(), Some(&b'\n'));
        assert!(!line[..line.len() - 1].contains(&b'\n'));
        assert_eq!(decode(&line[..line.len() - 1]), row);
    }
}
