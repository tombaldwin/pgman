//! Animated startup splash — a pixel-art (8-bit style) elephant.
//!
//! Sprites are authored as `#` (filled pixel) / space (empty) templates.
//! `expand` blows each pixel up to a two-cell block (`██`) so it renders
//! roughly square in a terminal. The renderer places the block in a centred
//! rect, left-aligned — each template line's leading spaces do the centring.

/// Sprite frames: eyes open, eyes blink, trunk swayed.
const SPRITES: &[&str] = &[SPRITE_OPEN, SPRITE_BLINK, SPRITE_SWAY];

const SPRITE_OPEN: &str = r#"
     ####
   ########
 ############
##############
##############
### ###### ###
##############
##############
 ############
  ##########
   ########
    ######
     ####
     ####
     ####
     #####
       ####
        ###
"#;

const SPRITE_BLINK: &str = r#"
     ####
   ########
 ############
##############
##############
##############
##############
##############
 ############
  ##########
   ########
    ######
     ####
     ####
     ####
     #####
       ####
        ###
"#;

const SPRITE_SWAY: &str = r#"
     ####
   ########
 ############
##############
##############
### ###### ###
##############
##############
 ############
  ##########
   ########
    ######
     ####
     ####
     ####
    #####
   ####
   ###
"#;

/// Block-art frame for animation tick `tick`, cycling through the sprites.
pub fn frame(tick: usize) -> String {
    expand(SPRITES[tick % SPRITES.len()].trim_matches('\n'))
}

/// Expand a `#`/space pixel template to block art — each pixel becomes a
/// two-cell `██` square (empty pixels become two spaces).
fn expand(template: &str) -> String {
    template
        .lines()
        .map(|line| {
            line.chars()
                .map(|c| if c == '#' { "██" } else { "  " })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_cycles_through_all() {
        assert_eq!(frame(0), frame(SPRITES.len()));
        assert_eq!(frame(1), frame(SPRITES.len() + 1));
        // The open and blink sprites must differ (the eyes).
        assert_ne!(frame(0), frame(1));
    }

    #[test]
    fn expand_maps_pixels_to_blocks() {
        assert_eq!(expand("#.#\n.#."), "██  ██\n  ██  ");
    }

    #[test]
    fn expanded_frame_contains_block_glyphs() {
        assert!(frame(0).contains('█'));
    }

    #[test]
    fn sprites_share_a_line_count() {
        // The renderer assumes a stable block height across frames.
        let h = SPRITES[0].trim_matches('\n').lines().count();
        for s in SPRITES {
            assert_eq!(s.trim_matches('\n').lines().count(), h);
        }
    }
}
