# Print backend test harness (developer machine)

Checks a DocuView build's Windows print backend against the office copiers' own drivers without
a copier: two local queues using the installed TOSHIBA drivers write their output to files.

1. `add-test-printers.ps1` (run elevated, once): creates "DocuView Test PS3" and "DocuView Test
   PCL6" on file ports. Needs the TOSHIBA Universal PS3 and Universal Printer 2 drivers installed
   (any PC that has printed to the office copiers has them).
2. `python make_test_pdf.py`, `python make_mixed_pdf.py`, and copy
   `../docuview-print-test-pack.pdf` here as `pack.pdf`.
3. `matrix.ps1 [-Cli <path to pdfcraft-cli.exe>]`: prints eleven cases through the installed
   DocuView's command line (default `C:\Program Files\DocuView\pdfcraft-cli.exe`) with the sheet
   dump on, and checks each sheet's pixel size, depth (mono / gray / rgb / jpeg), band count,
   output size and time. All cases should say `ok`.
4. `measure.ps1` times one print command and keeps the driver output as `out-<label>.prn`;
   `analyze_ps.py` breaks a PostScript output down page by page.
5. `remove-test-printers.ps1` (elevated) removes the queues.
