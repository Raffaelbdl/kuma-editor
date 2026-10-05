// The top bar: project, sequence picker, frame size, undo/redo, Render and Queue. Buttons
// run commands. Both open the render dialog (`render.ts`), on its own tab: Render to choose
// a sequence and preset, Queue to watch and cancel every job the daemon runs, whoever asked
// for it (the editor, an agent, the CLI).

import { Cmd, command, run } from "../commands";
import { button, el, options, plural } from "../dom";
import { icon } from "../icons";
import { keysFor } from "../shortcuts";
import { store } from "../store";
import { orientation, sequenceIds, type JobState } from "../types";
import { openRenderDialog } from "./render";

export function mountTopbar(host: HTMLElement): void {
  const name = el("span", "name");
  const rev = el("span", "muted mono");
  // Which timeline is being edited. The master is one entry in this list like any other:
  // that is the whole point of a sequence being a bin entry.
  const seqSel = el("select");
  seqSel.style.width = "auto";
  seqSel.title = "Sequence being edited (the preview follows it)";
  seqSel.onchange = () => {
    const picked = seqSel.value;
    // Put the picker back where it was before acting: a cancelled prompt, or a refused
    // open, must not leave it showing the "+" sentinel.
    seqSel.value = store.state.sequence;
    if (picked === "+") run(Cmd.newSequence);
    else void store.openSequence(picked);
  };

  // Landscape or portrait is nothing but the frame size, so the control that shows it is
  // the control that flips it: one click swaps width and height.
  const size = button("", () => run(Cmd.swapOrientation), "", "mono");

  command(Cmd.render, "Render…", () => openRenderDialog("render"));
  command(Cmd.queue, "Job queue", () => openRenderDialog("queue"));

  // Spins while the reload it asked for is in flight: it can bake, and a button that does
  // nothing visible for five seconds reads as a button that did nothing.
  const reload = button(icon("refresh"), () => run(Cmd.reloadPreview), "Reload the preview: recompile the timeline, bake the overlays of nested sequences, and say how it went", "icon reload");
  store.select((s) => [s.reloading], (s) => {
    reload.classList.toggle("busy", s.reloading);
    reload.disabled = s.reloading;
  });

  const render = button("Render", () => run(Cmd.render), "Render a sequence to a file", "primary");
  const queue = button("Queue", () => run(Cmd.queue), "Every job: renders, samples, measurements, bakes, transcripts — from the editor, agents and the CLI");
  // What is running, and the way to the queue: a click on it is a click on the queue tab.
  const status = el("span", "status");
  status.onclick = () => openRenderDialog("queue");
  status.title = "Open the job queue";
  host.append(
    name, seqSel, rev, size,
    // Icons where the tooltip says it all: the bar is for what you reach for, and these
    // are reached for by their keys.
    button(icon("undo"), () => run(Cmd.undo), `Undo (${keysFor(Cmd.undo)})`, "icon"),
    button(icon("redo"), () => run(Cmd.redo), `Redo (${keysFor(Cmd.redo)})`, "icon"),
    el("span", "spacer"),
    render,
    queue,
    reload,
    // Chosen once and then left alone, so it is an icon rather than a word: the bar is
    // for what you reach for while editing. The desktop shell's View menu names the same
    // command, and a browser has only this.
    button(icon("palette"), () => run(Cmd.theme), "Theme — how the editor looks", "icon"),
    // What the keys are, where a person looks for it. The list is the bindings themselves
    // (`shortcuts.ts`), so it cannot go stale, and `?` opens the same one.
    button("?", () => run(Cmd.shortcuts), `Keyboard shortcuts (${keysFor(Cmd.shortcuts)})`),
    status,
  );

  // The picker rebuilds its options, so it gets its own subscription: job progress ticks
  // many times a second and must not tear the dropdown down with it.
  store.select((s) => [s.project, s.sequence], (s) => {
    if (!s.project) return;
    const entry = (id: string) => ({ value: id, label: `${s.project!.bin[id].name || id}${id === s.project!.master ? " (master)" : ""}` });
    options(seqSel, [...sequenceIds(s.project).map(entry), { value: "+", label: "+ new sequence…" }], s.sequence);
    const { width, height } = s.project;
    size.textContent = `${width}×${height} ⇄`;
    size.title = `Switch to ${orientation({ width: height, height: width })} (${height}×${width})`;
  });
  store.select((s) => [s.project, s.sequence, s.connected, s.jobs], (s) => {
    name.textContent = s.project?.name || "kuma";
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    // A count, not a transcript: the detail — paths, frames, errors — is the queue's, one
    // click away. Only what there is gets said.
    const jobs = Object.values(s.jobs);
    const n: Record<JobState, number> = { running: 0, queued: 0, done: 0, failed: 0, cancelled: 0 };
    for (const j of jobs) n[j.state]++;
    const said: [JobState, string][] = [["running", "running"], ["queued", "queued"], ["done", "finished"], ["failed", "failed"], ["cancelled", "cancelled"]];
    status.textContent = said.filter(([k]) => n[k]).map(([k, word]) => `${plural(n[k], "job")} ${word}`).join(", ");
    status.classList.toggle("link", jobs.length > 0);
  });
}
