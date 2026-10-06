// `h` — the history: one undo stack, shared by you and the agent, newest first. ⟲ makes the
// document what it was at that revision, as one more edit (so it can be undone too).
// Refused patches are listed, greyed: an agent's stale patch is part of what happened.
//
// It is the daemon's history (`/api/history`), never a model kept here: the client only
// draws it.

import { api } from "../api";
import { clock, who } from "../agent";
import { button, el, plural } from "../dom";
import { store, toast } from "../store";
import type { HistoryEntry } from "../types";

type Filter = "all" | "agent" | "user";

export function historyPanel(): { body: HTMLElement; tools: HTMLElement } {
  const body = el("div", "history-panel");
  const tools = el("span", "hist-rev");
  const filters = el("div", "hist-filters");
  let filter: Filter = "all";
  const chips = (["all", "agent", "user"] as Filter[]).map((f) => {
    const b = button(f === "all" ? "all" : who(f), () => { filter = f; draw(); }, f === "all" ? "Every step" : `Only ${who(f)}'s steps`, "chip");
    b.dataset.f = f;
    return b;
  });
  filters.append(...chips);
  const list = el("div", "hist-list");
  body.append(filters, list);

  const draw = () => {
    for (const c of chips) c.classList.toggle("on", c.dataset.f === filter);
    const s = store.state;
    // Every revision this session is a step here, so the newest one that landed is now.
    const current = s.history.find((h) => h.revision !== null)?.revision ?? s.project?.revision ?? 0;
    const rows = s.history.filter((h) => filter === "all" || h.author === filter);
    if (!rows.length) {
      list.replaceChildren(el("p", "muted tiny", s.history.length ? "Nothing by them yet." : "No steps yet this session. Every edit, yours and the agent's, lands here."));
      return;
    }
    list.replaceChildren(...rows.map((h) => entryRow(h, current)));
  };
  store.select((s) => [s.history], draw);
  store.select((s) => [s.project?.revision], (s) => { tools.textContent = `r${s.project?.revision ?? 0}`; });
  return { body, tools };
}

function entryRow(h: HistoryEntry, current: number): HTMLElement {
  const refused = h.kind === "refused";
  const row = el("div", `hist-row${refused ? " refused" : ""}${h.revision === current ? " current" : ""}`);
  row.appendChild(el("span", "rev", h.revision === null ? `r${h.base_revision ?? "?"}` : `r${h.revision}`));
  const main = el("div", "hist-main");
  main.appendChild(el("div", "hist-what", refused ? `refused · ${shortError(h)}` : describe(h)));
  const meta = [who(h.author), refused ? h.summary : h.ops ? plural(h.ops, "op") : h.kind, clock(h.at)].filter(Boolean).join(" · ");
  main.appendChild(el("div", "hist-meta", meta));
  row.appendChild(main);
  if (h.revision === current) row.appendChild(el("span", "hist-now", "now"));
  else if (h.revision !== null) {
    const rev = h.revision;
    row.appendChild(button("⟲", async () => {
      try {
        const r = await api.restore(rev);
        await store.refresh();
        toast(`Restored r${rev} as r${r.revision} — undo takes it back`, "ok");
      } catch (e) { toast((e as Error).message, "err", 8000); }
    }, `Go back to r${rev}: the document as it was then, as a new edit`, "hist-restore"));
  }
  row.title = h.origin;
  return row;
}

/** What a step did, in a line: its own words, then the values it changed. */
function describe(h: HistoryEntry): string {
  const changes = h.changes.slice(0, 2).map(change).join(", ");
  return [h.summary || h.kind, changes].filter(Boolean).join(" · ");
}

const fmt = (v: unknown): string => (typeof v === "number" ? String(+v.toFixed(2)) : typeof v === "string" ? v : JSON.stringify(v));
/** `x 0→120`: one value a step changed. */
const change = (c: HistoryEntry["changes"][number]): string => `${c.field} ${fmt(c.from)}→${fmt(c.to)}`;

function shortError(h: HistoryEntry): string {
  const e = h.error ?? "";
  return e.length > 60 ? `${e.slice(0, 57)}…` : e || "refused";
}

/** The bottom of the inspector: the steps that touched these clips or entries, newest
 *  first, saying what each changed of *them* and who made it. Fields are not coloured when
 *  they change; this is where that is said. It follows the history itself, so a step does
 *  not rebuild the inspector round it; `off` stops it. */
export function historyFor(ids: string[], label: string): { box: HTMLElement; off: () => void } {
  const want = new Set(ids);
  const box = el("div", "insp-history");
  const off = store.select((s) => [s.history], (s) => {
    const steps = s.history.filter((h) => h.kind !== "refused" && h.targets.some((t) => want.has(t))).slice(0, 6);
    box.replaceChildren(el("h3", "", `history · ${label}`));
    if (!steps.length) box.appendChild(el("p", "muted tiny", "Nothing has changed it this session."));
    for (const h of steps) {
      const mine = h.changes.filter((c) => want.has(c.target)).slice(0, 3).map(change).join(" · ");
      const row = el("div", "hist-mini");
      row.append(el("span", "rev", `r${h.revision}`), el("span", "what", mine || h.summary || h.kind), el("span", "who", who(h.author)));
      row.title = h.origin;
      box.appendChild(row);
    }
  });
  return { box, off };
}
