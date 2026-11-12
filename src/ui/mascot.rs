//! The zerd mascot: one small pixel grid, drawn with half-blocks (two pixel rows per text row).
//! docs/assets/banner.svg is the same grid scaled up, so both always look like the same mascot.
use super::style::{bg_code, fg_code, RESET};

const W: usize = 16;
const H: usize = 14;

fn color(c: char) -> &'static str {
    match c {
        'O' => "#3f7d3a",
        'g' => "#8fd16a",
        'G' => "#5aa84e",
        'k' => "#1d2a1a",
        'p' => "#f4a0a0",
        's' => "#8a5a2b",
        'l' => "#6fc25a",
        'w' => "#ffffff",
        _ => "#c9f0a8", // 'h', the shine
    }
}

fn grid() -> [[char; W]; H] {
    let mut g = [['.'; W]; H];
    // body span per row, widest in the middle so it reads as a round little cat
    for (y, a, b) in [(2, 2, 13), (3, 1, 14), (4, 0, 15), (5, 0, 15), (6, 0, 15), (7, 0, 15), (8, 0, 15), (9, 0, 15), (10, 1, 14), (11, 2, 13)] {
        for x in a..=b {
            let edge = x == a || x == b || y == 2 || y == 11;
            g[y][x] = if edge { 'O' } else if (x == a + 1 || x == b - 1) && (4..=9).contains(&y) { 'G' } else { 'g' };
        }
    }
    // pointy ears, with a lighter inside
    for (y, x, c) in [(0, 2, 'O'), (0, 13, 'O'), (1, 1, 'O'), (1, 2, 'p'), (1, 3, 'O'), (1, 12, 'O'), (1, 13, 'p'), (1, 14, 'O')] {
        g[y][x] = c;
    }
    // sprout between the ears
    for y in 0..=1 {
        g[y][7] = 's';
    }
    for x in 8..=9 {
        g[0][x] = 'l';
    }
    for x in 8..=10 {
        g[1][x] = 'l';
    }
    g[3][4] = 'h';
    g[3][5] = 'h'; // a little shine on the forehead
    // big eyes, two wide and three tall, each with a white glint
    for (x0, glint) in [(4, 4), (10, 10)] {
        for y in 5..=7 {
            for x in x0..x0 + 2 {
                g[y][x] = 'k';
            }
        }
        g[5][glint] = 'w';
    }
    // blush, nose and a little cat mouth
    for (y, x) in [(8, 2), (8, 3), (8, 12), (8, 13), (7, 7), (7, 8)] {
        g[y][x] = 'p';
    }
    for (y, x) in [(8, 6), (9, 7), (9, 8), (8, 9)] {
        g[y][x] = 'k';
    }
    // two stubby feet
    for x in (3..=5).chain(10..=12) {
        g[12][x] = 'G';
        g[13][x] = 'O';
    }
    g
}

/// The mascot as seven rows of coloured half-blocks, each `W` columns wide.
pub fn mascot() -> Vec<String> {
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

/// The same grid as SVG rects, for docs/assets/banner.svg.
pub fn svg(x0: usize, y0: usize, px: usize) -> String {
    let g = grid();
    (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| g[y][x] != '.')
        .map(|(x, y)| format!(r#"<rect x="{}" y="{}" width="{px}" height="{px}" fill="{}"/>"#, x0 + x * px, y0 + y * px, color(g[y][x])))
        .collect()
}
