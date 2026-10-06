// Keyboard shortcuts, as a table of key to command: a key and a menu item cannot drift
// because they name the same command. Bound in the page, not as accelerators, so they can
// tell whether a field has focus.
//
// The table is also the **shortcut list** (`?`, or the top bar's button): each binding
// carries how it is written and what it does, so the list cannot go stale. One that
// mirrors the binding above it — `→` of `←` — carries no text and is not listed twice.

import { Cmd, command, run } from "./commands";
import { el, row } from "./dom";
import { showInfo } from "./modal";
import { panels } from "./panelhost";
import { SEQUENCE_TAB, store } from "./store";

/** Which part of the shortcut list a binding belongs to, in the order they are shown. */
const SECTIONS = ["Panels", "Transport", "Editing", "Overlay workbench"] as const;
type Section = (typeof SECTIONS)[number];

/** Matched top to bottom; the first whose `when` passes wins. */
interface Binding {
  when(e: KeyboardEvent): boolean;
  /** A command id, or a function for the ones that carry a value (seeking). */
  action: string | ((e: KeyboardEvent) => void);
  /** Stop the browser doing its own thing (scrolling on Space, undo in a field). */
  preventDefault?: boolean;
  /** Its line in the shortcut list: where it goes, how the key is written and what it
   *  does. A binding without one is not shown — the mirror of the one above it. */
  list?: { where: Section; keys: string; what: string };
}

const mod = (e: KeyboardEvent) => e.ctrlKey || e.metaKey;
/** A bare letter: no Ctrl, Cmd or Alt (Shift is its own binding). */
const bare = (e: KeyboardEvent, k: string) => e.key.toLowerCase() === k && !mod(e) && !e.altKey && !e.shiftKey;
/** The panel letters, read off the panels that have one (`PanelSpec.key`): in den they
 *  summon a panel, in power they bring it forward. The panels register after this module
 *  loads, so they are asked for each time; `PANELS` marks where they go in the table. */
const panelBindings = (): Binding[] => panels.all.filter((s) => s.key).map((s) => ({
  when: (e: KeyboardEvent) => bare(e, s.letter), action: `panel.${s.id}`, list: { where: "Panels" as const, keys: s.letter.toUpperCase(), what: s.key! },
}));
const PANELS: Binding = { when: () => false, action: "" };
const bindings = (): Binding[] => BINDINGS.flatMap((b) => (b === PANELS ? panelBindings() : [b]));
const frame = () => 1 / store.fps;
/** The overlay workbench is on screen, and has its own clock. */
const onOverlay = () => store.state.tab !== SEQUENCE_TAB;

