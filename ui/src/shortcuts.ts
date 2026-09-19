// Keyboard shortcuts, as a table of key to command — one list to read, and a key and a
// menu item cannot drift because they name the same command. Bound in the page rather
// than as menu accelerators so they can tell whether a field has focus.

import { Cmd, run } from "./commands";
import { SEQUENCE_TAB, store } from "./store";

/** Matched top to bottom; the first whose `when` passes wins. */
interface Binding {
  when(e: KeyboardEvent): boolean;
  /** A command id, or a function for the ones that carry a value (seeking). */
  action: string | ((e: KeyboardEvent) => void);
  /** Stop the browser doing its own thing (scrolling on Space, undo in a field). */
  preventDefault?: boolean;
}

const mod = (e: KeyboardEvent) => e.ctrlKey || e.metaKey;
const frame = () => 1 / store.fps;
/** The overlay workbench is on screen, and has its own clock. */
const onOverlay = () => store.state.tab !== SEQUENCE_TAB;

const BINDINGS: Binding[] = [
  // Transport means the surface you are looking at: on the workbench it is that overlay's
  // own time, not the sequence's. Everything below here is the same key on the timeline.
  { when: (e) => e.code === "Space" && onOverlay(), action: Cmd.overlayTogglePlay, preventDefault: true },
  { when: (e) => e.key === "ArrowLeft" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? -store.fps : -1), preventDefault: true },
  { when: (e) => e.key === "ArrowRight" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? store.fps : 1), preventDefault: true },
  { when: (e) => e.code === "Space", action: Cmd.togglePlay, preventDefault: true },
  // Alt is the coarse jump: to the next place something starts or ends. Before the plain
  // arrows, because the first match wins.
  { when: (e) => e.key === "ArrowLeft" && e.altKey, action: () => run(Cmd.seekEdge, -1), preventDefault: true },
  { when: (e) => e.key === "ArrowRight" && e.altKey, action: () => run(Cmd.seekEdge, 1), preventDefault: true },
  // Every seek takes the key away from the browser: an arrow it is left to scrolls the
  // panel the last click focused something in, which looks like the timeline running away
  // from you and is the reason `preventDefault` is on all of these.
  { when: (e) => e.key === "ArrowLeft", action: (e) => store.seek(store.state.playhead - (e.shiftKey ? 1 : frame())), preventDefault: true },
  { when: (e) => e.key === "ArrowRight", action: (e) => store.seek(store.state.playhead + (e.shiftKey ? 1 : frame())), preventDefault: true },
  { when: (e) => e.key === "Home", action: () => store.seek(0), preventDefault: true },
  { when: (e) => e.key === "End", action: () => store.seek(store.duration), preventDefault: true },
  { when: (e) => e.key.toLowerCase() === "s" && !mod(e) && !e.altKey, action: Cmd.split },
  { when: (e) => e.key === "Delete" || e.key === "Backspace", action: Cmd.deleteClip },
  // The clipboard is the timeline's, so it is bound off the workbench only: the workbench
  // edits an entry and has no track to paste onto, and a paste landing on a timeline you
  // are not looking at is an edit out of sight. The guard above keeps Ctrl+C in a text
  // field doing what it has always done.
  { when: (e) => mod(e) && e.key.toLowerCase() === "c" && !onOverlay(), action: Cmd.copyClip, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "x" && !onOverlay(), action: Cmd.cutClip, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "v" && !onOverlay(), action: Cmd.pasteClip, preventDefault: true },
  // Ctrl/Cmd+G groups the selection, and with Shift ungroups it — the pair every editor
  // and every drawing program uses. Off the workbench, like the clipboard, because a group
  // is a relation between clips on a timeline.
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && !e.shiftKey && !onOverlay(), action: Cmd.group, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "g" && e.shiftKey && !onOverlay(), action: Cmd.ungroup, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && !e.shiftKey, action: Cmd.undo, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && e.shiftKey, action: Cmd.redo, preventDefault: true },
];

export function installShortcuts(): void {
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
