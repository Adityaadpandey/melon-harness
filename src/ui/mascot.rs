//! The melon mascot: one small pixel grid, drawn with half-blocks (two pixel rows per text row).
//! docs/banner.svg is the same grid scaled up, so both always look like the same melon.
use super::style::{bg_code, fg_code, RESET};

const W: usize = 14;
const H: usize = 12;

fn color(c: char) -> &'static str {
    match c {
        'O' => "#3f7d3a",
        'g' => "#8fd16a",
        'G' => "#5aa84e",
        'k' => "#1d2a1a",
        'p' => "#f4a0a0",
        's' => "#8a5a2b",
        'l' => "#6fc25a",
        _ => "#c9f0a8", // 'h', the shine
    }
}

fn grid() -> [[char; W]; H] {
    let mut g = [['.'; W]; H];
    // body width per row, widest in the middle, so it reads as round
    for (y, a, b) in [(2, 4, 9), (3, 3, 10), (4, 2, 11), (5, 2, 11), (6, 2, 11), (7, 2, 11), (8, 2, 11), (9, 2, 11), (10, 3, 10), (11, 4, 9)] {
        for x in a..=b {
            let edge = x == a || x == b || y == 2 || y == 11;
            g[y][x] = if edge { 'O' } else if (x == 3 || x == 10) && (4..=9).contains(&y) { 'G' } else { 'g' };
        }
    }
    g[3][5] = 'h';
    g[3][6] = 'h'; // a little shine
    g[0][6] = 's';
    g[1][6] = 's';
    for x in 7..=8 {
        g[0][x] = 'l';
    }
    for x in 7..=10 {
        g[1][x] = 'l';
    }
    for (y, x) in [(4, 5), (5, 5), (4, 8), (5, 8)] {
        g[y][x] = 'k'; // eyes
    }
    g[7][4] = 'p';
    g[7][9] = 'p'; // blush
    for (y, x) in [(7, 5), (7, 8), (8, 6), (8, 7)] {
        g[y][x] = 'k'; // smile
    }
    g
}

/// The melon as six rows of coloured half-blocks, each `W` columns wide.
pub fn melon() -> Vec<String> {
    let g = grid();
    (0..H).step_by(2).map(|y| {
        (0..W).map(|x| match (g[y][x], g[y + 1][x]) {
            ('.', '.') => " ".to_string(),
            (top, '.') => format!("{}▀{RESET}", fg_code(color(top))),
            ('.', bot) => format!("{}▄{RESET}", fg_code(color(bot))),
            (top, bot) => format!("{}{}▀{RESET}", fg_code(color(top)), bg_code(color(bot))),
        }).collect()
    }).collect()
}

/// The same grid as SVG rects, for docs/banner.svg.
pub fn svg(x0: usize, y0: usize, px: usize) -> String {
    let g = grid();
    (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| g[y][x] != '.')
        .map(|(x, y)| format!(r#"<rect x="{}" y="{}" width="{px}" height="{px}" fill="{}"/>"#, x0 + x * px, y0 + y * px, color(g[y][x])))
        .collect()
}
