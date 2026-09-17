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
  { when: (e) => e.key === "ArrowLeft" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? -store.fps : -1) },
  { when: (e) => e.key === "ArrowRight" && onOverlay(), action: (e) => run(Cmd.overlayStep, e.shiftKey ? store.fps : 1) },
  { when: (e) => e.code === "Space", action: Cmd.togglePlay, preventDefault: true },
  { when: (e) => e.key === "ArrowLeft", action: (e) => store.seek(store.state.playhead - (e.shiftKey ? 1 : frame())) },
  { when: (e) => e.key === "ArrowRight", action: (e) => store.seek(store.state.playhead + (e.shiftKey ? 1 : frame())) },
  { when: (e) => e.key === "Home", action: () => store.seek(0) },
  { when: (e) => e.key === "End", action: () => store.seek(store.duration) },
  { when: (e) => e.key.toLowerCase() === "s" && !mod(e) && !e.altKey, action: Cmd.split },
  { when: (e) => e.key === "Delete" || e.key === "Backspace", action: Cmd.deleteClip },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && !e.shiftKey, action: Cmd.undo, preventDefault: true },
  { when: (e) => mod(e) && e.key.toLowerCase() === "z" && e.shiftKey, action: Cmd.redo, preventDefault: true },
];

export function installShortcuts(): void {
  window.addEventListener("keydown", (e) => {
    // Typing in a field is typing, not a shortcut.
    if ((e.target as HTMLElement).closest?.("input, textarea, select, button, [contenteditable]")) return;
    const b = BINDINGS.find((x) => x.when(e));
    if (!b) return;
    if (b.preventDefault) e.preventDefault();
    if (typeof b.action === "string") run(b.action);
    else b.action(e);
  });
}
