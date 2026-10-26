// Preview stage: engine frames on a canvas, overlay clips as live iframes on top.
//
// Same HTML, two consumers: here the overlay is DOM driven by `seek(t)` from the playhead;
// at export it is baked by hyperframes. MLT-side effects on overlays (transform, blur,
// colour, crop, fade) are approximated with CSS so edits show immediately; the render is
// the reference. Blend modes are not approximated: they only show in the render.
//
// This class subscribes to every state change on purpose: overlays must follow the
// playhead frame by frame. It does no DOM rebuilding on that path.

import { api } from "../api";
import { plural } from "../dom";
import { Cmd, command } from "../commands";
import { seekOverlay } from "../overlay";
import { SEQUENCE_TAB, store, toast } from "../store";
import type { Clip, Project } from "../types";
import { binItem, covers, isOverlayClip, maskOf, paramAt, transformAt } from "../types";

interface OverlayView {
  clipId: string;
  key: string; // clip content hash: reload when it changes
  wrap: HTMLDivElement;
  iframe: HTMLIFrameElement;
  ready: boolean;
  lastT: number;
  /** The clip and playhead its effects were last applied for: unchanged, nothing to do. */
  lastClip?: Clip;
  lastAt?: number;
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
  /** A seek is out at the engine and has not been answered yet. */
  private seekInFlight = false;
  /** The newest position asked for while one was in flight; only the last survives. */
  private pendingSeek: number | null = null;
  private inFlightTimer = 0;
  /** Arrival order of engine frames, so an out-of-order decode never draws backwards. */
  private recvSeq = 0;
  private drawnSeq = 0;
  private lastRevision = -1;
  /** The sequence the stage is showing; opening another one rebuilds it like an edit. */
  private lastSequence = "";
  /** Why the engine is unavailable, as reported by the daemon (empty when it runs). */
  private engineError = "";
  /** Timeline time of the last frame the engine sent while playing. */
  private engineT = -1;
  private lastFrame = -1;

