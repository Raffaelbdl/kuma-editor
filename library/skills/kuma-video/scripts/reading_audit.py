#!/usr/bin/env python3
"""Is every scene on screen long enough to read, and not much longer? For each sequence,
overlay clips that end together form a scene; each text clip is readable from about
start + 0.4 s + characters / CPS (typed windows: their typing time + 1.2 s; lists: their
last item + its own reading). The scene's hold is its end minus the latest of those.

    reading_audit.py [project.kuma.json] [--cps 14] [--long 2.5]

A negative hold means more text than time: cut words rather than add seconds. A hold
over --long is dead air a cut could take. The master is skipped (it only places chapters).
"""
import argparse
import collections
import glob
import json
import os
import re


def visible(s):
    return len(re.sub(r"[*_~`]", "", s).replace("\n", ""))


def ready(clip, entry, cps):
    v = entry["source"].get("vars", {})
    t0 = clip["start"]
    text = v.get("text") if isinstance(v.get("text"), str) else ""
    if not text:
        return t0 + 1.0, ""
    if "cps" in v:  # typed windows
        return t0 + float(v.get("delay", 0.2)) + visible(text) / float(v["cps"]) + 1.2, text
    items = text.split("\n")
    if "times" in v or "stagger" in v:  # lists that reveal line by line
        if "times" in v:
            ts = [float(x) for x in str(v["times"]).split(",") if x.strip()]
        else:
            ts = [i * float(v["stagger"]) for i in range(len(items))]
        last = ts[-1] if ts else 0.0
        return max(t0 + last + visible(items[-1]) / cps + 0.5, t0 + visible(text) / cps + 0.8), text
    return t0 + 0.4 + max(1.0, visible(text) / cps), text


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("project", nargs="?")
    ap.add_argument("--cps", type=float, default=14.0)
    ap.add_argument("--long", type=float, default=2.5)
    a = ap.parse_args()
    path = a.project or glob.glob("*.kuma.json")[0]
    doc = json.load(open(path))
    b = doc["bin"]
    for sid, seq in b.items():
        if "tracks" not in seq.get("source", {}) or sid == doc["master"]:
            continue
        scenes = collections.defaultdict(list)
        for t in seq["source"]["tracks"]:
            if t.get("kind") != "overlay":
                continue
            for c in t["clips"]:
                e = b.get(c["bin"], {})
                if "overlay" not in e.get("source", {}) or c["end"] - c["start"] > 60:
                    continue  # HUDs and other chapter-long furniture
                r, text = ready(c, e, a.cps)
                scenes[round(c["end"], 1)].append((r, c["start"], text))
        # A scene starts with its first text, not with furniture that shares its end.
        rows = [(end, min((s for _, s, txt in g if txt), default=min(s for _, s, _ in g)), max(g))
                for end, g in sorted(scenes.items()) if len(g) >= 2]
        if not rows:
            continue
        print(f"== {sid}  ({seq.get('name', '')})")
        for end, first, (r, _, text) in rows:
            hold = end - r
            flag = "  SHORT: cut words" if hold < 0 else ("  long: a cut could take it" if hold > a.long else "")
            print(f"  {first:6.1f}–{end:6.1f}  readable at {r:6.1f}  hold {hold:+5.1f}{flag}  | {text[:50]!r}")


if __name__ == "__main__":
    main()
