#!/usr/bin/env python3
"""Every CJK character a kuma project shows must be drawn by a font the project controls:
the overlay's own font var, or its `jp` fallback var. Anything else is drawn by whatever
CJK font the baking machine picks — on Linux under `lang="en"` that is Noto CJK SC, with
Chinese glyph forms. Also flags overlay pages holding CJK text without `lang="ja"`.

    fontcheck.py [project.kuma.json]          exit 1 if anything is reported

Fonts are read from the project's `fonts/`, `~/.local/share/kuma/fonts` and every
`$KUMA_LIBRARY/fonts`, by the family names in the files (IDs 1 and 16). Needs fontTools.
"""
import collections
import glob
import json
import os
import re
import sys

from fontTools.ttLib import TTCollection, TTFont


def cjk(ch):
    o = ord(ch)
    return 0x3000 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF00 <= o <= 0xFFEF or 0x20000 <= o <= 0x2FFFF


def font_dirs(root):
    dirs = [os.path.join(root, "fonts"), os.path.expanduser("~/.local/share/kuma/fonts")]
    dirs += [os.path.join(d, "fonts") for d in os.environ.get("KUMA_LIBRARY", "").split(os.pathsep) if d]
    return [d for d in dirs if os.path.isdir(d)]


def load_families(root):
    fam = collections.defaultdict(set)
    for d in font_dirs(root):
        for path in glob.glob(os.path.join(d, "**", "*"), recursive=True):
            ext = path.lower().rsplit(".", 1)[-1]
            if ext not in ("ttf", "otf", "ttc", "otc"):
                continue
            faces = TTCollection(path).fonts if ext in ("ttc", "otc") else [TTFont(path, lazy=True)]
            for f in faces:
                cmap = set(f.getBestCmap() or {})
                for nid in (1, 16):
                    n = f["name"].getDebugName(nid)
                    if n:
                        fam[n] |= cmap
    return fam


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else (glob.glob("*.kuma.json") or [None])[0]
    if not path:
        sys.exit("give the project file")
    root = os.path.dirname(os.path.abspath(path))
    doc = json.load(open(path))
    fam = load_families(root)
    problems = 0
    lang_warned = set()
    for eid, e in sorted(doc["bin"].items()):
        src = e.get("source", {})
        if "overlay" not in src:
            continue
        folder = os.path.join(root, src["overlay"])
        try:
            spec = json.load(open(os.path.join(folder, "overlay.json"))).get("vars", {})
        except FileNotFoundError:
            continue
        vars_ = {k: v.get("default") for k, v in spec.items()}
        vars_.update(src.get("vars", {}))
        fonts = {k: vars_.get(k) for k, v in spec.items() if v.get("type") == "font"}
        main_font = fonts.get("font") or next(iter(fonts.values()), None)
        chain = [f for f in (main_font, fonts.get("jp")) if f]
        shown = set()
        for k, v in vars_.items():
            if isinstance(v, str) and spec.get(k, {}).get("type") != "font":
                shown |= {c for c in v if cjk(c)}
        if not shown:
            continue
        missing = {c for c in shown if not any(ord(c) in fam.get(f, ()) for f in chain)}
        if missing:
            problems += 1
            print(f"{eid}: {''.join(sorted(missing))} — not in {' or '.join(chain) or 'any declared font'}"
                  + ("" if "jp" in fonts else "  (the overlay has no `jp` fallback var)"))
        html = os.path.join(folder, "index.html")
        if folder not in lang_warned and os.path.isfile(html):
            m = re.search(r"<html[^>]*\blang=\"([^\"]+)\"", open(html, encoding="utf-8").read())
            if not m or not m.group(1).startswith(("ja", "zh", "ko")):
                lang_warned.add(folder)
                problems += 1
                print(f"{src['overlay']}/index.html: shows CJK text but its page is lang=\"{m.group(1) if m else ''}\" — use lang=\"ja\"")
    print("ok: every CJK character is drawn by a project font" if not problems else f"{problems} problem(s)")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
