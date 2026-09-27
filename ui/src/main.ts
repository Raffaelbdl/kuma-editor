// Bootstrap: load the project, mount the panels, wire the surfaces that drive them.
// Nothing here knows how a panel works — they meet through `commands.ts`.

import { Cmd, command } from "./commands";
import { desktop, initDesktop, pickAndAddMedia } from "./desktop";
import { addMarker, addTrack, copySelection, createFromBlueprint, creatable, cutSelection, deleteSelectedClips, groupSelection, pasteClipboard, promptNewOverlay, promptNewSequence, splitAtPlayhead, swapOrientation, transcribe, ungroupSelection } from "./edits";
import { Inspector } from "./panels/inspector";
import { Library } from "./panels/library";
import { Preview } from "./panels/preview";
import { mountTabs } from "./panels/tabs";
import { mountTopbar } from "./panels/topbar";
import { mountTransport } from "./panels/transport";
import { installShortcuts } from "./shortcuts";
import { installSplitters } from "./splitters";
import { store, toast } from "./store";
import { installTheme } from "./theme";
import { Timeline } from "./panels/timeline";
import { initTitlebar } from "./panels/titlebar";
import { Workbench } from "./panels/workbench";
import type { TrackKind } from "./types";

/** Actions that belong to the document rather than to any one panel. */
function projectCommands() {
  command(Cmd.undo, "Undo", () => store.undo());
  command(Cmd.redo, "Redo", () => store.redo());
  command(Cmd.newSequence, "New Sequence…", () => promptNewSequence());
  command(Cmd.newOverlay, "New Overlay…", () => promptNewOverlay());
  // What the New menu offers as "Title", whichever folder it is in.
  command(Cmd.newTitle, "New Title", () => {
    const pick = creatable().find((c) => c.label === "Title");
    if (pick) void createFromBlueprint(pick.id);
    else toast("No title blueprint installed: run `kuma library` to install the starter set", "err", 8000);
  });
  command(Cmd.swapOrientation, "Switch Orientation", () => swapOrientation());
  command(Cmd.transcribe, "Transcribe → Subtitles", () => transcribe());
  command(Cmd.addMedia, "Add Media…", () => pickAndAddMedia());
  command(Cmd.split, "Split at Playhead", () => splitAtPlayhead());
  command(Cmd.seekEdge, "Go to Clip Edge", (dir) => store.seekEdge(Number(dir) < 0 ? -1 : 1));
  command(Cmd.deleteClip, "Delete Clips", () => deleteSelectedClips());
  command(Cmd.addMarker, "Add Marker…", () => addMarker());
  command(Cmd.copyClip, "Copy Clips", () => copySelection());
  command(Cmd.cutClip, "Cut Clips", () => cutSelection());
  command(Cmd.pasteClip, "Paste Clips", () => pasteClipboard());
  command(Cmd.group, "Group Clips", () => groupSelection());
  command(Cmd.ungroup, "Ungroup Clips", () => ungroupSelection());
  command(Cmd.addTrack, "Add Track", (kind) => addTrack(kind as TrackKind));
  // Record mode lost its button (see *Auto-key* in `docs/decisions.md`); a key keeps it
  // reachable until it has a better home. The whole page says it is on: a red edge on the
  // panel where the recording edits are made.
  command(Cmd.autoKey, "Toggle Auto-key", () => store.set({ autoKey: !store.state.autoKey }));
  let was = store.state.autoKey;
  store.select((s) => [s.autoKey], (s) => {
    document.body.classList.toggle("autokey", s.autoKey);
    if (s.autoKey !== was) toast(s.autoKey ? "Auto-key on: changing an effect's value writes a keyframe at the playhead" : "Auto-key off", "");
    was = s.autoKey;
  });
}

async function main() {
  // First, before the document is painted with anything: a theme swapped after the panels
  // are up is a flash of the wrong colours. It needs nothing from the daemon — the choice
  // is this browser's, not the project's.
  installTheme();

  installSplitters();
  await store.init();
  // Before the panels: the title bar adds a row to the grid, and settling the layout once
  // is tidier than having every panel measure itself and then measure again.
  const app = desktop();
  if (app) initTitlebar(app, store.state.project?.name || "kuma");

  projectCommands();
  // `Preview` registers the transport and reload commands as it mounts.
  new Preview(document.getElementById("preview")!);
  new Workbench(document.getElementById("workbench")!);
  mountTabs(document.getElementById("editor-tabs")!);
  new Timeline(document.getElementById("timeline")!);
  new Inspector(document.getElementById("inspector")!);
  new Library(document.getElementById("library")!);
  mountTopbar(document.getElementById("topbar")!);
  mountTransport(document.getElementById("transport")!);
  installShortcuts();

  // The desktop application menu drives the same commands as the toolbar; in a browser
  // this is a no-op and nothing above changes.
  initDesktop();
}

main().catch((e) => {
  console.error(e);
  // Startup reads the document's shape, so a crash here is nearly always a `ui/dist`
  // built from older sources than the project it was handed — `ui/dist` is not in git,
  // so a pull leaves it behind. The raw message ("t.tracks is not iterable") names
  // nothing anyone can act on, so say the likely cause next to it.
  toast(`Failed to start: ${e.message}`, "err", 60000);
  toast("This usually means ui/dist was built from older sources. Run `npm run rebuild` in the kuma checkout (or `npm run build` in ui/), then reload.", "err", 60000);
});
