// Project edits that are no one panel's business: placing, splitting, adding a track,
// minting a sequence. Each is asked for from several places, so each lives here as one
// revision-checked patch with its rule checked once — and no panel has to import another
// to place a clip.

import { api } from "./api";
import { checkbox, el, plural, selectInput } from "./dom";
import { askForm, askText } from "./modal";
import { startJob, store, toast } from "./store";
import type { ClipboardEntry } from "./store";
import type { BinItem, BinSource, Clip, PatchOp, Project, Track, TrackKind, Transition } from "./types";
import { binItem, binPath, childrenOf, hasAudio, isFileSource, isOverlaySource, isTimedMedia, itemKind, orientation, parentOf, seqDuration, seqPath, shiftEffect, trackFor, trimEffect, wouldNest, wouldRecurse } from "./types";

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
  // One clip's worth of audio: the last one clicked, whatever else is selected with it.
  const clip = store.findClip(store.selectedClip)?.clip;
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
 * Split every selected clip the playhead crosses. Time-relative effects follow the cut:
 * the left half keeps its fade-in, the right half its fade-out, and keyframes are
 * re-based on the new start. A transition leaving a clip now leaves its right half.
 *
 * One patch, whatever it cuts: three clips selected across three tracks is one cut of the
 * timeline, and half of it applying is not an outcome anyone wants.
 */
export async function splitAtPlayhead() {
  const t = store.state.playhead;
  const cutting = store.selectedClips.map((id) => store.findClip(id)).filter((f) => f && t > f.clip.start && t < f.clip.end) as NonNullable<ReturnType<typeof store.findClip>>[];
  if (!cutting.length) { toast("Select a clip under the playhead to split", ""); return; }
  const ops: PatchOp[] = [];
  const minted: string[] = [];
  // One entry per track being cut: two clips on one track must not each rewrite the
  // track's transitions from the version they were handed.
  const transitions = new Map<string, Transition[]>();
  for (const f of cutting) {
    const c = f.clip;
    const cut = t - c.start;
    const left: Clip = { ...c, end: t, effects: c.effects.map((e) => trimEffect(e, cut)) };
    const right: Clip = { ...c, id: store.newId(c.id + "_", minted), start: t, effects: c.effects.map((e) => shiftEffect(e, cut)) };
    minted.push(right.id);
    // Both halves keep playing the same bin entry; only the right one moves its in point.
    if (isTimedMedia(binItem(store.state.project!, c))) right.in = (c.in ?? 0) + cut * Math.abs(c.speed);
    const track = `${seqPath(f.seq)}/tracks/${f.ti}`;
    // A replace leaves every index alone and an append takes the next one, so the paths
    // computed here stay right however many clips this patch cuts.
    ops.push({ op: "replace", path: `${track}/clips/${f.ci}`, value: left }, { op: "add", path: `${track}/clips/-`, value: right });
    const xs = (transitions.get(`${track}/transitions`) ?? f.track.transitions).map((x) => (x.from === c.id ? { ...x, from: right.id } : x));
    if (f.track.transitions.some((x) => x.from === c.id)) transitions.set(`${track}/transitions`, xs);
  }
  for (const [path, value] of transitions) ops.push({ op: "replace", path, value });
  await store.patch(ops, `split ${cutting.map((f) => f.clip.id).join(", ")}`);
}

// ---- the timeline clipboard --------------------------------------------------------
//
// A copy is not an edit: nothing about it belongs in the document, so the clipboard is
// state in the UI (see `store.ts`) and costs no revision and no undo step. The paste is
// the edit, and it is one patch like every other.
//
// What travels is the **placement** — when it plays, how fast, how loud, its label, its
// effects — never the bin entry. Two clips of the same entry play the same thing, which
// is the model's own split, so a paste adds clips and nothing else. A transition is a
// relation between two clips on one track, so it stays behind with the pair it names.

/** Copy the selection. Deeply, because the clipboard has to outlive the project object it
 *  came from: every refresh replaces that, and a cut removes the clips outright. The
 *  track each clip came off travels with it — see `pasteClipboard`. */
