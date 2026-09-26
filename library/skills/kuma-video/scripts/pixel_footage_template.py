#!/usr/bin/env python3
"""A starting point for footage MLT can cut: a pixel-art seascape (sky, sun or moon, a
mountain, hills, a glinting sea), drawn at 320×180 in a small palette with ordered
dithering, upscaled ×6 with nearest neighbour, and encoded as real H.264. Every frame is a
function of its time alone, so re-running reproduces the file byte for byte. Dusk and night
share every shape and differ only in palette, so a dissolve between them lines up.

    pixel_footage_template.py [--seconds 20] [--fps 24] [--out media] [--preview]

--preview writes stills at t = 0, middle and end (×3) instead of encoding: look before
you encode. Adapt the layer functions and palettes; keep the rules (small canvas,
palette + Bayer, pure function of t, even integer upscale, a still of the middle frame).
Make the clip longer than any placement will read into it. Needs numpy, Pillow, ffmpeg.
"""
import argparse
import math
import os
import subprocess

import numpy as np
from PIL import Image

W, H, S = 320, 180, 6
HORIZON = 118
BAYER = (np.array([[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]]) + 0.5) / 16

PALETTES = {
    "dusk": dict(sky=["#1a1640", "#2e1f5c", "#52286e", "#86306f", "#c2466b", "#e8705a", "#f59e5b", "#f8c77a"],
                 sea=["#5a3478", "#3f2a6a", "#2c2156", "#1c1840", "#120f2c"], glint="#ffd98a",
                 far="#6a3a78", mountain="#4d2c66", shade="#2f1c48", snow="#f7cdd0", near="#3a2152",
                 light="#ffd27a", stars=10),
    "night": dict(sky=["#05081c", "#0a1030", "#101944", "#172258", "#1f2d6a", "#2c3b7a"],
                  sea=["#1e2a60", "#16204c", "#10183c", "#0b112c", "#070b1e"], glint="#f4efd2",
                  far="#1c2452", mountain="#232a58", shade="#161a3e", snow="#c8d4f0", near="#121838",
                  light="#f4efd2", stars=140),
}


def rgb(h):
    return np.array([int(h.lstrip("#")[i:i + 2], 16) for i in (0, 2, 4)], np.uint8)


def dithered(y0, y1, colors):
    """A vertical gradient through `colors`, quantised to them with the Bayer matrix."""
    ys, xs = np.arange(y0, y1)[:, None], np.arange(W)[None, :]
    v = (ys - y0) / max(1, y1 - y0 - 1) * (len(colors) - 1)
    idx = np.floor(v + BAYER[ys % 4, xs % 4]).clip(0, len(colors) - 1).astype(int)
    return np.stack([rgb(c) for c in colors])[idx]


def ridge(cx, summit, half_width, power=1.9):
    """The mountain's top edge per column (large where it is absent)."""
    x = np.arange(W) + 0.5
    k = np.clip((np.abs(x - cx) - 8) / (half_width - 8), 0, None)
    return np.where(k < 1, HORIZON - (HORIZON - summit) * (1 - np.minimum(k, 1)) ** power, 1e9)


def hills(img, color, seed, amp, top):
    r = np.random.default_rng(seed)
    phase = r.random(3) * 6.28
    x = np.arange(W)
    edge = top + amp * (0.5 + 0.25 * np.sin(x * 0.03 + phase[0]) + 0.15 * np.sin(x * 0.071 + phase[1]) + 0.1 * np.sin(x * 0.17 + phase[2]))
    ys = np.arange(H)[:, None]
    img[(ys >= edge[None, :]) & (ys < HORIZON)] = rgb(color)


def frame(t, p, night):
    img = np.zeros((H, W, 3), np.uint8)
    img[:HORIZON] = dithered(0, HORIZON, p["sky"])
    r = np.random.default_rng(3)                                  # stars: fixed places, twinkling by t
    for i in range(p["stars"]):
        x, y = int(r.integers(0, W)), int(r.integers(0, 100 if night else 40))
        if math.sin(t * 2.1 + i) > -0.6:
            img[y, x] = rgb("#fffbe0" if i % 3 else "#8f9ad0")
    lx, ly, lr = 222, (40 if night else 84 + 1.0 * t), 13 if night else 28  # the moon, or a sun that sinks
    ys, xs = np.mgrid[0:HORIZON, 0:W]
    disc = (xs - lx) ** 2 + (ys - ly) ** 2 <= lr ** 2
    if not night:  # city-pop stripes cut through the lower half of the sun
        disc &= ~((ys > ly) & (((ys - ly) // 3) % 2 == 1))
    img[:HORIZON][disc] = rgb(p["light"])
    hills(img, p["far"], 5, 10, 96)
    top = ridge(96, 50, 92)
    for x in range(W):
        y0 = int(top[x])
        if y0 >= HORIZON:
            continue
        lit = x >= 96                                             # the light is to the right
        for y in range(y0, HORIZON):
            snow = y < 50 + 17 + 3 * math.sin(x * 0.9)
            c = p["snow"] if snow else (p["mountain"] if lit else p["shade"])
            if not snow and not lit and BAYER[y % 4, x % 4] > 0.8:
                c = p["mountain"]                                  # a dithered terminator
            img[y, x] = rgb(c)
    hills(img, p["near"], 9, 6, 108)
    img[HORIZON:] = dithered(HORIZON, H, p["sea"])
    r = np.random.default_rng(11)                                  # wave dashes drifting left
    for _ in range(90):
        x0, y, n, ph = int(r.integers(0, W)), int(r.integers(HORIZON + 2, H)), int(r.integers(3, 12)), r.random() * 6
        x = int(x0 - t * (4 + (y - HORIZON) * 0.2) + 3 * math.sin(t + ph)) % W
        img[y, x:x + n] = rgb(p["sea"][0])
    for y in range(HORIZON + 1, H):                                # the glint column under the light
        half = 3 + (y - HORIZON) * 0.25
        for seg in range(4):
            if math.sin(t * 3 + y * 0.7 + seg * 1.9) > 0.2:
                x = int(lx - half + seg * half / 2)
                img[y, max(0, x):min(W, x + 2)] = rgb(p["glint"])
    return img


def encode(name, p, night, seconds, fps, out):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, f"{name}.mp4")
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W * S}x{H * S}", "-r", str(fps),
           "-i", "-", "-c:v", "libx264", "-preset", "slow", "-crf", "14", "-pix_fmt", "yuv420p", "-movflags", "+faststart", path]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    n = int(seconds * fps)
    for i in range(n):
        big = frame(i / fps, p, night).repeat(S, 0).repeat(S, 1)
        proc.stdin.write(big.tobytes())
        if i == n // 2:
            Image.fromarray(big).save(os.path.join(out, f"{name}.png"))
    proc.stdin.close()
    proc.wait()
    print("wrote", path, "and", os.path.join(out, f"{name}.png"))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=20)
    ap.add_argument("--fps", type=int, default=24)
    ap.add_argument("--out", default="media")
    ap.add_argument("--preview", action="store_true")
    a = ap.parse_args()
    for name, p in PALETTES.items():
        if a.preview:
            os.makedirs(a.out, exist_ok=True)
            for t in (0, a.seconds / 2, a.seconds):
                f = os.path.join(a.out, f"{name}_preview_{t:g}.png")
                Image.fromarray(frame(t, p, name == "night").repeat(3, 0).repeat(3, 1)).save(f)
                print("wrote", f)
        else:
            encode(name, p, name == "night", a.seconds, a.fps, a.out)


if __name__ == "__main__":
    main()
