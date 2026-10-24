// Project edits that are no one panel's business: placing, splitting, adding a track,
// minting a sequence. Each is asked for from several places, so each lives here as one
// revision-checked patch with its rule checked once — and no panel has to import another
// to place a clip.

import { api } from "./api";
import { button, checkbox, el, loadLocal, plural, row, saveLocal, selectInput } from "./dom";
import { askForm, askText } from "./modal";
import { desktop } from "./desktop";
import { startJob, store, toast } from "./store";
import type { ClipboardEntry, Found } from "./store";
import type { MenuItem } from "./menu";
import type { BinItem, BinSource, Clip, Marker, OverlayManifest, OverlaySource, PatchOp, Project, Track, TrackKind, Transition } from "./types";
import { LUMAS, binItem, binPath, childrenOf, clipPathOf, hasAudio, isFileSource, isOverlaySource, isTimedMedia, itemKind, orientation, parentOf, seqDuration, shiftEffect, trackFor, trackPathOf, trimEffect, wouldNest, wouldRecurse } from "./types";

/** Clip ids are minted by kind: `o1` for an overlay, `c1` for anything else. */
const clipPrefix = (kind: TrackKind) => (kind === "overlay" ? "o" : "c");

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
  const clip: Clip = { id: store.newId(clipPrefix(kind)), start: t, end: t + d, bin: binId, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" };
  await place(clip, kind, ti, []);
}

/** An installed blueprint's manifest, and the name it goes by. */
const blueprintOf = (id: string) => store.state.blueprints.find((b) => b.id === id)?.manifest;
const blueprintName = (id: string) => blueprintOf(id)?.name || id;

/** The bin id an entry named `label` would get. */
const binIdFor = (label: string, fallback: string) => uniqueId(slug(label) || fallback, Object.keys(store.state.project!.bin));

/** Copy a blueprint into the project and add it to the bin as `binId`, with `more` ops in
 *  the same patch (one undo; undo leaves the copied folder on disk). */
async function addBlueprint(blueprintId: string, binId: string, label: string, more: PatchOp[] = [], parent: string | null = null): Promise<boolean> {
  try {
    const { overlay } = await api.copyBlueprint(blueprintId, slug(label) || undefined);
    return !!(await addBinItem(label, blueprintId, { overlay, vars: {} }, { id: binId, more, parent }));
  } catch (e) {
    toast((e as Error).message, "err");
    return false;
  }
}

/** Copy a blueprint in, add it and place it at `t`, in one patch. */
export async function placeBlueprint(blueprintId: string, t: number, ti = -1) {
  const label = blueprintName(blueprintId);
  const binId = binIdFor(label, blueprintId);
  const d = blueprintOf(blueprintId)?.duration_default ?? 4;
  const clip: Clip = { id: store.newId("o"), start: t, end: t + d, bin: binId, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" };
  if (await addBlueprint(blueprintId, binId, label, placeOps(clip, "overlay", ti))) store.selectItem("clip", clip.id);
}

function defaultDuration(item: BinItem, p: Project, binId: string): number {
  if (isFileSource(item.source)) return item.meta?.duration ?? 5;
  // A sequence goes down at its own length, the way dragging in a clip does.
  if (itemKind(item) === "sequence") return seqDuration(p, binId) || 4;
  // An overlay goes down at the length it was authored for — the same number
  // `placeBlueprint` uses, so placing an entry and dragging in its blueprint agree.
  if (isOverlaySource(item.source)) return overlayDuration(item);
  return 4;
}

/** The manifest of an overlay entry's folder, when it loads. */
export const manifestOf = (source: OverlaySource): OverlayManifest | undefined => store.state.manifests[source.overlay]?.manifest;

/** Why an overlay entry's folder has no manifest to show. */
export const overlayError = (source: OverlaySource): string => store.state.manifests[source.overlay]?.error ?? `${source.overlay} is not loaded yet`;

/** The bin entries using `folder`. */
export const sharersOf = (folder: string): string[] =>
  Object.entries(store.state.project?.bin ?? {}).filter(([, i]) => isOverlaySource(i.source) && i.source.overlay === folder).map(([id]) => id);

/** How long an overlay runs by default: its manifest's length. */
export const overlayDuration = (item: BinItem | undefined): number =>
  (item && isOverlaySource(item.source) && manifestOf(item.source)?.duration_default) || 4;

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
  const ops = [...pre, ...placeOps(clip, kind, ti)];
  const ok = await store.patch(ops, `add ${clip.id}`);
  if (ok) store.selectItem("clip", clip.id);
}

/** The ops `place` sends: a track of `kind` if there is none, then the clip. */
function placeOps(clip: Clip, kind: TrackKind, ti: number): PatchOp[] {
  const tracks = store.tracks;
  if (ti < 0 || tracks[ti]?.kind !== kind) ti = tracks.findIndex((tr) => tr.kind === kind);
  const ops: PatchOp[] = [];
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
  return ops;
}

/** Build the patch op that inserts a new empty track of `kind`. Tracks composite in array
 *  order, so a video track goes below the first overlay track and everything else on top
 *  (audio order does not matter). */
