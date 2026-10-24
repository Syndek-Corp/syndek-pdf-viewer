# DocuView printer test pack

`docuview-print-test-pack.pdf` is nine pages that exercise what office printing meets: small
text, grey tints and light grey text, hairlines, photographs and colour, landscape, Legal,
Tabloid, a page stored rotated, form fields, and a dense invoice-style page. Every page says
what to check, has a 1/4-inch border and one-inch ruler ticks.

## How to run it (10 minutes per printer)
1. Open the PDF in **DocuView**, File ▸ Print, choose the printer, leave the defaults
   (Fit, auto orientation) and print all pages.
2. Open the same PDF in **Acrobat** and print it the same way on the same printer.
3. Put the sheets side by side, page by page, and note anything that differs: text weight,
   tints, thin lines, colours, placement (the border should be 1/4 in from every edge on both),
   paper size used, and how long each job took from clicking Print to the last sheet.
4. Send the notes (photos of any page that differs are ideal), plus the two timings.

Pages 5 and 6 need Legal and Tabloid paper in the printer; if the printer has none, they
print shrunk onto Letter, which is expected.

## Regenerating
`python branding/print-test/make_test_pack.py` (needs `reportlab` and `Pillow`).
