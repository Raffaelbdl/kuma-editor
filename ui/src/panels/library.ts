// The left panel, in two tabs.
//
// **Bin** is what this project can place on a track: media files, configured overlays,
// colours, sequences and folders, one row each, as a tree by `parent`. **Templates** is
// where you shop — overlay templates on disk, shared across projects; adding one mints a
// bin entry.
//
// They are tabs rather than two stacked lists because they answer different questions
// ("what is in this video" against "what could be"), and stacked, the second one pushed
// the first out of sight as soon as a project had more than a few entries.

import { api } from "../api";
import { desktop, pickAndAddMedia } from "../desktop";
import { button, dragPayload, el, row, setDragPayload } from "../dom";
import { startJob, store, toast } from "../store";
import { addFromLibrary, moveBinItem, promptNewFolder } from "../edits";
import type { BinItem, Project } from "../types";
import { childrenMap, isFolderSource, isSequenceSource, itemKind, parentOf } from "../types";

export class Library {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.templates, s.sequence], () => this.render());
  }

  /** Which tab is showing, and which entries are folded. View state, so it lives here
   *  rather than in the store. */
  private tab: "bin" | "templates" = "bin";
  private collapsed = new Set<string>();

  private render() {
    const s = store.state;
    const p = s.project;
    this.host.innerHTML = "";
    if (!p) return;

    const tabs = el("div", "tabs");
    for (const [id, label] of [["bin", "Bin"], ["templates", "Templates"]] as const) {
      tabs.appendChild(button(label, () => { this.tab = id; this.render(); }, "", `tab${this.tab === id ? " on" : ""}`));
    }
    this.host.appendChild(tabs);
    if (this.tab === "bin") this.renderBin(p);
    else this.renderTemplates();
  }

  private renderBin(p: Project) {
    if (!Object.keys(p.bin).length) this.host.appendChild(el("p", "muted", "Empty. Add media below, or an overlay from the Templates tab."));
    const list = el("div", "bin-list");
    // One grouping pass, then a walk: an entry's children (a transcript under its media,
    // a folder's contents) sit under it, indented, behind a caret. Cycle-safe because
    // the validator refuses a ring.
    const kids = childrenMap(p);
    const walk = (parent: string | null, depth: number) => {
      for (const [id, item] of kids.get(parent) ?? []) {
        const folded = this.collapsed.has(id);
        let caret: HTMLElement | undefined;
        if (isFolderSource(item.source) || kids.has(id)) {
          caret = button(folded ? "▸" : "▾", () => { folded ? this.collapsed.delete(id) : this.collapsed.add(id); this.render(); }, folded ? "Expand" : "Collapse", "caret");
          caret.onpointerdown = (e) => e.stopPropagation();
        }
        const entry = binRow(p, id, item, caret, kids.get(parent) ?? []);
        entry.style.paddingLeft = `${6 + depth * 14}px`;
        list.appendChild(entry);
        if (!folded) walk(id, depth + 1);
      }
    };
    walk(null, 0);
    // Dropping a row on the list's empty space lists it last at the top level.
    dropTarget(list, () => ({ parent: null, before: null }));
    this.host.appendChild(list);
    this.host.appendChild(row("lib-add", button("+ folder", () => void promptNewFolder(), "Add a folder; drag entries onto it")));

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
    // In the desktop shell the native picker is the obvious way in, so it gets its own
    // full-width row; the typed path stays for both, because it is the only way to
    // reference a file outside the project.
    if (desktop()) {
      const browse = button("Browse…", () => void pickAndAddMedia(), "Add media files with the system file picker");
      browse.style.flex = "1";
      this.host.appendChild(row("lib-add", browse));
    }
    this.host.appendChild(row("lib-add", input, add));
  }

  private renderTemplates() {
    this.host.appendChild(row("lib-add",
      button("⟳ rescan", async () => { store.set({ templates: await api.reloadTemplates() }); }, "Rescan the template libraries"),
      // Thumbnails are what this tab is for looking at, so the job that makes them lives
      // here rather than over the bin, where it had nothing to do with anything on screen.
      button("thumbnails", () => startJob("Thumbnail", () => api.thumbs()), "Render thumbnails for every template and overlay entry"),
    ));

    const templates = store.state.templates;
    if (!templates.length) {
      this.host.appendChild(el("p", "muted", "No templates found. Run `kuma library` to install the starter set, or point KUMA_LIBRARY at a folder of them."));
      return;
    }
    for (const t of templates) {
      const card = templateCard(t.manifest.name || t.id, [
        ["desc", t.manifest.description],
        ["desc mono", Object.keys(t.manifest.vars).join(" · ")],
      ], t.id);
      if (t.has_thumb) prependThumb(card, `/overlays/${encodeURIComponent(t.id)}/thumb.png`);
      card.appendChild(row("actions", button("Add to bin", () => addFromLibrary(t.id, store.state.playhead))));
      this.host.appendChild(card);
    }
  }
}

