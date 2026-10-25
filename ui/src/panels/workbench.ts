// The overlay workbench: one overlay entry, on its own, in its own time.
//
// Kdenlive opens a title in its own editor rather than making you scrub the timeline to
// see it, and an overlay wants the same: the bin entry is *what is shown*, and it is a
// whole thing before any clip says *when*. So nothing here touches the sequence — the
// playhead does not move, no clip is created, and the length below the stage is what this
// panel is looking at, not a document value.
//
// What it is for is iterating. Three loops meet here:
//
//   - **vars**: edited in the inspector, which is where a bin entry is edited. Selecting
//     the entry is how this panel offers them, rather than growing a second copy that
//     would have to be kept in step with the first.
//   - **markup**: `Reload` re-reads the entry's folder and rebuilds the document, so an
//     overlay being written in an editor is one click from being on screen.
//   - **variants**: `Duplicate` adds a second entry on the same folder, so a variant
//     wording costs nothing.
//
// The backdrop and the frame picker are the same idea as a design tool's device frames:
// an overlay is transparent and must survive both orientations (the overlay contract
// requires it), so you need to see it over something, and in a frame the project is not
// necessarily in.

import { api } from "../api";
import { Cmd, command, type Step, stepSeconds } from "../commands";
import { button, el, selectInput } from "../dom";
import { duplicateOverlay, manifestOf, overlayDuration, overlayError, placeBinItem } from "../edits";
import { icon } from "../icons";
import { seekOverlay } from "../overlay";
import { fmtTime, isOverlayEntry, store, toast } from "../store";
import type { Project } from "../types";
import { isOverlaySource } from "../types";

/** What sits behind the overlay. Alpha is the default because the first question about an
 *  overlay is what it does *not* cover. */
const BACKDROPS = [
  { value: "alpha", label: "alpha" },
  { value: "dark", label: "dark" },
  { value: "light", label: "light" },
  { value: "frame", label: "frame at playhead" },
];

/** Frames to check an overlay in. `project` is the real one; the rest are the sizes an
 *  overlay is required to survive, and picking one changes nothing about the project. */
const FRAMES: Record<string, [number, number] | null> = {
  project: null,
  "16:9": [1920, 1080],
  "9:16": [1080, 1920],
  "1:1": [1080, 1080],
};

export class Workbench {
  private head = el("div", "wb-head");
  private title = el("span", "wb-title");
  private sub = el("span", "muted mono tiny");
  private stageHost = el("div", "wb-stage-host");
  private stage = el("div", "wb-stage");
  private backdrop = el("div", "wb-backdrop alpha");
  private shot = el("img", "wb-frame");
  private holder = el("div", "wb-holder");
  private iframe: HTMLIFrameElement | null = null;
  private foot = el("div", "wb-foot");
  private play = button(icon("play"), () => this.togglePlay(), "Play this overlay on its own (Space)", "icon");
  private clock = el("span", "time mono");
  private scrub = el("input");
  private lengthInput = el("input");
  private frameSel = selectInput(Object.keys(FRAMES).map((value) => ({ value })), "project", (v) => { this.frame = v; this.update(); });
  private hint = el("div", "muted wb-hint");
  /** The last check's findings, shown under the stage until the tab changes. */
  private notes: string[] = [];

  /** Where each entry was left, so flipping between tabs keeps your place. */
  private times = new Map<string, number>();
  /** The length each entry is being looked at, seconds. Not a document value: an entry
   *  has no length until a clip gives it one. */
  private lengths = new Map<string, number>();
  private frame = "project";
  private backdropKind = "alpha";
  /** Everything the loaded document is a function of; when it changes, reload it. */
  private key = "";
  private ready = false;
  private playing = false;
  private raf = 0;
  private last = 0;

  constructor(private host: HTMLElement) {
    this.build();
    store.select((s) => [s.project, s.tab, s.manifests, s.entries], () => this.update());
    // The backdrop is a frame of the sequence, so it follows the playhead — but only when
    // it is the one being shown, and never while something is being dragged over it.
    store.select((s) => [s.playhead], () => { if (this.backdropKind === "frame" && this.active()) this.loadShot(); });
    new ResizeObserver(() => this.layout()).observe(this.stageHost);
    command(Cmd.overlayTogglePlay, "Play/Pause Overlay", () => this.togglePlay());
    command(Cmd.overlayStep, "Step Overlay", (by) => this.step(by as Step));
  }

  // ---- chrome ------------------------------------------------------------------

