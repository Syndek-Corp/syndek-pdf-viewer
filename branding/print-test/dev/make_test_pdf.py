"""A 10-page office-style PDF: headings, body text, a vector logo, a table, a photo-like image and
a gradient band on every page, so the print path meets text, vectors, images and shading."""

import io
import math
from pathlib import Path

from PIL import Image, ImageDraw
from reportlab.lib import colors
from reportlab.lib.pagesizes import letter
from reportlab.lib.units import inch
from reportlab.lib.utils import ImageReader
from reportlab.pdfgen import canvas

OUT = Path(__file__).resolve().parent / "test-10pages.pdf"
W, H = letter


def photo(seed: int) -> ImageReader:
    im = Image.new("RGB", (900, 600))
    px = im.load()
    for y in range(600):
        for x in range(900):
            px[x, y] = (int(120 + 100 * math.sin(x / 90 + seed)), int(100 + 90 * math.cos(y / 70)), int(140 + 80 * math.sin((x + y) / 120)))
    d = ImageDraw.Draw(im)
    for i in range(12):
        d.ellipse((60 * i, 40 * i, 60 * i + 200, 40 * i + 150), outline=(255, 255, 255), width=3)
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=85)
    buf.seek(0)
    return ImageReader(buf)


def gradient() -> ImageReader:
    im = Image.new("RGB", (600, 40))
    for x in range(600):
        for y in range(40):
            im.putpixel((x, y), (31 + x // 5, 95 + x // 8, 191 - x // 12))
    buf = io.BytesIO()
    im.save(buf, "PNG")
    buf.seek(0)
    return ImageReader(buf)


BODY = (
    "This agreement is entered into by the parties named above. Each party represents that it has the full "
    "authority to enter into this agreement and to perform its obligations. The scope of work, schedule and "
    "pricing are set out in the attached exhibits, which form part of this agreement. Any change to the scope "
    "must be agreed in writing by both parties before the work is performed. "
)


def main() -> None:
    c = canvas.Canvas(str(OUT), pagesize=letter)
    c.setTitle("DocuView print test")
    grad = gradient()
    for p in range(1, 11):
        # Vector logo: overlapping rounded shapes with a stroke.
        c.setFillColor(colors.HexColor("#1f5fbf"))
        c.roundRect(0.75 * inch, H - 1.35 * inch, 0.6 * inch, 0.6 * inch, 8, fill=1, stroke=0)
        c.setFillColor(colors.white)
        c.rect(0.9 * inch, H - 1.2 * inch, 0.3 * inch, 0.35 * inch, fill=1, stroke=0)
        c.setStrokeColor(colors.HexColor("#1f5fbf"))
        c.setLineWidth(2)
        c.line(0.75 * inch, H - 1.5 * inch, W - 0.75 * inch, H - 1.5 * inch)
        c.setFillColor(colors.black)
        c.setFont("Helvetica-Bold", 20)
        c.drawString(1.5 * inch, H - 1.15 * inch, f"Syndek Corp — Service Agreement, section {p}")
        c.drawImage(grad, 0.75 * inch, H - 1.9 * inch, width=W - 1.5 * inch, height=0.25 * inch)
        c.setFont("Helvetica", 10.5)
        y = H - 2.3 * inch
        text = c.beginText(0.75 * inch, y)
        text.setLeading(14)
        words = (BODY * 6).split()
        line, lines = [], []
        for w in words:
            if c.stringWidth(" ".join(line + [w]), "Helvetica", 10.5) > W - 1.5 * inch:
                lines.append(" ".join(line))
                line = [w]
            else:
                line.append(w)
        for ln in lines[:22]:
            text.textLine(ln)
        c.drawText(text)
        # Table
        ty = H - 6.6 * inch
        c.setFont("Helvetica-Bold", 10)
        for i, col in enumerate(("Item", "Description", "Qty", "Unit", "Total")):
            c.drawString(0.8 * inch + i * 1.35 * inch, ty, col)
        c.setFont("Helvetica", 10)
        for r in range(8):
            ry = ty - 16 * (r + 1)
            if r % 2 == 0:
                c.setFillColor(colors.HexColor("#eef3fb"))
                c.rect(0.75 * inch, ry - 4, W - 1.5 * inch, 16, fill=1, stroke=0)
                c.setFillColor(colors.black)
            for i, val in enumerate((f"{p}{r:02d}", "Field service and materials", str(3 + r), f"${120 + 7 * r:.2f}", f"${(3 + r) * (120 + 7 * r):.2f}")):
                c.drawString(0.8 * inch + i * 1.35 * inch, ry, val)
        c.drawImage(photo(p), 0.75 * inch, 1.0 * inch, width=3.0 * inch, height=2.0 * inch)
        c.setFont("Helvetica-Oblique", 9)
        c.drawString(4.0 * inch, 1.2 * inch, f"Page {p} of 10 · printed from DocuView test")
        c.showPage()
    c.save()
    print(OUT, OUT.stat().st_size, "bytes")


if __name__ == "__main__":
    main()