function newTrack(kind: TrackKind, name?: string, minted: string[] = []): Track {
  const id = store.newTrackId(kind[0], minted);
  return { id, kind, name: name ?? `${kind[0].toUpperCase()}${kind.slice(1)} ${id.slice(1)}`, muted: false, hidden: false, clips: [], transitions: [] };
}

/** `tracks` is the array the new one is going into — the open sequence's, or the version
 *  a patch that adds several is building up. `minted` names the track ids that patch has
 *  already taken, which are not in the project yet and must not be handed out twice. */
function newTrackOp(kind: TrackKind, tracks: { kind: TrackKind }[] = store.tracks, minted: string[] = []): { op: PatchOp; index: number; track: Track } {
  const track = newTrack(kind, undefined, minted);
  const firstOverlay = tracks.findIndex((t) => t.kind === "overlay");
  const index = kind === "video" && firstOverlay >= 0 ? firstOverlay : tracks.length;
  return { op: { op: "add", path: `${store.seqPath}/tracks/${index}`, value: track }, index, track };
}

/** Mint a bin entry named by the user: a slug of the name (or `fallback`) made unique,
 *  one patch. Returns the id, or null when the patch was refused. */
async function addBinItem(name: string, fallback: string, source: BinSource, o: { id?: string; more?: PatchOp[]; parent?: string | null } = {}): Promise<string | null> {
  const { id = binIdFor(name, fallback), more = [], parent } = o;
  const value: BinItem = { name, source };
  if (parent) value.parent = parent;
  return (await store.patch([{ op: "add", path: binPath(id), value }, ...more], `add ${fallback} ${id}`)) ? id : null;
}

/** Bin ids must match [A-Za-z_][A-Za-z0-9_-]*, so a leading digit takes an `s`. */
const slug = (name: string) => name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").replace(/^(?=[0-9])/, "s");

/** Ask for a name, then mint an empty sequence and open it. It starts with the same
 *  three tracks a new project does, because an empty one has nowhere to drop a clip.
 *  `window.prompt` is not an option: Electron does not implement it (see `modal.ts`). */
export async function promptNewSequence(parent: string | null = null) {
  const name = await askText({ title: "New sequence", label: "Name", placeholder: "Intro" });
  if (!name) return;
  const id = await addBinItem(name, "sequence", { tracks: [newTrack("video", "Video"), newTrack("overlay", "Overlays"), newTrack("audio", "Audio")], markers: [] }, { parent });
  if (id) await store.openSequence(id);
}

/** A folder is a bin entry with nothing but a name and children. */
export async function promptNewFolder(parent: string | null = null) {
  const name = await askText({ title: "New folder", label: "Name", placeholder: "Speech" });
  if (name) await addBinItem(name, "folder", { folder: true }, { parent });
}

/** The blueprints the bin's New menu offers — those whose manifest says `create` — by label. */
export const creatable = (): { id: string; label: string; description: string }[] =>
  store.state.blueprints
    .filter((b) => b.manifest.create)
    .map((b) => ({ id: b.id, label: b.manifest.create!, description: b.manifest.description }))
    .sort((a, b) => a.label.localeCompare(b.label));

/**
 * New ▸ Title: a blueprint copied in as a new entry of its own, under `parent`, named
 * "Title", "Title 2"… and opened on the workbench with its vars in the inspector. No
 * dialog — the name is the least interesting thing about a title, and the bin renames.
 * The copy is its own folder, as adding any blueprint is, so every title can be restyled
 * or edited without touching another.
 */
export async function createFromBlueprint(blueprintId: string, parent: string | null = null) {
  const label = blueprintOf(blueprintId)?.create || blueprintName(blueprintId);
  const names = new Set(Object.values(store.state.project!.bin).map((i) => i.name));
  let name = label;
  for (let n = 2; names.has(name); n++) name = `${label} ${n}`;
  const id = binIdFor(name, blueprintId);
  if (await addBlueprint(blueprintId, id, name, [], parent)) store.openEntry(id);
}

/** What the bin's New menu and right-click offer, creating under `parent`. */
export function newItems(parent: string | null): MenuItem[] {
  const made = creatable();
  const items: MenuItem[] = made.map((c) => ({ label: c.label, title: c.description, run: () => void createFromBlueprint(c.id, parent) }));
  if (!made.length) items.push({ label: "Title", disabled: true, run: () => {}, title: "No blueprint offers itself here yet: run `kuma library` to install the starter set" });
  items.push("separator",
    { label: "Overlay from a blueprint…", run: () => void promptNewOverlay(undefined, parent), title: "Any blueprint in your library, copied in and opened on the workbench" },
    { label: "Sequence…", run: () => void promptNewSequence(parent) },
    { label: "Folder…", run: () => void promptNewFolder(parent) },
  );
  return items;
}

/** A blueprint's two ways into the project: the workbench, or the timeline at the playhead. */
export function blueprintActions(id: string, cls = "actions"): HTMLElement {
  return row(cls,
    button("Workbench", () => void promptNewOverlay(id), "Copy it into the project and open it in the workbench, unplaced"),
    button("Timeline", () => void placeBlueprint(id, store.state.playhead), "Copy it into the project and place it at the playhead"),
  );
}

/** Render the preview of one blueprint or overlay entry. */
export const renderPreview = (which: { blueprints?: string[]; bins?: string[] }) => startJob("Preview", () => api.thumbs(which));

