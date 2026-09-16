import { api } from "./api";
import { button, el, options } from "./dom";
import { Inspector } from "./inspector";
import { Library } from "./library";
import { Preview } from "./preview";
import { fmtTime, store, toast } from "./store";
import { initDesktop, pickAndAddMedia } from "./desktop";
import { addTrack, newSequence, Timeline } from "./timeline";
import type { Clip, PatchOp, TrackKind } from "./types";
import { binItem, isTimedMedia, sequenceIds, shiftEffect, trimEffect } from "./types";

async function main() {
  await store.init();
  const preview = new Preview(document.getElementById("preview")!);
  new Timeline(document.getElementById("timeline")!);
  new Inspector(document.getElementById("inspector")!);
  new Library(document.getElementById("library")!);
  const { render } = topbar(preview);
  transport(preview);
  shortcuts(preview);
  // In the desktop shell the application menu drives the same actions as the toolbar;
  // in a browser this is a no-op and nothing above changes.
  initDesktop({
    undo: () => void store.undo(),
    redo: () => void store.redo(),
    render,
    reloadPreview: () => preview.reload(),
    newSequence: () => void promptNewSequence(),
    addMedia: () => void pickAndAddMedia(),
  });
}

function topbar(preview: Preview): { render: () => void } {
  const host = document.getElementById("topbar")!;
  const name = el("span", "name");
  const rev = el("span", "muted mono");
  // Which timeline is being edited. The master is one entry in this list like any other:
  // that is the whole point of a sequence being a bin entry.
  const seqSel = el("select");
  seqSel.style.width = "auto";
  seqSel.title = "Sequence being edited (the preview follows it)";
  seqSel.onchange = () => {
    const picked = seqSel.value;
    // Put the picker back where it was before acting: a cancelled prompt, or a refused
    // open, must not leave it showing the "+" sentinel.
    seqSel.value = store.state.sequence;
    if (picked === "+") promptNewSequence();
    else store.openSequence(picked);
  };
  const presets = el("select");
  presets.style.width = "auto";
  api.presets().then((ps) => options(presets, ps.map((p) => ({ value: p.id, title: p.description }))));
  const doRender = async () => {
    try { const r = await api.render(presets.value); toast(`Render job #${r.job} started`, "ok"); } catch (e) { toast((e as Error).message, "err"); }
  };
  const render = button("Render", () => void doRender());
  render.className = "primary";
  const status = el("span", "status");
  host.append(name, seqSel, rev, button("Undo", () => store.undo()), button("Redo", () => store.redo()), el("span", "spacer"), presets, render, button("Reload preview", () => preview.reload()), status);
  // The picker rebuilds its options, so it gets its own subscription: job progress ticks
  // many times a second and must not tear the dropdown down with it.
  store.select((s) => [s.project, s.sequence], (s) => {
    if (!s.project) return;
    const entry = (id: string) => ({ value: id, label: `${s.project!.bin[id].name || id}${id === s.project!.master ? " (master)" : ""}` });
    options(seqSel, [...sequenceIds(s.project).map(entry), { value: "+", label: "+ new sequence…" }], s.sequence);
  });
  store.select((s) => [s.project, s.sequence, s.connected, s.jobs], (s) => {
    name.textContent = s.project?.name || "kuma";
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    const running = Object.entries(s.jobs).filter(([, j]) => !j.done);
    const last = Object.values(s.jobs).at(-1);
    const output = last?.ok && last.result && typeof last.result === "object" && "output" in last.result ? String((last.result as { output: unknown }).output) : "";
    status.textContent = running.length ? running.map(([id, j]) => `${j.kind} #${id} ${j.percent}% ${j.message}`).join(" · ") : output && `wrote ${output}`;
  });
  return { render: () => void doRender() };
}

