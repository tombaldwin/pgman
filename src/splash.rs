//! Animated startup splash — an elephant, the Postgres mascot.
//!
//! Pure frame data. The animation is driven by the shared frame clock (see
//! CLAUDE.md "one frame clock, multiple animation sources"); the splash only
//! supplies `frame(tick)` and is dismissed on keypress or connection-ready.
//!
//! M0 wires this into the TUI; for now `main.rs` prints a single static frame.

/// Each frame blinks the eyes and sways the trunk. Frames are printed
/// line-by-line, so they need not be equal width.
pub const FRAMES: &[&str] = &[FRAME_OPEN_LEFT, FRAME_BLINK, FRAME_OPEN_RIGHT];

const FRAME_OPEN_LEFT: &str = r#"
        _____         _____
      ,'     `.     ,'     `.
     /         `---'         \
    |          pgman          |
    |     (o)         (o)     |
     \         _____         /
      `.      /     \      ,'
        `----|  ___  |----'
            | /   \ |
            |/     \|
            '       '
"#;

const FRAME_BLINK: &str = r#"
        _____         _____
      ,'     `.     ,'     `.
     /         `---'         \
    |          pgman          |
    |     (-)         (-)     |
     \         _____         /
      `.      /     \      ,'
        `----|  ___  |----'
             | /   \ |
             |/     \|
             '       '
"#;

const FRAME_OPEN_RIGHT: &str = r#"
        _____         _____
      ,'     `.     ,'     `.
     /         `---'         \
    |          pgman          |
    |     (o)         (o)     |
     \         _____         /
      `.      /     \      ,'
        `----|  ___  |----'
              | /   \ |
              |/     \|
              '       '
"#;

/// Frame for animation tick `tick`, cycling through `FRAMES`.
pub fn frame(tick: usize) -> &'static str {
    FRAMES[tick % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_cycles_through_all() {
        assert_eq!(frame(0), FRAMES[0]);
        assert_eq!(frame(1), FRAMES[1]);
        assert_eq!(frame(2), FRAMES[2]);
        // Wraps.
        assert_eq!(frame(3), FRAMES[0]);
        assert_eq!(frame(FRAMES.len() * 99 + 1), FRAMES[1]);
    }

    #[test]
    fn every_frame_is_branded() {
        for f in FRAMES {
            assert!(f.contains("pgman"), "frame should carry the brand");
        }
    }
}