  private build() {
    this.head.append(
      this.title, this.sub, el("span", "spacer"),
      button("Reload", () => void this.rescan(), "Re-read this overlay's folder and rebuild the document — the loop for editing its markup"),
      button("Check", () => void this.check(), "Check this overlay's folder against the authoring contract, without baking it"),
      button("Duplicate", () => void duplicateOverlay(store.state.tab), "A second entry on the same folder, vars and all, opened here"),
      button("Place at playhead", () => void this.place(), "Add a clip of this entry to the open sequence, at the length below", "primary"),
    );

    this.stage.append(this.backdrop, this.shot, this.holder);
    this.stageHost.appendChild(this.stage);
    this.shot.alt = "";
    this.shot.onerror = () => { this.shot.style.display = "none"; };

    this.scrub.type = "range";
    this.scrub.min = "0";
    this.scrub.step = "0.001";
    this.scrub.className = "wb-scrub";
    this.scrub.oninput = () => { this.pause(); this.setTime(Number(this.scrub.value)); };

    this.lengthInput.type = "number";
    this.lengthInput.step = "0.1";
    this.lengthInput.min = "0.1";
    this.lengthInput.title = "How long to render it for. An entry has no length of its own — this is what the workbench is looking at, and what “Place at playhead” uses.";
    this.lengthInput.onchange = () => {
      const v = Number(this.lengthInput.value);
      if (Number.isFinite(v) && v > 0) this.lengths.set(store.state.tab, v);
      this.update();
    };

    const backdrop = selectInput(BACKDROPS, this.backdropKind, (v) => { this.backdropKind = v; this.update(); });
    backdrop.title = "What to show behind it";
    for (const s of [this.frameSel, backdrop]) s.style.width = "auto";
    this.frameSel.title = "Render it into another frame. An overlay has to look right either way round; this is how you check without touching the project.";

    this.foot.append(
      this.play, this.clock, this.scrub,
      el("span", "muted", "length"), this.lengthInput,
      el("span", "muted", "frame"), this.frameSel,
      el("span", "muted", "behind"), backdrop,
    );
    this.host.append(this.head, this.stageHost, this.foot, this.hint);
  }

  // ---- state -------------------------------------------------------------------

  /** True when an overlay entry is the tab on screen — the only time this panel works. */
  private active(): boolean {
    return isOverlayEntry(store.state.project, store.state.tab);
  }

  private length(): number {
    const id = store.state.tab;
    return this.lengths.get(id) ?? overlayDuration(store.state.project?.bin[id]);
  }

  /** Where the entry was left; the first time, the middle of its length — past any
   *  entrance and before the exit, so a title just made from the bin is on screen to be
   *  styled rather than a blank frame at 0 waiting to fade in. */
  private time(): number {
    return Math.min(this.times.get(store.state.tab) ?? this.length() / 2, this.length());
  }

  /** The frame this panel is rendering into: the project's, or the one picked. */
  private size(p: Project): [number, number] {
    return FRAMES[this.frame] ?? [p.width, p.height];
  }

  private update() {
    const s = store.state;
    const p = s.project;
    const id = s.tab;
    const on = this.active();
    // Hidden by the tab strip, not by us: this only stops work that would be invisible.
    if (!on) {
      this.pause();
      this.key = "";
      this.iframe?.remove();
      this.iframe = null;
      return;
    }
    const item = p!.bin[id]!;
    const source = isOverlaySource(item.source) ? item.source : null;
    this.title.textContent = item.name || id;
    this.sub.textContent = source ? source.overlay : "";
    const loads = source && manifestOf(source);
    const lines = loads ? this.notes : [`${source ? overlayError(source) : "Not an overlay"} — fix the folder, then Reload.`];
    this.hint.replaceChildren(...lines.map((l) => el("div", "", l)));

    const [w, h] = this.size(p!);
    const len = this.length();
    this.lengthInput.value = String(Number(len.toFixed(3)));
    this.scrub.max = String(len);
    this.backdrop.className = `wb-backdrop ${this.backdropKind}`;
    this.shot.style.display = this.backdropKind === "frame" ? "" : "none";
    if (this.backdropKind === "frame") this.loadShot();

    // The document carries the vars, the length and the frame, so any of them changing is
    // a new document — the same rule the preview stage applies to its clip overlays.
    const key = JSON.stringify([id, source, len, w, h, p!.fps]);
    if (key !== this.key) {
      this.key = key;
      this.mount(id, len, w, h, p!.revision);
    }
    this.layout();
    this.setTime(this.time());
  }

  /** (Re)create the iframe. A fresh document is the only honest reload: the overlay's
   *  scripts read `window.kuma` once, at load. */
  private mount(id: string, len: number, w: number, h: number, revision: number) {
    this.ready = false;
    // Only reached when the document changed, and the last check was about the old one.
    this.notes = [];
    this.iframe?.remove();
    const f = el("iframe");
    f.width = String(w);
    f.height = String(h);
    f.src = api.overlayEntryUrl(id, len, w, h, revision);
    f.addEventListener("load", () => { this.ready = true; this.setTime(this.time()); });
    this.holder.appendChild(f);
    this.iframe = f;
  }

