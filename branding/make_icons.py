"""Draw the placeholder DocuView icon and write every size the packaging needs into branding/assets/.

Run once after changing the design (needs Pillow); the outputs are committed, so builds don't run this.
Replace branding/assets/ with real artwork later and keep the same file names.

    python branding/make_icons.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
OUT = HERE / "assets"

BLUE = "#1f5fbf"
PAGE = "#ffffff"
FOLD = "#c9d8f0"
LINE = "#9fb6dc"

# Geometry on a 512 x 512 canvas, shared by the PNG drawing and the SVG.
TILE_RADIUS = 112
PAGE_BOX = (136, 88, 376, 424)  # left, top, right, bottom
FOLD_SIZE = 72
LINES = [(176, 220, 336), (176, 270, 336), (176, 320, 296)]  # x0, y, x1
LINE_WIDTH = 22


def draw(size: int) -> Image.Image:
    s = 4  # supersample, then shrink for smooth edges
    big = 512 * s
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle((0, 0, big - 1, big - 1), radius=TILE_RADIUS * s, fill=BLUE)
    l, t, r, b = (v * s for v in PAGE_BOX)
    f = FOLD_SIZE * s
    d.polygon([(l, t), (r - f, t), (r, t + f), (r, b), (l, b)], fill=PAGE)
    d.polygon([(r - f, t), (r - f, t + f), (r, t + f)], fill=FOLD)
    for x0, y, x1 in LINES:
        d.rounded_rectangle((x0 * s, (y - LINE_WIDTH // 2) * s, x1 * s, (y + LINE_WIDTH // 2) * s), radius=LINE_WIDTH // 2 * s, fill=LINE)
    return im.resize((size, size), Image.LANCZOS)


def svg() -> str:
    l, t, r, b = PAGE_BOX
    f = FOLD_SIZE
    lines = "".join(
        f'<line x1="{x0}" y1="{y}" x2="{x1}" y2="{y}" stroke="{LINE}" stroke-width="{LINE_WIDTH}" stroke-linecap="round"/>'
        for x0, y, x1 in LINES
    )
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" role="img" aria-label="DocuView">'
        f'<rect width="512" height="512" rx="{TILE_RADIUS}" fill="{BLUE}"/>'
        f'<path d="M{l} {t}H{r - f}L{r} {t + f}V{b}H{l}Z" fill="{PAGE}"/>'
        f'<path d="M{r - f} {t}V{t + f}H{r}Z" fill="{FOLD}"/>'
        f"{lines}</svg>\n"
    )


def wordmark(color: str) -> str:
    """'Syndek' set as plain SVG text for the About dialog's "Part of" line (placeholder)."""
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 100" role="img" aria-label="Syndek">'
        f'<text x="0" y="78" font-family="Segoe UI, Helvetica, Arial, sans-serif" font-size="86" font-weight="600" fill="{color}">Syndek</text>'
        "</svg>\n"
    )


def main() -> None:
    OUT.mkdir(exist_ok=True)
    master = draw(1024)
    master.save(OUT / "app-1024.png")
    master.save(OUT / "app.ico", sizes=[(n, n) for n in (16, 20, 24, 32, 40, 48, 64, 128, 256)])
    master.save(OUT / "app.icns")
    for n in (16, 24, 32, 48, 64, 128, 256, 512):
        (OUT / "hicolor" / f"{n}x{n}").mkdir(parents=True, exist_ok=True)
        draw(n).save(OUT / "hicolor" / f"{n}x{n}" / "app.png")
    (OUT / "app.svg").write_text(svg(), encoding="utf-8")
    (OUT / "wordmark.svg").write_text(wordmark("#0b0b0c"), encoding="utf-8")
    (OUT / "wordmark-white.svg").write_text(wordmark("#ffffff"), encoding="utf-8")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
