#!/usr/bin/env python3
"""Is every scene's text up long enough to read, and not much longer? Overlay clips that end
together form a scene (a title and its subtitle, a panel and its labels; a lone lower third
is a scene of one). Each clip's text, its `string` and `text` vars from the overlay's
manifest, is readable from about start + 0.4 s + characters / CPS. A scene is readable once
its slowest text is, and holds from then to its end.

    reading_audit.py [project.kuma.json] [--cps 14] [--long 2.5]

A negative hold means more text than time: cut words rather than add seconds. A hold over
--long is dead air a cut could take. It is an estimate for text shown at once: text an
overlay reveals as it goes (typed out, a list line by line) is readable later than it says.
Left out: subtitles (`cues` keep their own timing),
clips longer than a minute (a persistent frame, a watermark), and string vars that are
mostly digits and punctuation, which are data (timings, marks) rather than text.
"""
import argparse
import collections
import glob
import json
import os
import re


def visible(s):
    return len(re.sub(r"[*_~`\s]", "", s))


def is_text(v):
    return isinstance(v, str) and sum(c.isalpha() for c in v) * 2 > visible(v)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("project", nargs="?")
    ap.add_argument("--cps", type=float, default=14.0)
    ap.add_argument("--long", type=float, default=2.5)
    a = ap.parse_args()
    path = a.project or glob.glob("*.kuma.json")[0]
    root = os.path.dirname(os.path.abspath(path))
    doc = json.load(open(path))
    b = doc["bin"]
    manifests = {}

    def text_of(entry):
        folder = entry["source"]["overlay"]
        if folder not in manifests:
            try:
                manifests[folder] = json.load(open(os.path.join(root, folder, "overlay.json"))).get("vars", {})
            except (OSError, ValueError):
                manifests[folder] = {}
        spec = manifests[folder]
        vars_ = {k: v.get("default") for k, v in spec.items()}
        vars_.update(entry["source"].get("vars", {}))
        return " ".join(v for k, v in vars_.items() if spec.get(k, {}).get("type") in ("string", "text") and is_text(v))

    for sid, seq in b.items():
        if "tracks" not in seq.get("source", {}):
            continue
        scenes = collections.defaultdict(list)
        for t in seq["source"]["tracks"]:
            if t.get("kind") != "overlay":
                continue
            for c in t["clips"]:
                e = b.get(c["bin"], {})
                if "overlay" not in e.get("source", {}) or c["end"] - c["start"] > 60:
                    continue
                text = text_of(e)
                if visible(text):
                    scenes[round(c["end"], 1)].append((c["start"] + 0.4 + visible(text) / a.cps, c["start"], text))
        if not scenes:
            continue
        print(f"== {sid}  ({seq.get('name', '')})")
        for end, scene in sorted(scenes.items()):
            ready, _, text = max(scene)
            hold = end - ready
            flag = "  SHORT: cut words" if hold < 0 else ("  long: a cut could take it" if hold > a.long else "")
            print(f"  {min(s for _, s, _ in scene):6.1f}–{end:6.1f}  readable at {ready:6.1f}  hold {hold:+5.1f}{flag}  | {text[:50]!r}")


if __name__ == "__main__":
    main()