/** An image of a rendered preview under `base`: the animated one, else the still, else
 *  nothing. */
export function previewImg(base: string): HTMLImageElement {
  const img = el("img", "insp-thumb");
  const v = `?v=${store.state.previewStamp}`;
  img.src = `${base}/preview.webp${v}`;
  img.alt = "";
  img.onerror = () => {
    if (img.src.includes("preview.webp")) img.src = `${base}/thumb.png${v}`;
    else img.remove();
  };
  return img;
}

/** Copy a blueprint in as a new entry and open it in the workbench, without placing it. */
export async function promptNewOverlay(blueprint?: string, parent: string | null = null) {
  const blueprints = store.state.blueprints;
  if (!blueprints.length) {
    toast("No blueprints found. Run `kuma library`, or add a library of them from the Blueprints panel.", "err", 8000);
    return;
  }
  const pick = selectInput(blueprints.map((t) => ({ value: t.id, label: blueprintName(t.id), title: t.manifest.description })), blueprint ?? blueprints[0].id, () => {
    // The name follows the blueprint until it has been typed in, so the common case is
    // one click and the rarer one is still a free choice.
    if (!typed) name.value = blueprintName(pick.value);
  });
  const name = el("input");
  name.type = "text";
  name.value = blueprintName(pick.value);
  let typed = false;
  name.oninput = () => { typed = true; };
  const ok = await askForm({
    title: "New overlay",
    note: "Copies the blueprint into this project's overlays/ and adds it to the bin, then opens it in the workbench. The copy is this project's to edit; the blueprint is not touched. Nothing is placed on a timeline until you place it.",
    rows: [{ label: "Blueprint", input: pick }, { label: "Name", input: name }],
    ok: "Create",
  });
  if (!ok) return;
  const label = name.value.trim() || blueprintName(pick.value);
  const id = binIdFor(label, pick.value);
  if (await addBlueprint(pick.value, id, label, [], parent)) store.openEntry(id);
}

/** A second entry on the same folder, vars and all, opened in the workbench. */
export async function duplicateOverlay(id: string) {
  const p = store.state.project!;
  const item = p.bin[id];
  if (!item || !isOverlaySource(item.source)) return;
  const name = `${item.name || id} copy`;
  const copyId = binIdFor(name, "overlay");
  const value: BinItem = { name, source: { overlay: item.source.overlay, vars: { ...(item.source.vars ?? {}) } } };
  // Listed where the original is, so a variant does not turn up at the other end of the bin.
  if (item.parent) value.parent = item.parent;
  if (await store.patch([{ op: "add", path: binPath(copyId), value }], `duplicate ${id}`)) store.openEntry(copyId);
}

/** Give an overlay entry its own copy of the folder it shares. */
export async function makeOverlayUnique(id: string) {
  try {
    const { overlay } = await api.makeUnique(id);
    toast(`${id} now has its own folder: ${overlay}`, "ok");
  } catch (e) {
    toast((e as Error).message, "err");
  }
}

/** Read the libraries again; what was found arrives as a `libraries` event, as after any
 *  change to them. */
export async function rescanLibraries() {
  await api.rescanLibraries();
}

/**
 * Use a library folder in this project, as one edit — undone like any other, and seen by no
 * other project: the desktop's folder picker, or a typed path in a browser tab. Nothing is
 * copied; a blueprint is when it is used.
 */
export async function addLibrary() {
  const app = desktop();
  const path = app?.pickFolder
    ? await app.pickFolder("Use a library in this project").catch((e) => { toast(`Cannot open the folder picker: ${(e as Error).message}`, "err"); return null; })
    : await askText({ title: "Use a library in this project", label: "Folder", placeholder: "/path/to/library (holding blueprints/, presets/, fonts/…)", ok: "Add" });
  if (!path) return;
  try {
    const { library, revision } = await api.addLibrary(path);
    toast(revision === null ? `This project already uses ${library.name}` : `This project now uses ${library.name}: ${plural(library.contents.blueprints.length, "blueprint")}`, "ok", 5000);
  } catch (e) {
    toast((e as Error).message, "err", 8000);
  }
}

/** Stop using one of this project's libraries, as one edit. Its folder, and the overlays
 *  this project copied from it, stay. */
export async function removeLibrary(path: string, name: string) {
  const ok = await askForm({ title: `Stop using ${name} in this project?`, note: `Its blueprints leave this project's panel; other projects are not touched, and undo brings it back. Nothing is deleted: the folder stays where it is (${path}), and overlays this project copied from it keep working, unless they use its _vendor/ helpers or fonts.`, rows: [], ok: "Stop using" });
  if (!ok) return;
  try {
    await api.removeLibrary(path);
  } catch (e) {
    toast((e as Error).message, "err", 8000);
  }
}

/** Save an overlay entry as a blueprint (its vars as defaults), asking for the id and
 *  before replacing one. */
