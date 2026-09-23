// The render dialog: what to render, and the queue of renders asked for so far, as the two
// tabs Kdenlive's has. Every render is a daemon job, run one at a time; this only asks
// for them, watches them and cancels them. The master is preselected — render means the
// master unless another sequence is named, here as on every other surface.

import { api } from "../api";
import { desktop } from "../desktop";
import { button, el, options } from "../dom";
import { openDialog } from "../modal";
import { store, toast } from "../store";
import { sequenceIds, type Job, type Preset } from "../types";

type Tab = "render" | "queue";

/** A render's own account of itself, settled by the daemon when it was queued. */
interface RenderParams { sequence?: string; preset?: string; output?: string }

/** The dialog that is up, if one is: asking again switches its tab rather than stacking. */
let showing: ((tab: Tab) => void) | null = null;
/** The preset last rendered with, so the next render starts from it. */
let lastPreset = "";

const binName = (id: string | undefined): string => (id && store.state.project?.bin[id]?.name) || id || "?";

/** Open the render dialog on `tab`. */
export function openRenderDialog(tab: Tab = "render"): void {
  if (showing) return showing(tab);
  const project = store.state.project;
  if (!project) return;

  let unsubscribe = () => {};
  const { dlg, close } = openDialog("render", () => { unsubscribe(); showing = null; });

  const strip = el("div", "tabs");
  const tabs = { render: button("Render", () => show("render"), "", "tab"), queue: button("Job queue", () => show("queue"), "", "tab") };
  strip.append(tabs.render, tabs.queue);

  // ---- what to render
  const sequence = el("select");
  // The master heads the list: it is the default, and the finished video.
  const ids = [project.master, ...sequenceIds(project).filter((id) => id !== project.master)];
  options(sequence, ids.map((id) => ({ value: id, label: `${binName(id)}${id === project.master ? " (master)" : ""}` })), project.master);
  sequence.title = "Which timeline to render";
  const preset = el("select");
  const output = el("input");
  output.type = "text";
  let presets: Preset[] = [];
  // The daemon's own default name, shown rather than restated in prose: an empty field
  // means exactly this.
  const placeholder = () => {
    const p = presets.find((x) => x.id === preset.value);
    const part = sequence.value === project.master ? "" : `-${sequence.value}`;
    output.placeholder = p ? `renders/${project.name || "kuma"}${part}-${p.id}.${p.extension}` : "";
  };
  sequence.onchange = placeholder;
  preset.onchange = placeholder;
  void api.presets().then((ps) => {
    presets = ps;
    options(preset, ps.map((p) => ({ value: p.id, title: p.description })), lastPreset || ps[0]?.id);
    placeholder();
  });

  const field = (label: string, input: HTMLElement) => {
    const f = el("div", "field");
    f.append(el("label", "", label), input);
    return f;
  };
  const start = button("Render", async () => {
    // Left out for the master, so the daemon's default is what decides it, as it does
    // for every other client.
    const seq = sequence.value === project.master ? undefined : sequence.value;
    try {
      await api.render(preset.value, seq, output.value.trim() || undefined);
      lastPreset = preset.value;
      show("queue");
    } catch (e) {
      toast((e as Error).message, "err");
    }
  }, "", "primary");
  const renderPane = el("div", "render-pane");
  renderPane.append(
    field("Sequence", sequence),
    field("Preset", preset),
    field("Output", output),
    el("p", "muted tiny", "Renders run one at a time; one asked for while another is encoding waits in the queue."),
    actions(button("Close", close), start),
  );

  // ---- the queue
  const list = el("div", "job-list");
  const empty = el("p", "muted", "No renders yet.");
  const rows = new Map<number, JobRow>();
  // Rows are kept and updated in place: progress ticks many times a second, and a row
  // rebuilt under the pointer would swallow the click on its Cancel button.
  const draw = (jobs: Record<number, Job>) => {
    const renders = Object.values(jobs).filter((j) => j.kind === "render").sort((a, b) => b.id - a.id);
    const seen = new Set<number>();
    for (const j of renders) {
      seen.add(j.id);
      let r = rows.get(j.id);
      if (!r) rows.set(j.id, (r = jobRow(j.id)));
      r.update(j);
    }
    for (const [id, r] of rows) if (!seen.has(id)) { r.root.remove(); rows.delete(id); }
    list.replaceChildren(...(renders.length ? renders.map((j) => rows.get(j.id)!.root) : [empty]));
  };
  const queuePane = el("div", "queue-pane");
  queuePane.append(list, actions(button("Close", close)));

  const body = el("div", "render-body");
  body.append(renderPane, queuePane);
  dlg.append(el("h3", "", "Render"), strip, body);

  function show(t: Tab) {
    renderPane.hidden = t !== "render";
    queuePane.hidden = t !== "queue";
    tabs.render.classList.toggle("on", t === "render");
    tabs.queue.classList.toggle("on", t === "queue");
    if (t === "render") sequence.focus();
  }
  showing = show;
  unsubscribe = store.select((s) => [s.jobs], (s) => draw(s.jobs));
  show(tab);
}

function actions(...buttons: HTMLButtonElement[]): HTMLElement {
  const a = el("div", "modal-actions");
  a.append(...buttons);
  return a;
}

interface JobRow { root: HTMLElement; update: (j: Job) => void }

/** One render in the queue: what it is, how far it has got, and what can be done to it. */
function jobRow(id: number): JobRow {
  const root = el("div", "job");
  root.dataset.id = String(id);
  const title = el("div", "job-title");
  const state = el("div", "job-state");
  const bar = el("progress");
  bar.max = 100;
  const path = el("div", "job-path muted mono tiny");
  const cancel = button("Cancel", () => {
    cancel.disabled = true;
    api.cancelJob(id).catch((e) => { cancel.disabled = false; toast((e as Error).message, "err"); });
  }, "Stop this render; nothing it wrote is kept");
  const reveal = button("Show file", () => void desktop()?.reveal(String((root.dataset.output ?? ""))), "Open the folder it was written to");
  const side = el("div", "job-actions");
  side.append(cancel, reveal);
  const main = el("div", "job-main");
  main.append(title, bar, state, path);
  root.append(main, side);

  let last = "";
  return {
    root,
    update(j) {
      const p = (j.params ?? {}) as RenderParams;
      const key = `${j.percent}|${j.message}|${j.done}|${j.ok}|${j.cancelled}`;
      if (key === last) return;
      last = key;
      const queued = !j.done && j.message === "queued";
      const status = j.cancelled ? "cancelled" : j.done ? (j.ok ? "done" : "failed") : queued ? "queued" : "running";
      root.className = `job ${status}`;
      title.textContent = `#${j.id} · ${binName(p.sequence)} · ${p.preset ?? ""}`;
      bar.value = j.done && j.ok ? 100 : j.percent;
      state.textContent = status === "running" ? `${j.percent}% · ${j.message}`
        : status === "failed" ? `Failed: ${j.message}`
        : status === "queued" ? "Queued" : status === "done" ? "Done" : "Cancelled";
      root.dataset.output = p.output ?? "";
      path.textContent = p.output ?? "";
      path.title = p.output ?? "";
      cancel.hidden = j.done || !j.cancellable;
      if (j.done) cancel.disabled = false;
      reveal.hidden = !(status === "done" && desktop());
    },
  };
}
