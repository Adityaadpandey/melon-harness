"""The melon mascot: one small pixel grid, drawn with half-blocks in the terminal (two pixel rows per
text row). docs/banner.svg is the same grid scaled up, so both always look like the same melon."""
from rich.style import Style
from rich.text import Text

COLORS = {"O": "#3f7d3a", "g": "#8fd16a", "G": "#5aa84e", "k": "#1d2a1a", "p": "#f4a0a0", "s": "#8a5a2b", "l": "#6fc25a", "h": "#c9f0a8"}
# body width per row, widest in the middle, so it reads as round
_BODY = {2: (4, 9), 3: (3, 10), 4: (2, 11), 5: (2, 11), 6: (2, 11), 7: (2, 11), 8: (2, 11), 9: (2, 11), 10: (3, 10), 11: (4, 9)}
W, H = 14, 12


def grid() -> list[list[str]]:
    g = [["."] * W for _ in range(H)]
    for y, (a, b) in _BODY.items():
        for x in range(a, b + 1):
            edge = x in (a, b) or y in (2, 11)
            g[y][x] = "O" if edge else ("G" if x in (3, 10) and 4 <= y <= 9 else "g")
    g[3][5] = g[3][6] = "h"  # a little shine
    g[0][6] = g[1][6] = "s"
    for x in (7, 8):
        g[0][x] = "l"
    for x in (7, 8, 9, 10):
        g[1][x] = "l"
    g[4][5] = g[5][5] = g[4][8] = g[5][8] = "k"  # eyes
    g[7][4] = g[7][9] = "p"  # blush
    g[7][5] = g[7][8] = g[8][6] = g[8][7] = "k"  # smile
    return g


def melon() -> Text:
    g, out = grid(), Text()
    for y in range(0, H, 2):
        for x in range(W):
            top, bot = g[y][x], g[y + 1][x]
            if top == "." and bot == ".":
                out.append(" ")
            elif bot == ".":
                out.append("▀", Style(color=COLORS[top]))
            elif top == ".":
                out.append("▄", Style(color=COLORS[bot]))
            else:
                out.append("▀", Style(color=COLORS[top], bgcolor=COLORS[bot]))
        out.append("\n")
    out.rstrip()
    return out


def svg(x0: int, y0: int, px: int) -> str:
    return "".join(f'<rect x="{x0 + x * px}" y="{y0 + y * px}" width="{px}" height="{px}" fill="{COLORS[c]}"/>' for y, row in enumerate(grid()) for x, c in enumerate(row) if c != ".")
