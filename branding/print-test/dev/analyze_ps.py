"""Per-page breakdown of a PostScript print file: bytes per page, the image operators on it (size,
bits, filters) and whether any text is drawn with fonts."""

import re
import sys
from pathlib import Path

data = Path(sys.argv[1]).read_bytes()
print(f"{sys.argv[1]}: {len(data):,} bytes")
pages = [m.start() for m in re.finditer(rb"%%Page:", data)]
pages.append(len(data))
print(f"header/prolog: {pages[0]:,} bytes")
dict_re = re.compile(rb"/ImageType\s*\d|/Width\s+\d+|/Height\s+\d+|/BitsPerComponent\s+\d+|/Decode\s*\[[^\]]*\]|/\w+Decode\b|image(?:mask)?\b|/MultipleDataSources|/DataSource|(?:\d+\s+){2,6}(?:scale|translate|concat)\b")
for i in range(len(pages) - 1):
    seg = data[pages[i]:pages[i + 1]]
    text_ops = len(re.findall(rb"\b(?:show|xshow|yshow|xyshow|glyphshow|ashow|widthshow)\b", seg))
    fonts = len(re.findall(rb"\b(?:findfont|selectfont|definefont|scalefont)\b", seg))
    images = len(re.findall(rb"\b(?:image|imagemask|colorimage)\b", seg))
    paths = len(re.findall(rb"\b(?:lineto|curveto|rlineto|rcurveto)\b", seg))
    print(f"\n--- page {i + 1}: {len(seg):,} bytes · image ops {images} · text ops {text_ops} · font ops {fonts} · path ops {paths}")
    # Show the image dictionaries (the readable tokens near each image operator).
    shown = 0
    for m in re.finditer(rb"<<[^<>]{0,600}?/ImageType[^<>]{0,900}?>>", seg):
        d = m.group(0)
        toks = dict_re.findall(d)
        print("   ", b" ".join(t for t in toks if t)[:300].decode("latin-1"))
        shown += 1
        if shown >= 6:
            break
