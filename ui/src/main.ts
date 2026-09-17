// Bootstrap: load the project, mount the panels, wire the surfaces that drive them.
// Nothing here knows how a panel works — they meet through `commands.ts`.

import { Cmd, command } from "./commands";
import { desktop, initDesktop, pickAndAddMedia } from "./desktop";
import { addTrack, deleteSelectedClip, promptNewSequence, splitAtPlayhead, swapOrientation, transcribe } from "./edits";
import { Inspector } from "./panels/inspector";
import { Library } from "./panels/library";
import { Preview } from "./panels/preview";
import { mountTopbar } from "./panels/topbar";
import { mountTransport } from "./panels/transport";
import { installShortcuts } from "./shortcuts";
import { store, toast } from "./store";
import { Timeline } from "./panels/timeline";
import { initTitlebar } from "./panels/titlebar";
import type { TrackKind } from "./types";

/** Actions that belong to the document rather than to any one panel. */
function projectCommands() {
  command(Cmd.undo, "Undo", () => store.undo());
  command(Cmd.redo, "Redo", () => store.redo());
  command(Cmd.newSequence, "New Sequence…", () => promptNewSequence());
  command(Cmd.swapOrientation, "Switch Orientation", () => swapOrientation());
  command(Cmd.transcribe, "Transcribe → Subtitles", () => transcribe());
  command(Cmd.addMedia, "Add Media…", () => pickAndAddMedia());
  command(Cmd.split, "Split at Playhead", () => splitAtPlayhead());
  command(Cmd.deleteClip, "Delete Clip", () => deleteSelectedClip());
  command(Cmd.addTrack, "Add Track", (kind) => addTrack(kind as TrackKind));
}

async function main() {
  await store.init();
  // Before the panels: the title bar adds a row to the grid, and settling the layout once
  // is tidier than having every panel measure itself and then measure again.
  const app = desktop();
  if (app) initTitlebar(app, store.state.project?.name || "kuma");

  projectCommands();
  // `Preview` registers the transport and reload commands as it mounts.
  new Preview(document.getElementById("preview")!);
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
