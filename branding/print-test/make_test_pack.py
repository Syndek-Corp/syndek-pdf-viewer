"""The DocuView printer test pack: one PDF whose pages exercise what office printing meets.

Print it from DocuView and from Acrobat on the same printer and compare the sheets side by side.
Every page carries a 1/4-inch border and one-inch ruler ticks, so scaling and placement can be
checked with a ruler, and says what to look for.

    python branding/print-test/make_test_pack.py      (needs reportlab and Pillow)
"""

import io
import math
from pathlib import Path

from PIL import Image, ImageDraw
from reportlab.lib import colors
from reportlab.lib.pagesizes import elevenSeventeen, landscape, legal, letter
from reportlab.lib.units import inch
from reportlab.lib.utils import ImageReader
from reportlab.pdfgen import canvas

OUT = Path(__file__).resolve().parent / "docuview-print-test-pack.pdf"
TOTAL = 9


def frame(c: canvas.Canvas, w: float, h: float, n: int, title: str, check: str) -> None:
    """Border, ruler ticks, header and the 'what to check' line."""
    c.setStrokeColor(colors.black)
    c.setLineWidth(1)
    c.rect(0.25 * inch, 0.25 * inch, w - 0.5 * inch, h - 0.5 * inch)
    # One-inch ticks along the top and left edges, half-inch minor ticks.
    x = 0.25 * inch
    while x <= w - 0.25 * inch:
        c.line(x, h - 0.25 * inch, x, h - 0.4 * inch)
        c.line(x + 0.5 * inch, h - 0.25 * inch, x + 0.5 * inch, h - 0.32 * inch)
        x += inch
    y = h - 0.25 * inch
    while y >= 0.25 * inch:
        c.line(0.25 * inch, y, 0.4 * inch, y)
        c.line(0.25 * inch, y - 0.5 * inch, 0.32 * inch, y - 0.5 * inch)
        y -= inch
    c.setFont("Helvetica-Bold", 14)
    c.drawString(0.6 * inch, h - 0.75 * inch, f"DocuView print test {n}/{TOTAL}: {title}")
    c.setFont("Helvetica", 9)
    c.drawString(0.6 * inch, h - 0.95 * inch, f"Check: {check}")
    c.drawString(0.6 * inch, 0.4 * inch, f"Sheet {w / inch:.1f} x {h / inch:.1f} in. Border is 1/4 in from every edge; ticks are 1 in apart.")


def photo() -> ImageReader:
    im = Image.new("RGB", (1200, 800))
    px = im.load()
    for y in range(800):
        for x in range(1200):
            px[x, y] = (int(128 + 110 * math.sin(x / 120)), int(128 + 100 * math.cos(y / 90)), int(128 + 100 * math.sin((x + y) / 160)))
    d = ImageDraw.Draw(im)
    for i in range(16):
        d.ellipse((50 * i, 30 * i, 50 * i + 260, 30 * i + 200), outline=(255, 255, 255), width=4)
    d.rectangle((40, 40, 400, 200), fill=(20, 20, 20))
    d.text((60, 60), "dark area: shadow detail", fill=(90, 90, 90))
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=92)
    buf.seek(0)
    return ImageReader(buf)


def gray_photo() -> ImageReader:
    im = Image.new("L", (1200, 800))
    px = im.load()
    for y in range(800):
        for x in range(1200):
            px[x, y] = int(128 + 120 * math.sin(x / 150) * math.cos(y / 110))
    buf = io.BytesIO()
    im.save(buf, "PNG")
    buf.seek(0)
    return ImageReader(buf)


def body_text(c: canvas.Canvas, x: float, y: float, width: float, size: float, lines: int, text: str) -> float:
    c.setFont("Helvetica", size)
    words = (text * 8).split()
    line, out = [], []
    for wd in words:
        if c.stringWidth(" ".join(line + [wd]), "Helvetica", size) > width:
            out.append(" ".join(line))
            line = [wd]
        else:
            line.append(wd)
        if len(out) >= lines:
            break
    t = c.beginText(x, y)
    t.setLeading(size * 1.3)
    for ln in out:
        t.textLine(ln)
    c.drawText(t)
    return y - size * 1.3 * len(out)


LOREM = "Printing must reproduce small text, fine rules, light tints and photographs faithfully, on every paper size the office uses. "


