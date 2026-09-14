// Preview stage: engine frames on a canvas, overlay clips as live iframes on top.
//
// Same HTML, two consumers: here the overlay is DOM driven by `seek(t)` from the playhead;
// at export it is baked by hyperframes. MLT-side effects on overlays (transform, blur,
// colour, crop, fade) are approximated with CSS so edits show immediately; the render is
// the reference. Blend modes are not approximated: they only show in the render.
//
// This class subscribes to every state change on purpose: overlays must follow the
// playhead frame by frame. It does no DOM rebuilding on that path.

import { api } from "./api";
import { store, toast } from "./store";
import type { Clip, Project } from "./types";
import { isOverlay, transformAt } from "./types";

interface OverlayView {
  clipId: string;
  key: string; // clip content hash: reload when it changes
  wrap: HTMLDivElement;
  iframe: HTMLIFrameElement;
  ready: boolean;
  lastT: number;
}

export class Preview {
  private stage: HTMLDivElement;
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private layer: HTMLDivElement;
  private badge: HTMLDivElement;
  private views = new Map<string, OverlayView>();
  private ws: WebSocket | null = null;
  /** True between the daemon's `ready` and the socket closing: the engine takes seeks. */
  private wsOk = false;
  private snapshotTimer = 0;
  private lastSentSeek = -1;
  private lastRevision = -1;
  private engineKey = "";
  /** Why the engine is unavailable, as reported by the daemon (empty when it runs). */
  private engineError = "";
  /** Timeline time of the last frame the engine sent while playing. */
  private engineT = -1;
  private lastFrame = -1;
  /** Frame index of the last seek; frames from superseded seeks are dropped while paused. */
  private wantFrame = -1;

  constructor(private host: HTMLElement) {
    this.stage = document.createElement("div");
    this.stage.className = "stage";
    this.canvas = document.createElement("canvas");
    this.ctx = this.canvas.getContext("2d")!;
    this.layer = document.createElement("div");
    this.layer.className = "overlay-layer";
    this.badge = document.createElement("div");
    this.badge.className = "badge";
    this.stage.append(this.canvas, this.layer, this.badge);
    host.appendChild(this.stage);
    new ResizeObserver(() => this.layout()).observe(host);
    store.subscribe((s) => this.onState(s.playhead, s.playing, s.project?.revision ?? -1));
    this.connect();
  }

  // ---- engine transport -------------------------------------------------------

