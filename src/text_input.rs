//! A minimal single-line editable text buffer shared by the modal
//! prompts (`:param` value entry, saved-query rename, saved-query
//! filter). Each prompt used to hand-roll append-only editing
//! (`Backspace`/`Char` only); this owns the text plus a cursor and the
//! editing operations once, so the prompts behave consistently and a
//! fix here (cursor movement, word-delete, paste) lands everywhere.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Single-line editable text with a cursor. `cursor` is a byte offset
/// into `buf`, always kept on a `char` boundary and within
/// `0..=buf.len()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    buf: String,
    cursor: usize,
}

impl TextInput {
    /// Empty buffer, cursor at the start.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with existing text, cursor at the end — the natural spot
    /// when editing a pre-filled value (e.g. a rename).
    pub fn with_text(text: impl Into<String>) -> Self {
        let buf = text.into();
        let cursor = buf.len();
        Self { buf, cursor }
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.buf
    }

    /// The text with surrounding whitespace trimmed (for accept-time
    /// validation).
    pub fn trimmed(&self) -> &str {
        self.buf.trim()
    }

    /// True when the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Empty the buffer and reset the cursor to the start.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    /// Display column of the cursor — the number of characters before
    /// it. Renderers add this to the input box's left edge to place the
    /// terminal cursor.
    pub fn cursor_col(&self) -> usize {
        self.buf[..self.cursor].chars().count()
    }

    /// Insert a character at the cursor, advancing past it.
    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a string at the cursor (paste), advancing past it.
    pub fn insert_str(&mut self, s: &str) {
        self.buf.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the char before the cursor (Backspace). No-op at the start.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char_len();
        self.cursor -= prev;
        self.buf.remove(self.cursor);
    }

    /// Delete the char at the cursor (Delete). No-op at the end.
    pub fn delete_forward(&mut self) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
        }
    }

    /// Delete the word before the cursor (Ctrl-W): the run of trailing
    /// spaces, then the run of non-space chars up to the previous space.
    pub fn delete_word_back(&mut self) {
        let head = &self.buf[..self.cursor];
        let trimmed = head.trim_end_matches(' ');
        let start = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
        self.buf.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Move the cursor one char left.
    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= self.prev_char_len();
        }
    }

    /// Move the cursor one char right.
    pub fn right(&mut self) {
        if self.cursor < self.buf.len() {
            let next = self.buf[self.cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
            self.cursor += next;
        }
    }

    /// Move the cursor to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end.
    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }

    /// Apply a single editing keypress. Returns `true` if the key was an
    /// editing action (consumed), `false` if the caller should handle it
    /// (Enter/Esc/Tab/…). `Ctrl`/`Alt`-modified keys other than the
    /// recognised emacs-style chords are left unhandled so global chords
    /// still reach the dispatcher.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('a') if ctrl => self.home(),
            KeyCode::Char('e') if ctrl => self.end(),
            KeyCode::Char(c) if !ctrl && !alt => self.insert(c),
            _ => return false,
        }
        true
    }

    /// Byte length of the char immediately before the cursor (0 at the
    /// start). Cursor is always on a boundary, so this is exact.
    fn prev_char_len(&self) -> usize {
        self.buf[..self.cursor]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(0)
    }
}

impl From<&str> for TextInput {
    fn from(s: &str) -> Self {
        Self::with_text(s)
    }
}

impl From<String> for TextInput {
    fn from(s: String) -> Self {
        Self::with_text(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn insert_and_backspace_at_end() {
        let mut t = TextInput::new();
        for c in "abc".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "abc");
        assert_eq!(t.cursor_col(), 3);
        t.backspace();
        assert_eq!(t.text(), "ab");
        assert_eq!(t.cursor_col(), 2);
    }

    #[test]
    fn mid_string_insert_and_delete() {
        let mut t = TextInput::with_text("ac");
        t.left(); // cursor between a and c
        t.insert('b');
        assert_eq!(t.text(), "abc");
        assert_eq!(t.cursor_col(), 2);
        t.home();
        t.delete_forward(); // delete 'a'
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor_col(), 0);
    }

    #[test]
    fn backspace_and_left_are_noops_at_start() {
        let mut t = TextInput::with_text("x");
        t.home();
        t.left();
        assert_eq!(t.cursor_col(), 0);
        t.backspace();
        assert_eq!(t.text(), "x");
        // Delete-forward at end is a no-op too.
        t.end();
        t.delete_forward();
        assert_eq!(t.text(), "x");
    }

    #[test]
    fn home_end_and_cursor_col() {
        let mut t = TextInput::with_text("hello");
        assert_eq!(t.cursor_col(), 5);
        t.home();
        assert_eq!(t.cursor_col(), 0);
        t.right();
        t.right();
        assert_eq!(t.cursor_col(), 2);
        t.end();
        assert_eq!(t.cursor_col(), 5);
    }

    #[test]
    fn delete_word_back_eats_trailing_spaces_then_word() {
        let mut t = TextInput::with_text("foo bar baz");
        t.delete_word_back();
        assert_eq!(t.text(), "foo bar ");
        t.delete_word_back();
        assert_eq!(t.text(), "foo ");
        t.delete_word_back();
        assert_eq!(t.text(), "");
        assert_eq!(t.cursor_col(), 0);
    }

    #[test]
    fn utf8_cursor_stays_on_char_boundaries() {
        let mut t = TextInput::new();
        t.insert('é');
        t.insert('🦀');
        assert_eq!(t.text(), "é🦀");
        assert_eq!(t.cursor_col(), 2);
        t.backspace(); // remove the crab, not a partial byte
        assert_eq!(t.text(), "é");
        assert_eq!(t.cursor_col(), 1);
        t.left();
        t.insert('x'); // insert before the é
        assert_eq!(t.text(), "xé");
    }

    #[test]
    fn handle_key_consumes_edits_but_not_enter_or_esc() {
        let mut t = TextInput::new();
        assert!(t.handle_key(key(KeyCode::Char('h'))));
        assert!(t.handle_key(key(KeyCode::Char('i'))));
        assert_eq!(t.text(), "hi");
        assert!(t.handle_key(ctrl('w'))); // word-delete
        assert_eq!(t.text(), "");
        // Enter / Esc are not editing actions — the prompt handles them.
        assert!(!t.handle_key(key(KeyCode::Enter)));
        assert!(!t.handle_key(key(KeyCode::Esc)));
        // A Ctrl-modified char that isn't a known chord is left alone.
        assert!(!t.handle_key(ctrl('t')));
        assert_eq!(t.text(), "");
    }

    #[test]
    fn insert_str_pastes_at_cursor() {
        let mut t = TextInput::with_text("ad");
        t.left();
        t.insert_str("bc");
        assert_eq!(t.text(), "abcd");
        assert_eq!(t.cursor_col(), 3);
    }
}