  private layout() {
    const p = store.state.project;
    if (!p) return;
    const [w, h] = this.size(p);
    const bw = this.stageHost.clientWidth - 24;
    const bh = this.stageHost.clientHeight - 24;
    // Nothing to fit into while the tab is hidden; the observer fires again when it shows.
    if (bw <= 0 || bh <= 0) return;
    const s = Math.min(bw / w, bh / h);
    this.stage.style.width = `${Math.floor(w * s)}px`;
    this.stage.style.height = `${Math.floor(h * s)}px`;
    this.holder.style.width = `${w}px`;
    this.holder.style.height = `${h}px`;
    this.holder.style.transform = `scale(${s})`;
  }

  private shotTimer = 0;

  /** The engine's own picture is not reachable from here; this is the daemon's melt
   *  snapshot, the same one the preview falls back to — and melt renders it, so a scrub
   *  must ask once when the pointer settles rather than once per pointer move. */
  private loadShot() {
    this.shot.style.display = "";
    clearTimeout(this.shotTimer);
    this.shotTimer = window.setTimeout(() => {
      this.shot.src = api.snapshotUrl(store.state.playhead, Math.max(640, this.stage.clientWidth * 2));
    }, 250);
  }

  /**
   * Check the overlay's folder against the contract. What it reports is what would otherwise
   * surface as a bake that times out after 45 seconds, or a frame that is quietly wrong —
   * so it belongs next to Reload, at the point where markup has just been edited.
   */
  private async check() {
    const id = store.state.tab;
    const item = store.state.project?.bin[id];
    const source = item && isOverlaySource(item.source) ? item.source : null;
    if (!source) return;
    this.notes = ["Checking…"];
    this.update();
    try {
      const report = await api.lintOverlay(source.overlay);
      const errors = report.findings.filter((f) => f.level === "error").length;
      this.notes = report.findings.map((f) => `${f.level === "error" ? "✗" : "⚠"} ${f.code}: ${f.message}${f.fix ? ` — ${f.fix}` : ""}`);
      if (report.renderer_skipped) this.notes.push(`(hyperframes' own linter did not run: ${report.renderer_skipped})`);
      toast(errors ? `${source.overlay}: ${errors} contract error(s)` : `${source.overlay}: contract ok`, errors ? "err" : "ok");
    } catch (e) {
      this.notes = [];
      toast((e as Error).message, "err");
    }
    this.update();
  }

  private async rescan() {
    try {
      await store.reloadOverlays();
    } catch (e) {
      toast((e as Error).message, "err");
    }
    // Markup can change with nothing in the document changing, so the key would not:
    // reload unconditionally, which is what the button is for.
    this.key = "";
    this.update();
  }

  private async place() {
    const id = store.state.tab;
    if (!isOverlayEntry(store.state.project, id)) return;
    await placeBinItem(id, store.state.playhead, -1, this.length());
    // Placing does not change what you are working on. `placeBinItem` selects the new
    // clip, which is right from the timeline and wrong from here: the vars have to stay
    // under your hands, or every placement costs you the panel you are iterating in.
    if (isOverlayEntry(store.state.project, id)) store.selectItem("bin", id);
  }

  // ---- local transport ----------------------------------------------------------

  private setTime(t: number) {
    const len = this.length();
    const at = Math.max(0, Math.min(t, len));
    this.times.set(store.state.tab, at);
    this.scrub.value = String(at);
    this.clock.textContent = `${fmtTime(at, store.fps)} / ${fmtTime(len, store.fps)}`;
    if (this.iframe && this.ready) seekOverlay(this.iframe, at);
  }

  step(by: Step) {
    this.pause();
    this.setTime(this.time() + stepSeconds(by, store.fps));
  }

  togglePlay() {
    if (this.playing) this.pause();
    else this.startPlaying();
  }

  /**
   * Play it here, on this panel's own clock. Nothing in MLT is involved: an overlay is a
   * pure function of time and the document is already loaded, so playing it is asking for
   * a frame per animation frame. It loops, because watching an animation once is never
   * how you decide whether it is right.
   */
  private startPlaying() {
    if (!this.iframe) return;
    this.playing = true;
    this.play.replaceChildren(icon("pause"));
    this.last = performance.now();
    const tick = (now: number) => {
      if (!this.playing) return;
      const dt = (now - this.last) / 1000;
      this.last = now;
      const next = this.time() + dt;
      this.setTime(next >= this.length() ? 0 : next);
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  pause() {
    this.playing = false;
    this.play.replaceChildren(icon("play"));
    cancelAnimationFrame(this.raf);
  }
}
