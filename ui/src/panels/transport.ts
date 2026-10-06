// The transport, under the frame: back to the start, play, the time; then the preview's
// resolution, a reload, and the frame size, which is also the control that flips it.
// Zoom and "+ track" are the timeline's own, in its ruler's corner.

import { Cmd, run } from "../commands";
import { button, el, options, setText } from "../dom";
import { icon } from "../icons";
import { keysFor } from "../shortcuts";
import { fmtTime, store } from "../store";
import { orientation } from "../types";

/** Play/pause, its icon following the store (swapped only when playing flips: the playhead
 *  ticks 25 times a second). The transport's, and den's rail's. */
export function playButton(cls: string): HTMLButtonElement {
  const play = button(icon("play"), () => run(Cmd.togglePlay), `Play/pause (${keysFor(Cmd.togglePlay)})`, cls);
  store.select((s) => [s.playing], (s) => {
    play.dataset.playing = String(s.playing);
    play.replaceChildren(icon(s.playing ? "pause" : "play"));
  });
  return play;
}

export function mountTransport(host: HTMLElement): void {
  const play = playButton("icon");
  const time = el("span", "time");
  const now = el("span", "now");
  const dur = el("span", "dur");
  time.append(now, dur);

  const quality = el("select");
  quality.title = "Preview resolution (a fraction of the project's size)";
  options(quality, [{ value: "0.5", label: "½ res" }, { value: "0.75", label: "¾ res" }, { value: "1", label: "full res" }]);
  quality.onchange = () => run(Cmd.previewScale, Number(quality.value));

  // Spins while the reload it asked for is in flight: it can bake.
  const reload = button(icon("refresh", 13), () => run(Cmd.reloadPreview), "Reload the preview: recompile the timeline, bake the overlays of nested sequences, and say how it went", "icon reload");
  store.select((s) => [s.reloading], (s) => {
    reload.classList.toggle("busy", s.reloading);
    reload.disabled = s.reloading;
  });

  // Landscape or portrait is nothing but the frame size, so the control that shows it is
  // the control that flips it: one click swaps width and height.
  const size = button("", () => run(Cmd.swapOrientation), "", "size");

  host.append(
    button(icon("skip-back"), () => store.seek(0), "Back to the start (Home)", "icon"),
    play, time, el("span", "spacer"), quality, reload, size,
  );

  store.select((s) => [s.project], (s) => {
    if (!s.project) return;
    const { width, height } = s.project;
    size.textContent = `${width}×${height} ⇄`;
    size.title = `Switch to ${orientation({ width: height, height: width })} (${height}×${width})`;
  });
  store.select((s) => [s.playhead, s.project], (s) => setText(now, fmtTime(s.playhead, store.fps)));
  store.select((s) => [s.project, s.sequence], () => setText(dur, ` / ${fmtTime(store.duration, store.fps)}`));
}
