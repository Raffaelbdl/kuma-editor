// The left panel, in two tabs.
//
// **Bin** is what this project can place on a track: media files, configured overlays,
// colours, sequences and folders, one row each, as a tree by `parent`. **Blueprints** is
// where you shop — reusable overlays in the user's library; adding one copies it into
// this project.
//
// They are tabs rather than two stacked lists because they answer different questions
// ("what is in this video" against "what could be"), and stacked, the second one pushed
// the first out of sight as soon as a project had more than a few entries.

import { api } from "../api";
import { addMediaPaths, desktop, dropMedia, pickAndAddMedia } from "../desktop";
import { button, dragPayload, el, isFileDrag, row, setDragPayload } from "../dom";
import { icon, type IconName } from "../icons";
import { askText } from "../modal";
import { store } from "../store";
import { blueprintActions, moveBinItem, newItems, promptNewFolder } from "../edits";
import { menuBelow, showMenu } from "../menu";
import type { BinItem, Project } from "../types";
import { childrenMap, isFolderSource, isOverlaySource, isSequenceSource, itemKind, parentOf } from "../types";

export class Library {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.blueprints, s.sequence, s.tab], () => this.render());
    // Selection moves a class; it does not rebuild the panel.
    store.select((s) => [s.selection], () => this.markSelected());
    this.acceptFiles();
    this.host.oncontextmenu = (e) => this.newMenu(e);
  }

  /** Right-click makes something new where the pointer is: inside a folder row, beside any
   *  other row, at the top level below them. The bin's, so not over the Blueprints tab. */
  private newMenu(e: MouseEvent) {
    const p = store.state.project;
    if (!p || this.tab !== "bin" || (e.target as HTMLElement).closest("input, textarea, select")) return;
    e.preventDefault();
    const id = (e.target as HTMLElement).closest<HTMLElement>(".bin-row")?.dataset.sel?.replace(/^bin:/, "");
    const parent = id ? (isFolderSource(p.bin[id]?.source) ? id : parentOf(p, id)) : null;
    const where = parent ? p.bin[parent]?.name || parent : null;
    showMenu([{ heading: where ? `New in ${where}` : "New" }, ...newItems(parent)], e.clientX, e.clientY);
  }

  /** Files from the desktop, dropped anywhere in the window, join the bin at the top level
   *  unless a folder row claimed them (`defaultPrevented`). While one is over the window the
   *  bin is framed — grey until the drag says it carries files, since it may land empty. */
  private acceptFiles() {
    const label = el("span");
    const hint = el("div", "bin-drop-hint");
    hint.append(label);
    // Enters and leaves are counted for every drag, in the capture phase, and hiding waits
    // for a `dragover` that does not come: a drag from the desktop can leave while inside.
    let depth = 0;
    let pending = 0;
    const hide = () => { depth = 0; clearTimeout(pending); hint.remove(); };
    window.addEventListener("dragenter", () => { depth++; }, true);
    window.addEventListener("dragleave", () => {
      if (--depth <= 0) { clearTimeout(pending); pending = window.setTimeout(hide, 150); }
    }, true);
    window.addEventListener("dragend", hide, true);
    window.addEventListener("drop", hide, true);
    window.addEventListener("dragover", (e) => {
      if (!isFileDrag(e)) return;
      e.preventDefault();
      clearTimeout(pending);
      const ready = e.dataTransfer!.types.includes("Files");
      hint.classList.toggle("waiting", !ready);
      label.textContent = !desktop() ? "A browser cannot add dropped files — use the desktop app"
        : ready ? "Drop to add to the bin · onto a folder to put it there" : "Waiting for the file manager to hand the files over…";
      if (hint.isConnected) return;
      const r = this.host.getBoundingClientRect();
      Object.assign(hint.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
      document.body.appendChild(hint);
    });
    window.addEventListener("drop", (e) => {
      if (!isFileDrag(e) || e.defaultPrevented) return;
      e.preventDefault();
      if (this.tab !== "bin") { this.tab = "bin"; this.render(); }
      void dropMedia(e.dataTransfer!.files, null);
    });
  }

  /** Which tab is showing, and which entries are folded. View state, so it lives here
   *  rather than in the store. */
  private tab: "bin" | "blueprints" = "bin";
  private collapsed = new Set<string>();

  /** The list, which scrolls under the tabs and the toolbar. Kept across a rebuild with
   *  where it was scrolled to, or every agent edit would throw you back to the top. */
  private body: HTMLDivElement = el("div", "lib-body");

  private render() {
    const s = store.state;
    const p = s.project;
    const scrolled = this.tab === this.body.dataset.tab ? this.body.scrollTop : 0;
    this.host.replaceChildren();
    this.body.replaceChildren();
    if (!p) return;

    // The tabs and what you can do in the one showing, fixed above the list: the tools
    // used to be buttons under the last entry, and scrolled away with it.
    const tabs = el("div", "tabs");
    for (const [id, label] of [["bin", "Bin"], ["blueprints", "Blueprints"]] as const) {
      tabs.appendChild(button(label, () => { this.tab = id; this.render(); }, "", `tab${this.tab === id ? " on" : ""}`));
    }
    const tools = el("div", "lib-tools");
    this.host.append(row("lib-head", tabs, tools), this.body);
    this.body.dataset.tab = this.tab;
    if (this.tab === "bin") this.renderBin(p, tools);
    else this.renderBlueprints(tools);
    this.body.scrollTop = scrolled;
    this.markSelected();
  }

  private markSelected() {
    const sel = store.state.selection;
    const key = sel && (sel.kind === "bin" || sel.kind === "blueprint") ? `${sel.kind}:${sel.id}` : "";
    for (const n of this.host.querySelectorAll<HTMLElement>("[data-sel]")) n.classList.toggle("selected", n.dataset.sel === key);
  }

  private renderBin(p: Project, tools: HTMLElement) {
    const create = tool("plus", "New — a title, an overlay from a blueprint, a sequence or a folder (or right-click in the bin)", () => menuBelow(create, newItems(null)));
    tools.append(create, tool("folder-plus", "New folder", () => void promptNewFolder(null)));
    // In the desktop shell the native picker is the obvious way in; the typed path is for
    // both, because it is the only way to reference a file outside the project.
    if (desktop()) tools.appendChild(tool("import", "Import media files…", () => void pickAndAddMedia()));
    tools.appendChild(tool("path", "Add media by path (relative to the project, or absolute)…", () => void addByPath()));

    if (!Object.keys(p.bin).length) this.body.appendChild(el("p", "muted", "Empty. Add media or a title with the buttons above, or drop files here."));
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
    dropTarget(list, undefined, () => ({ parent: null, before: null }));
    this.body.appendChild(list);
  }

  private renderBlueprints(tools: HTMLElement) {
    tools.appendChild(tool("refresh", "Rescan the blueprint libraries", async () => { store.set({ blueprints: await api.reloadBlueprints() }); }));

    const blueprints = store.state.blueprints;
    if (!blueprints.length) {
      this.body.appendChild(el("p", "muted", "No blueprints found. Run `kuma library` to install the starter set, point KUMA_LIBRARY at a folder of them, or save an overlay from the bin as one."));
      return;
    }
    // Name, kind and the two ways in; clicking shows the details in the inspector.
    for (const t of blueprints) {
      const card = blueprintCard(t.manifest.name || t.id, t.id);
      card.appendChild(blueprintActions(t.id));
      this.body.appendChild(card);
    }
  }
}