export function copySelection(): boolean {
  const entries: ClipboardEntry[] = [];
  for (const id of store.selectedClips) {
    const f = store.findClip(id);
    if (f) entries.push({ clip: structuredClone(f.clip), track: f.track.id, kind: f.track.kind });
  }
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
 * **One clip** needs a *where* as well as a *when* — the playhead answers the second half,
 * and clicking a track answers the first. Any track of the kind it was copied from will
 * take it, in this sequence or another one, which is what makes "copy here, paste there"
 * one gesture rather than two features. With no track clicked it lands on the first of
 * that kind, the same rule a drop from the bin follows.
 *
 * **Several clips** are a shape, not a position: they keep their spacing and they keep
 * their tracks, so the clicked track does not come into it — there is no one track a
 * group belongs on. Each track they came off is matched by id in the sequence being
 * pasted into, else by kind, and a track is added when there is nothing to match; two of
 * them never collapse onto one, because that is the one way the shape could be lost
 * silently.
 *
 * Where the clips would land on something the whole group moves to the **first free slot
 * after it** that fits: a paste names a position, so an occupied one is answered with the
 * next one that is free rather than by refusing, or by silently landing at the end. That
 * also makes pasting twice in a row lay two copies back to back.
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

  // The tracks as this patch will leave them. Tracks are resolved as objects rather than
  // as indices because adding one renumbers the others; the indices are read off at the
  // end, when nothing more will move.
  const sim = store.tracks.map((t) => ({ id: t.id, kind: t.kind, clips: t.clips }));
  const ops: PatchOp[] = [];
  const minted: string[] = [];
  const claimed = new Map<string, typeof sim[number]>();
  const single = entries.length === 1;

  const targets = entries.map((entry) => {
    // One clip goes where you clicked, else back where it came from; several keep the
    // tracks they were copied from, and the click does not come into it.
    const wanted = single ? [store.state.track, entry.track] : [entry.track];
    let track = claimed.get(entry.track);
    if (!track) {
      // A track already taken by another clip of this paste is not a candidate: two of
      // them collapsing onto one track is the one way the shape is lost silently.
      const free = (t: typeof sim[number]) => t.kind === entry.kind && ![...claimed.values()].includes(t);
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
    }
    return { entry, track };
  });

  // One offset for the whole group, so the shape survives: push it past whatever any of
  // its clips lands on, and look again — clearing one clip can walk into the next.
  const anchor = Math.min(...entries.map((e) => e.clip.start));
  const at = store.snap(store.state.playhead);
  let dt = at - anchor;
  const room = sim.reduce((n, t) => n + t.clips.length, 0) + 2;
  for (let guard = 0; guard < room; guard++) {
    let push = 0;
    for (const { entry, track } of targets) {
      const start = entry.clip.start + dt, end = entry.clip.end + dt;
      for (const c of track.clips) if (start < c.end && end > c.start) push = Math.max(push, c.end - start);
    }
    if (push <= 0) break;
    dt += push;
  }

  const pasted: Clip[] = [];
  for (const { entry, track } of targets) {
    const clip: Clip = { ...structuredClone(entry.clip), id: store.newId(entry.kind === "overlay" ? "o" : "c", pasted.map((c) => c.id)), start: entry.clip.start + dt, end: entry.clip.end + dt };
    pasted.push(clip);
    ops.push({ op: "add", path: `${store.seqPath}/tracks/${sim.indexOf(track)}/clips/-`, value: clip });
  }
  if (!(await store.patch(ops, `paste ${pasted.map((c) => c.id).join(", ")}`))) return;
  // One update: what was pasted is what the inspector shows, and — for a single clip — the
  // track it landed on is where the next paste goes, including one just created for it.
  store.set({ selection: { kind: "clip", ids: pasted.map((c) => c.id) }, track: single ? targets[0].track.id : store.state.track });
  if (dt > at - anchor) toast(`The playhead was inside a clip — pasted at ${(anchor + dt).toFixed(2)}s`, "", 2500);
}

/** What to call a clip in a message: its label, else the entry it plays, else its id. */
const clipName = (c: Clip): string => c.label || store.state.project?.bin[c.bin]?.name || c.bin || c.id;

/**
 * Remove every selected clip, in one patch. Only clips: removing a bin entry throws away
 * what every placement of it plays, so that stays a deliberate button in the inspector.
 */
export async function deleteSelectedClips() {
  const found = store.selectedClips.map((id) => store.findClip(id)).filter(Boolean) as NonNullable<ReturnType<typeof store.findClip>>[];
  if (!found.length) return;
  const removed = new Set(found.map((f) => f.clip.id));
  const ops: PatchOp[] = [];
  // A transition names two clips, and one naming a clip that has gone is refused by the
  // validator — so a transition leaves with either of the clips it joins.
  const tracks = new Map(found.map((f) => [`${seqPath(f.seq)}/tracks/${f.ti}`, f.track]));
  for (const [path, track] of tracks) {
    const keep = track.transitions.filter((x) => !removed.has(x.from) && !removed.has(x.to));
    if (keep.length !== track.transitions.length) ops.push({ op: "replace", path: `${path}/transitions`, value: keep });
  }
  // Highest index first: removing a clip renumbers every clip after it on its track.
  for (const f of [...found].sort((a, b) => b.ci - a.ci)) ops.push({ op: "remove", path: `${seqPath(f.seq)}/tracks/${f.ti}/clips/${f.ci}` });
  await store.patch(ops, `delete ${[...removed].join(", ")}`);
}