function transport(preview: Preview) {
  const host = document.getElementById("transport")!;
  const play = button("▶", () => togglePlay(preview));
  const time = el("span", "time");
  const zoom = el("input");
  zoom.type = "range"; zoom.min = "8"; zoom.max = "600"; zoom.step = "1";
  zoom.oninput = () => store.set({ zoom: Number(zoom.value) });
  const addTrackSel = el("select");
  addTrackSel.style.width = "auto";
  addTrackSel.title = "Add a track";
  options(addTrackSel, ["video", "overlay", "audio"].map((value) => ({ value })), undefined, "+ track…");
  addTrackSel.onchange = async () => {
    const kind = addTrackSel.value as TrackKind | "";
    addTrackSel.value = "";
    if (kind) await addTrack(kind);
  };
  const quality = el("select");
  quality.style.width = "auto";
  quality.title = "Preview resolution (fraction of the project size)";
  options(quality, [{ value: "0.5", label: "preview ½" }, { value: "0.75", label: "preview ¾" }, { value: "1", label: "preview full" }]);
  quality.onchange = () => preview.setScale(Number(quality.value));
  host.append(button("⏮", () => store.seek(0)), play, time, button("Split (S)", () => splitAtPlayhead()), addTrackSel, quality, el("span", "muted", "zoom"), zoom);
  store.subscribe((s) => {
    play.textContent = s.playing ? "⏸" : "▶";
    time.textContent = `${fmtTime(s.playhead, store.fps)} / ${fmtTime(store.duration, store.fps)}`;
    zoom.value = String(s.zoom);
  });
}

function togglePlay(preview: Preview) {
  if (store.state.playing) { preview.pause(); store.set({ playing: false }); }
  else if (preview.play()) store.set({ playing: true });
}

/**
 * Split the selected clip at the playhead. Time-relative effects follow the cut: the
 * left half keeps its fade-in, the right half its fade-out, and keyframes are re-based
 * on the new start. A transition leaving the clip now leaves its right half.
 */
async function promptNewSequence() {
  const name = prompt("Name for the new sequence");
  if (name?.trim()) await newSequence(name.trim());
}

async function splitAtPlayhead() {
  const f = store.findClip(store.state.selection);
  const t = store.state.playhead;
  if (!f || t <= f.clip.start || t >= f.clip.end) { toast("Select a clip under the playhead to split", ""); return; }
  const c = f.clip;
  const cut = t - c.start;
  const left: Clip = { ...c, end: t, effects: c.effects.map((e) => trimEffect(e, cut)) };
  const right: Clip = { ...c, id: store.newId(c.id + "_"), start: t, effects: c.effects.map((e) => shiftEffect(e, cut)) };
  // Both halves keep playing the same bin entry; only the right one moves its in point.
  if (isTimedMedia(binItem(store.state.project!, c))) right.in = (c.in ?? 0) + cut * Math.abs(c.speed);
  const path = store.clipPath(c.id)!;
  const tracks = `${store.seqPath}/tracks/${f.ti}`;
  const ops: PatchOp[] = [{ op: "replace", path, value: left }, { op: "add", path: `${tracks}/clips/-`, value: right }];
  if (f.track.transitions.some((x) => x.from === c.id)) {
    ops.push({ op: "replace", path: `${tracks}/transitions`, value: f.track.transitions.map((x) => (x.from === c.id ? { ...x, from: right.id } : x)) });
  }
  await store.patch(ops, `split ${c.id}`);
}

function shortcuts(preview: Preview) {
  window.addEventListener("keydown", async (e) => {
    if ((e.target as HTMLElement).closest?.("input, textarea, select, button, [contenteditable]")) return;
    const mod = e.ctrlKey || e.metaKey;
    const frame = 1 / store.fps;
    if (e.code === "Space") { e.preventDefault(); togglePlay(preview); }
    else if (e.key === "ArrowLeft") store.seek(store.state.playhead - (e.shiftKey ? 1 : frame));
    else if (e.key === "ArrowRight") store.seek(store.state.playhead + (e.shiftKey ? 1 : frame));
    else if (e.key === "Home") store.seek(0);
    else if (e.key === "End") store.seek(store.duration);
    else if (e.key.toLowerCase() === "s" && !mod && !e.altKey) splitAtPlayhead();
    else if ((e.key === "Delete" || e.key === "Backspace") && store.state.selection) {
      const path = store.clipPath(store.state.selection);
      if (path) await store.patch([{ op: "remove", path }], `delete ${store.state.selection}`);
    } else if (mod && e.key.toLowerCase() === "z") {
      e.preventDefault();
      await (e.shiftKey ? store.redo() : store.undo());
    }
  });
}

main().catch((e) => {
  console.error(e);
  // Startup reads the document's shape, so a crash here is nearly always a `ui/dist`
  // built from older sources than the project it was handed — `ui/dist` is not in git,
  // so a pull leaves it behind. The raw message ("t.tracks is not iterable") names
  // nothing anyone can act on, so say the likely cause next to it.
  toast(`Failed to start: ${e.message}`, "err", 60000);
  toast("This usually means ui/dist was built from older sources. Run `npm run rebuild` in the kuma checkout (or `npm run build` in ui/), then reload.", "err", 60000);
});
