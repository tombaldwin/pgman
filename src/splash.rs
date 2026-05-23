//! Startup splash — a static pixel-art elephant transcribed from a Claude
//! Design export. Each template cell maps directly to a typed [`Pixel`] and
//! the renderer (`ui`) colours it from the theme.
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

const SPRITE: &str = r#"
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

/// The static elephant sprite as a grid of typed pixels.
pub fn frame() -> Vec<Vec<Pixel>> {
    SPRITE
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
    fn sprite_has_every_feature() {
        let flat: Vec<Pixel> = frame().into_iter().flatten().collect();
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
    fn sprite_has_a_stable_height() {
        // Any future tweak to the template should keep this row count stable
        // so the scaling code in ui has a predictable size to plan around.
        assert_eq!(frame().len(), 18);
    }
}