/**
 * One bin entry as a single row: caret slot, name and kind, nothing else.
 *
 * Everything that used to be on the card — the path, the vars, how long a sequence is,
 * the actions — is the entry's own detail, and detail belongs in the inspector now that
 * selecting an entry opens it there. A bin of twenty things should read as a list.
 */
function binRow(p: Project, id: string, item: BinItem, caret: HTMLElement | undefined, siblings: [string, BinItem][]): HTMLDivElement {
  const kind = itemKind(item);
  const entry = el("div", `bin-row${store.state.selection?.kind === "bin" && store.state.selection.id === id ? " selected" : ""}`);
  if (id === store.state.sequence) entry.classList.add("open");
  entry.draggable = true;
  entry.title = item.name || id;
  // Every row has the caret slot, so names line up whether or not there is one to show.
  entry.append(caret ?? el("span", "caret"), el("span", "bin-name", item.name || id), el("span", `badge-kind ${kind}`, kind));
  entry.onclick = () => store.selectItem("bin", id);
  // A sequence is a timeline; opening it is what double-clicking one means everywhere.
  if (isSequenceSource(item.source)) entry.ondblclick = () => store.openSequence(id);
  entry.ondragstart = (e) => setDragPayload(e, { bin: id });
  // A drop on the row's upper or lower part reorders around it; the middle of a folder
  // row drops into the folder. Any entry can be a parent, but only a folder is offered
  // as a drop target: dropping media on media is more often a slip.
  const folder = isFolderSource(item.source);
  dropTarget(entry, (e) => {
    const box = entry.getBoundingClientRect();
    const y = (e.clientY - box.top) / box.height;
    if (folder && y > 0.25 && y < 0.75) return { parent: id, before: null, zone: "into" };
    const i = siblings.findIndex(([sid]) => sid === id);
    const before = y < 0.5 ? id : siblings[i + 1]?.[0] ?? null;
    return { parent: parentOf(p, id), before, zone: y < 0.5 ? "before" : "after" };
  });
  return entry;
}

/** Where a drop on `node` would put the dragged row. */
interface DropSpot { parent: string | null; before: string | null; zone?: "before" | "after" | "into" }

/** Make `node` accept a dragged bin row, showing which spot the pointer means. The rule
 *  lives in `moveBinItem`; this only stops the drop reaching an enclosing target. */
function dropTarget(node: HTMLElement, spot: (e: DragEvent) => DropSpot) {
  const clear = () => node.classList.remove("drop", "drop-before", "drop-after");
  node.ondragover = (e) => {
    e.preventDefault();
    e.stopPropagation();
    clear();
    node.classList.add({ before: "drop-before", after: "drop-after", into: "drop" }[spot(e).zone ?? "into"]);
  };
  node.ondragleave = clear;
  node.ondrop = (e) => {
    e.preventDefault();
    e.stopPropagation();
    clear();
    const from = dragPayload(e)?.bin;
    const { parent, before } = spot(e);
    if (from && from !== parent && from !== before) void moveBinItem(from, parent, before);
  };
}

/** One draggable template card. The payload is what a drop on the timeline receives. */
function templateCard(title: string, lines: [string, string][], template: string): HTMLDivElement {
  const item = el("div", "lib-item");
  item.draggable = true;
  const head = el("div", "title", title);
  head.appendChild(el("span", "badge-kind overlay", "overlay"));
  item.appendChild(head);
  for (const [cls, line] of lines) if (line) item.appendChild(el("div", cls, line));
  item.ondragstart = (e) => setDragPayload(e, { template });
  return item;
}

/** A thumbnail that removes itself if the file is not there yet. */
function prependThumb(card: HTMLDivElement, src: string) {
  const img = el("img");
  img.src = src;
  img.alt = "";
  img.onerror = () => img.remove();
  card.prepend(img);
}
