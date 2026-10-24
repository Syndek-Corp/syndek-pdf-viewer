"""mixed.pdf: Letter, landscape Letter, Legal and Tabloid pages, each with a border, text and a
light grey box, for the matrix's mixed-size, duplex and copies case."""

from pathlib import Path

from reportlab.lib import colors
from reportlab.lib.pagesizes import elevenSeventeen, landscape, legal, letter
from reportlab.pdfgen import canvas

c = canvas.Canvas(str(Path(__file__).resolve().parent / "mixed.pdf"), pagesize=letter)
for i, size in enumerate([letter, landscape(letter), legal, elevenSeventeen], 1):
    c.setPageSize(size)
    w, h = size
    c.setStrokeColor(colors.black)
    c.setLineWidth(2)
    c.rect(18, 18, w - 36, h - 36)
    c.setFont("Helvetica-Bold", 28)
    c.drawString(54, h - 90, f"Sheet {i}: {w / 72:.1f} x {h / 72:.1f} in")
    c.setFont("Helvetica", 12)
    for k in range(12):
        c.drawString(54, h - 130 - 18 * k, "Border should sit 1/4 inch inside every edge; text must not be clipped or scaled unexpectedly.")
    c.setFillColor(colors.Color(0.85, 0.85, 0.85))
    c.rect(54, 72, 200, 60, fill=1, stroke=0)
    c.setFillColor(colors.black)
    c.drawString(60, 100, "light grey box (must survive)")
    c.showPage()
c.save()
