// Windows of one editor, kept in step: a panel popped out to its own window (`?panel=`) is
// this same page, with its own store, so the view state that panels read — the selection,
// the playhead, which sequence and tab are open, the zoom — is sent between them on a
// `BroadcastChannel`. The document is not: every window reads it from the daemon and is
// told when it changes, as every client is.
//
// Same origin only, which is a daemon's port: windows of another project never hear it.

import { forwardUnknown, run } from "./commands";
import { panels } from "./panelhost";
import { store, type State } from "./store";

type Shared = Pick<State, "selection" | "playhead" | "playing" | "sequence" | "sequences" | "tab" | "overlays" | "zoom" | "track">;
type Message =
  | { type: "state"; from: string; state: Partial<Shared> }
  | { type: "hello"; from: string; panel: string | null }
  | { type: "bye"; from: string; panel: string | null }
  | { type: "run"; from: string; id: string; arg?: unknown };

const KEYS: (keyof Shared)[] = ["selection", "playhead", "sequence", "sequences", "tab", "overlays", "zoom", "track"];
const me = Math.random().toString(36).slice(2);

export function installViewSync(panel: string | null): void {
  if (typeof BroadcastChannel === "undefined") return;
  const ch = new BroadcastChannel("kuma-view");
  const post = (m: Message) => ch.postMessage(m);
  let applying = false;
  // A panel's window says nothing until the editor window has told it where things are, or
  // its defaults (playhead 0, nothing selected) would overwrite the editor's.
  let ready = !panel;
  const snapshot = (): Partial<Shared> => {
    const s = store.state;
    const out: Partial<Shared> = Object.fromEntries(KEYS.map((k) => [k, s[k]]));
    // Only the editor window plays: it has the engine. A panel's window follows.
    if (!panel) out.playing = s.playing;
    return out;
  };

  ch.onmessage = (e: MessageEvent<Message>) => {
    const m = e.data;
    if (!m || m.from === me) return;
    if (m.type === "state") {
      ready = true;
      applying = true;
      try { store.set(m.state); } finally { applying = false; }
    } else if (m.type === "hello") {
      // A new window: tell it where things are, and, if it is a panel, take ours away.
      if (!panel) {
        post({ type: "state", from: me, state: snapshot() });
        if (m.panel) panels.claimed(m.panel);
      }
    } else if (m.type === "bye") {
      if (!panel && m.panel) panels.returned(m.panel);
    } else if (m.type === "run") {
      // A command a panel's window does not have (play, reload): the editor window runs it.
      if (!panel) run(m.id, m.arg);
    }
  };

  // Every change of these, from here, goes to the others. The playhead ticks 25 times a
  // second while playing; a channel message is cheap, and a timeline in another window
  // that lags the picture is the thing this exists to prevent.
  store.select((s) => KEYS.map((k) => s[k]).concat(panel ? [] : [s.playing]), () => {
    if (!applying && ready) post({ type: "state", from: me, state: snapshot() });
  });

  if (panel) {
    forwardUnknown((id, arg) => { post({ type: "run", from: me, id, arg }); return true; });
    addEventListener("pagehide", () => post({ type: "bye", from: me, panel }));
  }
  post({ type: "hello", from: me, panel });
}
