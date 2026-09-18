// The transport strip: play, time, split, add track, preview quality, zoom. Runs
// commands; the two pickers pass their value as the command's argument.

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

  // The clipboard, where the other timeline actions are: the keys are the fast way, but
  // nothing else on screen says the editor has one.
  const copy = button("Copy", () => run(Cmd.copyClip), "Copy the selected clip (Ctrl/Cmd+C)");
  const cut = button("Cut", () => run(Cmd.cutClip), "Cut the selected clip (Ctrl/Cmd+X)");
  const paste = button("Paste", () => run(Cmd.pasteClip), "Paste at the playhead, on the track you last clicked (Ctrl/Cmd+V)");

  host.append(
    button("⏮", () => store.seek(0)),
    play, time,
    button("Split (S)", () => run(Cmd.split)),
    copy, cut, paste,
    addTrackSel, quality,
    el("span", "muted", "zoom"), zoom,
  );

  store.subscribe((s) => {
    play.textContent = s.playing ? "⏸" : "▶";
    time.textContent = `${fmtTime(s.playhead, store.fps)} / ${fmtTime(store.duration, store.fps)}`;
    zoom.value = String(s.zoom);
    // Say what is possible rather than explaining it after the click.
    const clip = s.selection?.kind === "clip";
    copy.disabled = !clip;
    cut.disabled = !clip;
    paste.disabled = !s.clipboard;
    if (s.clipboard) {
      const track = store.tracks.find((t) => t.id === s.track && t.kind === s.clipboard!.kind) ?? store.tracks.find((t) => t.kind === s.clipboard!.kind);
      paste.title = `Paste onto ${track ? track.name || track.id : `a new ${s.clipboard.kind} track`} at the playhead (Ctrl/Cmd+V)`;
    }
  });
}
