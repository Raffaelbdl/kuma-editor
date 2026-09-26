#!/usr/bin/env python3
"""Model a kuma project's audio mix offline: every audible clip of the master (nested
sequences included, cut to their placements) with its volume, `volume` effects and
`audio_fade`, summed as kuma does. kuma has no limiter, so loudness and peak are whatever
the gains make them — measure before rendering.

    mixsim.py [project.kuma.json] [--out mix.wav] [--gain bin=vol ...] [--lufs]

--gain replaces the volume of every clip of that bin entry (try gains without editing).
--lufs measures the result with ffmpeg's ebur128: integrated, true peak, and per clip of
the master's first video track (the chapters). Matches a real render within ~0.5 LU; the
AAC encode adds ~0.5-0.8 dB of true peak on top. Needs numpy and ffmpeg.
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import wave

import numpy as np


def find_project(arg):
    if arg:
        return arg
    found = glob.glob("*.kuma.json")
    if len(found) != 1:
        sys.exit("give the project file (found %d *.kuma.json here)" % len(found))
    return found[0]


class Mix:
    def __init__(self, path, gains):
        self.root = os.path.dirname(os.path.abspath(path))
        self.doc = json.load(open(path))
        self.bin = self.doc["bin"]
        self.sr = int(self.doc.get("sample_rate", 48000))
        self.gains = gains
        self.cache = {}
        self.warned = set()

    def warn(self, msg):
        if msg not in self.warned:
            self.warned.add(msg)
            print("warning:", msg, file=sys.stderr)

    def decode(self, rel):
        if rel not in self.cache:
            path = rel if os.path.isabs(rel) else os.path.join(self.root, rel)
            raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-vn", "-ac", "2", "-ar", str(self.sr), "-f", "f32le", "-"],
                                 capture_output=True, check=True).stdout
            self.cache[rel] = np.frombuffer(raw, dtype="<f4").reshape(-1, 2).astype(np.float64)
        return self.cache[rel]

    def length(self, seq):
        return max((c["end"] for t in self.bin[seq]["source"]["tracks"] for c in t["clips"]), default=0.0)

    def audible(self, entry, clip):
        src = entry.get("source", {})
        if src.get("kind") == "audio":
            return True
        return src.get("kind") == "video" and entry.get("meta", {}).get("has_audio") and not clip.get("muted")

    def clips(self, seq, offset, lo, hi):
        """(start, end, source time at start, clip, entry) in master seconds, inside [lo, hi)."""
        for track in self.bin[seq]["source"]["tracks"]:
            if track.get("muted") or track.get("kind") == "overlay":
                continue
            for c in track["clips"]:
                if c.get("muted") and track.get("kind") == "audio":
                    continue
                s0, s1 = offset + c["start"], offset + c["end"]
                a, b = max(s0, lo), min(s1, hi)
                if b <= a:
                    continue
                e = self.bin.get(c["bin"], {})
                if "tracks" in e.get("source", {}):
                    yield from self.clips(c["bin"], s0 - c.get("in", 0.0), a, b)
                elif self.audible(e, c):
                    yield a, b, s0, c, e

    def render(self):
        total = self.length(self.doc["master"])
        out = np.zeros((int(total * self.sr) + 1, 2))
        for a, b, s0, c, e in self.clips(self.doc["master"], 0.0, 0.0, total):
            src = self.decode(e["source"]["path"])
            speed = c.get("speed", 1.0)
            n = int(round((b - a) * self.sr))
            t_clip = (a - s0) + np.arange(n) / self.sr          # seconds into the clip
            idx = ((c.get("in", 0.0) + t_clip * speed) * self.sr).astype(int)
            idx = idx[idx < len(src)]
            seg = src[idx]
            gain = self.gains.get(c["bin"], c.get("volume", 1.0))
            env = np.full(len(seg), float(gain))
            dur = c["end"] - c["start"]
            tc = t_clip[: len(seg)]
            for f in c.get("effects", []):
                if f["type"] == "volume":
                    if f.get("keyframes"):
                        self.warn(f"{c['id']}: volume keyframes are modelled at their static gain")
                    env *= f.get("gain", 1.0)
                elif f["type"] == "audio_fade":
                    fi, fo = f.get("in", 0.0), f.get("out", 0.0)
                    if 0 < fi < 2 / 24 or 0 < fo < 2 / 24:
                        self.warn(f"{c['id']}: a fade under 2 frames — kuma may apply it to the whole clip; make it longer")
                    if fi > 0:
                        env *= np.clip(tc / fi, 0, 1)
                    if fo > 0:
                        env *= np.clip((dur - tc) / fo, 0, 1)
            k = int(round(a * self.sr))
            out[k:k + len(seg)] += seg * env[:, None]
        return out, total


def ebur(path, ss=None, to=None):
    cmd = ["ffmpeg", "-hide_banner", "-nostats"] + (["-ss", str(ss), "-to", str(to)] if ss is not None else []) + ["-i", path, "-af", "ebur128=peak=true", "-f", "null", "-"]
    txt = subprocess.run(cmd, capture_output=True, text=True).stderr
    i = re.findall(r"I:\s+(-?[\d.]+) LUFS", txt)
    p = re.findall(r"Peak:\s+(-?[\d.]+) dBFS", txt)
    return (float(i[-1]) if i else None), (float(p[-1]) if p else None)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("project", nargs="?")
    ap.add_argument("--out", default="mixsim.wav")
    ap.add_argument("--gain", action="append", default=[], metavar="BIN=VOL")
    ap.add_argument("--lufs", action="store_true")
    a = ap.parse_args()
    gains = {g.split("=")[0]: float(g.split("=")[1]) for g in a.gain}
    m = Mix(find_project(a.project), gains)
    mix, total = m.render()
    peak = np.abs(mix).max(axis=1)
    i = int(peak.argmax())
    print(f"{total:.1f} s modelled; sample peak {20 * np.log10(max(peak[i], 1e-9)):.2f} dBFS at {i / m.sr:.2f} s")
    # The five loudest half-seconds: where to look when the peak is too hot.
    w = m.sr // 2
    blocks = peak[: len(peak) // w * w].reshape(-1, w).max(axis=1)
    top = sorted(np.argsort(blocks)[::-1][:5])
    print("hottest moments:", ", ".join(f"{t * 0.5:.1f}s ({20 * np.log10(max(blocks[t], 1e-9)):.1f})" for t in top))
    with wave.open(a.out, "wb") as f:
        f.setnchannels(2); f.setsampwidth(2); f.setframerate(m.sr)
        f.writeframes((np.clip(mix, -1, 1) * 32767).astype("<i2").tobytes())
    print("wrote", a.out)
    if a.lufs:
        il, tp = ebur(a.out)
        print(f"integrated {il} LUFS, true peak {tp} dBFS")
        master = m.bin[m.doc["master"]]["source"]["tracks"]
        chapters = next((t["clips"] for t in master if t.get("kind") == "video" and t["clips"]), [])
        for c in chapters:
            li, _ = ebur(a.out, c["start"], c["end"])
            print(f"  {c['start']:7.1f}-{c['end']:7.1f}  {li:6.1f} LUFS  {c.get('label') or c['bin']}")


if __name__ == "__main__":
    main()
