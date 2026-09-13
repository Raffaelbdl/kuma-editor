// Preview stage: engine frames on a canvas, overlay clips as live iframes on top.
//
// Same HTML, two consumers: here the overlay is DOM driven by `seek(t)` from the playhead;
// at export it is baked by hyperframes. The transform/blend/blur/crop effects are
// approximated with CSS so edits show immediately.

import { api } from "./api";
import { store, toast } from "./store";
import type { Clip, Effect, TransformKeyframe } from "./types";
import { isOverlay } from "./types";

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
  private wsOk = false;
  private snapshotTimer = 0;
  private lastSentSeek = -1;
  private lastRevision = -1;
  /** Why the engine is unavailable, as reported by the daemon. */
  private engineError = "";

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
    ws.onopen = () => { this.wsOk = true; this.badge.textContent = "engine"; this.sendSeek(store.state.playhead, true); };
    ws.onmessage = async (m) => {
      if (typeof m.data === "string") {
        try {
          const j = JSON.parse(m.data);
          if (j.error) {
            this.engineError = String(j.error);
            this.badge.textContent = "snapshot (no engine)";
            this.badge.title = this.engineError;
            this.wsOk = false;
            toast(`Preview engine unavailable: ${this.engineError}`, "err", 12000);
          } else if (j.ready) {
            this.engineError = "";
            this.badge.textContent = `engine · ${Math.round((j.scale ?? 0.5) * 100)}%`;
            this.badge.title = "";
          }
        } catch { /* ignore */ }
        return;
      }
      const buf = m.data as ArrayBuffer;
      const frame = Number(new DataView(buf).getBigInt64(0, true));
      const bmp = await createImageBitmap(new Blob([buf.slice(8)], { type: "image/jpeg" }));
      this.draw(bmp);
      if (store.state.playing) store.set({ playhead: frame / store.fps });
    };
    ws.onclose = () => { this.wsOk = false; this.badge.textContent = "snapshot (no engine)"; setTimeout(() => this.connect(), 3000); };
    ws.onerror = () => ws.close();
    this.ws = ws;
  }

  private send(cmd: string) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) this.ws.send(cmd);
  }

  private sendSeek(t: number, force = false) {
    if (!this.wsOk) { this.scheduleSnapshot(t); return; }
    if (!force && Math.abs(t - this.lastSentSeek) < 1e-6) return;
    this.lastSentSeek = t;
    this.send(`seek ${t.toFixed(4)}`);
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
        this.draw(bmp);
      } catch { /* daemon busy or missing media */ }
    }, 250);
  }

  play() {
    if (this.wsOk) { this.send("play"); return; }
    toast(`Cannot play: ${this.engineError || "preview engine not connected"}`, "err", 12000);
  }
  pause() { this.send("pause"); }

  /** Change the engine's preview resolution (fraction of the project size). */
  setScale(scale: number) {
    if (this.wsOk) this.send(`scale ${scale}`);
    else api.previewReload(scale).catch(() => {});
    setTimeout(() => this.sendSeek(store.state.playhead, true), 400);
  }

  /** Called after edits so the engine reloads the compiled timeline. */
  async reload() {
    if (!this.wsOk) return;
    this.send("reload");
    setTimeout(() => this.sendSeek(store.state.playhead, true), 300);
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
    if (revision !== this.lastRevision) {
      this.lastRevision = revision;
      this.layout();
      this.syncOverlays();
      if (this.wsOk) this.reload();
    }
    if (!playing || !this.wsOk) this.sendSeek(playhead);
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
      if (!c || contentKey(c) !== v.key) { v.wrap.remove(); this.views.delete(id); }
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
      const view: OverlayView = { clipId: id, key: contentKey(c), wrap, iframe, ready: false, lastT: -1 };
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
      applyEffects(v.wrap, v.iframe, c, playhead - c.start, p.width, p.height);
    }
  }
}

function contentKey(c: Clip): string {
  return JSON.stringify([c.src, c.speed, c.end - c.start]);
}

/**
 * Seek the overlay document to `t` seconds: every registered GSAP timeline plus every
 * Web Animation (CSS keyframes/transitions). This is the same contract hyperframes bakes
 * against, so preview and export agree.
 */
function seekFrame(v: OverlayView, t: number) {
  const win = v.iframe.contentWindow as (Window & { __timelines?: Record<string, { pause?: () => void; time?: (t: number) => void; seek?: (t: number) => void }> }) | null;
  if (!win) return;
  try {
    for (const tl of Object.values(win.__timelines ?? {})) { tl.pause?.(); if (tl.time) tl.time(t); else tl.seek?.(t); }
    for (const a of win.document.getAnimations()) { a.pause(); a.currentTime = t * 1000; }
  } catch { /* cross-origin or not ready */ }
}

function lerpTransform(e: Extract<Effect, { type: "transform" }>, local: number) {
  const base = { x: e.x ?? 0, y: e.y ?? 0, scale: e.scale ?? 1, rotation: e.rotation ?? 0, opacity: e.opacity ?? 1 };
  const kfs = e.keyframes ?? [];
  if (!kfs.length) return base;
  const fill = (k: TransformKeyframe) => ({ x: k.x ?? base.x, y: k.y ?? base.y, scale: k.scale ?? base.scale, rotation: k.rotation ?? base.rotation, opacity: k.opacity ?? base.opacity });
  if (local <= kfs[0].t) return fill(kfs[0]);
  for (let i = 1; i < kfs.length; i++) {
    if (local <= kfs[i].t) {
      const a = fill(kfs[i - 1]), b = fill(kfs[i]);
      const u = (local - kfs[i - 1].t) / Math.max(1e-6, kfs[i].t - kfs[i - 1].t);
      return { x: a.x + (b.x - a.x) * u, y: a.y + (b.y - a.y) * u, scale: a.scale + (b.scale - a.scale) * u, rotation: a.rotation + (b.rotation - a.rotation) * u, opacity: a.opacity + (b.opacity - a.opacity) * u };
    }
  }
  return fill(kfs[kfs.length - 1]);
}

/** Approximate MLT-side effects with CSS on the overlay wrapper (exact for transform). */
function applyEffects(wrap: HTMLDivElement, iframe: HTMLIFrameElement, c: Clip, local: number, W: number, H: number) {
  let transform = "";
  let opacity = 1;
  let filter = "";
  let blend = "normal";
  let clip = "";
  for (const e of c.effects) {
    if (e.type === "transform") {
      const t = lerpTransform(e, local);
      transform += ` translate(${t.x}px, ${t.y}px) scale(${t.scale}) rotate(${t.rotation}deg)`;
      opacity *= t.opacity;
    } else if (e.type === "blur") filter += ` blur(${e.radius}px)`;
    else if (e.type === "blend") blend = ({ color_dodge: "color-dodge", color_burn: "color-burn", hard_light: "hard-light", soft_light: "soft-light" } as Record<string, string>)[e.mode] ?? e.mode;
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
  wrap.style.mixBlendMode = blend;
}
