// `r` — the render queue as a panel: every job the daemon runs, whoever asked for it (the
// editor, an agent, the CLI), newest first, each with its progress and a way to stop it;
// then "render master", which is the one render nobody has to choose anything for. The
// render dialog still holds the choices (another sequence, a preset, an output, threads).
//
// Rows are updated in place and only re-placed when the set of jobs changes, so a cancel
// click is never lost to a redraw — the dialog's queue learnt that first.

import { api } from "../api";
import { desktop } from "../desktop";
import { button, el, plural } from "../dom";
import { store, toast } from "../store";
import { isFinal, type Job } from "../types";
import { binName, lastRenderPreset, openRenderDialog, subject } from "./render";

interface Row { root: HTMLElement; update(j: Job): void }

export function queuePanel(): { body: HTMLElement; tools: HTMLElement } {
  const body = el("div", "queue-panel");
  // The dialog's queue, which has room for the rest: every job's detail, the cache and its prune.
  const tools = el("span", "lib-tools");
  tools.appendChild(button("cache…", () => openRenderDialog("queue"), "The job queue in full, with the cache's size and its prune", "tool q-cache"));
  const list = el("div", "q-list");
  const renderMaster = button("render master", () => void renderMasterNow(), "Render the master with the preset last used (the dialog has the choices)", "primary q-render");
  const done = button("", () => void api.clearJobs().catch((e) => toast((e as Error).message, "err")), "Clear the finished jobs from the queue", "q-done");
  body.append(list, el("div", "q-foot"));
  body.lastElementChild!.append(renderMaster, done);

  const rows = new Map<number, Row>();
  let order = "";
  store.select((s) => [s.jobs], (s) => {
    const all = Object.values(s.jobs).sort((a, b) => b.id - a.id);
    const finished = all.filter((j) => j.state === "done").length;
    done.textContent = `${finished} done`;
    done.disabled = !all.some(isFinal);
    for (const j of all) {
      if (!rows.has(j.id)) rows.set(j.id, jobRow(j.id));
      rows.get(j.id)!.update(j);
    }
    const now = all.map((j) => j.id).join();
    if (now === order) return;
    order = now;
    for (const id of rows.keys()) if (!(id in s.jobs)) rows.delete(id);
    list.replaceChildren(...(all.length ? all.map((j) => rows.get(j.id)!.root) : [el("p", "muted tiny", "No jobs yet. Renders, bakes, samples and transcripts show up here, the agent's too.")]));
  });
  return { body, tools };
}

async function renderMasterNow() {
  try {
    const presets = await api.presets();
    const usable = presets.filter((p) => !p.unavailable);
    const preset = usable.find((p) => p.id === lastRenderPreset()) ?? usable[0];
    if (!preset) { toast("No export preset can run on this machine", "err"); return; }
    await api.render(preset.id);
    toast(`Rendering the master · ${preset.id}`, "ok");
  } catch (e) {
    toast((e as Error).message, "err", 8000);
  }
}

function jobRow(id: number): Row {
  const title = el("div", "q-title");
  const action = el("span", "q-action");
  const bar = el("div", "q-bar");
  const fill = el("div");
  bar.appendChild(fill);
  const state = el("div", "q-state");
  const root = el("div", "q-job");
  root.dataset.id = String(id);
  root.append(el("div", "q-head"), bar, state);
  root.firstElementChild!.append(title, action);
  let file = "";
  const cancel = button("cancel", () => {
    cancel.disabled = true;
    api.cancelJob(id).catch((e) => { cancel.disabled = false; toast((e as Error).message, "err"); });
  }, "Stop this job; a render keeps nothing it wrote", "linkish");
  const open = button("open", () => void desktop()?.reveal(file), "Show the file in its folder", "linkish");
  return {
    root,
    update(j) {
      const p = (j.params ?? {}) as { sequence?: string; preset?: string; output?: string; path?: string };
      file = p.output ?? "";
      root.className = `q-job ${j.state}`;
      title.textContent = j.kind === "render" ? `${binName(p.sequence)} · ${p.preset ?? ""}` : `${j.kind}${subject(j.params)}`;
      fill.style.width = `${j.state === "done" ? 100 : j.percent}%`;
      bar.hidden = j.state !== "running";
      const where = p.path ?? file;
      state.textContent = j.state === "running" ? `${j.message || "running"} · ${j.percent}%`
        : j.state === "done" ? `done${where ? ` · ${where}` : ""}`
        : j.state === "failed" ? `failed: ${j.message}`
        : j.state;
      state.title = j.message;
      action.replaceChildren(...(!isFinal(j) && j.cancellable ? [cancel] : j.state === "done" && file && desktop() ? [open] : []));
      root.title = `#${j.id} · ${j.kind}${j.state === "queued" ? ` · ${plural(1, "job")} ahead or more` : ""}`;
    },
  };
}
