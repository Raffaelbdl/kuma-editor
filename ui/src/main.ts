// Bootstrap: load the project, mount the panels, wire the surfaces that drive them.
// Nothing here knows how a panel works — they meet through `commands.ts`, and where each
// one is on screen is `panelhost.ts`'s business.

import { Cmd, command } from "./commands";
import { desktop, initDesktop, pickAndAddMedia } from "./desktop";
import { el } from "./dom";
import { addTrack, copySelection, createFromBlueprint, creatable, cutSelection, deleteSelectedClips, groupSelection, pasteClipboard, promptNewOverlay, promptNewSequence, splitAt, swapOrientation, transcribe, ungroupSelection } from "./edits";
import { panelCommands, panels } from "./panelhost";
import { installPalette } from "./palette";
import { agentPanel } from "./panels/agentpanel";
import { mountDen } from "./panels/den";
import { fontsPanel } from "./panels/fonts";
import { historyPanel } from "./panels/history";
import { Inspector } from "./panels/inspector";
import { Library } from "./panels/library";
import { markersPanel } from "./panels/markers";
import { Preview } from "./panels/preview";
import { queuePanel } from "./panels/queue";
import { scopesPanel } from "./panels/scopes";
import { mountRail } from "./panels/rail";
import { mountStatusline } from "./panels/statusline";
import { mountTopbar } from "./panels/topbar";
import { mountTransport } from "./panels/transport";
import { installShortcuts } from "./shortcuts";
import { binName, store, toast } from "./store";
import { installTheme } from "./theme";
import { Timeline } from "./panels/timeline";
import { initTitlebar } from "./panels/titlebar";
import { installViewSync } from "./viewsync";
import { Viewer } from "./panels/viewer";
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
  command(Cmd.split, "Split at Playhead", () => splitAt(store.state.playhead));
  command(Cmd.seekEdge, "Go to Clip Edge", (dir) => store.seekEdge(Number(dir) < 0 ? -1 : 1));
  command(Cmd.deleteClip, "Delete Clips", () => deleteSelectedClips());
  command(Cmd.copyClip, "Copy Clips", () => copySelection());
  command(Cmd.cutClip, "Cut Clips", () => cutSelection());
  command(Cmd.pasteClip, "Paste Clips", () => pasteClipboard());
  command(Cmd.group, "Group Clips", () => groupSelection());
  command(Cmd.ungroup, "Ungroup Clips", () => ungroupSelection());
  command(Cmd.addTrack, "Add Track", (kind) => addTrack(kind as TrackKind));
  // Record mode lost its button (see *Auto-key* in `docs/decisions.md`); a key keeps it
  // reachable until it has a better home — Shift+R now, R being the render queue's letter. The whole page says it is on: a red edge on the
  // panel where the recording edits are made.
  command(Cmd.autoKey, "Toggle Auto-key", () => {
    const on = !store.state.autoKey;
    store.set({ autoKey: on });
    toast(on ? "Auto-key on: changing an effect's value writes a keyframe at the playhead" : "Auto-key off", "");
  });
  store.select((s) => [s.autoKey], (s) => document.body.classList.toggle("autokey", s.autoKey));
}

/** The panels, as modules: each draws into one element of its own, and the panel host
 *  (`panelhost.ts`) moves that element between containers. */