const BINDINGS: Binding[] = [
  { when: (e) => mod(e) && e.key.toLowerCase() === "k", action: Cmd.palette, preventDefault: true,
    list: { where: "Panels", keys: "Ctrl + K", what: "Every command, by name" } },
  { when: (e) => e.key === "`" && !mod(e) && !e.altKey, action: Cmd.toggleMode, preventDefault: true,
    list: { where: "Panels", keys: "`", what: "Den or power: the frame and what the agent did, or every panel docked" } },
  PANELS,
  // A pin is a note on the frame you are looking at; the workbench has no timeline to
  // hold one.
  { when: (e) => bare(e, "p") && !onOverlay(), action: Cmd.addPin, preventDefault: true,
    list: { where: "Panels", keys: "P", what: "Pin the playhead: write a note on this frame for the agent" } },
  // Transport means the surface you are looking at: on the workbench it is that overlay's
  // own time, not the sequence's. Everything below here is the same key on the timeline.
  { when: (e) => e.code === "Space" && onOverlay(), action: Cmd.overlayTogglePlay, preventDefault: true,
    list: { where: "Overlay workbench", keys: "Space", what: "Play or pause this overlay on its own" } },
  { when: (e) => e.key === "ArrowLeft" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? -store.fps : -1), preventDefault: true,
    list: { where: "Overlay workbench", keys: "← →", what: "Step a frame in the overlay's own time (Shift: a second)" } },
  { when: (e) => e.key === "ArrowRight" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? store.fps : 1), preventDefault: true },
  { when: (e) => e.code === "Space", action: Cmd.togglePlay, preventDefault: true,
    list: { where: "Transport", keys: "Space", what: "Play or pause" } },
  // Alt is the coarse jump: to the next place something starts or ends. Before the plain
  // arrows, because the first match wins.
  { when: (e) => e.key === "ArrowLeft" && e.altKey, action: () => run(Cmd.seekEdge, -1), preventDefault: true,
    list: { where: "Transport", keys: "Alt + ← →", what: "To the next cut — any clip's start or end, on any track" } },
  { when: (e) => e.key === "ArrowRight" && e.altKey, action: () => run(Cmd.seekEdge, 1), preventDefault: true },
  // Every seek takes the key away from the browser: an arrow it is left to scrolls the
  // panel the last click focused something in, which looks like the timeline running away
  // from you and is the reason `preventDefault` is on all of these.
  { when: (e) => e.key === "ArrowLeft", action: (e) => store.seek(store.state.playhead - (e.shiftKey ? 1 : frame())), preventDefault: true,
    list: { where: "Transport", keys: "← →", what: "Step a frame (Shift: a second)" } },
  { when: (e) => e.key === "ArrowRight", action: (e) => store.seek(store.state.playhead + (e.shiftKey ? 1 : frame())), preventDefault: true },
  { when: (e) => e.key === "Home", action: () => store.seek(0), preventDefault: true,
    list: { where: "Transport", keys: "Home / End", what: "To the start, or to the end" } },
  { when: (e) => e.key === "End", action: () => store.seek(store.duration), preventDefault: true },
  { when: (e) => e.key.toLowerCase() === "s" && !mod(e) && !e.altKey, action: Cmd.split,
    list: { where: "Editing", keys: "S", what: "Split the selected clips at the playhead" } },
  // Auto-key gave R to the render queue (the letters are the panels' now) and kept it with
  // Shift: still R for record, as Kdenlive and Blender have it.
  { when: (e) => e.key.toLowerCase() === "r" && e.shiftKey && !mod(e) && !e.altKey && !onOverlay(), action: Cmd.autoKey,
    list: { where: "Editing", keys: "Shift + R", what: "Auto-key: while on, changing an effect's value writes a keyframe at the playhead" } },
  { when: (e) => e.key === "Delete" || e.key === "Backspace", action: Cmd.deleteClip,
    list: { where: "Editing", keys: "Del", what: "Delete the selected clips" } },
  // The clipboard is the timeline's, so it is bound off the workbench only: the workbench
  // edits an entry and has no track to paste onto, and a paste landing on a timeline you
  // are not looking at is an edit out of sight. The guard above keeps Ctrl+C in a text
  // field doing what it has always done.
  { when: (e) => mod(e) && e.key.toLowerCase() === "c" && !onOverlay(), action: Cmd.copyClip, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + C / X", what: "Copy or cut the selected clips" } },
  { when: (e) => mod(e) && e.key.toLowerCase() === "x" && !onOverlay(), action: Cmd.cutClip, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "v" && !onOverlay(), action: Cmd.pasteClip, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + V", what: "Paste at the playhead, on the track you last clicked" } },
  // Ctrl/Cmd+G groups the selection, and with Shift ungroups it — the pair every editor
  // and every drawing program uses. Off the workbench, like the clipboard, because a group
  // is a relation between clips on a timeline.
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && !e.shiftKey && !onOverlay(), action: Cmd.group, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + G", what: "Group the selected clips: one click selects them all, a drag moves the block" } },
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && e.shiftKey && !onOverlay(), action: Cmd.ungroup, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + Shift + G", what: "Ungroup them" } },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && !e.shiftKey, action: Cmd.undo, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + Z", what: "Undo" } },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && e.shiftKey, action: Cmd.redo, preventDefault: true,
    list: { where: "Editing", keys: "Ctrl + Shift + Z", what: "Redo" } },
  { when: (e) => e.key === "?", action: Cmd.shortcuts, preventDefault: true,
    list: { where: "Editing", keys: "?", what: "This list" } },
];

/**
 * The gestures that are not keys. They cannot be derived from anything — a pointer handler
 * is not a table — so this is the one place they are written down, and the list would be
 * dishonest without them: half of what this editor can do is done with the mouse.
 */
