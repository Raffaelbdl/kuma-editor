// Two panels, two different things.
//
// The **bin** is what this project can place on a track: media files, configured overlays
// and colours, all in one list. The **library** is where you shop — overlay templates on
// disk, shared across projects. Adding one from the library mints a bin entry.

import { api } from "./api";
import { desktop, pickAndAddMedia } from "./desktop";
import { button, el } from "./dom";
import { store, toast } from "./store";
import { addFromLibrary } from "./timeline";
import type { BinItem, Project } from "./types";
import { isSequenceSource, itemKind } from "./types";

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
    const list = el("div", "bin-list");
    for (const [id, item] of entries) list.appendChild(binRow(p, id, item));
    this.host.appendChild(list);

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
