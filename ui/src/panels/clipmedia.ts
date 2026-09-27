// What a media clip shows of itself on the timeline: frames along the top, its waveform
// along the bottom — Kdenlive's picture of a clip, without Kdenlive's split into a video
// and an audio track. A clip here is one placement of a file with its sound, which is what
// makes it one thing for an agent to move; the timeline only *draws* it in two bands.
//
// Both pictures are the daemon's (`/api/bin/{id}/strip.json` and `peaks.json`), made on
// first asking and cached in `.kuma/`. Here each is fetched once a session per entry and
// file, and a clip drawn before its data has arrived is filled in when it does.

import { api } from "../api";
import { el } from "../dom";
import type { Clip } from "../types";

interface Strip { count: number; step: number; width: number; height: number }
interface Peaks { rate: number; peaks: number[] }

/** A fetch, and its value once it has one — so a re-render draws synchronously. */
interface Loaded<T> { value?: T | null; wait: Promise<T | null> }
const strips = new Map<string, Loaded<Strip>>();
const waves = new Map<string, Loaded<Peaks>>();

function load<T>(cache: Map<string, Loaded<T>>, key: string, get: () => Promise<T>): Loaded<T> {
  let l = cache.get(key);
  if (!l) {
    // A failure (no ffmpeg, a missing file) draws nothing rather than toasting once per
    // clip: the bin and validation already say what is wrong with the entry.
    const entry: Loaded<T> = { wait: get().catch(() => null).then((v) => (entry.value = v)) };
    cache.set(key, entry);
    l = entry;
  }
  return l;
}

/** Draw `draw` now if the data is here, else when it arrives and the node is still up. */
function whenLoaded<T>(l: Loaded<T>, node: Element, draw: (v: T) => void) {
  if (l.value !== undefined) { if (l.value) draw(l.value); return; }
  void l.wait.then((v) => { if (v && node.isConnected) draw(v); });
}

/** Source seconds at `x` pixels into the clip. */
const sourceAt = (c: Clip, zoom: number, x: number) => (c.in ?? 0) + (x / zoom) * Math.abs(c.speed || 1);

/**
 * Frames across `band`, `width`×`height` pixels of clip: one per slot, each the frame at
 * that moment of the source. Zoomed in far, slots are spaced out rather than multiplied —
 * a few hundred divs per clip is the ceiling, and a gap shows the clip's colour.
 */
export function drawStrip(band: HTMLElement, bin: string, path: string, c: Clip, zoom: number, width: number, height: number) {
  whenLoaded(load(strips, `${bin}\n${path}`, () => api.strip(bin).then((r) => r.strip)), band, (s) => {
    const tile = (s.width * height) / s.height;
    const slot = Math.max(tile, width / 200);
    const url = `url("/api/bin/${encodeURIComponent(bin)}/strip.jpg?v=${encodeURIComponent(path)}")`;
    for (let x = 0; x < width; x += slot) {
      const i = s.step ? Math.min(s.count - 1, Math.max(0, Math.floor(sourceAt(c, zoom, x + tile / 2) / s.step))) : 0;
      const f = el("div");
      Object.assign(f.style, {
        left: `${x}px`, width: `${tile}px`, backgroundImage: url,
        backgroundSize: `${s.count * tile}px ${height}px`, backgroundPosition: `${-i * tile}px 0`,
      });
      band.appendChild(f);
    }
  });
}

/** The waveform on `canvas`: the loudest moment under each pixel column, mirrored. */
export function drawWave(canvas: HTMLCanvasElement, bin: string, path: string, c: Clip, zoom: number, width: number, height: number) {
  whenLoaded(load(waves, `${bin}\n${path}`, () => api.peaks(bin)), canvas, (w) => {
    // Its colour is the theme's, read off the page, so a canvas still being built — the
    // timeline assembles a row before attaching it — is painted once it is attached.
    if (!canvas.isConnected) { queueMicrotask(() => { if (canvas.isConnected) paint(canvas, w, c, zoom, width, height); }); return; }
    paint(canvas, w, c, zoom, width, height);
  });
}

function paint(canvas: HTMLCanvasElement, w: Peaks, c: Clip, zoom: number, width: number, height: number) {
  // A canvas has a size limit; past it the wave is stretched, which a wave survives.
  const dpr = window.devicePixelRatio || 1;
  const cw = Math.max(1, Math.min(8192, Math.round(width * dpr)));
  const ch = Math.max(1, Math.round(height * dpr));
  canvas.width = cw;
  canvas.height = ch;
  const g = canvas.getContext("2d");
  if (!g) return;
  g.fillStyle = getComputedStyle(canvas).color;
  const mid = ch / 2;
  for (let x = 0; x < cw; x++) {
    const a = Math.floor(sourceAt(c, zoom, (x / cw) * width) * w.rate);
    const b = Math.max(a + 1, Math.ceil(sourceAt(c, zoom, ((x + 1) / cw) * width) * w.rate));
    let m = 0;
    for (let i = Math.max(0, a); i < Math.min(b, w.peaks.length); i++) m = Math.max(m, w.peaks[i]);
    // Square-root, so speech at a sensible level is more than a hairline.
    const h = Math.max(m ? 1 : 0, Math.sqrt(m / 255) * mid);
    g.fillRect(x, mid - h, 1, 2 * h);
  }
}
