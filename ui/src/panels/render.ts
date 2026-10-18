// The render dialog, Kdenlive's two tabs: what to render, and the job queue. Renders are
// daemon jobs run one at a time; this asks for them. The queue lists every job the daemon
// runs, whoever asked (the editor, an agent, the CLI), and cancels those that can stop. The
// master is preselected, and the sequence picked is always sent. An Advanced section,
// folded by default, changes this render's frames at once and its preset's quality and
// speed; left empty, each is the preset's (or the machine's) own. "For YouTube" exports
// the sequence's subtitles as an SRT file and copies its chapters as description
// timestamps; the daemon computes both.

import { api, type RenderOptions, type RenderTarget } from "../api";
import { desktop } from "../desktop";
import { button, capitalize, el, options, plural, row } from "../dom";
import { icon } from "../icons";
import { askForm, openDialog } from "../modal";
import { binName, sequenceLabel, store, toast } from "../store";
import { isFinal, sequenceIds, type CacheUsage, type Job, type Preset } from "../types";

type Tab = "render" | "queue";

/** What the daemon settled when the render was queued; `path` is `output`, project-relative. */
export interface RenderParams { sequence?: string; preset?: string; output?: string; path?: string; threads?: number | null; quality?: number | null; speed?: string | null }

/** The open dialog's tab switch: asking again switches rather than stacking a second. */
let showing: ((tab: Tab) => void) | null = null;
let lastPreset = "";
/** What a render starts from — the dialog's choice, the queue panel's "render master": the
 *  preset last rendered with this session, else the first this machine can run. */
export function defaultPreset<P extends { id: string; unavailable?: unknown }>(ps: P[]): P | undefined {
  const usable = ps.filter((p) => !p.unavailable);
  return usable.find((p) => p.id === lastPreset) ?? usable[0];
}
/** The Advanced section as last left: open or folded, and the values last rendered with.
 *  Quality and speed are `lastPreset`'s, and only come back with it. */
