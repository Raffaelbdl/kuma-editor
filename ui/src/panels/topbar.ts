// The top bar: project, sequence picker, frame size, undo/redo, export preset, Render.
// Buttons run commands. Render is registered here rather than received, because the
// preset it needs is a control in this bar — and so is the timeline it renders.

import { api } from "../api";
import { Cmd, command, run } from "../commands";
import { button, el, options } from "../dom";
import { startJob, store } from "../store";
import { orientation, sequenceIds } from "../types";

/** A bin entry's name, for a label. */
const binName = (id: string | undefined): string => (id && store.state.project?.bin[id]?.name) || id || "";

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

  const presets = el("select");
  presets.style.width = "auto";
  void api.presets().then((ps) => options(presets, ps.map((p) => ({ value: p.id, title: p.description }))));

  // Which timeline Render means. The master is the finished video and stays the default,
  // here as in the daemon; this control only appears once the two can differ — that is,
  // once you are editing a sequence that is not the master — because until then there is
  // nothing to choose and a dropdown saying so is noise.
  const target = el("select");
  target.style.width = "auto";
  target.title = "Which timeline to render";
  const renderMaster = () => target.hidden || target.value !== "open";

  command(Cmd.render, "Render", () => {
    const sequence = renderMaster() ? undefined : store.state.sequence;
    startJob(`Render ${binName(sequence ?? store.state.project?.master)}`, () => api.render(presets.value, sequence));
  });

  const render = button("Render", () => run(Cmd.render), "", "primary");
  const status = el("span", "status");
  host.append(
    name, seqSel, rev, size,
    button("Undo", () => run(Cmd.undo)),
    button("Redo", () => run(Cmd.redo)),
    el("span", "spacer"),
    presets, target, render,
    button("Reload preview", () => run(Cmd.reloadPreview)),
    status,
  );

  // Say what Render will do, wherever the answer comes from: the button's tooltip is
  // where the click is, and it is the only place that was silent about it.
  const describe = () => {
    const id = renderMaster() ? store.state.project?.master : store.state.sequence;
    render.title = `Render ${binName(id)}${id === store.state.project?.master ? " — the master, the whole video" : " — this sequence on its own"}`;
  };
  target.onchange = describe;

  // The picker rebuilds its options, so it gets its own subscription: job progress ticks
  // many times a second and must not tear the dropdown down with it.
  store.select((s) => [s.project, s.sequence], (s) => {
    if (!s.project) return;
    const entry = (id: string) => ({ value: id, label: `${s.project!.bin[id].name || id}${id === s.project!.master ? " (master)" : ""}` });
    options(seqSel, [...sequenceIds(s.project).map(entry), { value: "+", label: "+ new sequence…" }], s.sequence);
    // The choice survives a switch — it is "the master" or "whichever is open", not a
    // sequence id — so only the labels are rebuilt, with the current names in them.
    const off = !!s.sequence && s.sequence !== s.project.master;
    target.hidden = !off;
    if (off) {
      const keep = target.value === "open" ? "open" : "master";
      options(target, [
        { value: "master", label: `master · ${binName(s.project.master)}` },
        { value: "open", label: `open · ${binName(s.sequence)}` },
      ], keep);
    }
    describe();
    const { width, height } = s.project;
    size.textContent = `${width}×${height} ⇄`;
    size.title = `Switch to ${orientation({ width: height, height: width })} (${height}×${width})`;
  });
  store.select((s) => [s.project, s.sequence, s.connected, s.jobs], (s) => {
    name.textContent = s.project?.name || "kuma";
    rev.textContent = s.project ? `r${s.project.revision}${s.connected ? "" : " · offline"}` : "";
    const running = Object.entries(s.jobs).filter(([, j]) => !j.done);
    const last = Object.values(s.jobs).at(-1);
    const output = last?.ok && last.result && typeof last.result === "object" && "output" in last.result ? String((last.result as { output: unknown }).output) : "";
    status.textContent = running.length ? running.map(([id, j]) => `${j.kind} #${id} ${j.percent}% ${j.message}`).join(" · ") : output && `wrote ${output}`;
  });
}
