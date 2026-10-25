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

import { Cmd, command, type Step, stepSeconds } from "../commands";
import { button, dragPointer, el, row } from "../dom";
import { placeBinItem } from "../edits";
import { icon } from "../icons";
import { drawWave, hasStrip, stripUrl } from "./clipmedia";
import { fmtTime, isMediaEntry, store, toast } from "../store";
import type { BinItem, FileSource } from "../types";
import { fpsValue, isFileSource, mediaFacts } from "../types";

export class Viewer {
  private title = el("span", "wb-title");
  private sub = el("span", "muted mono tiny");
  private stageHost = el("div", "wb-stage-host vw-stage");
  private foot = el("div", "wb-foot");
  private play = button(icon("play"), () => this.togglePlay(), "Play it on its own (Space)", "icon");
  private clock = el("span", "time mono");
  /** While it is dragged, playback does not move the thumb out from under the pointer. */
  private scrub: HTMLInputElement = Object.assign(el("input", "wb-scrub"), {
    type: "range", min: "0", step: "0.001", oninput: () => this.seek(Number(this.scrub.value)),
    onpointerdown: () => { this.scrubbing = true; }, onpointerup: () => { this.scrubbing = false; },
  });
  private scrubbing = false;
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
  /** What the waveform was last painted for; a resize that changes neither paints nothing. */
  private waveKey = "";
  /** Bumped by every unmount, so a callback from a file this panel has let go of — an
   *  image that fails late, a filmstrip that arrives late — can tell it is stale. */
  private mounted = 0;

  constructor(private host: HTMLElement) {
    this.build();
    store.select((s) => [s.project, s.tab, s.entries], () => this.update());
    new ResizeObserver(() => this.drawWave()).observe(this.waveBox);
    command(Cmd.viewerTogglePlay, "Play/Pause in the Viewer", () => this.togglePlay());
    command(Cmd.viewerStep, "Step in the Viewer", (by) => this.step(by as Step));
    // One thing plays at a time: the sequence starting pauses the file, and the file
    // starting (`onplay`) stops the sequence.
    store.select((s) => [s.playing], (s) => { if (s.playing) this.media?.pause(); });
  }

