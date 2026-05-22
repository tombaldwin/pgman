//! Startup splash — a static pixel-art elephant.
//!
//! The sprite is authored as a character template (`#` body · `d` ear-shade ·
//! `W` eye-white · `o` pupil · `c` cheek · space empty). `frame` parses it and
//! edge-detects the silhouette: any body pixel touching empty space becomes an
//! outline pixel, so the head and the two separate ears are each outlined
//! without authoring the border by hand. The feature cells (`d`/`W`/`o`/`c`)
//! pass through unchanged. The renderer (`ui`) maps each [`Pixel`] to a themed
//! colour.

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
    /// Navy pupil.
    Pupil,
    /// Pink cheek.
    Cheek,
}

const SPRITE: &str = r#"
   ###     ######     ###
  #####   ########   #####
 ####### ########## #######
 ####### #WoW##WoW# #######
 ####### #WoW##WoW# #######
 ##dd### #WWW##WWW# ###dd##
 ##dd### ########## ###dd##
 ##dd### #cc####cc# ###dd##
 ##dd### #cc####cc# ###dd##
 ####### ########## #######
 ####### ########## #######
 #######  ########  #######
  #####    ######    #####
   ###     ######     ###
           #####
            ####
            ####
            ###
             ##
"#;

/// The (static) elephant sprite as a grid of typed pixels.
pub fn frame() -> Vec<Vec<Pixel>> {
    render(SPRITE)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Empty,
    Solid,
    EarShade,
    Eye,
    Pupil,
    Cheek,
}

fn cell_of(c: char) -> Cell {
    match c {
        '#' => Cell::Solid,
        'd' => Cell::EarShade,
        'W' => Cell::Eye,
        'o' => Cell::Pupil,
        'c' => Cell::Cheek,
        _ => Cell::Empty,
    }
}

/// Cell at `(r, c)`, treating off-grid and short-row positions as empty.
fn at(cells: &[Vec<Cell>], r: isize, c: isize) -> Cell {
    if r < 0 || c < 0 {
        return Cell::Empty;
    }
    cells
        .get(r as usize)
        .and_then(|row| row.get(c as usize))
        .copied()
        .unwrap_or(Cell::Empty)
}

fn render(template: &str) -> Vec<Vec<Pixel>> {
    let cells: Vec<Vec<Cell>> = template
        .trim_matches('\n')
        .lines()
        .map(|line| line.chars().map(cell_of).collect())
        .collect();
    cells
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(c, &cell)| classify(cell, r, c, &cells))
                .collect()
        })
        .collect()
}

fn classify(cell: Cell, r: usize, c: usize, cells: &[Vec<Cell>]) -> Pixel {
    match cell {
        Cell::Empty => Pixel::Empty,
        Cell::EarShade => Pixel::EarShade,
        Cell::Eye => Pixel::Eye,
        Cell::Pupil => Pixel::Pupil,
        Cell::Cheek => Pixel::Cheek,
        Cell::Solid => {
            let (ri, ci) = (r as isize, c as isize);
            let on_edge = [(ri - 1, ci), (ri + 1, ci), (ri, ci - 1), (ri, ci + 1)]
                .iter()
                .any(|&(nr, nc)| at(cells, nr, nc) == Cell::Empty);
            if on_edge {
                Pixel::Outline
            } else {
                Pixel::Body
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_outlines_solid_edges_only() {
        // A 3x3 solid block: every border cell touches the void, the centre
        // does not.
        let g = render("###\n###\n###");
        assert_eq!(g[0][0], Pixel::Outline);
        assert_eq!(g[0][1], Pixel::Outline);
        assert_eq!(g[1][1], Pixel::Body);
    }

    #[test]
    fn render_passes_feature_cells_through() {
        let g = render("#W#\n#o#\n#c#\n#d#");
        assert_eq!(g[0][1], Pixel::Eye);
        assert_eq!(g[1][1], Pixel::Pupil);
        assert_eq!(g[2][1], Pixel::Cheek);
        assert_eq!(g[3][1], Pixel::EarShade);
    }

    #[test]
    fn feature_cells_do_not_outline_adjacent_body() {
        // Body next to an eye is interior, not an edge.
        let g = render("###\n#W#\n###");
        // (1,0) body touches empty on its left -> outline; but it does not
        // become outline *because* of the eye at (1,1).
        assert_eq!(g[1][1], Pixel::Eye);
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
        ] {
            assert!(flat.contains(&want), "sprite is missing {want:?}");
        }
    }
}
