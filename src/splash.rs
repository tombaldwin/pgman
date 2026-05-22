//! Animated startup splash — a pixel-art (8-bit style) elephant.
//!
//! Sprites are authored as character templates; `frame` parses one into a grid
//! of typed [`Pixel`]s. The renderer (`ui`) maps each pixel kind to a themed
//! colour and draws it as a two-cell `██` block, so colour and layout stay
//! the renderer's concern and this module stays pure and testable.
//!
//! Template legend: `#` body · `o` eye · `T` tusk · space empty.

/// One pixel of the sprite. The renderer colours these from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixel {
    Empty,
    Body,
    Eye,
    Tusk,
}

/// Sprite frames: eyes open, eyes blink, trunk swayed.
const SPRITES: &[&str] = &[SPRITE_OPEN, SPRITE_BLINK, SPRITE_SWAY];

const SPRITE_OPEN: &str = r#"
 ##    ##
####  ####
###########
###########
##o####o##
 #########
   ######
   T####T
  T #### T
 T  ####  T
    ####
    #####
      ###
"#;

const SPRITE_BLINK: &str = r#"
 ##    ##
####  ####
###########
###########
##########
 #########
   ######
   T####T
  T #### T
 T  ####  T
    ####
    #####
      ###
"#;

const SPRITE_SWAY: &str = r#"
 ##    ##
####  ####
###########
###########
##o####o##
 #########
   ######
   T####T
  T #### T
 T  ####  T
    ####
   #####
   ###
"#;

/// The sprite grid for animation tick `tick`, cycling through the frames.
pub fn frame(tick: usize) -> Vec<Vec<Pixel>> {
    SPRITES[tick % SPRITES.len()]
        .trim_matches('\n')
        .lines()
        .map(|line| line.chars().map(pixel_of).collect())
        .collect()
}

fn pixel_of(c: char) -> Pixel {
    match c {
        '#' => Pixel::Body,
        'o' => Pixel::Eye,
        'T' => Pixel::Tusk,
        _ => Pixel::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_cycles_through_all() {
        assert_eq!(frame(0), frame(SPRITES.len()));
        assert_eq!(frame(1), frame(SPRITES.len() + 1));
        // Open vs blink must differ (the eyes).
        assert_ne!(frame(0), frame(1));
    }

    #[test]
    fn pixel_of_maps_template_codes() {
        assert_eq!(pixel_of('#'), Pixel::Body);
        assert_eq!(pixel_of('o'), Pixel::Eye);
        assert_eq!(pixel_of('T'), Pixel::Tusk);
        assert_eq!(pixel_of(' '), Pixel::Empty);
    }

    #[test]
    fn open_frame_has_eyes_and_tusks() {
        let flat: Vec<Pixel> = frame(0).into_iter().flatten().collect();
        assert!(flat.contains(&Pixel::Eye), "elephant should have eyes");
        assert!(flat.contains(&Pixel::Tusk), "elephant should have tusks");
    }

    #[test]
    fn blink_frame_closes_the_eyes() {
        let flat: Vec<Pixel> = frame(1).into_iter().flatten().collect();
        assert!(!flat.contains(&Pixel::Eye), "blink frame has no open eyes");
    }

    #[test]
    fn sprites_share_a_line_count() {
        let h = SPRITES[0].trim_matches('\n').lines().count();
        for s in SPRITES {
            assert_eq!(s.trim_matches('\n').lines().count(), h);
        }
    }
}
