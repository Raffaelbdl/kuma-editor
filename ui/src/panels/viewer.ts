// The viewer: one media entry — a picture, a video, a sound — on its own, before any clip
// says when it plays. Kdenlive's clip monitor.
//
// Without it a file in the bin could only be seen by placing it and scrubbing the timeline
// to where the clip landed: looking at a take cost an edit, the same trap the overlay
// workbench was made to get out of. So it is the workbench's sibling and keeps its rules:
// nothing here touches the sequence, the time below the picture is where this panel is
// looking and not a document value, and `Place at playhead` is the one thing that writes.
//
// It is the browser playing the file (`/api/bin/{id}/source`), not MLT. Looking at a source
// needs no compile and no engine, so it works where the engine does not build — the same
// trade the workbench's iframe makes for overlays, and like it an optimisation of one kind
// of entry rather than a second rendering path: nothing the viewer shows is composited.
// What it cannot do is decode a file Chromium cannot (ProRes, say). It says so then, and
// shows the filmstrip the timeline draws; the timeline, the preview and renders read the
// file through MLT and are not affected.

import { api } from "../api";
import { Cmd, command } from "../commands";
import { button, el } from "../dom";
import { placeBinItem } from "../edits";
import { icon } from "../icons";
import { drawWave } from "./clipmedia";
import { fmtTime, isMediaEntry, store, toast } from "../store";
import type { BinItem, Clip } from "../types";
import { fpsValue, isFileSource } from "../types";

type FileSource = { path: string; kind: "video" | "audio" | "image"; proxy?: string | null };

export class Viewer {
  private title = el("span", "wb-title");
  private sub = el("span", "muted mono tiny");
  private stageHost = el("div", "wb-stage-host vw-stage");
  private foot = el("div", "wb-foot");
  private play = button(icon("play"), () => this.togglePlay(), "Play it on its own (Space)", "icon");
  private clock = el("span", "time mono");
  private scrub = el("input");
  private hint = el("div", "muted wb-hint");
  /** The sound, drawn: the waveform the timeline draws, the whole file across. */
  private waveBox = el("div", "vw-wave");
  private wave = el("canvas");
  private cursor = el("div", "vw-cursor");

  private media: HTMLMediaElement | null = null;
  /** Where each entry was left, so flipping between tabs keeps your place. */
  private times = new Map<string, number>();
  /** Everything the loaded file is a function of; when it changes, load it again. */
  private key = "";
  private raf = 0;

  constructor(private host: HTMLElement) {
    this.build();
    store.select((s) => [s.project, s.tab, s.entries], () => this.update());
    new ResizeObserver(() => this.drawWave()).observe(this.stageHost);
    command(Cmd.viewerTogglePlay, "Play/Pause in the Viewer", () => this.togglePlay());
    command(Cmd.viewerStep, "Step in the Viewer", (frames) => this.step(Number(frames)));
  }

  private build() {
    const head = el("div", "wb-head");
    head.append(
      this.title, this.sub, el("span", "spacer"),
      button("Place at playhead", () => void this.place(), "Add a clip of this entry to the open sequence, at the playhead", "primary"),
    );
    this.scrub.type = "range";
    this.scrub.min = "0";
    this.scrub.step = "0.001";
    this.scrub.className = "wb-scrub";
    this.scrub.oninput = () => this.seek(Number(this.scrub.value));
    this.foot.append(this.play, this.clock, this.scrub);

    this.waveBox.append(this.wave, this.cursor);
    // Pressing on the sound is scrubbing it, as on the timeline's ruler.
    this.waveBox.onpointerdown = (e) => {
      const at = (ev: PointerEvent) => {
        const box = this.waveBox.getBoundingClientRect();
        this.seek(((ev.clientX - box.left) / box.width) * this.duration());
      };
      at(e);
      this.waveBox.setPointerCapture(e.pointerId);
      this.waveBox.onpointermove = at;
      this.waveBox.onpointerup = () => { this.waveBox.onpointermove = null; };
    };
    this.host.append(head, this.stageHost, this.foot, this.hint);
  }

  /** True when a media entry is the tab on screen — the only time this panel works. */
  private active(): boolean {
    return isMediaEntry(store.state.project, store.state.tab);
  }

  private entry(): { id: string; item: BinItem; source: FileSource } | null {
    const id = store.state.tab;
    const item = store.state.project?.bin[id];
    return item && isFileSource(item.source) ? { id, item, source: item.source } : null;
  }

  private update() {
    const e = this.active() ? this.entry() : null;
    // Hidden by the tab strip, not by us: this only lets go of a file nobody can see.
    if (!e) {
      this.unmount();
      this.key = "";
      return;
    }
    const { id, item, source } = e;
    this.title.textContent = item.name || id;
    const m = item.meta;
    const rate = m?.fps ? `${+fpsValue(m.fps).toFixed(3)} fps` : "";
    const facts = [m?.width ? `${m.width}×${m.height}` : "", rate, m?.duration ? `${m.duration.toFixed(2)}s` : ""].filter(Boolean);
    this.sub.textContent = [source.proxy ? `${source.path} (playing its proxy)` : source.path, ...facts].join(" · ");
    const key = JSON.stringify([id, source.path, source.proxy ?? null, source.kind]);
    if (key !== this.key) {
      this.key = key;
      this.mount(id, source);
    }
  }

