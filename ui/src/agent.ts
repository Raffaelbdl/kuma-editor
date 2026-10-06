// What the agent is doing, read off its tool calls (`store.state.activity`), for the bear,
// the status line, den's HUD and the agent panel. The agent stays outside the editor —
// Claude Code or another MCP client — so all the editor can say is what it was told: the
// calls, their arguments and when they ended. Nothing here is a model of the agent.

import bearAwake from "./assets/bear.png";
import bearLook from "./assets/bear-look.png";
import bearSleep from "./assets/bear-sleep.png";
import { el } from "./dom";
import { store, type State } from "./store";
import type { Activity, Pin } from "./types";

/** Asleep after this long without a call. */
const IDLE_MS = 60_000;
/** An edit is "editing" while in flight and for this long after it lands. */
const EDIT_AFTERGLOW_MS = 4_000;

export type BearState = "idle" | "awake" | "editing" | "looking";

export interface AgentView {
  bear: BearState;
  /** The MCP client's name (`claude-code`), or empty when no agent has called. */
  client: string;
  /** One word or phrase: `looking`, `editing`, `reading pin 2`, `asleep`. */
  doing: string;
  /** The call it is on, or last made. */
  last: Activity | null;
  /** The pin it is reading, while it lists them. */
  pin?: Pin;
}

const BEARS: Record<BearState, string> = { idle: bearSleep, awake: bearAwake, editing: bearAwake, looking: bearLook };

/** The bear for a state, `scale` whole pixels per pixel (it is 12×10): anything else blurs. */
export function bear(state: BearState, scale = 2): HTMLImageElement {
  const img = el("img", "bear");
  img.alt = "";
  img.width = 12 * scale;
  img.height = 10 * scale;
  img.src = BEARS[state];
  img.dataset.state = state;
  return img;
}

export function setBear(img: HTMLImageElement, state: BearState): void {
  if (img.dataset.state === state) return;
  img.dataset.state = state;
  img.src = BEARS[state];
}

/** `46–49s`, `8.6s`: where a sample looked, in the editor's shorthand. */
export function sampleSpan(a: Activity): string {
  const times = a.sample?.times ?? [];
  if (!times.length) return "";
  const lo = Math.min(...times), hi = Math.max(...times);
  const f = (t: number) => (Number.isInteger(t) ? String(t) : t.toFixed(1));
  return lo === hi ? `${f(lo)}s` : `${f(lo)}–${f(hi)}s`;
}

/** The pins of `sequence` not yet answered, in order. */
export const openPins = (pins: Pin[], sequence: string): Pin[] => pins.filter((p) => p.sequence === sequence && p.done === null);

/** What a call is, as a phrase: what the agent is doing while it runs. */
function phrase(a: Activity, pin: Pin | undefined): string {
  switch (a.tool) {
    case "sample_frames": return "looking";
    case "apply_edits": return "editing";
    case "list_pins": return pin ? `reading pin ${pin.index}` : "reading pins";
    case "get_project": return "reading the project";
    case "check_overlay": return "checking an overlay";
    case "render": return "rendering";
    case "timeline_guide": case "overlay_guide": return "reading the guide";
    default: return a.tool.replace(/_/g, " ");
  }
}

export function agentView(s: State, now = Date.now()): AgentView {
  const calls = s.activity;
  const last = calls[0] ?? null;
  const client = last?.client ?? "";
  if (!last) return { bear: "idle", client, doing: "", last };
  const running = calls.filter((a) => a.state === "running");
  const looking = running.find((a) => a.tool === "sample_frames");
  if (looking) return { bear: "looking", client, doing: "looking", last: looking };
  const edit = calls.find((a) => a.tool === "apply_edits");
  if (edit && (edit.state === "running" || now - (edit.ended_at ?? now) < EDIT_AFTERGLOW_MS)) return { bear: "editing", client, doing: "editing", last: edit };
  const at = last.ended_at ?? last.started_at;
  if (!running.length && now - at > IDLE_MS) return { bear: "idle", client, doing: "asleep", last };
  const call = running[0] ?? last;
  const pin = call.tool === "list_pins" ? openPins(s.pins, s.sequence)[0] ?? s.pins.find((p) => p.done === null) : undefined;
  return { bear: "awake", client, doing: phrase(call, pin), last: call, pin };
}

const watchers: ((v: AgentView) => void)[] = [];
let view: AgentView | null = null;
let said = "";
function tick() {
  const v = agentView(store.state);
  const key = `${v.bear}|${v.client}|${v.doing}|${v.last?.id ?? ""}|${v.last?.state ?? ""}`;
  if (key === said) return;
  said = key;
  view = v;
  for (const fn of watchers) fn(v);
}

/** The agent's view, kept in step with the clock as well as the calls: "editing" fades and
 *  "asleep" arrives with nobody calling. Watchers get `fn` now and when what it says
 *  changes — one clock and one subscription for all of them. */
export function watchAgent(fn: (v: AgentView) => void): void {
  if (!watchers.length) {
    store.select((s) => [s.activity, s.pins, s.sequence], tick);
    setInterval(tick, 1000);
  }
  watchers.push(fn);
  fn(view ?? agentView(store.state));
}

/** `14s`, `2m`, `1h`: how long ago, as the agent panel says it. */
export function ago(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  return `${Math.round(s / 3600)}h`;
}

/** `14:02`: the clock time of a step, for the history. */
export const clock = (ms: number): string => new Date(ms).toTimeString().slice(0, 5);

/** The name the editor gives each author: the agent is kuma's, the person is you. */
export const who = (author: string): string => (author === "agent" ? "kuma" : author === "user" ? "you" : "other");