  private build() {
    const head = row("wb-head", this.title, this.sub, el("span", "spacer"),
      button("Place at playhead", () => void this.place(), "Add a clip of this entry to the open sequence, at the playhead", "primary"));
    this.foot.append(this.play, this.clock, this.scrub);

    this.waveBox.append(this.wave, this.cursor);
    // Pressing on the sound is scrubbing it, as on den's bar.
    this.waveBox.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      const box = this.waveBox.getBoundingClientRect();
      const at = (cx: number) => this.seek(((cx - box.left) / box.width) * this.duration());
      at(e.clientX);
      dragPointer(e, { axes: "x", move: (_dx, _dy, ev) => at(ev.clientX), end: () => {} });
    });
    this.host.append(head, this.stageHost, this.foot, this.hint);
  }

  /** The media entry that is the tab on screen — the only time this panel works. */
  private entry(): { id: string; item: BinItem; source: FileSource } | null {
    const id = store.state.tab;
    const item = store.state.project?.bin[id];
    return item && isFileSource(item.source) ? { id, item, source: item.source } : null;
  }

  private update() {
    const e = this.entry();
    // Hidden by the tab strip, not by us: this only lets go of a file nobody can see.
    if (!e) {
      if (this.key) this.unmount();
      this.key = "";
      return;
    }
    const { id, item, source } = e;
    this.title.textContent = item.name || id;
    this.sub.textContent = [source.proxy ? `${source.path} (playing its proxy)` : source.path, mediaFacts(item.meta)].filter(Boolean).join(" · ");
    const key = JSON.stringify([id, source.path, source.proxy ?? null, source.kind]);
    if (key !== this.key) {
      this.key = key;
      this.mount(id, source);
    }
  }

  private mount(id: string, source: FileSource) {
    this.unmount();
    const gen = this.mounted;
    this.hint.replaceChildren();
    // The file the entry names, in its URL, so pointing the entry elsewhere is a new URL.
    const url = `/api/bin/${encodeURIComponent(id)}/source?v=${encodeURIComponent(source.proxy ?? source.path)}`;
    if (source.kind === "image") {
      // `onerror` before `src`: `Object.assign` sets them in order.
      this.stageHost.append(Object.assign(el("img", "vw-picture image"), { alt: "", onerror: () => { if (gen === this.mounted) this.cannot(`show ${source.path}`, url); }, src: url }));
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
    m.onplay = () => {
      if (store.state.playing) store.set({ playing: false });
      this.play.replaceChildren(icon("pause"));
      this.tick();
    };
    m.onpause = () => { this.play.replaceChildren(icon("play")); cancelAnimationFrame(this.raf); this.sync(); };
    m.onerror = () => this.failed(id, source, url);
    m.src = url;
    if (source.kind === "audio") this.stageHost.append(this.waveBox);
    this.stageHost.append(m);
    this.media = m;
    this.foot.hidden = false;
    this.scrub.max = String(this.duration());
    this.sync();
  }

  /** Let go of the file: a hidden `<video>` would keep its decoder, and its sound. Its
   *  handlers go first — emptying it fires events, and a late one would sync a clock that
   *  is no longer its own. */
  private unmount() {
    cancelAnimationFrame(this.raf);
    const m = this.media;
    if (m) {
      m.onloadedmetadata = m.onplay = m.onpause = m.onerror = null;
      m.pause();
      m.removeAttribute("src");
      m.load();
    }
    this.media = null;
    this.mounted++;
    this.waveKey = "";
    this.stageHost.replaceChildren();
    this.play.replaceChildren(icon("play"));
  }

  /** What this window could not do with the file, and that nothing else is affected.
   *  Chromium names every answer it cannot use a format error, a refusal included, so the
   *  daemon is asked what it answered: a missing file, or a daemon older than this editor
   *  (it reads `ui/dist` from disk, so a rebuild reaches a running one half-way), is not
   *  the codec's fault. */
  private cannot(what: string, url: string) {
    const gen = this.mounted;
    this.hint.replaceChildren(el("div", "", `This window cannot ${what}. The timeline, the preview and renders read it through MLT and are not affected.`));
    void fetch(url, { headers: { Range: "bytes=0-0" } }).then(async (r) => {
      if (r.ok) return;
      const body = (await r.json().catch(() => ({}))) as { error?: string };
      if (gen !== this.mounted) return;
      // A daemon from before the viewer routes `source` to `/api/bin/{id}/{file}`, whose
      // answer is a bare "not found"; this route's own refusals name the file.
      const why = body.error && body.error !== "not found" ? body.error : `the daemon answered ${r.status}, so it is older than this editor and has no viewer route — restart it`;
      this.hint.replaceChildren(el("div", "", `This window cannot ${what}: ${why}.`));
    }, () => {});
  }

  /** Chromium could not decode it: say so, and show what the timeline draws of it instead —
   *  a sound's waveform, a video's filmstrip; the daemon makes both, whatever the codec. */
  private failed(id: string, source: FileSource, url: string) {
    const why = this.media?.error?.message || "it cannot decode this file";
    this.unmount();
    const gen = this.mounted;
    this.foot.hidden = true;
    this.cannot(`play ${source.proxy ?? source.path} (${why})`, url);
    if (source.kind === "audio") {
      this.stageHost.append(this.waveBox);
      this.drawWave();
      return;
    }
    void hasStrip(id, source.path).then((made) => {
      if (!made || gen !== this.mounted) return;
      this.stageHost.append(Object.assign(el("img", "vw-picture"), { alt: "", title: "Frames across the file, as the timeline draws them", src: stripUrl(id, source.path) }));
    });
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
    if (!this.scrubbing) this.scrub.value = String(t);
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
    const key = `${e.id}\n${e.source.path}\n${len}\n${w}x${h}`;
    if (w <= 0 || h <= 0 || key === this.waveKey) return;
    this.waveKey = key;
    // Read before the size is written, so reading it forces no second style pass.
    const color = getComputedStyle(document.documentElement).getPropertyValue("--audio").trim() || "#4a9";
    this.wave.style.width = `${w}px`;
    this.wave.style.height = `${h}px`;
    // The whole file across the box: a placement from its start, at the zoom that fits it.
    drawWave(this.wave, e.id, e.source.path, { in: 0, speed: 1 }, w / len, w, h, color);
  }

  togglePlay() {
    if (!this.media) return;
    if (this.media.paused) this.media.play().catch((err: Error) => toast(err.message, "err"));
    else this.media.pause();
  }

  step(by: Step) {
    if (!this.media) return;
    this.media.pause();
    this.seek(this.media.currentTime + stepSeconds(by, this.fps()));
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
