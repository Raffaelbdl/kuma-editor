import { api } from "./api";
import { button, el, options } from "./dom";
import { Inspector } from "./inspector";
import { Library } from "./library";
import { Preview } from "./preview";
import { fmtTime, store, toast } from "./store";
import { addTrack, Timeline } from "./timeline";
import type { Clip, PatchOp, TrackKind } from "./types";
import { binItem, isTimedMedia, shiftEffect, trimEffect } from "./types";

async function main() {
  await store.init();
  const preview = new Preview(document.getElementById("preview")!);
  new Timeline(document.getElementById("timeline")!);
  new Inspector(document.getElementById("inspector")!);
  new Library(document.getElementById("library")!);
  topbar(preview);
  transport(preview);
  shortcuts(preview);
}

function topbar(preview: Preview) {
  const host = document.getElementById("topbar")!;
  const name = el("span", "name");
  const rev = el("span", "muted mono");
  const presets = el("select");
  presets.style.width = "auto";
  api.presets().then((ps) => options(presets, ps.map((p) => ({ value: p.id, title: p.description }))));
  const render = button("Render", async () => {
    try { const r = await api.render(presets.value); toast(`Render job #${r.job} started`, "ok"); } catch (e) { toast((e as Error).message, "err"); }
  });
  render.className = "primary";
  const status = el("span", "status");
  host.append(name, rev, button("Undo", () => store.undo()), button("Redo", () => store.redo()), el("span", "spacer"), presets, render, button("Reload preview", () => preview.reload()), status);
  store.select((s) => [s.project, s.connected, s.jobs], (s) => {
    name.textContent = s.project?.name || "kuma";
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    const running = Object.entries(s.jobs).filter(([, j]) => !j.done);
    const last = Object.values(s.jobs).at(-1);
    const output = last?.ok && last.result && typeof last.result === "object" && "output" in last.result ? String((last.result as { output: unknown }).output) : "";
    status.textContent = running.length ? running.map(([id, j]) => `${j.kind} #${id} ${j.percent}% ${j.message}`).join(" · ") : output && `wrote ${output}`;
  });
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
  const path = `/tracks/${f.ti}/clips/${f.ci}`;
  const ops: PatchOp[] = [{ op: "replace", path, value: left }, { op: "add", path: `/tracks/${f.ti}/clips/-`, value: right }];
  if (f.track.transitions.some((x) => x.from === c.id)) {
    ops.push({ op: "replace", path: `/tracks/${f.ti}/transitions`, value: f.track.transitions.map((x) => (x.from === c.id ? { ...x, from: right.id } : x)) });
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

main().catch((e) => { console.error(e); toast(`Failed to start: ${e.message}`, "err", 10000); });
