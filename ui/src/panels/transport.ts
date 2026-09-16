// The transport strip under the preview: play, time, split, add track, preview quality,
// zoom. Like the top bar it runs commands rather than holding references to the panels
// that do the work — `+ track…` and the quality picker take an argument, which is why
// they pass one to `run` instead of being separate commands per value.

import { Cmd, run } from "../commands";
import { button, el, options } from "../dom";
import { fmtTime, store } from "../store";
import type { TrackKind } from "../types";

export function mountTransport(host: HTMLElement): void {
  const play = button("▶", () => run(Cmd.togglePlay));
  const time = el("span", "time");

  const zoom = el("input");
  zoom.type = "range"; zoom.min = "8"; zoom.max = "600"; zoom.step = "1";
  zoom.oninput = () => store.set({ zoom: Number(zoom.value) });

  const addTrackSel = el("select");
  addTrackSel.style.width = "auto";
  addTrackSel.title = "Add a track";
  options(addTrackSel, ["video", "overlay", "audio"].map((value) => ({ value })), undefined, "+ track…");
  addTrackSel.onchange = () => {
    const kind = addTrackSel.value as TrackKind | "";
    addTrackSel.value = "";
    if (kind) run(Cmd.addTrack, kind);
  };

  const quality = el("select");
  quality.style.width = "auto";
  quality.title = "Preview resolution (fraction of the project size)";
  options(quality, [{ value: "0.5", label: "preview ½" }, { value: "0.75", label: "preview ¾" }, { value: "1", label: "preview full" }]);
  quality.onchange = () => run(Cmd.previewScale, Number(quality.value));

  host.append(
    button("⏮", () => store.seek(0)),
    play, time,
    button("Split (S)", () => run(Cmd.split)),
    addTrackSel, quality,
    el("span", "muted", "zoom"), zoom,
  );

  store.subscribe((s) => {
    play.textContent = s.playing ? "⏸" : "▶";
    time.textContent = `${fmtTime(s.playhead, store.fps)} / ${fmtTime(store.duration, store.fps)}`;
    zoom.value = String(s.zoom);
  });
}
