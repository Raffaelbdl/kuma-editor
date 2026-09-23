// The render dialog, Kdenlive's two tabs: what to render, and the job queue. Renders are
// daemon jobs run one at a time; this asks for them, watches them and cancels them. The
// master is preselected, and the sequence picked is always sent.

import { api, type RenderTarget } from "../api";
import { desktop } from "../desktop";
import { button, capitalize, el, options, row } from "../dom";
import { askForm, openDialog } from "../modal";
import { store, toast } from "../store";
import { isFinal, sequenceIds, type Job } from "../types";

type Tab = "render" | "queue";

/** What the daemon settled when the render was queued; `path` is `output`, project-relative. */
interface RenderParams { sequence?: string; preset?: string; output?: string; path?: string }

/** The open dialog's tab switch: asking again switches rather than stacking a second. */
let showing: ((tab: Tab) => void) | null = null;
let lastPreset = "";

const binName = (id?: string) => (id && store.state.project?.bin[id]?.name) || id || "?";
const fail = (e: unknown) => toast((e as Error).message, "err");

export function openRenderDialog(tab: Tab = "render"): void {
  if (showing) return showing(tab);
  const project = store.state.project;
  if (!project) return;

  let unsubscribe = () => {};
  const { dlg, close } = openDialog("render", () => { unsubscribe(); showing = null; });
  const tabs = { render: button("Render", () => show("render"), "", "tab"), queue: button("Job queue", () => show("queue"), "", "tab") };

  // ---- what to render
  const sequence = el("select");
  sequence.title = "Which timeline to render";
  const ids = [project.master, ...sequenceIds(project).filter((id) => id !== project.master)];
  options(sequence, ids.map((id) => ({ value: id, label: `${binName(id)}${id === project.master ? " (master)" : ""}` })), project.master);
  const preset = el("select");
  const output = el("input");
  output.type = "text";
  const warning = el("p", "render-warning");
  warning.hidden = true;

  // The daemon says where a render would write and what it would replace; the default
  // name is its rule, so the placeholder is its answer. Stale answers are dropped.
  let target: RenderTarget | null = null;
  let asked = 0;
  const check = async () => {
    const n = ++asked;
    const t = await api.renderTarget(preset.value, sequence.value, output.value.trim() || undefined).catch(() => null);
    if (!t || n !== asked) return;
    target = t;
    output.placeholder = t.path;
    warning.hidden = !clashes(t);
    warning.textContent = clashes(t) ? overwriteNote(t) : "";
  };
  let typing = 0;
  sequence.onchange = preset.onchange = () => void check();
  output.oninput = () => { clearTimeout(typing); typing = window.setTimeout(check, 250); };
  void api.presets().then((ps) => {
    options(preset, ps.map((p) => ({ value: p.id, title: p.description })), lastPreset || ps[0]?.id);
    void check();
  });

  const start = button("Render", async () => {
    clearTimeout(typing);
    await check(); // fresh: another render may have been queued since
    if (target && clashes(target) && !(await askForm({ title: "Overwrite?", note: overwriteNote(target), rows: [], ok: "Render anyway" }))) return;
    try {
      await api.render(preset.value, sequence.value, output.value.trim() || undefined);
      lastPreset = preset.value;
      show("queue");
    } catch (e) { fail(e); }
  }, "", "primary");
  const field = (label: string, input: HTMLElement) => row("field", el("label", "", label), input);
  const renderPane = row("render-pane",
    field("Sequence", sequence), field("Preset", preset), field("Output", output), warning,
    el("p", "muted tiny", "Renders run one at a time; one asked for while another is encoding waits in the queue."),
    row("modal-actions", button("Close", close), start),
  );

  // ---- the queue: every job, newest first. Rows are updated in place and only
  // re-placed when the set of jobs changes, so a Cancel click is never lost to a redraw.
  const list = el("div", "job-list");
  const empty = el("p", "muted", "No jobs yet.");
  const clear = button("Clear finished", () => void api.clearJobs().catch(fail), "Remove done, failed and cancelled jobs", "push-left");
  const queuePane = row("queue-pane", list, row("modal-actions", clear, button("Close", close)));
  const rows = new Map<number, JobRow>();
  let order = "";
  const draw = (jobs: Record<number, Job>) => {
    const all = Object.values(jobs).sort((a, b) => b.id - a.id);
    clear.disabled = !all.some(isFinal);
    for (const j of all) {
      if (!rows.has(j.id)) rows.set(j.id, jobRow(j.id));
      rows.get(j.id)!.update(j);
    }
    const now = all.map((j) => j.id).join();
    if (now === order) return;
    order = now;
    for (const id of rows.keys()) if (!(id in jobs)) rows.delete(id);
    list.replaceChildren(...(all.length ? all.map((j) => rows.get(j.id)!.root) : [empty]));
  };

  dlg.append(el("h3", "", "Render"), row("tabs", tabs.render, tabs.queue), row("render-body", renderPane, queuePane));
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

function overwriteNote(t: RenderTarget): string {
  const name = t.path.split(/[\\/]/).pop();
  const jobs = t.pending.map((id) => `#${id}`).join(", ");
  if (!t.exists) return `${jobs} will also write ${name}. The last to finish wins.`;
  return `${name} already exists${jobs ? `, and ${jobs} will also write it` : ""}. Rendering replaces it.`;
}

interface JobRow { root: HTMLElement; update: (j: Job) => void }

function jobRow(id: number): JobRow {
  const title = el("div", "job-title");
  const bar = el("progress");
  bar.max = 100;
  const state = el("div", "job-state");
  const path = el("div", "job-path muted mono tiny");
  let file = "";
  const cancel = button("Cancel", () => {
    cancel.disabled = true;
    api.cancelJob(id).catch((e) => { cancel.disabled = false; fail(e); });
  }, "Stop this render; nothing it wrote is kept");
  const reveal = button("Show file", () => void desktop()?.reveal(file), "Open the folder it was written to");
  const root = row("job", row("job-main", title, bar, state, path), row("job-actions", cancel, reveal));
  root.dataset.id = String(id);
  return {
    root,
    update(j) {
      const p = (j.params ?? {}) as RenderParams;
      file = p.output ?? "";
      root.className = `job ${j.state}`;
      title.textContent = j.kind === "render" ? `#${j.id} · ${binName(p.sequence)} · ${p.preset ?? ""}` : `#${j.id} · ${j.kind}`;
      bar.value = j.state === "done" ? 100 : j.percent;
      state.textContent = j.state === "running" ? `${j.percent}% · ${j.message}` : j.state === "failed" ? `Failed: ${j.message}` : capitalize(j.state);
      path.textContent = p.path ?? file;
      path.title = file;
      cancel.hidden = isFinal(j) || !j.cancellable;
      reveal.hidden = j.state !== "done" || !desktop();
    },
  };
}
