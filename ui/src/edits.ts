// Project edits that are no one panel's business: placing, splitting, adding a track,
// minting a sequence. Each is asked for from several places, so each lives here as one
// revision-checked patch with its rule checked once — and no panel has to import another
// to place a clip.

import { api } from "./api";
import { checkbox, el, selectInput } from "./dom";
import { askForm, askText } from "./modal";
import { startJob, store, toast } from "./store";
import type { BinItem, BinSource, Clip, PatchOp, Project, Track, TrackKind } from "./types";
import { binItem, binPath, childrenOf, hasAudio, isFileSource, isOverlaySource, isTimedMedia, itemKind, orientation, parentOf, seqDuration, shiftEffect, trackFor, trimEffect, wouldNest, wouldRecurse } from "./types";

/** Place an existing bin entry at time `t` on track `ti`. Every way of placing something —
 *  a button, a drop — comes through here, so the one rule a placement can break is checked
 *  here rather than next to each gesture. */
export async function placeBinItem(binId: string, t: number, ti = -1, duration?: number) {
  const p = store.state.project!;
  const item = p.bin[binId];
  if (!item) return;
  if (wouldRecurse(p, binId, store.state.sequence)) {
    toast(`${item.name || binId} contains this sequence; placing it here would make a loop`, "err");
    return;
  }
  const kind = trackFor(item);
  if (!kind) {
    toast(`${item.name || binId} cannot be placed on a timeline`, "err");
    return;
  }
  // The workbench passes the length it was iterating at; everything else takes the
  // entry's own default.
  const d = duration && duration > 0 ? duration : defaultDuration(item, p, binId);
  const clip: Clip = { id: store.newId(kind === "overlay" ? "o" : "c"), start: t, end: t + d, bin: binId, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" };
  await place(clip, kind, ti, []);
}

/** Add a library template to the bin and place it, in one patch. */
export async function addFromLibrary(templateId: string, t: number, ti = -1) {
  const p = store.state.project!;
  const tpl = store.state.templates.find((x) => x.id === templateId);
  const binId = uniqueId(templateId, Object.keys(p.bin));
  const item: BinItem = { name: tpl?.manifest.name || templateId, source: { template: templateId, vars: {} } };
  const d = tpl?.manifest.duration_default ?? 4;
  const clip: Clip = { id: store.newId("o"), start: t, end: t + d, bin: binId, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" };
  await place(clip, "overlay", ti, [{ op: "add", path: binPath(binId), value: item }]);
}

function defaultDuration(item: BinItem, p: Project, binId: string): number {
  if (isFileSource(item.source)) return item.meta?.duration ?? 5;
  // A sequence goes down at its own length, the way dragging in a clip does.
  if (itemKind(item) === "sequence") return seqDuration(p, binId) || 4;
  // An overlay goes down at the length its template was authored for — the same number
  // `addFromLibrary` uses, so placing an entry and dragging in its template agree.
  if (isOverlaySource(item.source)) return overlayDuration(item);
  return 4;
}

/** The loaded template an overlay entry points at, if its library is on the search path. */
export const templateOf = (id: string) => store.state.templates.find((t) => t.id === id);

/** How long an overlay runs when nothing says otherwise: the length its template was
 *  authored for. The clip that places it and the workbench that previews it must agree,
 *  so they ask here. */
export const overlayDuration = (item: BinItem | undefined): number =>
  (item && isOverlaySource(item.source) && templateOf(item.source.template)?.manifest.duration_default) || 4;

/** `base`, or `base-2`, `base-3`… if taken. Bin ids allow letters, digits, `_` and `-`. */
function uniqueId(base: string, taken: Iterable<string>): string {
  const set = new Set(taken);
  if (!set.has(base)) return base;
  let n = 2;
  while (set.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/** Append `clip` to the first track of `kind` in the open sequence (created if none
 *  exists), after `pre` ops. If the slot is taken the clip goes after the track's last
 *  clip instead. */
async function place(clip: Clip, kind: TrackKind, ti: number, pre: PatchOp[]) {
  const tracks = store.tracks;
  if (ti < 0 || tracks[ti]?.kind !== kind) ti = tracks.findIndex((tr) => tr.kind === kind);
  const ops: PatchOp[] = [...pre];
  if (ti < 0) {
    // No track of that kind yet (e.g. a project created by hand): add one, then the clip.
    const { op, index } = newTrackOp(kind);
    ti = index;
    ops.push(op);
    toast(`Added a ${kind} track`, "ok");
  } else {
    const track = tracks[ti];
    if (track.clips.some((c) => clip.start < c.end && clip.end > c.start)) {
      const endT = Math.max(0, ...track.clips.map((c) => c.end));
      clip.end = endT + (clip.end - clip.start);
      clip.start = endT;
    }
  }
  ops.push({ op: "add", path: `${store.seqPath}/tracks/${ti}/clips/-`, value: clip });
  const ok = await store.patch(ops, `add ${clip.id}`);
  if (ok) store.selectItem("clip", clip.id);
}

/** Build the patch op that inserts a new empty track of `kind`. Tracks composite in array
 *  order, so a video track goes below the first overlay track and everything else on top
 *  (audio order does not matter). */
function newTrack(kind: TrackKind, name?: string): Track {
  const id = store.newTrackId(kind[0]);
  return { id, kind, name: name ?? `${kind[0].toUpperCase()}${kind.slice(1)} ${id.slice(1)}`, muted: false, hidden: false, clips: [], transitions: [] };
}

function newTrackOp(kind: TrackKind): { op: PatchOp; index: number } {
  const tracks = store.tracks;
  const track = newTrack(kind);
  const firstOverlay = tracks.findIndex((t) => t.kind === "overlay");
  const index = kind === "video" && firstOverlay >= 0 ? firstOverlay : tracks.length;
  return { op: { op: "add", path: `${store.seqPath}/tracks/${index}`, value: track }, index };
}

/** Mint a bin entry named by the user: a slug of the name (or `fallback`) made unique,
 *  one patch. Returns the id, or null when the patch was refused. */
async function addBinItem(name: string, fallback: string, source: BinSource): Promise<string | null> {
  const id = uniqueId(slug(name) || fallback, Object.keys(store.state.project!.bin));
  const value: BinItem = { name, source };
  return (await store.patch([{ op: "add", path: binPath(id), value }], `add ${fallback} ${id}`)) ? id : null;
}

/** Bin ids must match [A-Za-z_][A-Za-z0-9_-]*, so a leading digit takes an `s`. */
const slug = (name: string) => name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").replace(/^(?=[0-9])/, "s");

/** Ask for a name, then mint an empty sequence and open it. It starts with the same
 *  three tracks a new project does, because an empty one has nowhere to drop a clip.
 *  `window.prompt` is not an option: Electron does not implement it (see `modal.ts`). */
export async function promptNewSequence() {
  const name = await askText({ title: "New sequence", label: "Name", placeholder: "Intro" });
  if (!name) return;
  const id = await addBinItem(name, "sequence", { tracks: [newTrack("video", "Video"), newTrack("overlay", "Overlays"), newTrack("audio", "Audio")], markers: [] });
  if (id) await store.openSequence(id);
}

/** A folder is a bin entry with nothing but a name and children. */
export async function promptNewFolder() {
  const name = await askText({ title: "New folder", label: "Name", placeholder: "Speech" });
  if (name) await addBinItem(name, "folder", { folder: true });
}

/**
 * Mint an overlay entry from a template and open it in the workbench — **without placing
 * it**. Creating an overlay and deciding where it plays are two different decisions, and
 * the second one should not be forced on you while you are still making the first.
 */
export async function promptNewOverlay(template?: string) {
  const templates = store.state.templates;
  if (!templates.length) {
    toast("No templates found. Run `kuma library`, or point KUMA_LIBRARY at a folder of them.", "err", 8000);
    return;
  }
  const pick = selectInput(templates.map((t) => ({ value: t.id, label: t.manifest.name || t.id, title: t.manifest.description })), template ?? templates[0].id, () => {
    // The name follows the template until it has been typed in, so the common case is
    // one click and the rarer one is still a free choice.
    if (!typed) name.value = templateOf(pick.value)?.manifest.name ?? pick.value;
  });
  const name = el("input");
  name.type = "text";
  name.value = templateOf(pick.value)?.manifest.name ?? pick.value;
  let typed = false;
  name.oninput = () => { typed = true; };
  const ok = await askForm({
    title: "New overlay",
    note: "Adds a configured overlay to the bin and opens it in the workbench. Nothing is placed on a timeline until you place it.",
    rows: [{ label: "Template", input: pick }, { label: "Name", input: name }],
    ok: "Create",
  });
  if (!ok) return;
  const label = name.value.trim() || templateOf(pick.value)?.manifest.name || pick.value;
  const id = await addBinItem(label, pick.value, { template: pick.value, vars: {} });
  if (id) store.openOverlay(id);
}

/** Fork an overlay entry, vars and all, and open the copy. Iterating means keeping the
 *  version that works while trying another; a copy is the only way to do that, because
 *  editing an entry changes every clip that plays it. */
export async function duplicateOverlay(id: string) {
  const p = store.state.project!;
  const item = p.bin[id];
  if (!item || !isOverlaySource(item.source)) return;
  const name = `${item.name || id} copy`;
  const copyId = uniqueId(slug(name) || "overlay", Object.keys(p.bin));
  const value: BinItem = { name, source: { template: item.source.template, vars: { ...(item.source.vars ?? {}) } } };
  // Listed where the original is, so a variant does not turn up at the other end of the bin.
  if (item.parent) value.parent = item.parent;
  if (await store.patch([{ op: "add", path: binPath(copyId), value }], `duplicate ${id}`)) store.openOverlay(copyId);
}

/** List `id` under `parent` (null for the top level), just before sibling `before`, or
 *  last when that is null. The siblings are renumbered 0..n in their new order, so one
 *  patch carries the move and every `order` it changes. */
export async function moveBinItem(id: string, parent: string | null, before: string | null) {
  const p = store.state.project!;
  if (wouldNest(p, id, parent)) { toast("An entry cannot be listed inside itself", "err"); return; }
  const siblings = childrenOf(p, parent).map(([sid]) => sid).filter((sid) => sid !== id);
  const at = before ? siblings.indexOf(before) : -1;
  siblings.splice(at < 0 ? siblings.length : at, 0, id);
  const ops: PatchOp[] = [];
  const path = `${binPath(id)}/parent`;
  if (parentOf(p, id) !== parent) ops.push(parent ? { op: "add", path, value: parent } : { op: "remove", path });
  siblings.forEach((sid, i) => { if (p.bin[sid].order !== i) ops.push({ op: "add", path: `${binPath(sid)}/order`, value: i }); });
  if (ops.length) await store.patch(ops, `move ${id}`);
}

/** Languages offered for a transcript: whisper takes an ISO 639-1 code, or detects one.
 *  The choice is remembered, because a person's videos are usually in one language. */
const LANGUAGES: [string, string][] = [
  ["auto", "Detect"], ["ar", "Arabic"], ["zh", "Chinese"], ["nl", "Dutch"], ["en", "English"], ["fr", "French"],
  ["de", "German"], ["hi", "Hindi"], ["it", "Italian"], ["ja", "Japanese"], ["ko", "Korean"], ["pl", "Polish"],
  ["pt", "Portuguese"], ["ru", "Russian"], ["es", "Spanish"], ["sv", "Swedish"], ["tr", "Turkish"],
];
const LANGUAGE_KEY = "kuma.transcribe.language";
// A private window has no storage and throws on access, which must not cost the dialog.
const savedLanguage = (): string => { try { return localStorage.getItem(LANGUAGE_KEY) ?? "auto"; } catch { return "auto"; } };
const saveLanguage = (v: string) => { try { localStorage.setItem(LANGUAGE_KEY, v); } catch { /* no storage */ } };

/**
 * Speech to text for the selection. From a clip the subtitles are placed over it, aligned
 * (the daemon copies its start, end and speed, and sets the entry's offset to its `in`);
 * from a bin entry they are listed under it, unplaced.
 *
 * The dialog asks the two things only a person can answer: which language to force, and —
 * when this media has been transcribed before — whether to overwrite that transcript or
 * keep it and add another.
 */
export async function transcribe() {
  const p = store.state.project!;
  const sel = store.state.selection;
  const clip = sel?.kind === "clip" ? store.findClip(sel.id)?.clip : undefined;
  const binId = clip?.bin ?? (sel?.kind === "bin" ? sel.id : null);
  if (!binId || !hasAudio(p.bin[binId])) { toast("Select a clip or a media entry to transcribe", ""); return; }
  // An entry listed under this media that carries cues is already a transcript of it,
  // whatever template it has since been pointed at.
  const existing = childrenOf(p, binId).find(([, i]) => isOverlaySource(i.source) && "cues" in (i.source.vars ?? {}));

  const language = selectInput(LANGUAGES.map(([value, label]) => ({ value, label })), savedLanguage(), () => {});
  const replace = checkbox(true);
  const rows = [{ label: "Language", input: language as HTMLElement }];
  if (existing) rows.push({ label: "Replace", input: replace });
  const ok = await askForm({
    title: "Transcribe → subtitles",
    note: existing && `${existing[1].name || existing[0]} already holds a transcript of this media. Replacing changes only the words, keeping its styling and where it is placed; leave it unticked to add a second transcript.`,
    rows,
    ok: "Transcribe",
  });
  if (!ok) return;
  saveLanguage(language.value);

  const replacing = !!existing && replace.checked;
  // An id is only needed to name the entry being replaced; a new transcript is the
  // daemon's to name, once the words are in and nothing can take the name meanwhile.
  const what = clip ? { clip: clip.id } : { bin: binId };
  await startJob("Transcribe", () => api.transcribe({ ...what, id: replacing ? existing![0] : undefined, language: language.value, replace: replacing }));
}

/** Landscape ⇄ portrait: swap the frame's width and height. Everything else follows — the
 *  compiler reads the profile off the project and templates size in `vh` — except
 *  pixel-valued transforms, which stay where they were. */
export async function swapOrientation() {
  const p = store.state.project!;
  await store.patch([{ op: "replace", path: "/width", value: p.height }, { op: "replace", path: "/height", value: p.width }], `${orientation({ width: p.height, height: p.width })} ${p.height}×${p.width}`);
}

/** Add an empty track from the UI. */
export async function addTrack(kind: TrackKind) {
  const { op } = newTrackOp(kind);
  await store.patch([op], `add ${kind} track`);
}


/**
 * Split the selected clip at the playhead. Time-relative effects follow the cut: the
 * left half keeps its fade-in, the right half its fade-out, and keyframes are re-based
 * on the new start. A transition leaving the clip now leaves its right half.
 */
export async function splitAtPlayhead() {
  const f = store.findClip(store.selectedClip);
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

/** Remove the selected clip. Only clips: removing a bin entry throws away what every
 *  placement of it plays, so that stays a deliberate button in the inspector. */
export async function deleteSelectedClip() {
  const id = store.selectedClip;
  if (!id) return;
  const path = store.clipPath(id);
  if (path) await store.patch([{ op: "remove", path }], `delete ${id}`);
}
