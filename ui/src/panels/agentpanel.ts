// `a` — the agent: what it is doing now, and its last five calls. Read-only on purpose: the
// agent is outside the editor (Claude Code, over MCP), you talk to it in your terminal, and
// the one thing the editor can write to it is a pin.

import { ago, bear, sampleSpan, setBear, watchAgent } from "../agent";
import { el } from "../dom";
import { store } from "../store";
import type { Activity } from "../types";

export function agentPanel(): { body: HTMLElement; note: HTMLElement } {
  const body = el("div", "agent-panel");
  const note = el("span", "", "");
  const pet = bear("idle", 3);
  const state = el("div", "ag-state");
  const doing = el("div", "ag-doing");
  const now = el("div", "ag-now");
  now.append(pet, el("div", "ag-now-text"));
  now.lastElementChild!.append(state, doing);
  const calls = el("div", "ag-calls");
  body.append(now, calls);

  watchAgent((v) => {
    setBear(pet, v.bear);
    note.textContent = v.client;
    state.textContent = v.client ? v.doing : "no agent yet";
    doing.textContent = v.last ? callLine(v.last) : "An agent on this project over MCP shows up here: what it reads, edits and looks at.";
  });

  const draw = () => {
    const recent = store.state.activity.slice(0, 5);
    calls.replaceChildren(...recent.map(callRow));
  };
  store.select((s) => [s.activity], draw);
  // "14s ago" has to keep counting.
  setInterval(draw, 5000);
  return { body, note };
}

/** `sample_frames 46–49s · r59`: a call in one line. */
function callLine(a: Activity): string {
  const span = sampleSpan(a);
  const rev = a.revision_after ?? a.revision_before;
  return `${a.tool}${span ? ` ${span}` : ""} · r${rev}`;
}

function callRow(a: Activity): HTMLElement {
  const row = el("div", `ag-call ${a.state}`);
  const head = el("div", "ag-call-head");
  const edited = a.revision_after !== null && a.revision_after !== a.revision_before;
  if (edited) head.appendChild(el("span", "ag-rev", `r${a.revision_after}`));
  else if (a.tool === "check_overlay" && a.lint) head.appendChild(el("span", a.lint.ok ? "ag-ok" : "ag-bad", a.lint.ok ? "✓" : "✗"));
  else if (a.tool === "sample_frames") head.appendChild(el("span", "ag-look", "◉"));
  head.append(el("b", "", a.tool), el("span", "muted", ` · ${a.state === "running" ? "now" : ago(a.ended_at ?? a.started_at)}`));
  row.appendChild(head);
  if (a.summary) row.appendChild(el("div", "ag-summary", a.summary));
  if (a.error) row.appendChild(el("div", "ag-error", a.error));
  // What it looked at: the frames themselves when the daemon kept them, the rect it zoomed
  // into drawn over them.
  if (a.tool === "sample_frames" && a.frames?.length) {
    const strip = el("div", "ag-frames");
    for (const f of a.frames.slice(0, 4)) {
      const img = el("img");
      img.src = `/api/samples/${f.split("/").map(encodeURIComponent).join("/")}`;
      img.alt = "";
      img.loading = "lazy";
      strip.appendChild(img);
    }
    row.appendChild(strip);
  } else if (a.tool === "sample_frames" && a.sample) {
    row.appendChild(el("div", "ag-summary muted", `${sampleSpan(a)}${a.sample.rect ? ` · ${a.sample.rect.w}×${a.sample.rect.h} at ${a.sample.rect.x},${a.sample.rect.y}` : ""}`));
  }
  return row;
}
