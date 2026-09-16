// The left panel, in two tabs.
//
// **Bin** is what this project can place on a track: media files, configured overlays,
// colours and sequences, one row each. **Templates** is where you shop — overlay
// templates on disk, shared across projects; adding one mints a bin entry.
//
// They are tabs rather than two stacked lists because they answer different questions
// ("what is in this video" against "what could be"), and stacked, the second one pushed
// the first out of sight as soon as a project had more than a few entries.

import { api } from "../api";
import { desktop, pickAndAddMedia } from "../desktop";
import { button, el } from "../dom";
import { store, toast } from "../store";
import { addFromLibrary } from "../edits";
import type { BinItem, Project } from "../types";
import { isSequenceSource, itemKind } from "../types";

export class Library {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.templates, s.sequence], () => this.render());
  }

  /** Which tab is showing. View state, so it lives here rather than in the store. */
  private tab: "bin" | "templates" = "bin";

  private render() {
    const s = store.state;
    const p = s.project;
    this.host.innerHTML = "";
    if (!p) return;

    const tabs = el("div", "tabs");
    for (const [id, label] of [["bin", "Bin"], ["templates", "Templates"]] as const) {
      const b = button(label, () => { this.tab = id; this.render(); });
      b.className = `tab${this.tab === id ? " on" : ""}`;
      tabs.appendChild(b);
    }
    this.host.appendChild(tabs);
    if (this.tab === "bin") this.renderBin(p);
    else this.renderTemplates();
  }

  private renderBin(p: Project) {
    const entries = Object.entries(p.bin);
    if (!entries.length) this.host.appendChild(el("p", "muted", "Empty. Add media below, or an overlay from the Templates tab."));
    const list = el("div", "bin-list");
    for (const [id, item] of entries) list.appendChild(binRow(p, id, item));
    this.host.appendChild(list);

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
      const browseRow = el("div", "lib-add");
      browseRow.appendChild(browse);
      this.host.appendChild(browseRow);
    }
    const addRow = el("div", "lib-add");
    addRow.append(input, add);
    this.host.appendChild(addRow);
  }

  private renderTemplates() {
    const tools = el("div", "lib-add");
    tools.append(
      button("⟳ rescan", async () => { store.set({ templates: await api.reloadTemplates() }); }, "Rescan the template libraries"),
      // Thumbnails are what this tab is for looking at, so the job that makes them lives
      // here rather than over the bin, where it had nothing to do with anything on screen.
      button("thumbnails", async () => { const r = await api.thumbs(); toast(`Thumbnail job #${r.job}`, "ok"); }, "Render thumbnails for every template and overlay entry"),
    );
    this.host.appendChild(tools);

    const templates = store.state.templates;
    if (!templates.length) {
      this.host.appendChild(el("p", "muted", "No templates found. Run `kuma library` to install the starter set, or point KUMA_LIBRARY at a folder of them."));
      return;
    }
    for (const t of templates) {
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

/**
 * One bin entry as a single row: name and kind, nothing else.
 *
 * Everything that used to be on the card — the path, the vars, how long a sequence is,
 * the actions — is the entry's own detail, and detail belongs in the inspector now that
 * selecting an entry opens it there. A bin of twenty things should read as a list.
 */
function binRow(p: Project, id: string, item: BinItem): HTMLDivElement {
  const kind = itemKind(item);
  const row = el("div", `bin-row${store.state.selection?.kind === "bin" && store.state.selection.id === id ? " selected" : ""}`);
  if (id === store.state.sequence) row.classList.add("open");
  row.draggable = true;
  row.title = item.name || id;
  const name = el("span", "bin-name", item.name || id);
  row.append(name, el("span", `badge-kind ${kind}`, kind));
  row.onclick = () => store.selectItem("bin", id);
  // A sequence is a timeline; opening it is what double-clicking one means everywhere.
  if (isSequenceSource(item.source)) row.ondblclick = () => store.openSequence(id);
  row.ondragstart = (e) => e.dataTransfer?.setData("application/x-kuma", JSON.stringify({ bin: id }));
  return row;
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
