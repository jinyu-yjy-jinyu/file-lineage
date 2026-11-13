"""Generate the app icons used by Tauri's Windows / macOS / Linux bundles.

Run with the bundled Python (Pillow):

    python scripts/make_icons.py

The motif is a three-node lineage graph: an origin, a file, and a copy.
The base image is 1024x1024 — the minimum `tauri icon` needs to emit both a
crisp `.icns` for macOS and 512/256 PNGs for Linux. Everything is drawn with a
scale factor relative to the original 512 design so the proportions do not
change.

After this script, `npx tauri icon src-tauri/icons/icon.png` produces the
packaged set (including `icon.icns`); only the files Tauri's bundler needs are
kept in `src-tauri/icons/`.
"""

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src-tauri" / "icons"
OUT.mkdir(parents=True, exist_ok=True)

SIZE = 1024
# The original artwork was designed on a 512px canvas.
S = SIZE / 512

BG = (13, 20, 32, 255)
CARD = (22, 34, 52, 255)
ACCENT = (86, 214, 200, 255)
WARM = (255, 178, 92, 255)
LINE = (74, 104, 132, 255)


def px(value: float) -> int:
    return int(round(value * S))


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def draw_icon() -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Rounded app tile with a subtle gradient.
    for y in range(SIZE):
        t = y / (SIZE - 1)
        color = lerp((24, 40, 62, 255), (11, 17, 28, 255), t)
        d.line([(0, y), (SIZE, y)], fill=color)
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, SIZE - 1, SIZE - 1), radius=px(112), fill=255
    )
    img.putalpha(mask)

    d = ImageDraw.Draw(img)
    d.rounded_rectangle(
        (px(28), px(28), px(483), px(483)),
        radius=px(94),
        outline=(58, 88, 116, 255),
        width=px(4),
    )

    # Nodes of the lineage graph.
    nodes = [
        (124, 152, 46, ACCENT),      # origin
        (272, 262, 68, CARD),        # the file itself
        (392, 128, 40, WARM),        # a copy
        (382, 382, 34, ACCENT),      # a process
    ]

    edges = [(0, 1), (1, 2), (1, 3)]
    for a, b in edges:
        x1, y1, _, _ = nodes[a]
        x2, y2, _, _ = nodes[b]
        d.line(
            [(px(x1), px(y1)), (px(x2), px(y2))],
            fill=LINE,
            width=px(12),
        )
        # A small directional tick in the middle.
        mx, my = (x1 + x2) / 2, (y1 + y2) / 2
        d.ellipse(
            (px(mx) - px(9), px(my) - px(9), px(mx) + px(9), px(my) + px(9)),
            fill=ACCENT,
        )

    for x, y, r, color in nodes:
        d.ellipse(
            (px(x - r - 10), px(y - r - 10), px(x + r + 10), px(y + r + 10)),
            fill=(15, 24, 38, 255),
        )
        d.ellipse((px(x - r), px(y - r), px(x + r), px(y + r)), fill=color)
        if color == CARD:
            d.ellipse(
                (px(x - r + 10), px(y - r + 10), px(x + r - 10), px(y + r - 10)),
                outline=ACCENT,
                width=px(9),
            )
    return img


def main():
    base = draw_icon()
    base.save(OUT / "icon.png")
    base.resize((32, 32), Image.LANCZOS).save(OUT / "32x32.png")
    base.resize((128, 128), Image.LANCZOS).save(OUT / "128x128.png")
    base.resize((256, 256), Image.LANCZOS).save(OUT / "128x128@2x.png")
    base.save(
        OUT / "icon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
