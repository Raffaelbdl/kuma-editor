#!/usr/bin/env python3
"""Frames of a video at the given times, tiled into one image with each time printed on it.
Use it after every render: every cut ±0.3 s and each key scene, then look.

    contact_sheet.py video.mp4 sheet.png 3.5 41.7 42.3 94 … [--cols 2] [--width 640]

Needs ffmpeg and Pillow.
"""
import argparse
import io
import subprocess

from PIL import Image, ImageDraw


def frame(video, t, width):
    png = subprocess.run(["ffmpeg", "-v", "error", "-ss", str(t), "-i", video, "-frames:v", "1", "-vf", f"scale={width}:-2",
                          "-f", "image2pipe", "-vcodec", "png", "-"], capture_output=True, check=True).stdout
    return Image.open(io.BytesIO(png)).convert("RGB")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("video")
    ap.add_argument("out")
    ap.add_argument("times", nargs="+", type=float)
    ap.add_argument("--cols", type=int, default=2)
    ap.add_argument("--width", type=int, default=640)
    a = ap.parse_args()
    shots = [(t, frame(a.video, t, a.width)) for t in a.times]
    w, h = shots[0][1].size
    rows = (len(shots) + a.cols - 1) // a.cols
    sheet = Image.new("RGB", (w * a.cols, h * rows), "#202020")
    d = ImageDraw.Draw(sheet)
    for i, (t, im) in enumerate(shots):
        x, y = (i % a.cols) * w, (i // a.cols) * h
        sheet.paste(im, (x, y))
        d.rectangle([x, y, x + 64, y + 18], fill="#000000")
        d.text((x + 4, y + 3), f"{t:.2f}s", fill="#ffffff")
    sheet.save(a.out)
    print(f"wrote {a.out}: {len(shots)} frames, {sheet.size[0]}×{sheet.size[1]}")


if __name__ == "__main__":
    main()