function registerPanels() {
  // In the order den offers their keys (`b i t a`, then the side panels' `m h r`); the side
  // panels' order is also the rail's.
  const libraryEl = el("aside");
  libraryEl.id = "library";
  const library = new Library(libraryEl, "bin");
  panels.register({ id: "bin", letter: "b", key: "The bin", title: "bin", body: libraryEl, tools: library.tools, dock: "side", size: { w: 300, h: 420 } });
  const inspectorEl = el("aside");
  inspectorEl.id = "inspector";
  new Inspector(inspectorEl);
  panels.register({ id: "inspector", letter: "i", key: "The inspector", title: "inspector", body: inspectorEl, dock: "right", size: { w: 300, h: 460 } });

  const timelineEl = el("div");
  timelineEl.id = "timeline";
  new Timeline(timelineEl);
  const seqNote = el("span");
  store.select((s) => [s.project, s.sequence], (s) => { seqNote.textContent = binName(s.sequence); });
  panels.register({ id: "timeline", letter: "t", key: "The timeline", title: "timeline", body: timelineEl, note: seqNote, dock: "bottom", size: { w: 1180, h: 260 } });

  const agent = agentPanel();
  panels.register({ id: "agent", letter: "a", key: "The agent: what it is doing, its last calls", title: "agent", body: agent.body, note: agent.note, dock: null, size: { w: 300, h: 300 } });

  const blueprintsEl = el("aside");
  blueprintsEl.id = "blueprints";
  const blueprints = new Library(blueprintsEl, "blueprints");
  panels.register({ id: "blueprints", letter: "p", title: "blueprints", body: blueprintsEl, tools: blueprints.tools, dock: "side", size: { w: 300, h: 420 } });
  const fonts = fontsPanel();
  panels.register({ id: "fonts", letter: "f", title: "fonts", body: fonts.body, tools: fonts.tools, dock: "side" });
  const markers = markersPanel();
  panels.register({ id: "markers", letter: "m", key: "The markers: notes on frames, for the agent or for you", title: "markers", body: markers.body, note: markers.note, dock: "side", size: { w: 300, h: 300 } });
  const history = historyPanel();
  panels.register({ id: "history", letter: "h", key: "The history: every step, yours and the agent's", title: "history", body: history.body, tools: history.tools, dock: "side", size: { w: 380, h: 340 } });
  const scopes = scopesPanel();
  panels.register({ id: "scopes", letter: "s", title: "scopes", body: scopes.body, tools: scopes.tools, show: scopes.show, dock: "side", size: { w: 360, h: 420 } });
  const queue = queuePanel();
  panels.register({ id: "queue", letter: "r", key: "The render queue", title: "render queue", body: queue.body, tools: queue.tools, dock: "side", size: { w: 330, h: 380 } });
}

async function main() {
  // First, before the document is painted with anything: a theme swapped after the panels
  // are up is a flash of the wrong colours. It needs nothing from the daemon — the choice
  // is this browser's, not the project's.
  installTheme();
  // A panel in a window of its own is this page with `?panel=<id>`: that panel, alone.
  const solo = new URLSearchParams(location.search).get("panel");
  store.set({ mode: panels.initialMode() });
  await store.init();
  // The panels first: the dock is read back knowing which ones there are. They draw into
  // elements of their own, parked until the dock puts them somewhere.
  registerPanels();
  // The dock's containers, with the frame in its place, before the preview gives the frame
  // iframes that moving it would reload.
  if (!solo) panels.layoutDock();
  // Before the panels: the title bar adds a row to the grid, and settling the layout once
  // is tidier than having every panel measure itself and then measure again. A panel's
  // own window has no menu: its head is its title bar.
  const app = desktop();
  if (app && !solo) initTitlebar(app, store.state.project?.name || "kuma");

  projectCommands();
  if (!solo) {
    // `Preview` registers the transport and reload commands as it mounts.
    new Preview(document.getElementById("preview")!);
    new Workbench(document.getElementById("workbench")!);
    new Viewer(document.getElementById("viewer")!);
    mountTopbar(document.getElementById("topbar")!);
    mountTransport(document.getElementById("transport")!);
    mountStatusline(document.getElementById("statusline")!);
  }
  // A panel's own window has no dock and no modes: its letters, ⌘K's "Den mode" and the
  // rest are the editor window's, and reach it as commands this page does not have.
  if (!solo) panelCommands();
  installPalette();
  installShortcuts();
  if (solo) {
    if (!panels.mountSolo(solo)) toast(`There is no panel called “${solo}”`, "err", 60000);
  } else {
    mountRail(document.getElementById("rail")!);
    mountDen(document.getElementById("den")!);
    panels.apply();
  }
  installViewSync(solo);

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
