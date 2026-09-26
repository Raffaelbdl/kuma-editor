#!/usr/bin/env python3
"""Cut a Japanese font for a kuma project's `fonts/<Family>/` from a full font or
collection: the JP face, JIS X 0208 (6,356 kanji — every jōyō kanji), kana, Latin, the
symbols a video uses, and every character the project's text vars hold. `list_fonts` then
reports `han` and `kana`, and the next edit's kanji are covered too.

    subset_jp_fonts.py SRC OUT [--face N] [--project p.kuma.json]

  SRC   a .ttf/.otf, or a .ttc collection (Noto CJK: face 0 is JP, then KR, SC, TC, HK —
        taking another face draws Chinese or Korean forms for the same characters)
  OUT   e.g. "fonts/Noto Serif CJK JP SemiBold/NotoSerifCJKjp-SemiBold.otf"

Example (Debian/Ubuntu's fonts-noto-cjk):
  subset_jp_fonts.py /usr/share/fonts/opentype/noto/NotoSerifCJK-SemiBold.ttc \
      "fonts/Noto Serif CJK JP SemiBold/NotoSerifCJKjp-SemiBold.otf" --project myvideo.kuma.json

Keeps every layout feature (vertical `vert`/`vrt2`, proportional kana) and only the
legacy names (IDs 0-6): a collection's typographic family (ID 16) would merge
"… SemiBold" and "… Black" into one family. Needs fontTools.
"""
import argparse
import json
import os

from fontTools import subset
from fontTools.ttLib import TTFont


def jis_x_0208():
    out = set()
    for row in range(0xA1, 0xFF):
        for cell in range(0xA1, 0xFF):
            try:
                out.add(ord(bytes([row, cell]).decode("euc_jp")))
            except UnicodeDecodeError:
                pass
    return out


def project_text(path):
    chars = set()
    def walk(v):
        if isinstance(v, str):
            chars.update(map(ord, v))
        elif isinstance(v, dict):
            for x in v.values():
                walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)
    for e in json.load(open(path))["bin"].values():
        walk(e.get("source", {}).get("vars", {}))
    return chars


def spans(*pairs):
    return {c for a, b in pairs for c in range(a, b + 1)}


BASE = spans((0x20, 0x7E), (0xA0, 0x17F), (0x2000, 0x206F), (0x2070, 0x209F), (0x2190, 0x21FF), (0x2200, 0x22FF),
             (0x2460, 0x24FF), (0x2500, 0x257F), (0x25A0, 0x25FF), (0x2600, 0x266F), (0x2700, 0x27BF),
             (0x3000, 0x30FF), (0x31F0, 0x31FF), (0xFF00, 0xFFEF))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("out")
    ap.add_argument("--face", type=int, default=None, help="face in a collection (default: the one named JP, else 0)")
    ap.add_argument("--project", help="also keep every character this project's text vars use")
    a = ap.parse_args()
    face = a.face
    if face is None and a.src.lower().endswith((".ttc", ".otc")):
        face = 0
        for i in range(8):
            try:
                if " JP" in TTFont(a.src, fontNumber=i, lazy=True)["name"].getDebugName(4):
                    face = i
                    break
            except Exception:
                break
    font = TTFont(a.src, fontNumber=face or 0)
    print("face:", font["name"].getDebugName(4))
    wanted = BASE | jis_x_0208() | (project_text(a.project) if a.project else set())
    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.name_IDs = [0, 1, 2, 3, 4, 5, 6]
    opts.notdef_outline = True
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=wanted & set(font.getBestCmap()))
    sub.subset(font)
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    font.save(a.out)
    cmap = font.getBestCmap()
    kanji = sum(1 for c in cmap if 0x4E00 <= c <= 0x9FFF)
    print(f"{a.out}: {len(cmap)} characters, {kanji} kanji, {os.path.getsize(a.out) / 1e6:.2f} MB, family {font['name'].getDebugName(1)!r}")
    for probe in "永あ":
        if ord(probe) not in cmap:
            print(f"warning: {probe} missing — kuma's list_fonts will not report this family as setting Japanese")
    if a.project:
        missing = "".join(sorted(chr(c) for c in project_text(a.project) if c > 0x2FFF and c not in cmap))
        print("missing from the project's text:", missing or "-")


if __name__ == "__main__":
    main()
