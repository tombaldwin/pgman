//! Startup splash — a static pixel-art elephant based on the PostgreSQL logo
//! (Slonik): the three-bump top (two ears + head dome), big ears, a trunk
//! separated from the cheeks by gap-lines, tusks, and a curl.
//!
//! The sprite is authored as a plain silhouette template (`#` body, `o` eye,
//! `T` tusk, space empty). `frame` derives the detail from it: any body pixel
//! touching empty space becomes an outline pixel — so the whole figure, and
//! the trunk's gap-lines, are outlined — and interior body pixels are banded
//! top-to-bottom into light / body / shadow for a top-lit gradient. The
//! renderer (`ui`) maps each [`Pixel`] kind to a themed colour.

/// One pixel of the sprite. The renderer colours these from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixel {
    Empty,
    Outline,
    Shadow,
    Body,
    Light,
    Eye,
    Tusk,
}

const SPRITE: &str = r#"
            ####
          ########
   ####   ########   ####
  ######  ########  ######
 ##########################
############################
############################
#########oo######oo#########
############################
############################
 ########## #### ##########
 ########## #### ##########
  ######### #### #########
   ######## #### ########
    ####### #### #######
     ###### #### ######
      ##### #### #####
         TT #### TT
        TT  ####  TT
       TT   ####   TT
      TT    ####    TT
            ####
           ####
          ####
"#;

/// The (static) elephant sprite as a grid of typed pixels.
pub fn frame() -> Vec<Vec<Pixel>> {
    render(SPRITE)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Empty,
    Solid,
    Eye,
    Tusk,
}

fn cell_of(c: char) -> Cell {
    match c {
        '#' => Cell::Solid,
        'o' => Cell::Eye,
        'T' => Cell::Tusk,
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
    let height = cells.len();
    cells
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(c, &cell)| classify(cell, r, c, &cells, height))
                .collect()
        })
        .collect()
}

fn classify(cell: Cell, r: usize, c: usize, cells: &[Vec<Cell>], height: usize) -> Pixel {
    match cell {
        Cell::Empty => Pixel::Empty,
        Cell::Eye => Pixel::Eye,
        Cell::Tusk => Pixel::Tusk,
        Cell::Solid => {
            let (ri, ci) = (r as isize, c as isize);
            let on_edge = [(ri - 1, ci), (ri + 1, ci), (ri, ci - 1), (ri, ci + 1)]
                .iter()
                .any(|&(nr, nc)| at(cells, nr, nc) == Cell::Empty);
            if on_edge {
                Pixel::Outline
            } else {
                // Top-lit shading in three horizontal bands.
                let third = (height / 3).max(1);
                if r < third {
                    Pixel::Light
                } else if r < 2 * third {
                    Pixel::Body
                } else {
                    Pixel::Shadow
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_outlines_edges_and_shades_the_interior() {
        // A 3x3 solid block: corners touch empty space, the centre does not.
        let g = render("###\n###\n###");
        assert_eq!(g[0][0], Pixel::Outline, "corner touches the void");
        assert_eq!(g[0][1], Pixel::Outline, "top edge");
        assert_eq!(g[1][1], Pixel::Body, "centre is interior, middle band");
    }

    #[test]
    fn render_bands_interior_top_to_bottom() {
        // A tall solid column — interior pixels shade light → body → shadow.
        let tall = "###\n###\n###\n###\n###\n###\n###\n###\n###";
        let g = render(tall);
        assert_eq!(g[1][1], Pixel::Light);
        assert_eq!(g[4][1], Pixel::Body);
        assert_eq!(g[7][1], Pixel::Shadow);
    }

    #[test]
    fn render_marks_eyes_and_tusks() {
        let g = render("###\n#o#\n#T#\n###");
        assert_eq!(g[1][1], Pixel::Eye);
        assert_eq!(g[2][1], Pixel::Tusk);
    }

    #[test]
    fn sprite_has_outline_eyes_and_tusks() {
        let flat: Vec<Pixel> = frame().into_iter().flatten().collect();
        assert!(flat.contains(&Pixel::Outline), "figure is outlined");
        assert!(flat.contains(&Pixel::Eye), "elephant has eyes");
        assert!(flat.contains(&Pixel::Tusk), "elephant has tusks");
        assert!(flat.contains(&Pixel::Light), "shaded — light band");
        assert!(flat.contains(&Pixel::Shadow), "shaded — shadow band");
    }
}