let advanced = { open: false, threads: "", quality: "", speed: "" };

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
  options(sequence, ids.map((id) => ({ value: id, label: sequenceLabel(id) })), project.master);
  const preset = el("select");
  const output = el("input");
  output.type = "text";
  const warning = el("p", "render-warning");
  warning.hidden = true;

  // ---- advanced: what this render changes of its preset. Empty is the preset's own.
  const threads = numberInput("Frames rendered at once. The file is identical however many; fewer keeps the machine responsive while it renders.");
  threads.value = advanced.threads;
  const quality = numberInput("");
  const speed = el("select");
  speed.title = "Encoder speed";
  const fixed = el("p", "muted tiny render-fixed");
  let presets = new Map<string, Preset>();
  const field = (label: string, input: HTMLElement) => row("field", el("label", "", label), input);
  const qualityField = field("Quality", row("row", quality, el("span", "muted tiny", "CRF · lower is better and larger")));
  const speedField = field("Speed", row("row", speed, el("span", "muted tiny", "faster is larger")));
  // Fit the fields to the preset picked: its range and its own values, or nothing to
  // change. Quality and speed are the codec's, so each preset starts from its own.
  const fit = () => {
    const p = presets.get(preset.value);
    const q = p?.quality, sp = p?.speed;
    qualityField.hidden = !q;
    speedField.hidden = !sp;
    fixed.hidden = !!(q || sp);
    fixed.textContent = p ? `${p.id} has no quality or speed to change.` : "";
    quality.value = "";
    if (q) {
      quality.min = String(q.min);
      quality.max = String(q.max);
      quality.placeholder = `${q.default} (preset)`;
      quality.title = `Rate factor, ${q.min} to ${q.max}: lower is better and makes a larger file`;
    }
    if (sp) {
      options(speed, sp.options.map((o) => ({ value: o, label: o === sp.default ? `${o} (preset)` : o })), sp.default);
      speed.title = `Encoder speed (${sp.label}), fastest first. At the same quality, faster makes a larger file.`;
    }
    summarize();
  };
  // What the render asks for beyond its preset: nothing when every field is its default.
  const chosen = (): RenderOptions => {
    const p = presets.get(preset.value);
    const int = (i: HTMLInputElement) => (i.value.trim() === "" ? undefined : Number(i.value));
    return {
      threads: int(threads),
      quality: p?.quality ? int(quality) : undefined,
      speed: p?.speed && speed.value !== p.speed.default ? speed.value : undefined,
    };
  };
  // Folded, the head still says what it changes.
  const note = el("span", "muted tiny render-advanced-note");
  const summarize = () => { note.textContent = describe(chosen()); };
  threads.oninput = quality.oninput = speed.onchange = summarize;
  const box = el("div", `render-advanced${advanced.open ? "" : " closed"}`);
  const disclose = button(icon("chevron-right", 11), () => {
    advanced.open = box.classList.contains("closed");
    box.classList.toggle("closed", !advanced.open);
    disclose.setAttribute("aria-expanded", String(advanced.open));
  }, "Show or hide what this render changes of its preset", "disclose");
  disclose.append("Advanced");
  disclose.setAttribute("aria-expanded", String(advanced.open));
  box.append(
    row("head", disclose, note),
    row("render-advanced-body", field("Threads", threads), qualityField, speedField, fixed),
  );

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
    threads.placeholder = `${t.threads} (default)`;
    threads.max = String(t.max_threads);
    warning.hidden = !clashes(t);
    warning.textContent = clashes(t) ? overwriteNote(t) : "";
  };
  let typing = 0;
  sequence.onchange = () => void check();
  preset.onchange = () => { fit(); void check(); };
  output.oninput = () => { clearTimeout(typing); typing = window.setTimeout(check, 250); };
  void api.presets().then((ps) => {
    presets = new Map(ps.map((p) => [p.id, p]));
    // One this machine cannot run (NVENC without a GPU) is listed greyed, saying why.
    options(preset, ps.map((p) => (p.unavailable
      ? { value: p.id, label: `${p.id} (unavailable)`, title: `${p.description}. Needs ${p.needs}: ${p.unavailable}` }
      : { value: p.id, title: p.description })), defaultPreset(ps)?.id);
    for (const o of preset.options) o.disabled = !!presets.get(o.value)?.unavailable;
    fit();
    if (preset.value === lastPreset) {
      quality.value = advanced.quality;
      if (advanced.speed) speed.value = advanced.speed;
      summarize();
    }
    void check();
  });

  const start = button("Render", async () => {
    clearTimeout(typing);
    await check(); // fresh: another render may have been queued since
    if (target && clashes(target) && !(await askForm({ title: "Overwrite?", note: overwriteNote(target), rows: [], ok: "Render anyway" }))) return;
    try {
      const tuning = chosen();
      await api.render(preset.value, sequence.value, output.value.trim() || undefined, tuning);
      lastPreset = preset.value;
      advanced = { ...advanced, threads: threads.value, quality: quality.value, speed: tuning.speed ?? "" };
      show("queue");
    } catch (e) { fail(e); }
  }, "", "primary");
  // ---- published beside the video: captions to upload and chapters to paste, of the
  // sequence picked. The daemon works both out; this asks, and shows what it said.
  const exported = el("p", "muted tiny render-export-note");
  const exportWarnings = el("ul", "render-export-warnings");
  const chaptersText = el("textarea", "render-chapters mono");
  chaptersText.readOnly = true;
  chaptersText.hidden = true;
  let exportedFile = "";
  const showExportedFile = button("Show file", () => void desktop()?.reveal(exportedFile), "Open the folder it was written to");
  showExportedFile.hidden = true;
  const said = (note: string, warnings: string[]) => {
    exported.textContent = note;
    exportWarnings.replaceChildren(...warnings.map((w) => el("li", "", w)));
  };
  const exportSrt = button("Export subtitles (.srt)", async () => {
    try {
      chaptersText.hidden = showExportedFile.hidden = true;
      // Asked first without writing, so an empty SRT is never left beside the renders.
      const seen = await api.exportSubtitles(sequence.value);
      if (!seen.cues) return said("No subtitles to export.", seen.warnings);
      const r = await api.exportSubtitles(sequence.value, { write: true });
      exportedFile = r.path ?? "";
      showExportedFile.hidden = !desktop();
      said(`${plural(r.cues, "caption")} written to ${r.path}`, r.warnings);
    } catch (e) { fail(e); }
  }, "Write the subtitles this sequence shows as an SRT file beside the renders, timed in its seconds, to upload as captions");
  const copyChapters = button("Copy chapters", async () => {
    try {
      const r = await api.exportChapters(sequence.value);
      const text = r.text ?? "";
      showExportedFile.hidden = true;
      chaptersText.value = text;
      chaptersText.hidden = !text;
      chaptersText.rows = Math.min(8, Math.max(2, r.chapters.length));
      // The clipboard can be refused (an insecure origin, no focus): the text is shown
      // selected either way, so a copy by hand is one keystroke.
      const copied = !!text && await navigator.clipboard?.writeText(text).then(() => true, () => false);
      if (text) chaptersText.select();
      said(!text ? "" : copied ? `${plural(r.chapters.length, "chapter")} copied, for the video's description` : `${plural(r.chapters.length, "chapter")}: copy them from here`, r.warnings);
    } catch (e) { fail(e); }
  }, "Copy this sequence's chapters (the sequences placed on it) as YouTube description timestamps");
  const publish = row("render-export",
    row("row", el("span", "render-export-label", "For YouTube"), exportSrt, copyChapters, showExportedFile),
    exported, exportWarnings, chaptersText,
  );

  const renderPane = row("render-pane",
    field("Sequence", sequence), field("Preset", preset), field("Output", output), warning, box, publish,
    el("p", "muted tiny", "Renders run one at a time; one asked for while another is encoding waits in the queue."),
    row("modal-actions", button("Close", close), start),
  );

  // ---- the queue: every job, newest first. Rows are updated in place and only
  // re-placed when the set of jobs changes, so a Cancel click is never lost to a redraw.
  const list = el("div", "job-list");
  const empty = el("p", "muted", "No jobs yet.");
  const clear = button("Clear finished", () => void api.clearJobs().catch(fail), "Remove done, failed and cancelled jobs", "push-left");
  // The cache's size, and a prune: a job like the rest, listed here.
  const cacheSize = el("span", "muted tiny cache-size");
  const prune = button("Prune cache", () => {
    prune.disabled = true;
    api.pruneCache().catch((e) => { prune.disabled = false; fail(e); });
  }, "Delete overlay bakes the project no longer uses, sampled frames, and thumbnails of removed entries");
  const measure = () => void api.cache().then((u) => { cacheSize.textContent = cacheNote(u); }).catch(() => { cacheSize.textContent = ""; });
  // Prunes seen running, so each result is reported once.
  const pruning = new Set<number>();
  const queuePane = row("queue-pane", list, row("modal-actions", clear, cacheSize, prune, button("Close", close)));
  const rows = jobList(list, jobRow, empty);
  const draw = (jobs: Record<number, Job>) => {
    const all = rows(jobs);
    clear.disabled = !all.some(isFinal);
    const prunes = all.filter((j) => j.kind === "prune");
    prune.disabled = prunes.some((j) => !isFinal(j));
    for (const j of prunes) {
      if (!isFinal(j)) { pruning.add(j.id); continue; }
      if (!pruning.delete(j.id)) continue;
      const done = j.result as { freed: CacheUsage; kept: CacheUsage } | null;
      if (!done) continue;
      toast(`Freed ${bytes(done.freed.bytes)}`, "ok");
      cacheSize.textContent = cacheNote(done.kept);
    }
  };

  const heading = el("h3");
  dlg.append(heading, row("tabs", tabs.render, tabs.queue), row("render-body", renderPane, queuePane));
  function show(t: Tab) {
    heading.textContent = t === "render" ? "Render" : "Job queue";
    renderPane.hidden = t !== "render";
    queuePane.hidden = t !== "queue";
    tabs.render.classList.toggle("on", t === "render");
    tabs.queue.classList.toggle("on", t === "queue");
    if (t === "render") sequence.focus();
    else measure();
  }
  showing = show;
  unsubscribe = store.select((s) => [s.jobs], (s) => draw(s.jobs));
  show(tab);
}