def main() -> None:
    c = canvas.Canvas(str(OUT), pagesize=letter)
    c.setTitle("DocuView print test pack")
    w, h = letter

    # 1. Text at many sizes
    frame(c, w, h, 1, "text sizes", "every size is readable and sharp; 6 pt is small but clean; nothing bold or smeared")
    y = h - 1.4 * inch
    for size in (4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 18, 24):
        c.setFont("Helvetica", size)
        c.drawString(0.6 * inch, y, f"{size} pt  The quick brown fox jumps over the lazy dog 0123456789 {LOREM[:60]}")
        y -= size * 1.6 + 4
    c.setFont("Times-Roman", 10)
    y = body_text(c, 0.6 * inch, y - 10, w - 1.2 * inch, 10, 10, LOREM)
    c.setFont("Courier", 9)
    c.drawString(0.6 * inch, y - 10, "Courier 9 pt: ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789 !@#$%^&*()")
    c.showPage()

    # 2. Grey tints, light grey text, thin lines
    frame(c, w, h, 2, "tints, light text, thin lines", "every tint box is distinct (5% is faint but visible); light grey text is readable; all six lines print unbroken")
    y = h - 1.6 * inch
    c.setFont("Helvetica", 9)
    for i, pct in enumerate((5, 10, 20, 30, 50, 70, 85)):
        g = 1 - pct / 100
        c.setFillColor(colors.Color(g, g, g))
        c.rect(0.6 * inch + i * 1.0 * inch, y - 0.6 * inch, 0.85 * inch, 0.6 * inch, fill=1, stroke=0)
        c.setFillColor(colors.black)
        c.drawString(0.6 * inch + i * 1.0 * inch, y - 0.75 * inch, f"{pct}% grey")
    y -= 1.2 * inch
    for i, g in enumerate((0.3, 0.5, 0.65, 0.8)):
        c.setFillColor(colors.Color(g, g, g))
        c.setFont("Helvetica", 11)
        c.drawString(0.6 * inch, y - i * 0.25 * inch, f"Text in {int(g * 100)}% grey: typical form hint or watermark text, must stay readable")
    c.setFillColor(colors.black)
    y -= 1.4 * inch
    c.setFont("Helvetica", 9)
    for i, lw in enumerate((0.0, 0.1, 0.25, 0.5, 1.0, 2.0)):
        c.setLineWidth(lw)
        c.setStrokeColor(colors.black)
        c.line(0.6 * inch, y - i * 0.3 * inch, 4.5 * inch, y - i * 0.3 * inch)
        c.line(5.0 * inch, y - i * 0.3 * inch - 0.1 * inch, 7.5 * inch, y - i * 0.3 * inch + 0.1 * inch)
        c.drawString(4.6 * inch, y - i * 0.3 * inch - 3, f"{lw} pt")
    y -= 2.0 * inch
    c.setFont("Helvetica", 9)
    c.drawString(0.6 * inch, y, "Table rules at 0.25 pt with a 10% tinted header row and 50% grey text in the last column:")
    y -= 0.2 * inch
    c.setFillColor(colors.Color(0.9, 0.9, 0.9))
    c.rect(0.6 * inch, y - 0.22 * inch, 6.9 * inch, 0.22 * inch, fill=1, stroke=0)
    c.setFillColor(colors.black)
    c.setLineWidth(0.25)
    columns = [0.6, 1.5, 4.3, 5.1, 6.3, 7.5]  # inches: column edges
    for r in range(7):
        ry = y - r * 0.22 * inch
        c.line(0.6 * inch, ry, 7.5 * inch, ry)
        if r < 6:
            for k, col in enumerate(("Item", "Description of the line item", "Qty", "Unit price", "Total")):
                c.setFillColor(colors.Color(0.5, 0.5, 0.5) if k == 4 and r > 0 else colors.black)
                c.drawString(columns[k] * inch + 4, ry - 0.16 * inch, col if r == 0 else f"{['A100', 'service and materials', '3', '$120.00', '$360.00'][k]}")
    c.setFillColor(colors.black)
    for k in columns:
        c.line(k * inch, y, k * inch, y - 6 * 0.22 * inch)
    c.showPage()

    # 3. Colour: photo, colour text, gradient, grey photo
    frame(c, w, h, 3, "colour and photographs", "photo is smooth with shadow detail; gradient has no bands; colour text is crisp; grey photo is smooth")
    c.drawImage(photo(), 0.6 * inch, h - 4.6 * inch, width=3.4 * inch, height=2.27 * inch)
    c.drawImage(gray_photo(), 4.2 * inch, h - 4.6 * inch, width=3.4 * inch, height=2.27 * inch)
    grad = Image.new("RGB", (800, 60))
    for x in range(800):
        for yy in range(60):
            grad.putpixel((x, yy), (int(255 * x / 799), int(96 + 80 * math.sin(x / 120)), int(255 - 255 * x / 799)))
    buf = io.BytesIO()
    grad.save(buf, "PNG")
    buf.seek(0)
    c.drawImage(ImageReader(buf), 0.6 * inch, h - 5.4 * inch, width=6.9 * inch, height=0.5 * inch)
    y = h - 6.0 * inch
    for i, (name, col) in enumerate((("red", colors.red), ("green", colors.green), ("blue", colors.blue), ("orange", colors.orange), ("purple", colors.purple), ("teal", colors.teal))):
        c.setFillColor(col)
        c.setFont("Helvetica-Bold", 12)
        c.drawString(0.6 * inch + (i % 3) * 2.3 * inch, y - (i // 3) * 0.3 * inch, f"{name} text, 12 pt bold, sharp edges")
    c.setFillColor(colors.black)
    y -= 1.0 * inch
    for i, col in enumerate((colors.Color(1, 0.9, 0.9), colors.Color(0.9, 1, 0.9), colors.Color(0.9, 0.9, 1), colors.Color(1, 1, 0.85))):
        c.setFillColor(col)
        c.rect(0.6 * inch + i * 1.75 * inch, y - 0.6 * inch, 1.6 * inch, 0.6 * inch, fill=1, stroke=0)
        c.setFillColor(colors.black)
        c.setFont("Helvetica", 9)
        c.drawString(0.65 * inch + i * 1.75 * inch, y - 0.35 * inch, "black 9 pt on a pale tint")
    c.setFillColor(colors.black)
    y = body_text(c, 0.6 * inch, y - 1.0 * inch, w - 1.2 * inch, 9, 8, LOREM)
    c.showPage()

    # 4. Landscape Letter
    c.setPageSize(landscape(letter))
    lw_, lh_ = landscape(letter)
    frame(c, lw_, lh_, 4, "landscape Letter", "comes out landscape, not rotated or clipped; border 1/4 in from every edge")
    body_text(c, 0.6 * inch, lh_ - 1.5 * inch, lw_ - 1.2 * inch, 10, 20, LOREM)
    c.showPage()

    # 5. Legal
    c.setPageSize(legal)
    frame(c, legal[0], legal[1], 5, "Legal 8.5 x 14", "printed on Legal paper at 100%; bottom border present; not shrunk onto Letter")
    body_text(c, 0.6 * inch, legal[1] - 1.5 * inch, legal[0] - 1.2 * inch, 10, 40, LOREM)
    c.showPage()

    # 6. Tabloid with a drawing-style grid of thin lines
    c.setPageSize(elevenSeventeen)
    tw, th = elevenSeventeen
    frame(c, tw, th, 6, "Tabloid 11 x 17 drawing", "printed on Tabloid at 100%; the 0.25 pt grid is even and unbroken; small labels readable")
    c.setLineWidth(0.25)
    c.setStrokeColor(colors.Color(0.4, 0.4, 0.4))
    x = 0.6 * inch
    while x < tw - 0.6 * inch:
        c.line(x, 1.0 * inch, x, th - 1.3 * inch)
        x += 0.25 * inch
    y = 1.0 * inch
    while y < th - 1.3 * inch:
        c.line(0.6 * inch, y, tw - 0.6 * inch, y)
        y += 0.25 * inch
    c.setStrokeColor(colors.black)
    c.setLineWidth(1.5)
    c.rect(2 * inch, 3 * inch, 7 * inch, 9 * inch)
    c.setLineWidth(0.5)
    c.circle(5.5 * inch, 7.5 * inch, 3 * inch)
    c.setFont("Helvetica", 6)
    for i in range(20):
        c.drawString(2.1 * inch, 3.1 * inch + i * 0.4 * inch, f"DETAIL {i + 1:02d}  6 pt label  1/4\" = 1'-0\"  ELEV 100.{i:02d}")
    c.showPage()

    # 7. A landscape sheet stored as a portrait page with /Rotate 90 (what scanners produce).
    # Drawn readable here; `rotate_page_7` below turns it into the stored form.
    c.setPageSize(landscape(letter))
    frame(c, lw_, lh_, 7, "landscape sheet stored with /Rotate 90", "prints landscape and readable, the same way up as on screen; not sideways, mirrored or cut")
    body_text(c, 0.6 * inch, lh_ - 1.5 * inch, lw_ - 1.2 * inch, 11, 15, LOREM)
    c.showPage()

    # 8. Form fields
    c.setPageSize(letter)
    frame(c, w, h, 8, "form fields", "field borders and typed values print; the checked box prints; fields are not blank")
    form = c.acroForm
    c.setFont("Helvetica", 10)
    for i, label in enumerate(("Name", "Company", "Phone", "Email")):
        yy = h - 1.6 * inch - i * 0.45 * inch
        c.drawString(0.6 * inch, yy, label + ":")
        form.textfield(name=f"f{i}", tooltip=label, x=1.6 * inch, y=yy - 5, width=4 * inch, height=20, borderStyle="solid", borderWidth=1, fillColor=colors.white, value=f"Sample {label.lower()} value", forceBorder=True)
    form.checkbox(name="agree", tooltip="Agree", x=0.6 * inch, y=h - 3.8 * inch, size=16, checked=True, fillColor=colors.white, forceBorder=True)
    c.drawString(0.95 * inch, h - 3.75 * inch, "I agree (checked box must print)")
    form.checkbox(name="decline", tooltip="Decline", x=0.6 * inch, y=h - 4.2 * inch, size=16, checked=False, fillColor=colors.white, forceBorder=True)
    c.drawString(0.95 * inch, h - 4.15 * inch, "Decline (empty box)")
    c.showPage()

    # 9. Mixed: everything dense on one Letter page (the typical invoice)
    frame(c, w, h, 9, "a typical dense page", "overall impression equals Acrobat's print: text weight, tints, logo colour, photo")
    c.setFillColor(colors.HexColor("#1f5fbf"))
    c.roundRect(0.6 * inch, h - 2.0 * inch, 0.6 * inch, 0.6 * inch, 8, fill=1, stroke=0)
    c.setFillColor(colors.black)
    c.setFont("Helvetica-Bold", 16)
    c.drawString(1.35 * inch, h - 1.8 * inch, "Syndek Corp — Invoice 10042")
    y = body_text(c, 0.6 * inch, h - 2.5 * inch, w - 1.2 * inch, 9.5, 12, LOREM)
    c.drawImage(photo(), 0.6 * inch, y - 2.2 * inch, width=3.0 * inch, height=2.0 * inch)
    c.setFillColor(colors.Color(0.92, 0.92, 0.92))
    c.rect(4.0 * inch, y - 2.2 * inch, 3.5 * inch, 2.0 * inch, fill=1, stroke=0)
    c.setFillColor(colors.black)
    c.setFont("Helvetica", 8)
    for i in range(10):
        c.drawString(4.1 * inch, y - 0.4 * inch - i * 0.18 * inch, f"Line {i + 1}: 8 pt text on a 8% tint box, amount ${(i + 1) * 137.5:,.2f}")
    c.showPage()

    c.save()
    rotate_page_7()
    print(OUT, OUT.stat().st_size, "bytes,", TOTAL, "pages")


def rotate_page_7() -> None:
    """Store page 7 as a portrait 612 x 792 page whose content lies on its side, with /Rotate 90,
    so viewers turn it a quarter turn clockwise and show the landscape sheet readable."""
    from pypdf import PdfReader, PdfWriter, Transformation

    reader = PdfReader(str(OUT))
    writer = PdfWriter()
    for i, page in enumerate(reader.pages):
        if i == 6:
            # Landscape (x', y') onto the portrait page: x = 612 - y', y = x' (a quarter turn
            # anticlockwise, then shifted right), which /Rotate 90 undoes on display.
            page.add_transformation(Transformation().rotate(90).translate(612, 0))
            page.mediabox.lower_left = (0, 0)
            page.mediabox.upper_right = (612, 792)
            page.rotate(90)
        writer.add_page(page)
    with OUT.open("wb") as f:
        writer.write(f)


if __name__ == "__main__":
    main()
