// Two panels, two different things.
//
// The **bin** is what this project can place on a track: media files, configured overlays
// and colours, all in one list. The **library** is where you shop — overlay templates on
// disk, shared across projects. Adding one from the library mints a bin entry.

import { api } from "./api";
import { button, el } from "./dom";
import { store, toast } from "./store";
import { addFromLibrary, placeBinItem } from "./timeline";
import type { BinItem, Project } from "./types";
import { allClips, isFileSource, isOverlaySource, isSequenceSource, itemKind, seqDuration } from "./types";

export class Library {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.templates, s.sequence], () => this.render());
  }

  private render() {
    const s = store.state;
    const p = s.project;
    this.host.innerHTML = "";
    if (!p) return;

    const bin = el("h3", "", "Bin");
    const tools = el("span");
    tools.style.float = "right";
    tools.append(button("thumbs", async () => { const r = await api.thumbs(); toast(`Thumbnail job #${r.job}`, "ok"); }, "Render thumbnails for templates and overlay entries"));
    bin.appendChild(tools);
    this.host.appendChild(bin);

    const entries = Object.entries(p.bin);
    if (!entries.length) this.host.appendChild(el("p", "muted", "Empty. Add media below, or an overlay from the library."));
    for (const [id, item] of entries) this.host.appendChild(binCard(p, id, item));

    const addRow = el("div", "lib-add");
    const input = el("input");
    input.placeholder = "media/clip.mp4";
    const add = button("Add", async () => {
      if (!input.value) return;
      try {
        const r = await api.addMedia(input.value, p.revision);
        toast(`Added ${r.id} to the bin`, "ok");
        input.value = "";
        await store.refresh();
      } catch (e) {
        toast((e as Error).message, "err");
      }
    });
    addRow.append(input, add);
    this.host.appendChild(addRow);

    const lib = el("h3", "", "Template library");
    const reload = el("span");
    reload.style.float = "right";
    reload.appendChild(button("⟳", async () => { store.set({ templates: await api.reloadTemplates() }); }, "Rescan the template libraries"));
    lib.appendChild(reload);
    this.host.appendChild(lib);
    for (const t of s.templates) {
      const card = card2(t.manifest.name || t.id, "overlay", [
        ["desc", t.manifest.description],
        ["desc mono", Object.keys(t.manifest.vars).join(" · ")],
      ], { template: t.id });
      if (t.has_thumb) prependThumb(card, `/overlays/${encodeURIComponent(t.id)}/thumb.png`);
      card.appendChild(actionRow(button("Add to bin", () => addFromLibrary(t.id, store.state.playhead))));
      this.host.appendChild(card);
    }
  }
}

function binCard(p: Project, id: string, item: BinItem): HTMLDivElement {
  const kind = itemKind(item);
  const lines: [string, string][] = [];
  if (isFileSource(item.source)) {
    const m = item.meta;
    lines.push(["desc mono", item.source.path]);
    lines.push(["desc", [m?.duration ? `${m.duration.toFixed(1)}s` : "", m?.width ? `${m.width}×${m.height}` : "", m?.has_audio ? "audio" : ""].filter(Boolean).join(" · ")]);
  } else if (isOverlaySource(item.source)) {
    lines.push(["desc mono", item.source.template]);
    const vars = Object.entries(item.source.vars ?? {}).map(([k, v]) => `${k}: ${v}`).join(" · ");
    lines.push(["desc", vars]);
  } else if (isSequenceSource(item.source)) {
    const n = item.source.tracks.length;
    const clips = item.source.tracks.reduce((a, t) => a + t.clips.length, 0);
    lines.push(["desc", [`${seqDuration(p, id).toFixed(1)}s`, `${n} track${n === 1 ? "" : "s"}`, `${clips} clip${clips === 1 ? "" : "s"}`].join(" · ")]);
    if (id === p.master) lines.push(["desc mono", "master — this is what renders"]);
  } else {
    lines.push(["desc mono", item.source.color]);
  }
  const card = card2(item.name || id, kind, lines, { bin: id });
  if (id === store.state.sequence) card.classList.add("open");
  // Overlay entries get a thumbnail of their own configuration once `thumbs` has run.
  if (isOverlaySource(item.source)) prependThumb(card, `/api/bin/${encodeURIComponent(id)}/thumb.png`);
  const del = button("×", async () => {
    // Usage is counted across every sequence, not just the one on screen: an entry can be
    // placed on a timeline you are not looking at.
    const used = allClips(p).filter(({ clip }) => clip.bin === id).length;
    if (used) { toast(`${id} is used by ${used} clip${used > 1 ? "s" : ""}`, "err"); return; }
    if (id === p.master) { toast("The master sequence cannot be removed", "err"); return; }
    await store.patch([{ op: "remove", path: `/bin/${id}` }], `remove ${id}`);
  }, "Remove from the bin");
  const actions = [];
  // Opening a sequence is what "edit this part separately" means; placing it puts the
  // whole thing on the timeline you are looking at.
  if (isSequenceSource(item.source)) {
    const open = button(id === store.state.sequence ? "Open ✓" : "Open", () => store.openSequence(id), "Edit this timeline");
    (open as HTMLButtonElement).disabled = id === store.state.sequence;
    actions.push(open);
  }
  // A sequence cannot be placed on itself, directly or otherwise — the validator would
  // refuse the patch, so do not offer it.
  const wouldRecurse = isSequenceSource(item.source) && (id === store.state.sequence || reaches(p, id, store.state.sequence));
  if (!wouldRecurse) actions.push(button("Add at playhead", () => placeBinItem(id, store.state.playhead)));
  actions.push(del);
  card.appendChild(actionRow(...actions));
  return card;
}

/** Does sequence `from` contain `target`, at any depth? Used to hide the placements that
 *  would make a sequence contain itself. Cycle-safe, though a valid project has none. */
function reaches(p: Project, from: string, target: string, seen = new Set<string>()): boolean {
  const src = p.bin[from]?.source;
  if (!src || !isSequenceSource(src) || !seen.add(from)) return false;
  return src.tracks.some((t) => t.clips.some((c) => c.bin === target || reaches(p, c.bin, target, seen)));
}

/** One draggable card. The payload is what a drop on the timeline receives. */
function card2(title: string, kind: string, lines: [string, string][], payload: { bin?: string; template?: string }): HTMLDivElement {
  const item = el("div", "lib-item");
  item.draggable = true;
  const head = el("div", "title", title);
  head.appendChild(el("span", `badge-kind ${kind}`, kind));
  item.appendChild(head);
  for (const [cls, line] of lines) if (line) item.appendChild(el("div", cls, line));
  item.ondragstart = (e) => e.dataTransfer?.setData("application/x-kuma", JSON.stringify(payload));
  return item;
}

function actionRow(...buttons: HTMLElement[]): HTMLDivElement {
  const row = el("div", "actions");
  row.append(...buttons);
  return row;
}

/** A thumbnail that removes itself if the file is not there yet. */
function prependThumb(card: HTMLDivElement, src: string) {
  const img = el("img");
  img.src = src;
  img.alt = "";
  img.onerror = () => img.remove();
  card.prepend(img);
}