function numberInput(title: string): HTMLInputElement {
  const i = el("input");
  i.type = "number";
  i.step = "1";
  i.min = "1";
  i.title = title;
  return i;
}

/** A render's choices beyond its preset, as the dialog's head and the queue name them. */
function describe(o: { threads?: number | null; quality?: number | null; speed?: string | null }): string {
  const parts = [o.quality != null && `CRF ${o.quality}`, o.speed, o.threads != null && plural(o.threads, "thread")];
  return parts.filter(Boolean).join(" · ");
}

/** `1.4 GB`, decimal, as a file manager says it. */
function bytes(n: number): string {
  const units = ["B", "kB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
  return i === 0 ? `${n} B` : `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

const cacheNote = (u: CacheUsage) =>
  `Cache ${bytes(u.bytes)} · ${plural(u.bakes.entries, "bake")}, ${plural(u.samples.entries, "frame")}`;

const clashes = (t: RenderTarget) => t.exists || t.pending.length > 0;

function overwriteNote(t: RenderTarget): string {
  const name = t.path.split(/[\\/]/).pop();
  const jobs = t.pending.map((id) => `#${id}`).join(", ");
  if (!t.exists) return `${jobs} will also write ${name}. The last to finish wins.`;
  return `${name} already exists${jobs ? `, and ${jobs} will also write it` : ""}. Rendering replaces it.`;
}

export interface JobRow { root: HTMLElement; update: (j: Job) => void }

/** A job's cancel: off while the daemon is asked, back on if it says no. */
export function cancelButton(id: number, label: string, cls = ""): HTMLButtonElement {
  const b = button(label, () => {
    b.disabled = true;
    api.cancelJob(id).catch((e) => { b.disabled = false; fail(e); });
  }, "Stop this job; a render keeps nothing it wrote", cls);
  return b;
}

/**
 * Jobs in `list`, newest first, one row each kept by id: updated in place, and re-placed only
 * when the set of jobs changes, so a click on a row's Cancel is never lost to a redraw. The
 * dialog's queue and the queue panel draw rows of their own through it. Returns the jobs.
 */
export function jobList(list: HTMLElement, make: (id: number) => JobRow, empty: HTMLElement): (jobs: Record<number, Job>) => Job[] {
  const rows = new Map<number, JobRow & { shown?: Job }>();
  let order = "";
  return (jobs) => {
    const all = Object.values(jobs).sort((a, b) => b.id - a.id);
    for (const j of all) {
      if (!rows.has(j.id)) rows.set(j.id, make(j.id));
      // A job's record is replaced when it changes: the same one has nothing new to show.
      const row = rows.get(j.id)!;
      if (row.shown !== j) { row.shown = j; row.update(j); }
    }
    const now = all.map((j) => j.id).join();
    if (now !== order) {
      order = now;
      for (const id of rows.keys()) if (!(id in jobs)) rows.delete(id);
      list.replaceChildren(...(all.length ? all.map((j) => rows.get(j.id)!.root) : [empty]));
    }
    return all;
  };
}

/** What a job other than a render works on, as its params name it: `· Main`, `· talk`. */
export function subject(params: unknown): string {
  const p = (params ?? {}) as { sequence?: string | null; overlay?: string | null; bin?: string | null; clip?: string | null; path?: string | null };
  const what = p.overlay ?? p.bin ?? (p.sequence ? binName(p.sequence) : null) ?? (p.clip ? `clip ${p.clip}` : null) ?? p.path;
  return what ? ` · ${what}` : "";
}

function jobRow(id: number): JobRow {
  const title = el("div", "job-title");
  const bar = el("progress");
  bar.max = 100;
  const state = el("div", "job-state");
  const path = el("div", "job-path muted mono tiny");
  let file = "";
  const cancel = cancelButton(id, "Cancel");
  const reveal = button("Show file", () => void desktop()?.reveal(file), "Open the folder it was written to");
  const root = row("job", row("job-main", title, bar, state, path), row("job-actions", cancel, reveal));
  root.dataset.id = String(id);
  return {
    root,
    update(j) {
      const p = (j.params ?? {}) as RenderParams;
      file = p.output ?? "";
      root.className = `job ${j.state}`;
      const asked = describe(p);
      title.textContent = j.kind === "render" ? `#${j.id} · ${binName(p.sequence)} · ${p.preset ?? ""}${asked ? ` (${asked})` : ""}` : `#${j.id} · ${j.kind}${subject(j.params)}`;
      bar.value = j.state === "done" ? 100 : j.percent;
      state.textContent = j.state === "running" ? `${j.percent}% · ${j.message}` : j.state === "failed" ? `Failed: ${j.message}` : capitalize(j.state);
      path.textContent = p.path ?? file;
      path.title = file;
      cancel.hidden = isFinal(j) || !j.cancellable;
      reveal.hidden = j.state !== "done" || !desktop();
    },
  };
}
