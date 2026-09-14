// Library: overlay templates and project assets. Drag onto the timeline or click to add
// at the playhead.

import { api } from "./api";
import { button, el } from "./dom";
import { store, toast } from "./store";
import { addItem } from "./timeline";

export class Library {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.templates], () => this.render());
  }

  private render() {
    const s = store.state;
    const p = s.project;
    this.host.innerHTML = "";
    if (!p) return;

    const h = el("h3", "", "Templates");
    const tools = el("span");
    tools.style.float = "right";
    tools.append(
      button("thumbs", async () => { const r = await api.thumbs(); toast(`Thumbnail job #${r.job}`, "ok"); }, "Generate thumbnails for all templates"),
      button("⟳", async () => { store.set({ templates: await api.reloadTemplates() }); }, "Reload templates from disk"),
    );
    h.appendChild(tools);
    this.host.appendChild(h);
    for (const t of s.templates) {
      const item = libItem(t.manifest.name || t.id, [t.manifest.description, Object.keys(t.manifest.vars).join(" · ")], { template: t.id });
      if (t.has_thumb) {
        const img = el("img");
        img.src = `/overlays/${encodeURIComponent(t.id)}/thumb.png`;
        img.alt = "";
        item.prepend(img);
      }
      this.host.appendChild(item);
    }

    this.host.appendChild(el("h3", "", "Assets"));
    for (const [id, a] of Object.entries(p.assets)) {
      const m = a.meta;
      const facts = [a.kind, m?.duration ? `${m.duration.toFixed(1)}s` : "", m?.width ? `${m.width}×${m.height}` : "", m?.has_audio ? "audio" : ""].filter(Boolean).join(" · ");
      this.host.appendChild(libItem(id, [a.path, facts], { asset: id }));
    }
    const addRow = el("div", "lib-add");
    const input = el("input");
    input.placeholder = "media/clip.mp4";
    const btn = button("Add", async () => {
      if (!input.value) return;
      try {
        const r = await api.addAsset(input.value, p.revision);
        toast(`Added asset ${r.id}`, "ok");
        input.value = "";
        await store.refresh();
      } catch (e) {
        toast((e as Error).message, "err");
      }
    });
    addRow.append(input, btn);
    this.host.appendChild(addRow);
  }
}

/** One draggable library entry. Reads the playhead at click time, not at render time. */
function libItem(title: string, lines: string[], payload: { template?: string; asset?: string }): HTMLDivElement {
  const item = el("div", "lib-item");
  item.draggable = true;
  item.appendChild(el("div", "title", title));
  for (const [i, line] of lines.entries()) if (line) item.appendChild(el("div", i === 1 && payload.template ? "desc mono" : "desc", line));
  item.ondragstart = (e) => e.dataTransfer?.setData("application/x-kuma", JSON.stringify(payload));
  const actions = el("div", "actions");
  actions.appendChild(button("Add at playhead", () => addItem(payload, store.state.playhead)));
  item.appendChild(actions);
  return item;
}
