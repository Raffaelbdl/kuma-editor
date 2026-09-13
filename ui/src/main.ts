import { api } from "./api";
import { Inspector } from "./inspector";
import { Library } from "./library";
import { Preview } from "./preview";
import { fmtTime, store, toast } from "./store";
import { addTrack, Timeline } from "./timeline";
import type { PatchOp, Preset } from "./types";
import { isAsset } from "./types";

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
  const el = document.getElementById("topbar")!;
  const name = document.createElement("span");
  name.className = "name";
  const rev = document.createElement("span");
  rev.className = "muted mono";
  const undo = button("Undo", async () => { await api.undo(); await store.refresh(); });
  const redo = button("Redo", async () => { await api.redo(); await store.refresh(); });
  const presets = document.createElement("select");
  presets.style.width = "auto";
  api.presets().then((ps: Preset[]) => { for (const p of ps) { const o = document.createElement("option"); o.value = p.id; o.textContent = p.id; o.title = p.description; presets.appendChild(o); } });
  const render = button("Render", async () => {
    try { const r = await api.render(presets.value); toast(`Render job #${r.job} started`, "ok"); } catch (e) { toast((e as Error).message, "err"); }
  });
  render.className = "primary";
  const reload = button("Reload preview", () => preview.reload());
  const spacer = document.createElement("span");
  spacer.className = "spacer";
  const status = document.createElement("span");
  status.className = "status";
  el.append(name, rev, undo, redo, spacer, presets, render, reload, status);
  store.subscribe((s) => {
    name.textContent = s.project?.name || "kuma";
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    const running = Object.entries(s.jobs).filter(([, j]) => !j.done);
    status.innerHTML = running.length
      ? running.map(([id, j]) => `${j.kind} #${id} ${j.percent}% ${j.message}`).join(" · ")
      : Object.values(s.jobs).slice(-1).map((j) => (j.ok && j.result && typeof j.result === "object" && "output" in (j.result as object)) ? `wrote ${(j.result as { output: string }).output}` : "").join("");
  });
}

function transport(preview: Preview) {
  const el = document.getElementById("transport")!;
  const play = button("▶", () => togglePlay(preview));
  const toStart = button("⏮", () => store.seek(0));
  const time = document.createElement("span");
  time.className = "time";
  const zoomLabel = document.createElement("span");
  zoomLabel.className = "muted";
  zoomLabel.textContent = "zoom";
  const zoom = document.createElement("input");
  zoom.type = "range"; zoom.min = "8"; zoom.max = "600"; zoom.step = "1";
  zoom.oninput = () => store.set({ zoom: Number(zoom.value) });
  const split = button("Split (S)", () => splitAtPlayhead());
  const addTrackSel = document.createElement("select");
  addTrackSel.style.width = "auto";
  addTrackSel.title = "Add a track";
  addTrackSel.innerHTML = `<option value="">+ track…</option><option value="video">video</option><option value="overlay">overlay</option><option value="audio">audio</option>`;
  addTrackSel.onchange = async () => {
    const kind = addTrackSel.value as "video" | "overlay" | "audio" | "";
    addTrackSel.value = "";
    if (kind) await addTrack(kind);
  };
  const quality = document.createElement("select");
  quality.style.width = "auto";
  quality.title = "Preview resolution (fraction of the project size)";
  quality.innerHTML = `<option value="0.5">preview ½</option><option value="0.75">preview ¾</option><option value="1">preview full</option>`;
  quality.onchange = () => preview.setScale(Number(quality.value));
  el.append(toStart, play, time, split, addTrackSel, quality, zoomLabel, zoom);
  store.subscribe((s) => {
    play.textContent = s.playing ? "⏸" : "▶";
    time.textContent = `${fmtTime(s.playhead, store.fps)} / ${fmtTime(store.duration, store.fps)}`;
    zoom.value = String(s.zoom);
  });
}

function togglePlay(preview: Preview) {
  if (store.state.playing) { preview.pause(); store.set({ playing: false }); }
  else { preview.play(); store.set({ playing: true }); }
}

async function splitAtPlayhead() {
  const f = store.findClip(store.state.selection);
  const t = store.state.playhead;
  if (!f || t <= f.clip.start || t >= f.clip.end) { toast("Select a clip under the playhead to split", ""); return; }
  const c = f.clip;
  const left = { ...c, end: t };
  const right = { ...c, id: store.newId(c.id + "_"), start: t };
  if (isAsset(right)) right.src = { ...right.src, in: ((c.src as { in?: number }).in ?? 0) + (t - c.start) * Math.abs(c.speed) };
  const path = `/tracks/${f.ti}/clips/${f.ci}`;
  const ops: PatchOp[] = [{ op: "replace", path, value: left }, { op: "add", path: `/tracks/${f.ti}/clips/-`, value: right }];
  await store.patch(ops, `split ${c.id}`);
}

function shortcuts(preview: Preview) {
  window.addEventListener("keydown", async (e) => {
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    const frame = 1 / store.fps;
    if (e.code === "Space") { e.preventDefault(); togglePlay(preview); }
    else if (e.key === "ArrowLeft") store.seek(store.state.playhead - (e.shiftKey ? 1 : frame));
    else if (e.key === "ArrowRight") store.seek(store.state.playhead + (e.shiftKey ? 1 : frame));
    else if (e.key === "Home") store.seek(0);
    else if (e.key === "End") store.seek(store.duration);
    else if (e.key === "s" || e.key === "S") splitAtPlayhead();
    else if ((e.key === "Delete" || e.key === "Backspace") && store.state.selection) {
      const path = store.clipPath(store.state.selection);
      if (path) await store.patch([{ op: "remove", path }], `delete ${store.state.selection}`);
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
      e.preventDefault();
      await (e.shiftKey ? api.redo() : api.undo());
      await store.refresh();
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
      e.preventDefault();
      await api.redo();
      await store.refresh();
    }
  });
}

function button(label: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  b.onclick = onClick;
  return b;
}

main().catch((e) => { console.error(e); toast(`Failed to start: ${e.message}`, "err", 10000); });