  private mount(id: string, source: FileSource) {
    this.unmount();
    this.hint.replaceChildren();
    // The file the entry names, in its URL, so pointing the entry elsewhere is a new URL.
    const url = `/api/bin/${encodeURIComponent(id)}/source?v=${encodeURIComponent(source.proxy ?? source.path)}`;
    if (source.kind === "image") {
      const img = el("img", "vw-picture image");
      img.alt = "";
      img.onerror = () => this.say(`This window cannot show ${source.path}. The timeline, the preview and renders read it through MLT and are not affected.`);
      img.src = url;
      this.stageHost.append(img);
      this.foot.hidden = true;
      return;
    }
    const m = el(source.kind === "video" ? "video" : "audio", "vw-picture");
    m.preload = "auto";
    m.onloadedmetadata = () => {
      m.currentTime = Math.min(this.times.get(id) ?? 0, this.duration());
      this.scrub.max = String(this.duration());
      this.sync();
      this.drawWave();
    };
    m.ontimeupdate = () => this.sync();
    m.onplay = () => { this.play.replaceChildren(icon("pause")); this.tick(); };
    m.onpause = () => { this.play.replaceChildren(icon("play")); cancelAnimationFrame(this.raf); this.sync(); };
    m.onerror = () => this.failed(id, source);
    m.src = url;
    if (source.kind === "audio") this.stageHost.append(this.waveBox);
    this.stageHost.append(m);
    this.media = m;
    this.foot.hidden = false;
    this.scrub.max = String(this.duration());
    this.sync();
  }

  /** Let go of the file: a hidden `<video>` would keep its decoder, and its sound. Its
   *  handlers go first — emptying it fires events, and a late `timeupdate` at 0 would
   *  overwrite where it was left. */
  private unmount() {
    cancelAnimationFrame(this.raf);
    if (this.media) {
      const m = this.media;
      m.onloadedmetadata = m.ontimeupdate = m.onplay = m.onpause = m.onerror = null;
      this.media.pause();
      this.media.removeAttribute("src");
      this.media.load();
    }
    this.media = null;
    this.stageHost.replaceChildren();
    this.play.replaceChildren(icon("play"));
  }

  private say(text: string) {
    this.hint.replaceChildren(el("div", "", text));
  }

  /** Chromium could not decode it: say so, and show what the timeline draws of it instead. */
  private failed(id: string, source: FileSource) {
    const why = this.media?.error?.message || "it cannot decode this file";
    this.unmount();
    this.foot.hidden = true;
    this.say(`This window cannot play ${source.proxy ?? source.path} (${why}). The timeline, the preview and renders read it through MLT and are not affected.`);
    if (source.kind !== "video") return;
    void api.strip(id).then(() => {
      if (!this.active() || store.state.tab !== id) return;
      const strip = el("img", "vw-picture");
      strip.alt = "";
      strip.title = "Frames across the file, as the timeline draws them";
      strip.src = `/api/bin/${encodeURIComponent(id)}/strip.jpg?v=${encodeURIComponent(source.path)}`;
      this.stageHost.append(strip);
    }).catch(() => {});
  }

  private duration(): number {
    const d = this.media?.duration;
    if (d !== undefined && Number.isFinite(d)) return d;
    return this.entry()?.item.meta?.duration ?? 0;
  }

  private fps(): number {
    const f = this.entry()?.item.meta?.fps;
    return f ? fpsValue(f) : store.fps;
  }

  /** The clock, the scrub and the cursor on the sound, from where the file is. */
  private sync() {
    const id = store.state.tab;
    const t = this.media?.currentTime ?? 0;
    const len = this.duration();
    // Only a file that has loaded knows where it is; before that its 0 is not a place.
    if (this.media && this.media.readyState >= 1) this.times.set(id, t);
    this.scrub.value = String(t);
    this.clock.textContent = `${fmtTime(t, this.fps())} / ${fmtTime(len, this.fps())}`;
    this.cursor.style.left = `${len ? (100 * t) / len : 0}%`;
  }

  /** While playing, follow every frame rather than `timeupdate`'s few a second. */
  private tick() {
    cancelAnimationFrame(this.raf);
    const step = () => {
      if (!this.media || this.media.paused) return;
      this.sync();
      this.raf = requestAnimationFrame(step);
    };
    this.raf = requestAnimationFrame(step);
  }

  private seek(t: number) {
    if (!this.media) return;
    this.media.currentTime = Math.max(0, Math.min(t, this.duration()));
    this.sync();
  }

  private drawWave() {
    const e = this.entry();
    const len = this.duration();
    if (!e || e.source.kind !== "audio" || !this.waveBox.isConnected || !len) return;
    const w = this.waveBox.clientWidth;
    const h = this.waveBox.clientHeight;
    if (w <= 0 || h <= 0) return;
    this.wave.style.width = `${w}px`;
    this.wave.style.height = `${h}px`;
    // The whole file across the box: a placement from its start at the speed that fits.
    const whole = { in: 0, speed: 1 } as Clip;
    const color = getComputedStyle(document.documentElement).getPropertyValue("--audio").trim() || "#4a9";
    drawWave(this.wave, e.id, e.source.path, whole, w / len, w, h, color);
  }

  togglePlay() {
    if (!this.media) return;
    if (this.media.paused) this.media.play().catch((err: Error) => toast(err.message, "err"));
    else this.media.pause();
  }

  step(frames: number) {
    if (!this.media) return;
    this.media.pause();
    this.seek(this.media.currentTime + frames / this.fps());
  }

  private async place() {
    const e = this.entry();
    if (!e) return;
    await placeBinItem(e.id, store.state.playhead);
    // Placing does not change what you are looking at; `placeBinItem` selects the new
    // clip, and the entry stays the one in the inspector, as on the workbench.
    if (isMediaEntry(store.state.project, e.id)) store.selectItem("bin", e.id);
  }
}
