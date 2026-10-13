#!/usr/bin/env bash
# Regenerates assets/fonts/ (embedded into the binary) from pinned upstream
# Noto sources. Subsetting keeps the binary small (spec 10.4):
#   - Noto Sans:    Latin (incl. extended), Greek, Cyrillic, punctuation, symbols
#   - Noto Sans JP: kana, CJK punctuation, full/half-width forms and every
#                   JIS X 0208 kanji (levels 1 and 2, ~6,355 characters)
# Requires python3 and network access to raw.githubusercontent.com and PyPI.
# Usage: scripts/subset-fonts.sh
set -euo pipefail

# Upstream commits (record changes in docs/VERSIONS.md).
NOTO_LATIN_COMMIT=4e15001b9fb484cc871d7b2ad930cd2226edf260   # notofonts/notofonts.github.io
NOTO_CJK_COMMIT=f8d157532fbfaeda587e826d4cd5b21a49186f7c     # notofonts/noto-cjk
FONTTOOLS_VERSION=4.66.1

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$HERE/assets/fonts"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

python3 -m pip install -q --target "$WORK/py" "fonttools==$FONTTOOLS_VERSION" brotli
export PYTHONPATH="$WORK/py"

fetch() { curl -fsSL -o "$2" "$1"; }
LATIN="https://raw.githubusercontent.com/notofonts/notofonts.github.io/$NOTO_LATIN_COMMIT/fonts/NotoSans/unhinted/ttf"
CJK="https://raw.githubusercontent.com/notofonts/noto-cjk/$NOTO_CJK_COMMIT/Sans/SubsetOTF/JP"
for w in Regular Bold; do
  fetch "$LATIN/NotoSans-$w.ttf" "$WORK/NotoSans-$w.ttf"
  fetch "$CJK/NotoSansJP-$w.otf" "$WORK/NotoSansJP-$w.otf"
done
fetch "https://raw.githubusercontent.com/notofonts/noto-cjk/$NOTO_CJK_COMMIT/Sans/LICENSE" "$OUT/OFL.txt"

LATIN_RANGES="U+0020-007E,U+00A0-024F,U+0259,U+02B0-02FF,U+0300-036F,U+0370-03FF,U+0400-052F,U+1E00-1EFF,U+2000-206F,U+20A0-20CF,U+2100-214F,U+2190-21FF,U+2200-22FF,U+2460-24FF,U+25A0-25FF,U+FFFD"

# JIS X 0208 is exactly what Shift_JIS can encode.
python3 - "$WORK/jp.txt" <<'PY'
import sys
cps = set(range(0x3000, 0x3100)) | set(range(0x31F0, 0x3200)) | set(range(0xFF00, 0xFFF0))
for cp in range(0x4E00, 0xA000):
    try:
        chr(cp).encode("shift_jis")
        cps.add(cp)
    except UnicodeEncodeError:
        pass
open(sys.argv[1], "w").write(",".join(f"U+{c:04X}" for c in sorted(cps)))
PY

for w in Regular Bold; do
  python3 -m fontTools.subset "$WORK/NotoSans-$w.ttf" --unicodes="$LATIN_RANGES" \
    --layout-features='*' --no-hinting --output-file="$OUT/NotoSans-$w.ttf"
  python3 -m fontTools.subset "$WORK/NotoSansJP-$w.otf" --unicodes-file="$WORK/jp.txt" \
    --layout-features='*' --no-hinting --output-file="$OUT/NotoSansJP-$w.otf"
done
ls -l "$OUT"