  private connect() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/api/preview/ws`);
    ws.binaryType = "arraybuffer";
    ws.onmessage = async (m) => {
      if (typeof m.data === "string") {
        try { this.onStatus(JSON.parse(m.data)); } catch { /* ignore */ }
        return;
      }
      const buf = m.data as ArrayBuffer;
      const frame = Number(new DataView(buf).getBigInt64(0, true));
      const bmp = await createImageBitmap(new Blob([buf.slice(8)], { type: "image/jpeg" }));
      // Decoding is async: a frame can finish after a later one. While playing never step
      // back; while paused only the frame of the latest seek counts.
      if (store.state.playing ? frame < this.lastFrame : frame !== this.wantFrame) { bmp.close(); return; }
      this.lastFrame = frame;
      this.draw(bmp);
      if (store.state.playing) {
        this.engineT = frame / store.fps;
        this.lastSentSeek = this.engineT;
        store.set({ playhead: this.engineT });
      }
    };
    ws.onclose = () => {
      this.wsOk = false;
      this.badge.textContent = "snapshot (no engine)";
      if (store.state.playing) store.set({ playing: false });
      // A missing engine will not appear by itself: back off instead of respawning it every 3 s.
      setTimeout(() => this.connect(), this.engineError ? 30000 : 3000);
    };
    ws.onerror = () => ws.close();
    this.ws = ws;
  }

  /** Daemon status lines: `unavailable` (no engine, socket closes), `ready`, `error`
   *  (a compile failure; the engine keeps its last timeline), `end`. */
  private onStatus(j: { unavailable?: string; ready?: boolean; scale?: number; error?: string; end?: boolean }) {
    if (j.unavailable) {
      // Report a given failure once; the daemon repeats it on every reconnect.
      if (j.unavailable !== this.engineError) toast(`Preview engine unavailable: ${j.unavailable}`, "err", 12000);
      this.engineError = j.unavailable;
      this.badge.title = j.unavailable;
    } else if (j.ready) {
      this.engineError = "";
      this.wsOk = true;
      this.lastFrame = -1;
      this.badge.textContent = `engine · ${Math.round((j.scale ?? 0.5) * 100)}%`;
      this.badge.title = "";
      this.sendSeek(store.state.playhead, true);
    } else if (j.error) {
      toast(`Preview: ${j.error}`, "err", 8000);
    } else if (j.end && store.state.playing) {
      store.set({ playing: false }); // the engine paused itself at the end of the timeline
    }
  }

  private send(cmd: string) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) this.ws.send(cmd);
  }

  private sendSeek(t: number, force = false) {
    if (!force && Math.abs(t - this.lastSentSeek) < 1e-6) return;
    this.lastSentSeek = t;
    this.wantFrame = Math.round(t * store.fps);
    if (this.wsOk) this.send(`seek ${t.toFixed(4)}`);
    else this.scheduleSnapshot(t);
  }

  private scheduleSnapshot(t: number) {
    clearTimeout(this.snapshotTimer);
    this.snapshotTimer = window.setTimeout(async () => {
      try {
        // Ask for as many pixels as the stage actually shows (device pixels), never more
        // than the project is.
        const p = store.state.project;
        const want = Math.ceil(this.stage.clientWidth * (window.devicePixelRatio || 1));
        const width = Math.min(p?.width ?? 1920, Math.max(640, want));
        const bmp = await createImageBitmap(await (await fetch(api.snapshotUrl(t, width))).blob());
        // The engine may have connected while melt was rendering; its frame wins.
        if (this.wsOk) { bmp.close(); return; }
        this.draw(bmp);
      } catch { /* daemon busy or missing media */ }
    }, 250);
  }

  /** Start playback; false (with a toast) when there is no engine to play on. */
  play(): boolean {
    if (!this.wsOk) {
      toast(`Cannot play: ${this.engineError || "preview engine not connected"}`, "err", 12000);
      return false;
    }
    this.lastFrame = -1;
    this.engineT = store.state.playhead;
    this.send("play");
    return true;
  }
  pause() {
    this.send("pause");
    // The next scrub back to this position must fetch a frame again.
    this.lastSentSeek = -1;
  }

  /** Change the engine's preview resolution (fraction of the project size). */
  setScale(scale: number) {
    if (this.wsOk) this.send(`scale ${scale}`);
    else api.previewReload(scale).catch(() => {});
    setTimeout(() => this.sendSeek(store.state.playhead, true), 400);
  }

  /** Called after edits so the engine reloads the compiled timeline. */
  reload() {
    if (!this.wsOk) return;
    this.send("reload");
    // The engine keeps playing through a reload; only a paused preview needs a fresh frame.
    setTimeout(() => { if (!store.state.playing) this.sendSeek(store.state.playhead, true); }, 300);
  }

  // ---- rendering ---------------------------------------------------------------

  private layout() {
    const p = store.state.project;
    if (!p) return;
    const hw = this.host.clientWidth - 24, hh = this.host.clientHeight - 24;
    const s = Math.min(hw / p.width, hh / p.height);
    const w = Math.floor(p.width * s), h = Math.floor(p.height * s);
    this.stage.style.width = `${w}px`;
    this.stage.style.height = `${h}px`;
    for (const v of this.views.values()) v.wrap.style.transform = `scale(${s})`;
  }

  private draw(bmp: ImageBitmap) {
    if (this.canvas.width !== bmp.width || this.canvas.height !== bmp.height) {
      this.canvas.width = bmp.width;
      this.canvas.height = bmp.height;
    }
    this.ctx.drawImage(bmp, 0, 0);
    bmp.close();
  }

  private onState(playhead: number, playing: boolean, revision: number) {
    const p = store.state.project;
    if (!p) return;
    const edited = revision !== this.lastRevision;
    if (edited) {
      this.lastRevision = revision;
      this.syncOverlays();
      // Overlays are live DOM here, never part of the preview compile: an overlay-only
      // edit (vars, a nudge) must not make the engine reload the whole timeline.
      const key = engineKey(p);
      if (key !== this.engineKey) {
        this.engineKey = key;
        this.reload();
      }
    }
    if (playing && this.wsOk) {
      // The engine drives the playhead; a jump that did not come from it is a user seek.
      if (Math.abs(playhead - this.engineT) > 1.5 / store.fps) {
        this.engineT = playhead;
        this.lastFrame = -1;
        this.send(`seek ${playhead.toFixed(4)}`);
        this.send("play");
      }
    } else if (!this.wsOk) {
      this.sendSeek(playhead, edited); // snapshot fallback: an edit changes the picture too
    } else {
      this.sendSeek(playhead);
    }
    this.updateOverlays(playhead);
  }

  // ---- overlays -----------------------------------------------------------------

  private syncOverlays() {
    const p = store.state.project!;
    const wanted = new Map<string, Clip>();
    for (const t of p.tracks) {
      if (t.hidden) continue;
      for (const c of t.clips) if (isOverlay(c)) wanted.set(c.id, c);
    }
    for (const [id, v] of this.views) {
      const c = wanted.get(id);
      if (!c || contentKey(p, c) !== v.key) { v.wrap.remove(); this.views.delete(id); }
    }
    for (const [id, c] of wanted) {
      if (this.views.has(id)) continue;
      const wrap = document.createElement("div");
      wrap.className = "ov";
      wrap.style.width = `${p.width}px`;
      wrap.style.height = `${p.height}px`;
      wrap.style.display = "none";
      const iframe = document.createElement("iframe");
      iframe.width = String(p.width);
      iframe.height = String(p.height);
      iframe.src = `/overlay-preview/${encodeURIComponent(id)}?rev=${p.revision}`;
      wrap.appendChild(iframe);
      this.layer.appendChild(wrap);
      // `?rev=` only defeats the browser cache on (re)creation; a view survives revisions
      // whose content key is unchanged, so a stale rev in the URL is expected.
      const view: OverlayView = { clipId: id, key: contentKey(p, c), wrap, iframe, ready: false, lastT: -1 };
      iframe.addEventListener("load", () => {
        // The hyperframes runtime (injected by the daemon) pauses CSS/WAAPI animations and
        // registers GSAP timelines; from here on the page is a pure function of `seek(t)`.
        view.ready = true;
        this.updateOverlays(store.state.playhead);
      });
      this.views.set(id, view);
    }
    this.layout();
  }

  private updateOverlays(playhead: number) {
    const p = store.state.project;
    if (!p) return;
    for (const v of this.views.values()) {
      const f = store.findClip(v.clipId);
      if (!f) continue;
      const c = f.clip;
      const visible = playhead >= c.start && playhead < c.end;
      v.wrap.style.display = visible ? "" : "none";
      if (!visible || !v.ready) continue;
      const local = (playhead - c.start) * Math.abs(c.speed || 1);
      const t = c.speed < 0 ? (c.end - c.start) * Math.abs(c.speed) - local : local;
      if (Math.abs(t - v.lastT) > 1e-4) {
        v.lastT = t;
        seekFrame(v, t);
      }
      applyEffects(v.iframe, c, playhead - c.start, p.width, p.height);
    }
  }
}

/** Everything the preview compile depends on; overlay tracks are live DOM instead. */
function engineKey(p: Project): string {
  return JSON.stringify([p.fps, p.width, p.height, p.sample_rate, p.assets, p.tracks.filter((t) => t.kind !== "overlay")]);
}

/** Everything the overlay document is a function of: its injected `window.kuma`. */
function contentKey(p: Project, c: Clip): string {
  return JSON.stringify([c.src, c.speed, c.end - c.start, p.width, p.height, p.fps]);
}

/**
 * Seek the overlay document to `t` seconds. The hyperframes runtime's player scrubs
 * every registered GSAP timeline plus Web Animations (CSS keyframes/transitions) exactly
 * as the bake does; until it is up, drive those directly.
 */
function seekFrame(v: OverlayView, t: number) {
  type Timeline = { pause?: () => void; time?: (t: number) => void; seek?: (t: number) => void };
  const win = v.iframe.contentWindow as (Window & { __player?: { renderSeek?: (t: number) => void }; __timelines?: Record<string, Timeline> }) | null;
  if (!win) return;
  try {
    if (win.__player?.renderSeek) { win.__player.renderSeek(t); return; }
    for (const tl of Object.values(win.__timelines ?? {})) { tl.pause?.(); if (tl.time) tl.time(t); else tl.seek?.(t); }
    for (const a of win.document.getAnimations()) { a.pause(); a.currentTime = t * 1000; }
  } catch { /* cross-origin or not ready */ }
}

/** Approximate MLT-side effects with CSS on the overlay iframe (see the header comment). */
function applyEffects(iframe: HTMLIFrameElement, c: Clip, local: number, W: number, H: number) {
  let transform = "";
  let opacity = 1;
  let filter = "";
  let clip = "";
  for (const e of c.effects) {
    if (e.type === "transform") {
      const t = transformAt(e, local);
      transform += ` translate(${t.x}px, ${t.y}px) scale(${t.scale}) rotate(${t.rotation}deg)`;
      opacity *= t.opacity;
    } else if (e.type === "blur") filter += ` blur(${e.radius}px)`;
    else if (e.type === "color") filter += ` brightness(${1 + (e.brightness ?? 0)}) contrast(${e.contrast ?? 1}) saturate(${e.saturation ?? 1})`;
    else if (e.type === "crop") clip = `inset(${(e.top ?? 0) * 100}% ${(e.right ?? 0) * 100}% ${(e.bottom ?? 0) * 100}% ${(e.left ?? 0) * 100}% round ${(e.radius ?? 0) * Math.min(W, H) * 0.5}px)`;
    else if (e.type === "fade") {
      const d = c.end - c.start;
      if (e.in && local < e.in) opacity *= local / e.in;
      if (e.out && local > d - e.out) opacity *= Math.max(0, (d - local) / e.out);
    }
  }
  iframe.style.transformOrigin = "50% 50%";
  iframe.style.transform = transform;
  iframe.style.opacity = String(opacity);
  iframe.style.filter = filter;
  iframe.style.clipPath = clip;
}
