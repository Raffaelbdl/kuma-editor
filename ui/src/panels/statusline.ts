// The status line, vim's: the mode, the revision, what is selected and where the playhead
// is; then the frame, and what the agent is doing, and the jobs. Everything here is read
// off the store; the three things you can click are the same commands as their keys.

import { watchAgent } from "../agent";
import { Cmd, run } from "../commands";
import { el, plural } from "../dom";
import { panels } from "../panelhost";
import { fmtTime, store } from "../store";
import { fpsValue } from "../types";

export function mountStatusline(host: HTMLElement): void {
  const mode = el("span", "mode");
  mode.title = "Switch den / power";
  mode.onclick = () => run(Cmd.toggleMode);
  const rev = el("span", "rev");
  const sel = el("span", "sel-count");
  const at = el("span", "at");
  const frame = el("span", "frame");
  const agent = el("span", "agent clickable");
  agent.title = "The agent: what it is doing and its last calls (A)";
  agent.onclick = () => panels.toggle("agent");
  const jobs = el("span", "jobs clickable last");
  jobs.title = "The render queue (R)";
  jobs.onclick = () => run(Cmd.queue);
  host.append(mode, rev, sel, at, el("span", "grow"), frame, agent, jobs);

  store.select((s) => [s.mode], (s) => { mode.textContent = s.mode; });
  store.select((s) => [s.project, s.connected], (s) => {
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    frame.textContent = s.project ? `${s.project.width}×${s.project.height} · ${+fpsValue(s.project.fps).toFixed(3)} fps` : "";
  });
  store.select((s) => [s.selection], (s) => {
    const n = store.selectedClips.length;
    sel.textContent = n ? `${plural(n, "clip")} selected` : s.selection?.kind === "bin" ? "1 entry selected" : "";
    sel.title = n > 1 ? "Copy, cut, delete and split act on all of them" : "";
  });
  store.select((s) => [s.playhead, s.project], (s) => { at.textContent = fmtTime(s.playhead, store.fps); });
  watchAgent((v) => {
    agent.textContent = v.client ? `${v.client} · ${v.doing}` : "no agent yet";
    agent.classList.toggle("idle", v.bear === "idle");
  });
  store.select((s) => [s.jobs], (s) => {
    const all = Object.values(s.jobs);
    const running = all.filter((j) => j.state === "running" || j.state === "queued").length;
    const done = all.filter((j) => j.state === "done").length;
    jobs.textContent = running ? `${plural(running, "job")} running · ${done} done` : done ? `${plural(done, "job")} done` : "";
  });
}