export async function saveAsBlueprint(id: string) {
  const item = store.state.project?.bin[id];
  if (!item || !isOverlaySource(item.source)) return;
  const folder = item.source.overlay;
  const suggested = slug(manifestOf(item.source)?.name || item.name || "") || folder.split("/").pop() || id;
  const name = await askText({ title: "Save as blueprint", label: "Blueprint id", placeholder: suggested, value: suggested, ok: "Save" });
  if (!name) return;
  const bp = slug(name);
  const exists = store.state.blueprints.some((b) => b.id === bp);
  if (exists && !(await askForm({ title: "Replace blueprint?", note: `A blueprint “${bp}” is already in your library. Replace it with this overlay? Projects that copied it keep their copies.`, rows: [], ok: "Replace" }))) return;
  try {
    await api.saveBlueprint(id, bp, exists);
    await rescanLibraries();
    toast(`Saved ${folder} as blueprint “${bp}”`, "ok");
  } catch (e) {
    toast((e as Error).message, "err");
  }
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
// Remembered, because a person's videos are usually in one language and one model.
const saved = (key: string, fallback: string): string => loadLocal(`kuma.transcribe.${key}`) ?? fallback;
const save = (key: string, v: string) => saveLocal(`kuma.transcribe.${key}`, v);

/**
 * Speech to text for the selection. From a clip the subtitles are placed over it, aligned
 * (the daemon copies its start, end and speed, and sets the entry's offset to its `in`);
 * from a bin entry they are listed under it, unplaced.
 *
 * The dialog asks which language to force, which model to run when several are installed,
 * whether to snap word edges to pauses, how long each subtitles entry may run, and — when
 * this media has been transcribed before — whether to overwrite that transcript.
 */
export async function transcribe() {
  const p = store.state.project!;
  const sel = store.state.selection;
  // One clip's worth of audio: the last one clicked, whatever else is selected with it.
  const clip = store.findClip(store.selectedClip)?.clip;
  const binId = clip?.bin ?? (sel?.kind === "bin" ? sel.id : null);
  if (!binId || !hasAudio(p.bin[binId])) { toast("Select a clip or a media entry to transcribe", ""); return; }
  // An entry listed under this media that carries cues is already a transcript of it,
  // whatever folder it has since been pointed at. A transcript cut into pieces is several
  // such entries, and replacing overwrites the words of one: offered only when there is one.
  const transcripts = childrenOf(p, binId).filter(([, i]) => isOverlaySource(i.source) && "cues" in (i.source.vars ?? {}));
  const existing = transcripts.length === 1 ? transcripts[0] : undefined;

  const models = await api.whisperModels().catch(() => [] as string[]);
  const language = selectInput(LANGUAGES.map(([value, label]) => ({ value, label })), saved("language", "auto"), () => {});
  // The daemon's default is the first installed; a remembered model that is gone falls back to it.
  const model = selectInput(models.map((m) => ({ value: m, label: m.replace(/^ggml-|\.bin$/g, "") })), models.includes(saved("model", "")) ? saved("model", "") : models[0], () => {});
  const snap = checkbox(true);
  snap.title = "Move each word's edges onto the pauses in the sound, so a gap between words is a real pause";
  const split = el("input");
  Object.assign(split, { type: "number", min: "0", step: "1", value: saved("split", "60"), title: "Seconds of transcript per subtitles entry, cut at a pause; 0 keeps one entry" });
  const replace = checkbox(true);
  const rows = [{ label: "Language", input: language as HTMLElement }];
  if (models.length > 1) rows.push({ label: "Model", input: model });
  rows.push({ label: "Snap to pauses", input: snap }, { label: "Split every (s)", input: split });
  if (existing) rows.push({ label: "Replace", input: replace });
  const ok = await askForm({
    title: "Transcribe → subtitles",
    note: existing && `${existing[1].name || existing[0]} already holds a transcript of this media. Replacing changes only the words, keeping its styling and where it is placed; leave it unticked to add a second transcript.`,
    rows,
    ok: "Transcribe",
  });
  if (!ok) return;
  save("language", language.value);
  if (models.length > 1) save("model", model.value);
  // 0 keeps one entry; the daemon cuts no finer than 5 s.
  const asked = Math.max(0, Number(split.value) || 0);
  const seconds = asked > 0 ? Math.max(5, asked) : 0;
  save("split", String(seconds));

  const replacing = !!existing && replace.checked;
  // An id is only needed to name the entry being replaced; a new transcript is the
  // daemon's to name, once the words are in and nothing can take the name meanwhile.
  const what = clip ? { clip: clip.id } : { bin: binId };
  await startJob("Transcribe", () => api.transcribe({
    ...what, id: replacing ? existing![0] : undefined, language: language.value, replace: replacing,
    model: models.length > 1 ? model.value : undefined, snap: snap.checked,
    // Replacing overwrites one entry, which a split cannot; the daemon then keeps one.
    split: replacing ? undefined : seconds,
  }));
}

/** Landscape ⇄ portrait: swap the frame's width and height. Everything else follows — the
 *  compiler reads the profile off the project and overlays size in `vh` — except
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


/** Split every selected clip the playhead crosses, in one patch — half a cut applying is
 *  not an outcome anyone wants. Time-relative effects follow: the left half keeps its
 *  fade-in, the right its fade-out, and keyframes are re-based on the new start. */
/** Split the selected clips at `t` — the playhead for S, the pointer for a clip's
 *  right-click. */
export async function splitAt(t: number) {
  const cutting = store.selectedFound.filter((f) => t > f.clip.start && t < f.clip.end);
  if (!cutting.length) { toast("Select a clip that runs across that moment to split it", ""); return; }
  const ops: PatchOp[] = [];
  const minted: string[] = [];
  /** The right half's id, by the clip it was cut from: a transition leaving the clip now
   *  leaves that half. */
  const renamed = new Map<string, string>();
  for (const f of cutting) {
    const c = f.clip;
    const cut = t - c.start;
    const left: Clip = { ...c, end: t, effects: c.effects.map((e) => trimEffect(e, cut)) };
    const right: Clip = { ...c, id: store.newId(c.id + "_", minted), start: t, effects: c.effects.map((e) => shiftEffect(e, cut)) };
    minted.push(right.id);
    renamed.set(c.id, right.id);
    // Both halves keep playing the same bin entry; only the right one moves its in point.
    if (isTimedMedia(binItem(store.state.project!, c))) right.in = (c.in ?? 0) + cut * Math.abs(c.speed);
    // A replace leaves every index alone and an append takes the next one, so the paths
    // computed here stay right however many clips this patch cuts.
    ops.push({ op: "replace", path: clipPathOf(f), value: left }, { op: "add", path: `${trackPathOf(f)}/clips/-`, value: right });
  }
  // One rewrite per track being cut, whatever it holds of the clips above: two clips on
  // one track must not each rewrite the track's transitions from the version they were
  // handed.
  for (const [path, track] of tracksOf(cutting)) {
    if (track.transitions.some((x) => renamed.has(x.from))) {
      ops.push({ op: "replace", path: `${path}/transitions`, value: track.transitions.map((x) => (renamed.has(x.from) ? { ...x, from: renamed.get(x.from)! } : x)) });
    }
  }
  await store.patch(ops, `split ${cutting.map((f) => f.clip.id).join(", ")}`);
}

/** The tracks the found clips are on, by pointer — one entry per track, however many of
 *  the clips share it. */
const tracksOf = (found: Found[]) => new Map(found.map((f) => [trackPathOf(f), f.track]));

// ---- groups --------------------------------------------------------------------------
//
// A group holds clips together for editing: selecting one selects them all, and a drag
// moves the block. The id lives on the clip, so a delete leaves nothing dangling, and the
// compiler never reads it.

/** Group the selected clips. Two or more — a group of one is not a group. That they must
 *  be on one timeline is the validator's rule, and its refusal is the message. */
export async function groupSelection() {
  const found = store.selectedFound;
  if (found.length < 2) { toast("Select two or more clips to group (Ctrl/Cmd+click)", ""); return; }
  // Any group the selection already carries is absorbed into the new one, so grouping a
  // group with a third clip makes one group of three rather than a group of a group.
  const id = store.newGroupId();
  await store.patch(found.map((f) => ({ op: "add" as const, path: `${clipPathOf(f)}/group`, value: id })), `group ${found.map((f) => f.clip.id).join(", ")}`);
}

/** Ungroup. The selection *is* the group, so this clears the field on every clip in it.
 *  Empty means "no group" and is not written back, so the project returns to what it was. */
export async function ungroupSelection() {
  const found = store.selectedFound.filter((f) => f.clip.group);
  if (!found.length) { toast("Select a grouped clip to ungroup", ""); return; }
  await store.patch(found.map((f) => ({ op: "add" as const, path: `${clipPathOf(f)}/group`, value: "" })), `ungroup ${found.map((f) => f.clip.id).join(", ")}`);
}

// ---- the timeline clipboard --------------------------------------------------------
//
// A copy is not an edit, so the clipboard is UI state (`store.ts`) and costs no revision;
// the paste is the edit. What travels is the **placement** — when it plays, how fast, its
// effects — never the bin entry, so a paste adds clips and nothing else. A transition
// names two clips on one track and stays behind with the pair it names.

/** Copy the selection, deeply: the clipboard has to outlive the project object it came
 *  from, which every refresh replaces and a cut empties. */
export function copySelection(): boolean {
  const entries: ClipboardEntry[] = store.selectedFound.map((f) => ({ clip: structuredClone(f.clip), track: f.track.id, kind: f.track.kind }));
  if (!entries.length) { toast("Select a clip to copy", ""); return false; }
  // In time order, so the first one is the anchor a paste lands on the playhead.
  entries.sort((a, b) => a.clip.start - b.clip.start);
  store.set({ clipboard: { entries } });
  toast(entries.length === 1 ? `Copied ${clipName(entries[0].clip)}` : `Copied ${plural(entries.length, "clip")}`, "ok", 1500);
  return true;
}

/** Cut: the copy first, so a refused delete never leaves the clipboard holding clips that
 *  are still on the timeline under a different revision. */
export async function cutSelection() {
  if (copySelection()) await deleteSelectedClips();
}

/**
 * Paste the clipboard onto the open sequence, at the playhead.
 *
 * **One clip** lands on the track you last clicked, any track of the kind it was copied
 * from, which is what makes "copy here, paste there" one gesture; with none clicked, the
 * first of that kind, as a drop from the bin does.
 *
 * **Several** are a shape, not a position: they keep their spacing and their own tracks,
 * so the clicked track does not come into it. See `pasteTargets` and `freeOffset` for how
 * the tracks are matched and how an occupied slot is answered.
 */
export async function pasteClipboard() {
  const entries = store.state.clipboard?.entries ?? [];
  if (!entries.length) { toast("Nothing to paste — copy a clip first (Ctrl/Cmd+C)", ""); return; }
  const p = store.state.project!;
  const gone = entries.find((e) => !p.bin[e.clip.bin]);
  if (gone) { toast(`${gone.clip.bin} is no longer in the bin`, "err"); return; }
  // The same rule `placeBinItem` checks, for the same reason: a sequence that contains the
  // one you are editing cannot be placed on it.
  const loop = entries.find((e) => wouldRecurse(p, e.clip.bin, store.state.sequence));
  if (loop) {
    toast(`${p.bin[loop.clip.bin].name || loop.clip.bin} contains this sequence; pasting it here would make a loop`, "err");
    return;
  }

  const ops: PatchOp[] = [];
  const sim: SimTrack[] = store.tracks.map((t) => ({ id: t.id, kind: t.kind, clips: t.clips }));
  const targets = pasteTargets(entries, sim, ops);
  const anchor = Math.min(...entries.map((e) => e.clip.start));
  const at = store.snap(store.state.playhead);
  const dt = freeOffset(targets, at - anchor);

  const regroup = pasteGroups(entries);
  const pasted: Clip[] = [];
  for (const { entry, track } of targets) {
    const { group, ...copied } = structuredClone(entry.clip);
    const clip: Clip = { ...copied, id: store.newId(clipPrefix(entry.kind), pasted.map((c) => c.id)), start: entry.clip.start + dt, end: entry.clip.end + dt, ...regroup(group) };
    pasted.push(clip);
    // The indices are read off now, when nothing more will move: adding a track above
    // renumbered the ones after it.
    ops.push({ op: "add", path: `${store.seqPath}/tracks/${sim.indexOf(track)}/clips/-`, value: clip });
  }
  if (!(await store.patch(ops, `paste ${pasted.map((c) => c.id).join(", ")}`))) return;
  // One update: what was pasted is what the inspector shows, and — for a single clip — the
  // track it landed on is where the next paste goes, including one just created for it.
  store.selectClips(pasted.map((c) => c.id), entries.length === 1 ? targets[0].track.id : undefined);
  if (dt > at - anchor) toast(`The playhead was inside a clip — pasted at ${(anchor + dt).toFixed(2)}s`, "", 2500);
}

/**
 * What each copied group becomes on the way in: a group of its own, so pasting a block
 * gives you a second block rather than enlarging the first — the two copies would
 * otherwise move together forever. A group only one of whose clips was copied is no group
 * at all on arrival, so it arrives loose.
 */
function pasteGroups(entries: ClipboardEntry[]): (old: string | undefined) => { group?: string } {
  const size = new Map<string, number>();
  for (const e of entries) if (e.clip.group) size.set(e.clip.group, (size.get(e.clip.group) ?? 0) + 1);
  const minted = new Map<string, string>();
  for (const [g, n] of size) if (n > 1) minted.set(g, store.newGroupId([...minted.values()]));
  return (old) => { const group = old && minted.get(old); return group ? { group } : {}; };
}

/** A track as a paste sees it: enough to match one, to add one, and to find room on one. */
interface SimTrack { id: string; kind: TrackKind; clips: Clip[] }
interface PasteTarget { entry: ClipboardEntry; track: SimTrack }

/**
 * Which track each entry lands on. Tracks are resolved as objects rather than as indices
 * because adding one renumbers the others; `sim` is the open sequence's tracks as this
 * patch will leave them, and gains the tracks it adds (their ops go on `ops`).
 *
 * One clip goes where you clicked, else back where it came from; several keep the tracks
 * they were copied from, and the click does not come into it. A track already taken by
 * another entry of this paste is not a candidate — two of them collapsing onto one track
 * is the one way the shape is lost silently — and a track is added when nothing is free.
 */
function pasteTargets(entries: ClipboardEntry[], sim: SimTrack[], ops: PatchOp[]): PasteTarget[] {
  const minted: string[] = [];
  /** The track each source track resolved to. */
  const claimed = new Map<string, SimTrack>();
  const taken = new Set<SimTrack>();
  const single = entries.length === 1;
  return entries.map((entry) => {
    let track = claimed.get(entry.track);
    if (!track) {
      const wanted = single ? [store.state.track, entry.track] : [entry.track];
      const free = (t: SimTrack) => t.kind === entry.kind && !taken.has(t);
      track = wanted.map((id) => sim.find((t) => t.id === id && free(t))).find(Boolean) ?? sim.find(free);
      if (!track) {
        const { op, index, track: added } = newTrackOp(entry.kind, sim, minted);
        ops.push(op);
        minted.push(added.id);
        track = { id: added.id, kind: added.kind, clips: [] };
        sim.splice(index, 0, track);
        toast(`Added a ${entry.kind} track`, "ok");
      }
      claimed.set(entry.track, track);
      taken.add(track);
    }
    return { entry, track };
  });
}

/**
 * One offset for the whole group, so the shape survives: from `dt`, push it past whatever
 * any of its clips lands on, and look again — clearing one clip can walk into the next.
 * A paste names a position, so an occupied one is answered with the next one that is free
 * rather than by refusing, which also lays two pastes in a row back to back.
 */
function freeOffset(targets: PasteTarget[], dt: number): number {
  const room = targets.reduce((n, { track }) => n + track.clips.length, 0) + 2;
  for (let guard = 0; guard < room; guard++) {
    let push = 0;
    for (const { entry, track } of targets) {
      const start = entry.clip.start + dt, end = entry.clip.end + dt;
      for (const c of track.clips) if (start < c.end && end > c.start) push = Math.max(push, c.end - start);
    }
    if (push <= 0) break;
    dt += push;
  }
  return dt;
}

/** What to call a clip in a message: its label, else the entry it plays, else its id. */
const clipName = (c: Clip): string => c.label || store.state.project?.bin[c.bin]?.name || c.bin || c.id;

/**
 * Remove every selected clip, in one patch. Only clips: removing a bin entry throws away
 * what every placement of it plays, so that stays a deliberate button in the inspector.
 */
export async function deleteSelectedClips() {
  const found = store.selectedFound;
  if (!found.length) return;
  const removed = new Set(found.map((f) => f.clip.id));
  const ops: PatchOp[] = [];
  // A transition names two clips, and one naming a clip that has gone is refused by the
  // validator — so a transition leaves with either of the clips it joins.
  for (const [path, track] of tracksOf(found)) {
    const keep = track.transitions.filter((x) => !removed.has(x.from) && !removed.has(x.to));
    if (keep.length !== track.transitions.length) ops.push({ op: "replace", path: `${path}/transitions`, value: keep });
  }
  // Highest index first: removing a clip renumbers every clip after it on its track.
  for (const f of [...found].sort((a, b) => b.ci - a.ci)) ops.push({ op: "remove", path: clipPathOf(f) });
  await store.patch(ops, `delete ${[...removed].join(", ")}`);
}

// ---- markers ---------------------------------------------------------------------------

/** How a marker's moment is written in a title or an undo note. */
const at = (t: number) => `${t.toFixed(2)}s`;

/**
 * Write the open sequence's markers whole, sorted by time so the file reads the way the
 * timeline does. Whole rather than one op per marker because a move reorders them: the
 * index a marker was drawn with names it only until the next edit. `add` rather than
 * `replace`, since a sequence written without the field is valid and `replace` would
 * refuse it.
 */
async function writeMarkers(ms: Marker[], note: string) {
  await store.patch([{ op: "add", path: `${store.seqPath}/markers`, value: [...ms].sort((a, b) => a.t - b.t) }], note);
}

/** Change marker `i` of the open sequence, or remove it when `change` is null. */
async function patchMarker(i: number, change: Partial<Marker> | null, note: string) {
  const ms = store.markers;
  if (!ms[i]) return;
  await writeMarkers(change ? ms.map((x, k) => (k === i ? { ...x, ...change } : x)) : ms.filter((_, k) => k !== i), note);
}

/** Ask for the comment of the marker at `t`. Null only when cancelled: an empty comment
 *  still marks a place. A textarea, since a note on a cut is a sentence or two. */
async function askComment(t: number, value: string, ok: string): Promise<string | null> {
  const input = el("textarea", "comment");
  input.value = value;
  input.placeholder = "What should change here? An agent reading the project sees this.";
  const sent = await askForm({ title: `Marker at ${at(t)}`, rows: [{ label: "Comment", input }], ok });
  return sent ? input.value.trim() : null;
}

/** Which marker is on `t`'s frame, or -1. */
const markerAt = (t: number): number => store.markers.findIndex((m) => Math.abs(m.t - t) < store.halfFrame);

/** The comment of the marker on `t`'s frame, if there is one: what the marker field starts
 *  from, so a marker on a frame that has one edits it rather than stacking a second. */
export const markerOn = (t: number): string | null => store.markers[markerAt(t)]?.label ?? null;

/** A marker at `t` with the comment typed in the markers panel (P, or the ruler's right-click)
 *  — or none: a moment marked. On a frame that already has a marker, this is its comment now:
 *  two markers on one frame are one note written twice. Who left it is the daemon's to stamp
 *  (`origin`), from this edit's. */
export async function placeMarker(t: number, label: string): Promise<boolean> {
  const text = label.trim();
  const i = markerAt(t);
  if (i >= 0) {
    if (store.markers[i].label !== text) await patchMarker(i, { label: text }, `marker at ${at(t)}`);
    return true;
  }
  await writeMarkers([...store.markers, { t, label: text }], `marker at ${at(t)}`);
  return true;
}

export async function editMarker(i: number) {
  const m = store.markers[i];
  if (!m) return;
  const label = await askComment(m.t, m.label, "Save");
  if (label === null || label === m.label) return;
  // Found again: the dialog was up for as long as the person was typing.
  const j = store.markers.findIndex((x) => x.t === m.t && x.label === m.label);
  if (j < 0) { toast("That marker changed while you were writing — try again", "err"); return; }
  await patchMarker(j, { label }, `edit marker at ${at(m.t)}`);
}

export const moveMarker = (i: number, t: number) => patchMarker(i, { t }, `move marker to ${at(t)}`);

export const removeMarker = (i: number) => patchMarker(i, null, `remove marker at ${at(store.markers[i]?.t ?? 0)}`);

/** Remove the marker on `t`'s frame — whoever left it — from the markers panel, which knows
 *  a marker by its time, not by where it sits in the document's array. */
export async function removeMarkerAt(t: number) {
  const i = markerAt(t);
  if (i < 0) { toast("That marker is gone already", "err"); return; }
  await removeMarker(i);
}

// ---------- transitions ----------
//
// A transition joins two clips that touch on one track, `from` → `to`, and is centred on
// the cut: half its length is drawn from after `from`'s out point and half from before
// `to`'s in point. Those are the *handles*, and the validator refuses a transition that has
// none — which is the one thing about them nobody guesses, so the editor says it in words.

/** The clip that starts where `c` ends on its track: the one a transition from it goes to. */
export const joinedAfter = (track: Track, c: Clip): Clip | undefined =>
  track.clips.find((b) => b.id !== c.id && Math.abs(b.start - c.end) < 1e-6);

/** Why `a` cannot take a transition into the clip it touches, or null when it can: what
 *  the clip menu disables, and what `addTransition` says if asked anyway. */
export function transitionBlocked(track: Track, a: Clip): string | null {
  const b = joinedAfter(track, a);
  if (!b) return "A transition joins two clips that touch: move one against the other first";
  if (track.kind === "overlay") return "Overlays take no transitions yet: their bakes carry no handles";
  if (track.transitions.some((x) => x.from === a.id && x.to === b.id)) return "These clips already have a transition — right-click it to change it";
  return null;
}

/** A dissolve from `from` into the clip it touches: a second, or as much as the clips and
 *  the handle before the second one's in point allow — the validator's one hard rule on
 *  handles. A short tail after the first is its warning to give, not a refusal here. */
export async function addTransition(from: string) {
  const f = store.findClip(from);
  if (!f) return;
  const blocked = transitionBlocked(f.track, f.clip);
  const b = joinedAfter(f.track, f.clip);
  if (blocked || !b) { toast(blocked ?? "", ""); return; }
  const used = (c: Clip) => f.track.transitions.filter((x) => x.from === c.id || x.to === c.id).reduce((n, x) => n + x.duration / 2, 0);
  let half = Math.min(f.clip.end - f.clip.start - used(f.clip), b.end - b.start - used(b));
  let why = "the clips are too short";
  if (isTimedMedia(binItem(store.state.project!, b))) {
    const room = (b.in ?? 0) / Math.abs(b.speed || 1);
    if (room < half) { half = room; why = `“${b.label || b.bin}” starts at the very beginning of its source: trim its start so there is material to fade in from`; }
  }
  const frames = Math.floor(Math.min(0.5, half) * 2 * store.fps + 1e-6);
  if (frames < 2) { toast(`No room for a transition: ${why}`, "err", 8000); return; }
  const value: Transition = { from, to: b.id, kind: "dissolve", duration: frames / store.fps };
  await store.patch([{ op: "add", path: `${trackPathOf(f)}/transitions/-`, value }], `dissolve ${from} → ${b.id}`);
}

/** Where transition `i` of track `ti` (on the open sequence) is, if it is still there. */
const transitionPath = (ti: number, x: Transition): string | null => {
  const i = store.tracks[ti]?.transitions.findIndex((y) => y.from === x.from && y.to === x.to) ?? -1;
  return i < 0 ? null : `${store.seqPath}/tracks/${ti}/transitions/${i}`;
};

/** Change a transition's fields, as one edit. */
export async function updateTransition(ti: number, x: Transition, changes: Partial<Transition>, note: string) {
  const path = transitionPath(ti, x);
  if (path) await store.patch([{ op: "replace", path, value: { ...x, ...changes } }], `transition ${x.from} → ${x.to}: ${note}`);
}

/** A length, snapped to the frame and never less than one. */
export const setTransitionDuration = (ti: number, x: Transition, d: number) =>
  updateTransition(ti, x, { duration: Math.max(1 / store.fps, store.snap(d)) }, `${d}s`);

export async function editTransitionDuration(ti: number, x: Transition) {
  const text = await askText({ title: "Transition length", label: "Seconds", value: String(+x.duration.toFixed(3)), ok: "Set" });
  const d = Number(text);
  if (text !== null && d > 0) await setTransitionDuration(ti, x, d);
}

/** A wipe needs a pattern; a dissolve carries none. */
export const setTransitionKind = (ti: number, x: Transition, kind: Transition["kind"]) =>
  updateTransition(ti, x, { kind, luma: kind === "wipe" ? x.luma ?? LUMAS[0] : null }, kind);

export async function removeTransition(ti: number, x: Transition) {
  const path = transitionPath(ti, x);
  if (path) await store.patch([{ op: "remove", path }], `remove transition ${x.from} → ${x.to}`);
}

/** Rename a track of the open sequence. A name is the track's label and nothing else —
 *  edits address it by index, pastes match it by id — so any text will do. */
export async function renameTrack(id: string) {
  const t = store.tracks.find((x) => x.id === id);
  if (!t) return;
  const name = await askText({ title: "Rename track", label: "Name", value: t.name, ok: "Rename" });
  // Found again: the dialog was up for as long as the person was typing.
  const ti = store.tracks.findIndex((x) => x.id === id);
  if (name === null || ti < 0 || name === store.tracks[ti].name) return;
  await store.patch([{ op: "replace", path: `${store.seqPath}/tracks/${ti}/name`, value: name }], `rename track ${id}`);
}
