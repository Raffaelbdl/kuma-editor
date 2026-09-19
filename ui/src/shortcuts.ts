// Keyboard shortcuts, as a table of key to command — one list to read, and a key and a
// menu item cannot drift because they name the same command. Bound in the page rather
// than as menu accelerators so they can tell whether a field has focus.
//
// The table is also what the **shortcut list** shows (`?`, or the button in the top bar):
// a binding carries how it is written and what it does, so the list is the bindings rather
// than a second copy of them that would quietly go stale. A binding that is the mirror of
// the one above it — `→` of `←` — says nothing and is not listed twice.

import { Cmd, command, run } from "./commands";
import { el, row } from "./dom";
import { showInfo } from "./modal";
import { SEQUENCE_TAB, store } from "./store";

/** Which part of the shortcut list a binding belongs to, in the order they are shown. */
const SECTIONS = ["Transport", "Editing", "Overlay workbench"] as const;
type Section = (typeof SECTIONS)[number];

/** Matched top to bottom; the first whose `when` passes wins. */
interface Binding {
  when(e: KeyboardEvent): boolean;
  /** A command id, or a function for the ones that carry a value (seeking). */
  action: string | ((e: KeyboardEvent) => void);
  /** Stop the browser doing its own thing (scrolling on Space, undo in a field). */
  preventDefault?: boolean;
  /** How this key is written in the list, and what it does there. Both or neither: a
   *  binding without them is not shown. */
  keys?: string;
  what?: string;
  where?: Section;
}

const mod = (e: KeyboardEvent) => e.ctrlKey || e.metaKey;
const frame = () => 1 / store.fps;
/** The overlay workbench is on screen, and has its own clock. */
const onOverlay = () => store.state.tab !== SEQUENCE_TAB;

const BINDINGS: Binding[] = [
  // Transport means the surface you are looking at: on the workbench it is that overlay's
  // own time, not the sequence's. Everything below here is the same key on the timeline.
  { when: (e) => e.code === "Space" && onOverlay(), action: Cmd.overlayTogglePlay, preventDefault: true,
    keys: "Space", what: "Play or pause this overlay on its own", where: "Overlay workbench" },
  { when: (e) => e.key === "ArrowLeft" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? -store.fps : -1), preventDefault: true,
    keys: "← →", what: "Step a frame in the overlay's own time (Shift: a second)", where: "Overlay workbench" },
  { when: (e) => e.key === "ArrowRight" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? store.fps : 1), preventDefault: true },
  { when: (e) => e.code === "Space", action: Cmd.togglePlay, preventDefault: true,
    keys: "Space", what: "Play or pause", where: "Transport" },
  // Alt is the coarse jump: to the next place something starts or ends. Before the plain
  // arrows, because the first match wins.
  { when: (e) => e.key === "ArrowLeft" && e.altKey, action: () => run(Cmd.seekEdge, -1), preventDefault: true,
    keys: "Alt + ← →", what: "To the next cut — any clip's start or end, on any track", where: "Transport" },
  { when: (e) => e.key === "ArrowRight" && e.altKey, action: () => run(Cmd.seekEdge, 1), preventDefault: true },
  // Every seek takes the key away from the browser: an arrow it is left to scrolls the
  // panel the last click focused something in, which looks like the timeline running away
  // from you and is the reason `preventDefault` is on all of these.
  { when: (e) => e.key === "ArrowLeft", action: (e) => store.seek(store.state.playhead - (e.shiftKey ? 1 : frame())), preventDefault: true,
    keys: "← →", what: "Step a frame (Shift: a second)", where: "Transport" },
  { when: (e) => e.key === "ArrowRight", action: (e) => store.seek(store.state.playhead + (e.shiftKey ? 1 : frame())), preventDefault: true },
  { when: (e) => e.key === "Home", action: () => store.seek(0), preventDefault: true,
    keys: "Home / End", what: "To the start, or to the end", where: "Transport" },
  { when: (e) => e.key === "End", action: () => store.seek(store.duration), preventDefault: true },
  { when: (e) => e.key.toLowerCase() === "s" && !mod(e) && !e.altKey, action: Cmd.split,
    keys: "S", what: "Split the selected clips at the playhead", where: "Editing" },
  { when: (e) => e.key === "Delete" || e.key === "Backspace", action: Cmd.deleteClip,
    keys: "Del", what: "Delete the selected clips", where: "Editing" },
  // The clipboard is the timeline's, so it is bound off the workbench only: the workbench
  // edits an entry and has no track to paste onto, and a paste landing on a timeline you
  // are not looking at is an edit out of sight. The guard above keeps Ctrl+C in a text
  // field doing what it has always done.
  { when: (e) => mod(e) && e.key.toLowerCase() === "c" && !onOverlay(), action: Cmd.copyClip, preventDefault: true,
    keys: "Ctrl + C / X", what: "Copy or cut the selected clips", where: "Editing" },
  { when: (e) => mod(e) && e.key.toLowerCase() === "x" && !onOverlay(), action: Cmd.cutClip, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "v" && !onOverlay(), action: Cmd.pasteClip, preventDefault: true,
    keys: "Ctrl + V", what: "Paste at the playhead, on the track you last clicked", where: "Editing" },
  // Ctrl/Cmd+G groups the selection, and with Shift ungroups it — the pair every editor
  // and every drawing program uses. Off the workbench, like the clipboard, because a group
  // is a relation between clips on a timeline.
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && !e.shiftKey && !onOverlay(), action: Cmd.group, preventDefault: true,
    keys: "Ctrl + G", what: "Group the selected clips: one click selects them all, a drag moves the block", where: "Editing" },
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && e.shiftKey && !onOverlay(), action: Cmd.ungroup, preventDefault: true,
    keys: "Ctrl + Shift + G", what: "Ungroup them", where: "Editing" },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && !e.shiftKey, action: Cmd.undo, preventDefault: true,
    keys: "Ctrl + Z", what: "Undo", where: "Editing" },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && e.shiftKey, action: Cmd.redo, preventDefault: true,
    keys: "Ctrl + Shift + Z", what: "Redo", where: "Editing" },
  { when: (e) => e.key === "?", action: () => showShortcuts(), preventDefault: true,
    keys: "?", what: "This list", where: "Editing" },
];

