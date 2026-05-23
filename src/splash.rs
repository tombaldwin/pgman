//! Startup splash — a pixel-art elephant transcribed from a Claude Design
//! export, lightly animated. The trunk tip flips left ↔ right; once in a
//! while he blinks.
//!
//! Four templates (cross-product of trunk direction × eye state). `frame`
//! selects one given the animation tick — the trunk swap is fairly frequent
//! (~0.9s per phase), the blink is rarer (every ~5.5s, lasting ~220ms).
//!
//! Template legend:
//!   `#` body · `O` outline · `d` ear-shade · `W` eye-white ·
//!   `o` pupil · `c` cheek · `T` tusk · space empty.

/// One pixel of the sprite. The renderer colours these from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixel {
    Empty,
    Outline,
    Body,
    /// Darker inner-ear detail.
    EarShade,
    /// Eye white.
    Eye,
    /// Near-black pupil.
    Pupil,
    /// Pink cheek.
    Cheek,
    /// Cream tusk.
    Tusk,
}

/// Frame at animation tick `tick`. The runtime ticks `anim_tick` at ~110 ms
/// while the splash is visible, so the timings are in those units.
pub fn frame(tick: usize) -> Vec<Vec<Pixel>> {
    let trunk_left = (tick / 8) % 2 == 1; // ~880 ms per phase
    let blinking = tick % 50 >= 46; // 4 ticks (~440 ms) every ~5.5 s
    let template = match (trunk_left, blinking) {
        (false, false) => SPRITE_RIGHT_OPEN,
        (false, true) => SPRITE_RIGHT_BLINK,
        (true, false) => SPRITE_LEFT_OPEN,
        (true, true) => SPRITE_LEFT_BLINK,
    };
    parse_template(template)
}

fn parse_template(template: &str) -> Vec<Vec<Pixel>> {
    template
        .trim_matches('\n')
        .lines()
        .map(|line| line.chars().map(cell_of).collect())
        .collect()
}

fn cell_of(c: char) -> Pixel {
    match c {
        'O' => Pixel::Outline,
        '#' => Pixel::Body,
        'd' => Pixel::EarShade,
        'W' => Pixel::Eye,
        'o' => Pixel::Pupil,
        'c' => Pixel::Cheek,
        'T' => Pixel::Tusk,
        _ => Pixel::Empty,
    }
}

// Four templates — only rows 4-5 (eyes) and 15-17 (trunk tip) differ. The rest
// is duplicated for clarity (it's only ~70 lines of data).

const SPRITE_RIGHT_OPEN: &str = r#"
   OOO         OOO
  O###OOOOOOOO###O
 O####O######O####O
O#####O######O#####O
O#####OoW##oWO#####O
O#d###OWW##WWO#d###O
O#d###O######O#d###O
O#####Oc####cO#####O
O#####O######O#####O
 O####O######O####O
  O###O######O###O
   OOOO######OOOO
      O######O
      TO####OT
      T O##O T
        O##O
        O###O
         OOO
"#;

const SPRITE_RIGHT_BLINK: &str = r#"
   OOO         OOO
  O###OOOOOOOO###O
 O####O######O####O
O#####O######O#####O
O#####OOO##OOO#####O
O#d###O######O#d###O
O#d###O######O#d###O
O#####Oc####cO#####O
O#####O######O#####O
 O####O######O####O
  O###O######O###O
   OOOO######OOOO
      O######O
      TO####OT
      T O##O T
        O##O
        O###O
         OOO
"#;

const SPRITE_LEFT_OPEN: &str = r#"
   OOO         OOO
  O###OOOOOOOO###O
 O####O######O####O
O#####O######O#####O
O#####OoW##oWO#####O
O#d###OWW##WWO#d###O
O#d###O######O#d###O
O#####Oc####cO#####O
O#####O######O#####O
 O####O######O####O
  O###O######O###O
   OOOO######OOOO
      O######O
      TO####OT
      T O##O T
        O##O
       O###O
        OOO
"#;

const SPRITE_LEFT_BLINK: &str = r#"
   OOO         OOO
  O###OOOOOOOO###O
 O####O######O####O
O#####O######O#####O
O#####OOO##OOO#####O
O#d###O######O#d###O
O#d###O######O#d###O
O#####Oc####cO#####O
O#####O######O#####O
 O####O######O####O
  O###O######O###O
   OOOO######OOOO
      O######O
      TO####OT
      T O##O T
        O##O
       O###O
        OOO
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_of_maps_template_codes() {
        assert_eq!(cell_of('O'), Pixel::Outline);
        assert_eq!(cell_of('#'), Pixel::Body);
        assert_eq!(cell_of('d'), Pixel::EarShade);
        assert_eq!(cell_of('W'), Pixel::Eye);
        assert_eq!(cell_of('o'), Pixel::Pupil);
        assert_eq!(cell_of('c'), Pixel::Cheek);
        assert_eq!(cell_of('T'), Pixel::Tusk);
        assert_eq!(cell_of(' '), Pixel::Empty);
        assert_eq!(cell_of('?'), Pixel::Empty);
    }

    #[test]
    fn open_frame_has_every_feature() {
        // tick=0 is right-trunk, eyes open — should contain every kind.
        let flat: Vec<Pixel> = frame(0).into_iter().flatten().collect();
        for want in [
            Pixel::Outline,
            Pixel::Body,
            Pixel::EarShade,
            Pixel::Eye,
            Pixel::Pupil,
            Pixel::Cheek,
            Pixel::Tusk,
        ] {
            assert!(flat.contains(&want), "sprite is missing {want:?}");
        }
    }

    #[test]
    fn every_frame_has_a_stable_height() {
        for tick in [0, 7, 8, 15, 28, 30, 48, 49, 50, 60, 999] {
            assert_eq!(frame(tick).len(), 18, "tick {tick} broke the height");
        }
    }

    #[test]
    fn blink_frames_close_the_eyes() {
        // Blink window is `tick % 50 >= 48` — pick a tick that lands in it.
        let flat: Vec<Pixel> = frame(48).into_iter().flatten().collect();
        assert!(!flat.contains(&Pixel::Eye), "blink frame has no eye whites");
        assert!(!flat.contains(&Pixel::Pupil), "blink frame has no pupils");
        assert!(flat.contains(&Pixel::Body), "but the body's still there");
    }

    #[test]
    fn trunk_flips_direction_with_the_tick() {
        // tick 0 → right phase, tick 8 → left phase.
        assert_ne!(frame(0), frame(8), "trunk should flip between phases");
        // Within a phase the trunk shape is stable (eyes-only differ if blink hits).
        assert_eq!(frame(0), frame(1));
    }
}
