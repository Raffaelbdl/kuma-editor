// The transport strip: play, time, split, add track, preview quality, zoom. Runs
// commands; the two pickers pass their value as the command's argument.

import { Cmd, run } from "../commands";
import { button, el, options, plural } from "../dom";
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

  // How many clips a keypress would act on. Copy, cut and paste had buttons here and lost
  // them: three more buttons for gestures everyone already knows the keys for, and the
  // strip is a transport. What is worth saying is the thing the outlines make you count.
  const selected = el("span", "muted sel-count");

  host.append(
    button("⏮", () => store.seek(0)),
    play, time,
    button("Split (S)", () => run(Cmd.split)),
    selected,
    addTrackSel, quality,
    el("span", "muted", "zoom"), zoom,
  );

  store.subscribe((s) => {
    play.textContent = s.playing ? "⏸" : "▶";
    time.textContent = `${fmtTime(s.playhead, store.fps)} / ${fmtTime(store.duration, store.fps)}`;
    zoom.value = String(s.zoom);
    // One clip is visible as one outline; several are worth counting for you. The
    // clipboard's own count stays out of it — it is not what this strip is about.
    const n = store.selectedClips.length;
    selected.textContent = n > 1 ? `${plural(n, "clip")} selected` : "";
    selected.title = n > 1 ? "Copy, cut, delete and split act on all of them" : "";
  });
}
