// `r` — the render queue as a panel: every job the daemon runs, whoever asked for it (the
// editor, an agent, the CLI), newest first, each with its progress and a way to stop it;
// then "render master", which is the one render nobody has to choose anything for. The
// render dialog still holds the choices (another sequence, a preset, an output, threads).
//
// The rows are the dialog's list (`jobList`), drawn smaller.

import { api } from "../api";
import { desktop } from "../desktop";
import { button, el, plural } from "../dom";
import { binName, store, toast } from "../store";
import { isFinal, jobCounts } from "../types";
import { jobList, lastRenderPreset, openRenderDialog, subject, type JobRow } from "./render";

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

  const rows = jobList(list, jobRow, el("p", "muted tiny", "No jobs yet. Renders, bakes, samples and transcripts show up here, the agent's too."));
  store.select((s) => [s.jobs], (s) => {
    const all = rows(s.jobs);
    done.textContent = `${jobCounts(s.jobs).done} done`;
    done.disabled = !all.some(isFinal);
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

function jobRow(id: number): JobRow {
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
