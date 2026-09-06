"""Generate small UI-label shards without changing fonts or full glyph coverage.

Run with fonttools and brotli installed, passing both client src directories.
The original WOFF2 shards remain the source and fallback for arbitrary content.
"""
import argparse
import re
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

parser = argparse.ArgumentParser()
parser.add_argument("sources", nargs="+", type=Path)
args = parser.parse_args()
directories = [source.resolve() / "assets" / "fonts" for source in args.sources]
codepoints = set()
for source in args.sources:
    for path in sorted(source.rglob("*")):
        if path.suffix in {".ts", ".tsx", ".js", ".jsx", ".mjs"}:
            codepoints.update(ord(char) for char in path.read_text(encoding="utf-8") if 0x4E00 <= ord(char) <= 0x9FFF)

marker = "/* Generated UI-label subsets; rebuild with build-ui-font-subsets.py. */"
css_path = directories[0] / "noto-sans-cjk-sc.css"
original_css = css_path.read_text(encoding="utf-8").split(marker)[0].rstrip()
faces = []
for source in sorted(directories[0].glob("NotoSansCJKsc-cjk-*.woff2")):
    if not re.fullmatch(r"NotoSansCJKsc-cjk-\d+\.woff2", source.name):
        continue
    font = TTFont(source, recalcTimestamp=False)
    used = sorted(codepoints.intersection(font.getBestCmap()))
    if not used:
        font.close()
        continue
    subsetter = subset.Subsetter()
    subsetter.populate(unicodes=used)
    subsetter.subset(font)
    filename = source.name.replace("-cjk-", "-ui-cjk-")
    for directory in directories:
        font.save(directory / filename)
    font.close()
    ranges = ",".join(f"U+{value:04X}" for value in used)
    faces.append(f'''@font-face {{
  font-family: "Noto Sans CJK SC";
  src: url("./{filename}") format("woff2");
  font-style: normal;
  font-weight: 100 900;
  font-display: swap;
  unicode-range: {ranges};
}}''')
css = original_css + "\n\n" + marker + "\n" + "\n\n".join(faces) + "\n"
for directory in directories:
    (directory / "noto-sans-cjk-sc.css").write_text(css, encoding="utf-8", newline="\n")
print(f"Generated {len(faces)} shared UI-label shards for {len(codepoints)} codepoints.")
