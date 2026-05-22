//! Animated startup splash — an elephant, the Postgres mascot.
//!
//! Pure frame data. The animation is driven by the shared frame clock; the
//! splash supplies `frame(tick)` and is dismissed on keypress or
//! connection-ready. Frames are a fixed-width art block — the renderer places
//! the whole block in a centred rect and draws it left-aligned, so the lines
//! must keep their relative columns (do not centre lines individually).

/// Frames cycle: eyes open, eyes blink, trunk sways.
pub const FRAMES: &[&str] = &[FRAME_OPEN, FRAME_BLINK, FRAME_SWAY];

const FRAME_OPEN: &str = r#"
       ___                       ___
      /   \                     /   \
     /     \___________________/     \
    |                                 |
    |   (o)       pgman       (o)      |
     \                               /
      \            ___              /
       \__________|   |____________/
                  |   |
                  |   |
                  |   |
                  |    \____
                   \        \
                    \____    |
                         \__/
"#;

const FRAME_BLINK: &str = r#"
       ___                       ___
      /   \                     /   \
     /     \___________________/     \
    |                                 |
    |   (-)       pgman       (-)      |
     \                               /
      \            ___              /
       \__________|   |____________/
                  |   |
                  |   |
                  |   |
                  |    \____
                   \        \
                    \____    |
                         \__/
"#;

const FRAME_SWAY: &str = r#"
       ___                       ___
      /   \                     /   \
     /     \___________________/     \
    |                                 |
    |   (o)       pgman       (o)      |
     \                               /
      \            ___              /
       \__________|   |____________/
                  |   |
                  |   |
                  |   |
              ____/    |
             /        /
            |    ____/
             \__/
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
        assert_eq!(frame(3), FRAMES[0]);
        assert_eq!(frame(FRAMES.len() * 99 + 1), FRAMES[1]);
    }

    #[test]
    fn every_frame_is_branded() {
        for f in FRAMES {
            assert!(f.contains("pgman"), "frame should carry the brand");
        }
    }

    #[test]
    fn frames_share_a_line_count() {
        // The renderer assumes a stable block height across frames.
        let h = FRAMES[0].trim_matches('\n').lines().count();
        for f in FRAMES {
            assert_eq!(f.trim_matches('\n').lines().count(), h);
        }
    }
}