/** One button of the toolbar: an icon, and what it does in its tooltip. */
function tool(name: IconName, title: string, run: () => void): HTMLButtonElement {
  return button(icon(name, 15), run, title, "icon tool");
}

/** Add a media file by its path, as typed. */
async function addByPath() {
  const path = await askText({ title: "Add media by path", label: "Path", placeholder: "media/clip.mp4", ok: "Add" });
  if (path) await addMediaPaths([path], null);
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
  const entry = el("div", "bin-row");
  entry.dataset.sel = `bin:${id}`;
  // The row of whatever the centre is showing, timeline or workbench, is marked open.
  if (id === store.state.sequence || id === store.state.tab) entry.classList.add("open");
  entry.draggable = true;
  entry.title = item.name || id;
  // Every row has the caret slot, so names line up whether or not there is one to show.
  entry.append(caret ?? el("span", "caret"), el("span", "bin-name", item.name || id), el("span", `badge-kind ${kind}`, kind));
  entry.onclick = () => store.selectItem("bin", id);
  // Double-clicking opens the thing in its own editor, whatever kind of thing it is: a
  // sequence is a timeline, an overlay is a composition on the workbench.
  if (isSequenceSource(item.source)) entry.ondblclick = () => store.openSequence(id);
  if (isOverlaySource(item.source)) entry.ondblclick = () => store.openOverlay(id);
  entry.ondragstart = (e) => setDragPayload(e, { bin: id });
  // A drop on the row's upper or lower part reorders around it; the middle of a folder
  // row drops into the folder. Any entry can be a parent, but only a folder is offered
  // as a drop target: dropping media on media is more often a slip.
  const folder = isFolderSource(item.source);
  dropTarget(entry, folder ? id : undefined, (e) => {
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
 *  lives in `moveBinItem`; this only stops the drop reaching an enclosing target. Files
 *  from the desktop are taken only where there is a `folder` to add them to, and are
 *  claimed rather than stopped, so the window's own handler knows to leave them. */
function dropTarget(node: HTMLElement, folder: string | undefined, spot: (e: DragEvent) => DropSpot) {
  const clear = () => node.classList.remove("drop", "drop-before", "drop-after");
  node.ondragover = (e) => {
    const files = isFileDrag(e);
    if (files && folder === undefined) return;
    e.preventDefault();
    if (!files) e.stopPropagation();
    clear();
    node.classList.add(files ? "drop" : { before: "drop-before", after: "drop-after", into: "drop" }[spot(e).zone ?? "into"]);
  };
  node.ondragleave = clear;
  node.ondrop = (e) => {
    const files = isFileDrag(e);
    if (files && folder === undefined) return;
    e.preventDefault();
    clear();
    if (files) { void dropMedia(e.dataTransfer!.files, folder!); return; }
    e.stopPropagation();
    const from = dragPayload(e)?.bin;
    const { parent, before } = spot(e);
    if (from && from !== parent && from !== before) void moveBinItem(from, parent, before);
  };
}

/** One draggable blueprint card. The payload is what a drop on the timeline receives. */
function blueprintCard(title: string, blueprint: string): HTMLDivElement {
  const item = el("div", "lib-item");
  item.dataset.sel = `blueprint:${blueprint}`;
  item.draggable = true;
  item.onclick = (e) => { if (!(e.target as HTMLElement).closest("button")) store.selectItem("blueprint", blueprint); };
  const head = el("div", "title", title);
  head.appendChild(el("span", "badge-kind overlay", "overlay"));
  item.appendChild(head);
  item.ondragstart = (e) => setDragPayload(e, { blueprint });
  return item;
}
