// The render dialog: what to render, and the queue of renders asked for so far, as the two
// tabs Kdenlive's has. Every render is a daemon job, run one at a time; this only asks
// for them, watches them and cancels them. The master is preselected — render means the
// master unless another sequence is named, here as on every other surface — and the
// sequence picked is always sent: a person who named one has named one.

import { api, type RenderTarget } from "../api";
import { desktop } from "../desktop";
import { button, capitalize, el, options, row } from "../dom";
import { askForm, openDialog } from "../modal";
import { store, toast } from "../store";
import { sequenceIds, type Job } from "../types";

type Tab = "render" | "queue";

/** A render's own account of itself, settled by the daemon when it was queued. */
interface RenderParams { sequence?: string; preset?: string; output?: string; /** `output`, project-relative when inside it. */ path?: string }

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

  // What the render would write and replace, from the daemon — its default name is its
  // own rule, so the empty field's placeholder is the answer it gives, not a copy of it.
  // Asked on every choice: picking a sequence or preset can land on a file already there.
  const warning = el("p", "render-warning");
  warning.hidden = true;
  let target: RenderTarget | null = null;
  let asked = 0;
  const check = async () => {
    const n = ++asked;
    try {
      const t = await api.renderTarget(preset.value, sequence.value, output.value.trim() || undefined);
      if (n !== asked) return; // a newer choice has been made meanwhile
      target = t;
      output.placeholder = t.path;
      warning.hidden = !clashes(t);
      warning.textContent = clashes(t) ? overwriteNote(t) : "";
    } catch { /* an unknown preset mid-load, say: the render itself will say so */ }
  };
  let typing = 0;
  sequence.onchange = preset.onchange = () => void check();
  output.oninput = () => { clearTimeout(typing); typing = window.setTimeout(() => void check(), 250); };
  void api.presets().then((ps) => {
    options(preset, ps.map((p) => ({ value: p.id, title: p.description })), lastPreset || ps[0]?.id);
    void check();
  });

  const field = (label: string, input: HTMLElement) => row("field", el("label", "", label), input);
  const start = button("Render", async () => {
    // Asked again rather than trusted: another render may have been queued since.
    clearTimeout(typing);
    await check();
    if (target && clashes(target) && !(await askForm({ title: "Overwrite?", note: overwriteNote(target), rows: [], ok: "Render anyway" }))) return;
    try {
      await api.render(preset.value, sequence.value, output.value.trim() || undefined);
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
    warning,
    el("p", "muted tiny", "Renders run one at a time; one asked for while another is encoding waits in the queue."),
    row("modal-actions", button("Close", close), start),
  );

  // ---- the queue
  const list = el("div", "job-list");
  const empty = el("p", "muted", "No jobs yet.");
  const rows = new Map<number, JobRow>();
  // Rows are kept and updated in place: progress ticks many times a second, and a row
  // rebuilt under the pointer would swallow the click on its Cancel button.
  let order = "";
  const draw = (jobs: Record<number, Job>) => {
    // Every job, renders or not: the top bar counts them all, and this is where its
    // click lands. Only renders carry a Cancel.
    const all = Object.values(jobs).sort((a, b) => b.id - a.id);
    clear.disabled = !all.some((j) => j.done);
    for (const j of all) {
      let r = rows.get(j.id);
      if (!r) rows.set(j.id, (r = jobRow(j.id)));
      r.update(j);
    }
    // Rows are only re-placed when the set of jobs changes, not on every tick.
    const now = all.map((j) => j.id).join();
    if (now === order) return;
    order = now;
    for (const id of rows.keys()) if (!(id in jobs)) rows.delete(id);
    list.replaceChildren(...(all.length ? all.map((j) => rows.get(j.id)!.root) : [empty]));
  };
  const queuePane = el("div", "queue-pane");
  const clear = button("Clear finished", () => void api.clearJobs().catch((e) => toast((e as Error).message, "err")), "Remove done, failed and cancelled jobs from the list");
  clear.classList.add("push-left");
  queuePane.append(list, row("modal-actions", clear, button("Close", close)));

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

const clashes = (t: RenderTarget) => t.exists || t.pending.length > 0;

/** What a render would clobber, in a sentence. */
function overwriteNote(t: RenderTarget): string {
  const name = t.path.split(/[\\/]/).pop();
  const jobs = t.pending.map((id) => `#${id}`).join(", ");
  return t.exists && t.pending.length ? `${name} already exists, and ${jobs} will also write it. Rendering replaces it.`
    : t.exists ? `${name} already exists. Rendering replaces it.`
    : `${jobs} will also write ${name}. The last to finish wins.`;
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

  return {
    root,
    update(j) {
      const p = (j.params ?? {}) as RenderParams;
      const status = j.state;
      root.className = `job ${status}`;
      title.textContent = j.kind === "render" ? `#${j.id} · ${binName(p.sequence)} · ${p.preset ?? ""}` : `#${j.id} · ${j.kind}`;
      bar.value = j.done && j.ok ? 100 : j.percent;
      state.textContent = status === "running" ? `${j.percent}% · ${j.message}`
        : status === "failed" ? `Failed: ${j.message}`
        : capitalize(status);
      root.dataset.output = p.output ?? "";
      path.textContent = p.path ?? p.output ?? "";
      path.title = p.output ?? "";
      cancel.hidden = j.done || !j.cancellable;
      reveal.hidden = !(status === "done" && desktop());
    },
  };
}