const GESTURES: { section: Section; does: string; what: string }[] = [
  { section: "Editing", does: "Ctrl + click a clip", what: "Add it to the selection, or take it out again" },
  { section: "Editing", does: "Drag a clip", what: "Move it — or the whole selection, or its group, when it is part of one" },
  { section: "Editing", does: "Drag onto another track", what: "Moves every clip being dragged by that many tracks" },
  { section: "Editing", does: "Drag a clip's edge", what: "Trim it. One clip's own, never the block's" },
  { section: "Editing", does: "Drag a number", what: "Change it (Shift: fine, Alt: coarse); click to type in it instead" },
  { section: "Editing", does: "Double-click a keyframe strip", what: "Add a keyframe there" },
  { section: "Editing", does: "Drag a marker's flag", what: "Move it; click it to go there" },
  { section: "Editing", does: "Double-click a marker's flag", what: "Edit its comment (right-click: delete it)" },
  { section: "Editing", does: "Right-click a clip", what: "Split it there, add a transition, copy, group, delete" },
  { section: "Editing", does: "Right-click the ruler", what: "Add a marker there (a pin, when it has a comment)" },
  { section: "Panels", does: "Drag a panel's title bar", what: "Move it; near an edge it docks there, past the window's edge it becomes a window" },
  { section: "Panels", does: "⧉ on a panel", what: "Open it in a window of its own" },
  { section: "Editing", does: "Double-click a track's name", what: "Rename it (or right-click its head)" },
  { section: "Editing", does: "Drag a track's bottom edge", what: "Make it taller or shorter; its caret folds it to a line" },
  { section: "Transport", does: "Ctrl + wheel", what: "Zoom the timeline around the pointer" },
  { section: "Transport", does: "Middle-click a tab", what: "Close it" },
  { section: "Transport", does: "Click a track's head", what: "Aim the next paste at that track" },
];

/** How a command's key is written, for the places that name one beside a button or in a
 *  tooltip — read off the binding, so it cannot say something the key does not do. */
export function keysFor(cmd: string): string | undefined {
  return bindings().find((b) => b.action === cmd)?.list?.keys;
}

/** Show the list: the documented bindings, then the gestures, by section. Registered as a
 *  command so the top bar's button, the `?` key and the inspector's link are all the same
 *  thing. */
function showShortcuts(): void {
  const body = el("div", "shortcuts");
  const line = (how: HTMLElement, what: string) => body.appendChild(row("sc-row", how, el("span", "sc-what", what)));
  for (const section of SECTIONS) {
    body.appendChild(el("h4", "", section));
    // A chord is drawn as keys — `Ctrl + C / X` as three of them with the words between —
    // and a gesture as the sentence it is, because "Drag a clip" is three words, not three
    // keys to press.
    for (const b of bindings()) {
      if (b.list?.where !== section) continue;
      const how = el("span", "sc-keys");
      for (const part of b.list.keys.split(" ")) how.appendChild(part === "+" || part === "/" ? el("span", "muted", part) : el("kbd", "", part));
      line(how, b.list.what);
    }
    for (const g of GESTURES) {
      if (g.section !== section) continue;
      const how = el("span", "sc-keys");
      how.appendChild(el("span", "sc-does", g.does));
      line(how, g.what);
    }
  }
  // Mac writes ⌘ where this says Ctrl, and the bindings take either.
  if (navigator.platform.startsWith("Mac")) body.appendChild(el("p", "muted tiny", "Ctrl means ⌘ on this machine."));
  // Wider than a prompt: two columns of text, and a description that wraps on every line
  // turns a list you scan into a paragraph you read.
  showInfo({ title: "Shortcuts", body, className: "wide" });
}

export function installShortcuts(): void {
  command(Cmd.shortcuts, "Shortcuts", () => showShortcuts());
  window.addEventListener("keydown", (e) => {
    const target = e.target as HTMLElement;
    // Typing in a field is typing, not a shortcut. A slider is the same: the arrows are
    // how it is dragged from the keyboard.
    if (target.closest?.("input, textarea, select, [contenteditable]")) return;
    // A button owns only the keys that press it. It used to own all of them, which meant
    // that after clicking a track's mute — the last thing clicked keeps the focus — the
    // arrows did nothing here and scrolled the timeline instead.
    if (target.closest?.("button") && (e.code === "Space" || e.key === "Enter")) return;
    const b = bindings().find((x) => x.when(e));
    if (!b) return;
    if (b.preventDefault) e.preventDefault();
    if (typeof b.action === "string") run(b.action);
    else b.action(e);
  });
}
