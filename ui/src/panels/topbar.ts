// The top bar: the bear and the wordmark, the editor tabs, ⌘K, the mode, Render and the
// queue. Undo, redo, reload, the theme and the shortcut list are not buttons here any more:
// they are commands, so ⌘K, the keys and the shell's menus reach them (`palette.ts`).
//
// Render opens the render dialog (`render.ts`); the queue button and its count open the
// render queue panel (`r`), which lists every job the daemon runs, whoever asked for it.

import { watchAgent, bear, setBear } from "../agent";
import { Cmd, command, run } from "../commands";
import { button, el } from "../dom";
import { panels } from "../panelhost";
import { keysFor } from "../shortcuts";
import { store } from "../store";
import { jobCounts, type JobState } from "../types";
import { openRenderDialog } from "./render";
import { mountTabs } from "./tabs";

export function mountTopbar(host: HTMLElement): void {
  command(Cmd.render, "Render…", () => openRenderDialog("render"));
  command(Cmd.queue, "Render queue", () => panels.show("queue"));

  const pet = bear("awake", 2);
  pet.title = "kuma";
  watchAgent((v) => setBear(pet, v.bear));
  const tabs = el("nav");
  tabs.id = "editor-tabs";

  const k = button("", () => run(Cmd.palette), `Every command, by name (${keysFor(Cmd.palette)})`, "kfield");
  k.append(el("kbd", "", navigator.platform.startsWith("Mac") ? "⌘K" : "Ctrl K"), el("span", "", "split, add track, render…"));

  // The two modes over one set of panels. The playhead and the selection stay put: they
  // are the store's, and a mode only moves panels.
  const den = button("den", () => panels.setMode("den"), "Den: the frame, full-bleed, and what the agent did and where it looked");
  const power = button("power", () => panels.setMode("power"), "Power: every panel, docked");
  const modes = el("div", "modes");
  modes.append(den, power);
  store.select((s) => [s.mode], (s) => {
    den.classList.toggle("on", s.mode === "den");
    power.classList.toggle("on", s.mode === "power");
  });

  const render = button("render", () => run(Cmd.render), "Render a sequence to a file — the master unless you pick another", "primary");
  // What is running, and the way to the queue: a count, not a transcript. The detail —
  // paths, frames, errors — is the queue's.
  const status = el("span", "status");
  const queue = button("queue", () => run(Cmd.queue), "Every job: renders, samples, measurements, bakes, transcripts — from the editor, agents and the CLI", "queue-btn");
  queue.appendChild(status);

  host.append(pet, el("span", "wordmark", "kuma"), tabs, el("span", "spacer"), k, el("span", "spacer"), modes, render, queue);
  mountTabs(tabs);

  store.select((s) => [s.jobs], (s) => {
    const n = jobCounts(s.jobs);
    status.textContent = (Object.keys(n) as JobState[]).filter((k) => n[k]).map((k) => `${n[k]} ${k}`).join(" · ");
  });
}
