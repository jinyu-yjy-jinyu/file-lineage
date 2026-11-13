"""Generate the app icons used by Tauri's Windows bundle.

Run with the bundled Python (Pillow):

    python scripts/make_icons.py

The motif is a three-node lineage graph: an origin, a file, and a copy.
Drawing at 4x and downsampling keeps the small sizes crisp.
"""

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src-tauri" / "icons"
OUT.mkdir(parents=True, exist_ok=True)

SIZE = 512
BG = (13, 20, 32, 255)
CARD = (22, 34, 52, 255)
ACCENT = (86, 214, 200, 255)
WARM = (255, 178, 92, 255)
LINE = (74, 104, 132, 255)


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
        (0, 0, SIZE - 1, SIZE - 1), radius=112, fill=255
    )
    img.putalpha(mask)

    d = ImageDraw.Draw(img)
    d.rounded_rectangle(
        (28, 28, SIZE - 29, SIZE - 29), radius=94, outline=(58, 88, 116, 255), width=4
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
        d.line([(x1, y1), (x2, y2)], fill=LINE, width=12)
        # A small directional tick in the middle.
        mx, my = (x1 + x2) / 2, (y1 + y2) / 2
        d.ellipse((mx - 9, my - 9, mx + 9, my + 9), fill=ACCENT)

    for x, y, r, color in nodes:
        d.ellipse((x - r - 10, y - r - 10, x + r + 10, y + r + 10), fill=(15, 24, 38, 255))
        d.ellipse((x - r, y - r, x + r, y + r), fill=color)
        if color == CARD:
            d.ellipse(
                (x - r + 10, y - r + 10, x + r - 10, y + r - 10),
                outline=ACCENT,
                width=9,
            )
    return img


def main():
    base = draw_icon()
    for size in (16, 24, 32, 48, 64, 128, 256):
        base.resize((size, size), Image.LANCZOS).save(OUT / f"_{size}.png")
    base.resize((32, 32), Image.LANCZOS).save(OUT / "32x32.png")
    base.resize((128, 128), Image.LANCZOS).save(OUT / "128x128.png")
    base.resize((256, 256), Image.LANCZOS).save(OUT / "128x128@2x.png")
    base.resize((512, 512), Image.LANCZOS).save(OUT / "icon.png")
    base.save(
        OUT / "icon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    # Intermediate files are not needed at runtime or in git.
    for size in (16, 24, 32, 48, 64, 128, 256):
        (OUT / f"_{size}.png").unlink(missing_ok=True)
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
