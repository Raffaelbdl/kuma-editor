// Library: overlay templates and project assets. Drag onto the timeline or click to add
// at the playhead.

import { api } from "./api";
import { store, toast } from "./store";
import { addItem } from "./timeline";

export class Library {
  constructor(private host: HTMLElement) {
    store.subscribe(() => this.render());
  }

  private render() {
    const s = store.state;
    const p = s.project;
    this.host.innerHTML = "";
    if (!p) return;

    const h = document.createElement("h3");
    h.textContent = "Templates";
    const tools = document.createElement("span");
    tools.style.float = "right";
    const thumbs = document.createElement("button");
    thumbs.textContent = "thumbs";
    thumbs.title = "Generate thumbnails for all templates";
    thumbs.onclick = async () => { const r = await api.thumbs(); toast(`Thumbnail job #${r.job}`, "ok"); };
    const reload = document.createElement("button");
    reload.textContent = "⟳";
    reload.title = "Reload templates from disk";
    reload.onclick = async () => { store.set({ templates: await api.reloadTemplates() }); };
    tools.append(thumbs, reload);
    h.appendChild(tools);
    this.host.appendChild(h);
    for (const t of s.templates) {
      const el = document.createElement("div");
      el.className = "lib-item";
      el.draggable = true;
      el.innerHTML = `${t.has_thumb ? `<img src="/overlays/${t.id}/thumb.png" alt="">` : ""}<div class="title">${t.manifest.name || t.id}</div><div class="desc">${t.manifest.description}</div>
        <div class="desc mono">${Object.keys(t.manifest.vars).join(" · ")}</div>`;
      el.ondragstart = (e) => e.dataTransfer?.setData("application/x-kuma", JSON.stringify({ template: t.id }));
      const actions = document.createElement("div");
      actions.className = "actions";
      const add = document.createElement("button");
      add.textContent = "Add at playhead";
      add.onclick = () => addItem({ template: t.id }, s.playhead);
      actions.appendChild(add);
      el.appendChild(actions);
      this.host.appendChild(el);
    }

    const h2 = document.createElement("h3");
    h2.textContent = "Assets";
    this.host.appendChild(h2);
    for (const [id, a] of Object.entries(p.assets)) {
      const el = document.createElement("div");
      el.className = "lib-item";
      el.draggable = true;
      const m = a.meta;
      el.innerHTML = `<div class="title">${id}</div><div class="desc mono">${a.path}</div><div class="desc">${a.kind}${m?.duration ? ` · ${m.duration.toFixed(1)}s` : ""}${m?.width ? ` · ${m.width}×${m.height}` : ""}${m?.has_audio ? " · audio" : ""}</div>`;
      el.ondragstart = (e) => e.dataTransfer?.setData("application/x-kuma", JSON.stringify({ asset: id }));
      const actions = document.createElement("div");
      actions.className = "actions";
      const add = document.createElement("button");
      add.textContent = "Add at playhead";
      add.onclick = () => addItem({ asset: id }, s.playhead);
      actions.appendChild(add);
      el.appendChild(actions);
      this.host.appendChild(el);
    }
    const addRow = document.createElement("div");
    addRow.className = "lib-add";
    const input = document.createElement("input");
    input.placeholder = "media/clip.mp4";
    const btn = document.createElement("button");
    btn.textContent = "Add";
    btn.onclick = async () => {
      if (!input.value) return;
      try {
        const r = await api.addAsset(input.value, p.revision);
        toast(`Added asset ${r.id}`, "ok");
        input.value = "";
        await store.refresh();
      } catch (e) {
        toast((e as Error).message, "err");
      }
    };
    addRow.append(input, btn);
    this.host.appendChild(addRow);
  }
}