  constructor(host: HTMLElement) {
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
    // The host's size as the observer last saw it: measured by the browser after a layout,
    // so fitting the stage never forces one (an edit used to, right after the timeline was
    // rebuilt, at the cost of laying the whole new timeline out on the spot).
    new ResizeObserver(([e]) => { this.hostSize = { w: e.contentRect.width, h: e.contentRect.height }; this.layout(); }).observe(host);
    store.select((s) => [s.playhead, s.playing, s.project, s.sequence], (s) => this.onState(s.playhead, s.playing, s.project?.revision ?? -1, s.sequence));
    // A monitor nobody is looking at does not play: leaving the tab stops the engine
    // rather than leaving it streaming frames into a hidden canvas.
    store.select((s) => [s.tab], (s) => { if (s.tab !== SEQUENCE_TAB && s.playing) this.togglePlay(); });
    // The store's "not playing" is the engine's too, whoever said it: opening another
    // sequence, the engine reaching the end, another window. Only `togglePlay` used to
    // pause the engine, so a sequence opened while one played went on playing — under
    // controls that no longer reached it.
    store.select((s) => [s.playing], (s) => { if (!s.playing && this.streaming) this.pause(); });
    // A sequence that changes under a playing preview stops it, however it changed (a
    // deleted sequence falling back to the master, another window opening one).
    store.select((s) => [s.sequence], () => { if (store.state.playing) store.set({ playing: false }); });
    // The engine socket lives here, so the transport actions do too. Anything that wants
    // them — the strip under the stage, a key, the application menu — asks by id.
    command(Cmd.togglePlay, "Play/Pause", () => this.togglePlay());
    command(Cmd.reloadPreview, "Reload Preview", () => this.reloadNow());
    this.watchBakes();
    command(Cmd.previewScale, "Preview Resolution", (scale) => this.setScale(Number(scale)));
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
      // Taken before the await: decoding is async and can finish out of order, so order
      // of arrival — not of completion — decides which frame is the newer one.
      const seq = ++this.recvSeq;
      const bmp = await decodeImage(new Uint8Array(buf, 8), "image/jpeg");
      if (seq < this.drawnSeq) { bmp.close(); return; }
      this.drawnSeq = seq;
      // While playing the engine streams in order; never step back over a late arrival.
      if (store.state.playing && frame < this.lastFrame) { bmp.close(); return; }
      this.lastFrame = frame;
      this.draw(bmp);
      // Paused, this frame answers the seek that asked for it: let the next one go.
      if (!store.state.playing) this.seekAnswered();
      if (store.state.playing) {
        this.engineT = frame / store.fps;
        this.lastSentSeek = this.engineT;
        store.set({ playhead: this.engineT });
      }
    };
    ws.onclose = () => {
      this.wsOk = false;
      this.streaming = false;
      this.resetSeeks();
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
      this.resetSeeks();
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

  /**
   * Ask for a frame at `t`.
   *
   * Scrubbing fires a seek per pointer move, which is far more than the engine can
   * answer. Sending them all queued hundreds of renders and the stage sat on its last
   * frame until the backlog drained — the drag looked dead while a click worked fine.
   * So only one seek is ever out: later positions replace each other in `pendingSeek`,
   * and the newest goes the moment a frame comes back. The engine is then always working
   * on a position the pointer actually passed through, and the picture tracks the drag
   * as closely as the engine can render.
   */
  private sendSeek(t: number, force = false) {
    if (!force && Math.abs(t - this.lastSentSeek) < 1e-6) return;
    this.lastSentSeek = t;
    if (!this.wsOk) { this.scheduleSnapshot(t); return; }
    if (this.seekInFlight) { this.pendingSeek = t; return; }
    this.dispatchSeek(t);
  }

  private dispatchSeek(t: number) {
    this.seekInFlight = true;
    this.pendingSeek = null;
    this.send(`seek ${t.toFixed(4)}`);
    clearTimeout(this.inFlightTimer);
    // A seek that never produces a frame (a reload mid-flight, a dropped socket) must not
    // wedge scrubbing for the rest of the session.
    this.inFlightTimer = window.setTimeout(() => this.seekAnswered(), 1000);
  }

  /** A frame arrived, or we gave up waiting: release the slot and send the newest wanted. */
  private seekAnswered() {
    clearTimeout(this.inFlightTimer);
    this.seekInFlight = false;
    if (this.pendingSeek !== null) this.dispatchSeek(this.pendingSeek);
  }

  /** Forget anything outstanding — the frames it would produce are no longer wanted. */
  private resetSeeks() {
    clearTimeout(this.inFlightTimer);
    this.seekInFlight = false;
    this.pendingSeek = null;
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
        const bmp = await decodeImage(new Uint8Array(await (await fetch(api.snapshotUrl(t, width))).arrayBuffer()), "image/png");
        // The engine may have connected while melt was rendering; its frame wins.
        if (this.wsOk) { bmp.close(); return; }
        this.draw(bmp);
      } catch { /* daemon busy or missing media */ }
    }, 250);
  }

  /** Flip between playing and paused, keeping the store's flag in step with the engine. */
  togglePlay() {
    if (store.state.playing) {
      this.pause();
      store.set({ playing: false });
    } else if (this.play()) {
      store.set({ playing: true });
    }
  }

  /** Start playback; false (with a toast) when there is no engine to play on. */
  play(): boolean {
    if (!this.wsOk) {
      toast(`Cannot play: ${this.engineError || "preview engine not connected"}`, "err", 12000);
      return false;
    }
    this.lastFrame = -1;
    this.engineT = store.state.playhead;
    // Playing streams frames unprompted; nothing is answering a seek any more.
    this.resetSeeks();
    this.send("play");
    this.streaming = true;
    return true;
  }
  /** The engine was told to play and has not been told to stop. */
  private streaming = false;
  pause() {
    this.streaming = false;
    this.send("pause");
    this.resetSeeks();
    // The next scrub back to this position must fetch a frame again.
    this.lastSentSeek = -1;
  }

  /** Change the engine's preview resolution (fraction of the project size). */
  setScale(scale: number) {
    if (this.wsOk) this.send(`scale ${scale}`);
    else api.previewReload({ scale }).catch(() => {});
    setTimeout(() => this.sendSeek(store.state.playhead, true), 400);
  }

  /**
   * The Reload button: the timeline handed to the engine again whether or not it changed —
   * a media file replaced on disk is a change the document cannot see — and what came of
   * it said, including the nested overlays still baking behind it (`watchBakes`).
   */
  private async reloadNow() {
    if (store.state.reloading) return;
    store.set({ reloading: true });
    try {
      const r = await api.previewReload({ force: true });
      this.resetSeeks();
      if (!store.state.playing) this.sendSeek(store.state.playhead, true);
      const live = store.tracks.filter((t) => t.kind === "overlay").reduce((n, t) => n + t.clips.length, 0);
      const said = [
        live ? `${plural(live, "overlay")} drawn live` : "",
        r.baked ? `${plural(r.baked, "overlay")} of nested sequences baked` : "",
        r.baking.length ? `${plural(r.baking.length, "more")} baking — the preview shows ${r.baking.length === 1 ? "it" : "them"} when that is done` : "",
      ].filter(Boolean);
      toast(`Preview reloaded${said.length ? ` — ${said.join(", ")}` : ""}`, "ok", r.baking.length ? 6000 : undefined);
    } catch (e) {
      toast(`The preview did not reload: ${(e as Error).message}`, "err", 10000);
    } finally {
      store.set({ reloading: false });
    }
  }

  /**
   * The daemon never makes a compile wait on hyperframes: nested overlays the cache lacks
   * are baked by a `preview-bake` job, and when one ends the preview asks for a reload — as
   * it does after an edit — so they appear. One that would not bake is said once, with why.
   */
  private watchBakes() {
    const seen = new Set<number>();
    store.select((s) => [s.jobs], (s) => {
      for (const j of Object.values(s.jobs)) {
        if (j.kind !== "preview-bake" || seen.has(j.id) || (j.state !== "done" && j.state !== "failed")) continue;
        seen.add(j.id);
        this.reload();
        if (j.state === "failed") toast(`Some nested overlays would not bake, so the preview leaves them out: ${j.message}`, "err", 10000);
      }
    });
  }

  /** Called after edits so the engine reloads the compiled timeline. */
  reload() {
    if (!this.wsOk) return;
    this.send("reload");
    // Whatever was in flight was for the old timeline.
    this.resetSeeks();
    // The engine keeps playing through a reload; only a paused preview needs a fresh frame.
    setTimeout(() => { if (!store.state.playing) this.sendSeek(store.state.playhead, true); }, 300);
  }

  // ---- rendering ---------------------------------------------------------------

  private hostSize = { w: 0, h: 0 };
  /** The frame size the stage was last fitted for: a change (the orientation flipped) is
   *  the one edit that has to fit it again. */
  private fitted = "";

  private layout() {
    const p = store.state.project;
    if (!p) return;
    this.fitted = `${p.width}x${p.height}`;
    // Den shows the frame full-bleed; power leaves it a margin inside the viewer.
    const pad = document.getElementById("app")?.classList.contains("den") ? 0 : 56;
    const hw = this.hostSize.w - pad, hh = this.hostSize.h - pad;
    // Hidden behind another tab: there is nothing to fit into, and sizing the stage to
    // zero would only have to be undone. The observer fires again when it comes back.
    if (hw <= 0 || hh <= 0) return;
    const s = Math.min(hw / p.width, hh / p.height);
    const w = Math.floor(p.width * s), h = Math.floor(p.height * s);
    this.stage.style.width = `${w}px`;
    this.stage.style.height = `${h}px`;
    // One scale for every overlay: the layer is the project's size, scaled to the stage.
    Object.assign(this.layer.style, { width: `${p.width}px`, height: `${p.height}px`, transform: `scale(${s})` });
  }

  private draw(bmp: Decoded) {
    if (this.canvas.width !== bmp.width || this.canvas.height !== bmp.height) {
      this.canvas.width = bmp.width;
      this.canvas.height = bmp.height;
    }
    this.ctx.drawImage(bmp.image, 0, 0);
    bmp.close();
    // Said for what draws over the frame until the engine catches up (`gradepreview.ts`).
    this.stage.dispatchEvent(new Event("frame", { bubbles: true }));
  }

  private onState(playhead: number, playing: boolean, revision: number, sequence: string) {
    const p = store.state.project;
    if (!p) return;
    // Opening another sequence replaces the whole picture, overlays included, though no
    // revision changed: it is nobody's edit.
    const edited = revision !== this.lastRevision || sequence !== this.lastSequence;
    if (edited) {
      this.lastRevision = revision;
      this.lastSequence = sequence;
      this.syncOverlays();
      // Ask on every edit and let the daemon decide. It compiles either way — cheap — and
      // only hands the engine a new timeline when the document actually differs, so the
      // client does not have to model what the compiler depends on. It used to, and got
      // it wrong twice as the compiler grew.
      this.reload();
    }
    if (playing && this.wsOk) {
      // The engine drives the playhead; a jump that did not come from it is a user seek.
      if (Math.abs(playhead - this.engineT) > 1.5 / store.fps) {
        this.engineT = playhead;
        this.lastFrame = -1;
        this.resetSeeks();
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
    // Only the open sequence's own overlays are live DOM here: their clip times are the
    // playhead's times. An overlay inside a nested sequence is skipped by the preview
    // compile too, so it shows up in the render and when you open that sequence, but not
    // while scrubbing the timeline above it.
    for (const t of store.tracks) {
      if (t.hidden) continue;
      for (const c of t.clips) if (isOverlayClip(p, c)) wanted.set(c.id, c);
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
      // Hidden by **visibility**, never `display`: a `display: none` wrapper gives its
      // iframe a 0×0 viewport, so an overlay that measures itself as it loads would read
      // zeros and keep whatever it had, while the bake — full-size — got it right.
      wrap.style.visibility = "hidden";
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
    if (this.fitted !== `${p.width}x${p.height}`) this.layout();
  }

  private updateOverlays(playhead: number) {
    const p = store.state.project;
    if (!p) return;
    for (const v of this.views.values()) {
      const f = store.findClip(v.clipId);
      if (!f) continue;
      const c = f.clip;
      const visible = covers(c, playhead);
      v.wrap.style.visibility = visible ? "" : "hidden";
      if (!visible || !v.ready || (v.lastClip === c && v.lastAt === playhead)) continue;
      v.lastClip = c;
      v.lastAt = playhead;
      // A freeze holds the overlay where the clip is `t` in, as the render's held frame
      // does; the clip's other effects still follow the playhead.
      const freeze = c.effects.find((e) => e.type === "freeze");
      const at = freeze?.type === "freeze" ? Math.min(Math.max(freeze.t ?? 0, 0), c.end - c.start) : playhead - c.start;
      const local = at * Math.abs(c.speed || 1);
      const t = c.speed < 0 ? (c.end - c.start) * Math.abs(c.speed) - local : local;
      if (Math.abs(t - v.lastT) > 1e-4) {
        v.lastT = t;
        seekOverlay(v.iframe, t);
      }
      applyEffects(v.iframe, c, playhead - c.start, p.width, p.height);
    }
  }
}

/** A decoded engine frame: what to draw, its size, and how to let it go. */
interface Decoded { image: CanvasImageSource; width: number; height: number; close(): void }

/**
 * A frame's image — the engine's JPEG, a snapshot's PNG — decoded. `ImageDecoder` reads the bytes where they are, off the
 * main thread; a `Blob` copies them first and `createImageBitmap` decodes later and slower
 * (measured: about 2 ms of main thread a frame against 0.2, at 25 frames a second). The
 * Blob is the fallback where WebCodecs is missing.
 */
async function decodeImage(bytes: Uint8Array<ArrayBuffer>, type: string): Promise<Decoded> {
  if (typeof ImageDecoder !== "undefined") {
    const decoder = new ImageDecoder({ data: bytes, type });
    try {
      const { image } = await decoder.decode();
      return { image, width: image.displayWidth, height: image.displayHeight, close: () => image.close() };
    } finally {
      decoder.close();
    }
  }
  const bmp = await createImageBitmap(new Blob([bytes], { type }));
  return { image: bmp, width: bmp.width, height: bmp.height, close: () => bmp.close() };
}

/** Everything the overlay document is a function of: its injected `window.kuma`. The bin
 *  entry carries the overlay folder and its vars; the clip decides how long it runs. */
function contentKey(p: Project, c: Clip): string {
  const source = binItem(p, c)?.source;
  return `${source ? sourceJSON(source) : ""}|${JSON.stringify([c.speed, c.end - c.start, p.width, p.height, p.fps])}`;
}

/** An entry's source as JSON, once per document: a subtitles entry's source is the whole
 *  transcript, and every clip placing it asks. */
const sourceJSONs = new WeakMap<object, string>();
function sourceJSON(source: object): string {
  let s = sourceJSONs.get(source);
  if (s === undefined) sourceJSONs.set(source, (s = JSON.stringify(source)));
  return s;
}

/** Approximate MLT-side effects with CSS on the overlay iframe (see the header comment). */
function applyEffects(iframe: HTMLIFrameElement, c: Clip, local: number, W: number, H: number) {
  let transform = "";
  let opacity = 1;
  let filter = "";
  let clip = "";
  for (const e of c.effects) {
    // A masked effect acts on part of the picture, which a filter on the whole iframe
    // cannot say: only the render draws it.
    if (maskOf(e)) continue;
    if (e.type === "transform") {
      const t = transformAt(e, local);
      transform += ` translate(${t.x}px, ${t.y}px) scale(${t.scale}) rotate(${t.rotation}deg)`;
      opacity *= t.opacity;
    }
    // CSS takes the standard deviation, which the render makes half the radius (`Blur` in
    // `compile.rs`): `blur(r)` drew every overlay blur twice as strong as the export.
    else if (e.type === "blur") filter += ` blur(${paramAt(e, "radius", local) / 2}px)`;
    else if (e.type === "color") filter += ` brightness(${1 + paramAt(e, "brightness", local)}) contrast(${paramAt(e, "contrast", local)}) saturate(${paramAt(e, "saturation", local)})`;
    // A grade's three whole-picture numbers; its white balance, wheels, curve and LUTs only
    // the render draws. Exposure is stops of light, which brightens the encoded value by
    // 2^(EV / 2.4).
    else if (e.type === "grade") filter += ` brightness(${2 ** (paramAt(e, "exposure", local) / 2.4)}) contrast(${e.contrast ?? 1}) saturate(${e.saturation ?? 1})`;
    else if (e.type === "crop") {
      const at = (p: string) => paramAt(e, p, local);
      clip = `inset(${at("top") * 100}% ${at("right") * 100}% ${at("bottom") * 100}% ${at("left") * 100}% round ${at("radius") * Math.min(W, H) * 0.5}px)`;
    }
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
