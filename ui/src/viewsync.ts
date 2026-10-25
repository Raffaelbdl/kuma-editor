// Windows of one editor, kept in step: a panel popped out to its own window (`?panel=`) is
// this same page, with its own store, so the view state that panels read — the selection,
// the playhead, which sequence and tab are open, the zoom — is sent between them on a
// `BroadcastChannel`. The document is not: every window reads it from the daemon and is
// told when it changes, as every client is.
//
// Same origin only, which is a daemon's port: windows of another project never hear it.

import { forwardUnknown, run } from "./commands";
import { desktop } from "./desktop";
import { panels } from "./panelhost";
import { store, type State } from "./store";

type Shared = Pick<State, "selection" | "playhead" | "playing" | "sequence" | "sequences" | "tab" | "entries" | "zoom" | "track">;
type Message =
  | { type: "state"; from: string; state: Partial<Shared> }
  | { type: "hello"; from: string; panel: string | null }
  | { type: "bye"; from: string; panel: string | null }
  | { type: "alive"; from: string; panel: string }
  | { type: "recall"; from: string; panel: string }
  | { type: "run"; from: string; id: string; arg?: unknown };

const KEYS: (keyof Shared)[] = ["selection", "playhead", "sequence", "sequences", "tab", "entries", "zoom", "track"];
const me = Math.random().toString(36).slice(2);
/** A panel's window says it is there this often; the editor window takes the panel back when
 *  it has not heard for `LOST`, after a `GRACE` for the window to load. */
const BEAT = 1000;
const LOST = 4000;
const GRACE = 15000;

export function installViewSync(panel: string | null): void {
  if (typeof BroadcastChannel === "undefined") return;
  const ch = new BroadcastChannel("kuma-view");
  const post = (m: Message) => ch.postMessage(m);
  let applying = false;
  // A panel's window says nothing until the editor window has told it where things are, or
  // its defaults (playhead 0, nothing selected) would overwrite the editor's.
  let ready = !panel;
  // Nobody to tell until another window has spoken: an editor window alone keeps quiet
  // through playback. A panel's window always has the editor's.
  let others = !!panel;
  /** What this window last told the others. */
  let sent: Partial<Shared> = {};
  // Only the editor window plays: it has the engine. A panel's window follows.
  const keys: (keyof Shared)[] = panel ? KEYS : [...KEYS, "playing"];
  const snapshot = (): Partial<Shared> => Object.fromEntries(keys.map((k) => [k, store.state[k]]));

  ch.onmessage = (e: MessageEvent<Message>) => {
    const m = e.data;
    if (!m || m.from === me) return;
    others = true;
    if (m.type === "state") {
      ready = true;
      // The channel copies what it carries, so an object (the selection) arrives as a new
      // one every time: only what differs is set, or every panel keyed on it redraws.
      const s = store.state as unknown as Record<string, unknown>;
      const changed = Object.fromEntries(Object.entries(m.state).filter(([k, v]) => JSON.stringify(s[k]) !== JSON.stringify(v)));
      if (!Object.keys(changed).length) return;
      applying = true;
      try { store.set(changed); } finally { applying = false; }
    } else if (m.type === "hello") {
      // A new window: tell it where things are, and, if it is a panel, take ours away.
      if (!panel) {
        post({ type: "state", from: me, state: (sent = snapshot()) });
        if (m.panel) panels.claimed(m.panel);
      }
    } else if (m.type === "bye") {
      if (!panel && m.panel) { gone.set(m.panel, Date.now()); panels.returned(m.panel); }
    } else if (m.type === "alive") {
      if (panel) return;
      heard.set(m.panel, Date.now());
      // A beat already on its way when the panel was called back is not a window coming
      // back: it would hide the panel again until the beats stopped.
      if (!panels.out.has(m.panel) && Date.now() - (gone.get(m.panel) ?? 0) > LOST) panels.claimed(m.panel);
    } else if (m.type === "recall") {
      if (panel && m.panel === panel) close();
    } else if (m.type === "run") {
      // A command a panel's window does not have (play, reload): the editor window runs it.
      if (!panel) run(m.id, m.arg);
    }
  };

  // Every change of these, from here, goes to the others — what changed, not the lot. The
  // playhead ticks 25 times a second while playing; a channel message is cheap, and a
  // timeline in another window that lags the picture is the thing this exists to prevent.
  store.select((s) => keys.map((k) => s[k]), () => {
    if (applying || !ready || !others) return;
    const now = snapshot();
    const state = Object.fromEntries(Object.entries(now).filter(([k, v]) => sent[k as keyof Shared] !== v)) as Partial<Shared>;
    sent = now;
    if (Object.keys(state).length) post({ type: "state", from: me, state });
  });

  // A panel's window closes itself only after saying so: a message posted while the page
  // unloads can be lost (the desktop shell's windows lost it), and an editor window that
  // still thinks the panel is out keeps it hidden.
  const close = () => {
    post({ type: "bye", from: me, panel });
    const app = desktop();
    if (app) void app.windowControl("close");
    else window.close();
  };
  const heard = new Map<string, number>();
  /** When each panel last came back, by its window's goodbye, a recall or a lost beat. */
  const gone = new Map<string, number>();
  if (panel) {
    forwardUnknown((id, arg) => { post({ type: "run", from: me, id, arg }); return true; });
    panels.leave = close;
    addEventListener("pagehide", () => post({ type: "bye", from: me, panel }));
    setInterval(() => post({ type: "alive", from: me, panel }), BEAT);
  } else {
    panels.onRecall = (id) => { gone.set(id, Date.now()); heard.delete(id); post({ type: "recall", from: me, panel: id }); };
    // However a panel's window went — closed by the window manager, crashed, its goodbye
    // lost — its heartbeat stops, and the panel comes back here.
    setInterval(() => {
      const now = Date.now();
      for (const [id, since] of panels.out) {
        const last = Math.max(heard.get(id) ?? 0, since);
        const wait = heard.has(id) ? LOST : GRACE;
        if (now - last > wait) { heard.delete(id); gone.set(id, now); panels.returned(id); }
      }
    }, BEAT);
  }
  post({ type: "hello", from: me, panel });
}