/**
 * The gestures that are not keys. They cannot be derived from anything — a pointer handler
 * is not a table — so this is the one place they are written down, and the list would be
 * dishonest without them: half of what this editor can do is done with the mouse.
 */
const GESTURES: { section: Section; does: string; what: string }[] = [
  { section: "Editing", does: "Ctrl + click a clip", what: "Add it to the selection, or take it out again" },
  { section: "Editing", does: "Drag a clip", what: "Move it — or the whole selection, or its group, when it is part of one" },
  { section: "Editing", does: "Drag a clip's edge", what: "Trim it. One clip's own, never the block's" },
  { section: "Editing", does: "Drag a number", what: "Change it (Shift: fine, Alt: coarse); click to type in it instead" },
  { section: "Editing", does: "Double-click a keyframe strip", what: "Add a keyframe there" },
  { section: "Transport", does: "Ctrl + wheel", what: "Zoom the timeline" },
  { section: "Transport", does: "Click a track's head", what: "Aim the next paste at that track" },
];

/** One line of the list: a chord to press, or a thing to do with the pointer. */
export interface ShortcutRow { keys?: string; does?: string; what: string }

/** The list as it is shown: the documented bindings, then the gestures, by section. */
export function shortcutSections(): { section: Section; rows: ShortcutRow[] }[] {
  const bound = BINDINGS.filter((b): b is Binding & { keys: string; what: string; where: Section } => !!b.keys && !!b.what && !!b.where);
  return SECTIONS.map((section) => ({
    section,
    rows: [
      ...bound.filter((b) => b.where === section).map(({ keys, what }) => ({ keys, what })),
      ...GESTURES.filter((g) => g.section === section).map(({ does, what }) => ({ does, what })),
    ],
  })).filter((s) => s.rows.length > 0);
}

/** Show the list. Registered as a command so the top bar's button, the `?` key and the
 *  inspector's link are all the same thing. */
export function showShortcuts(): void {
  const body = el("div", "shortcuts");
  for (const { section, rows } of shortcutSections()) {
    body.appendChild(el("h4", "", section));
    for (const r of rows) {
      const how = el("span", "sc-keys");
      // A chord is drawn as keys — `Ctrl + C / X` as three of them with the words between
      // — and a gesture as the sentence it is, because "Drag a clip" is three words, not
      // three keys to press.
      if (r.keys) for (const part of r.keys.split(" ")) how.appendChild(part === "+" || part === "/" ? el("span", "muted", part) : el("kbd", "", part));
      else how.appendChild(el("span", "sc-does", r.does ?? ""));
      body.appendChild(row("sc-row", how, el("span", "sc-what", r.what)));
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
    const b = BINDINGS.find((x) => x.when(e));
    if (!b) return;
    if (b.preventDefault) e.preventDefault();
    if (typeof b.action === "string") run(b.action);
    else b.action(e);
  });
}
