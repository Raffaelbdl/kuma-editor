// Den: the frame, full-bleed, and what the agent did and where it looked.
//
// Over the picture: the bear and the mode, the sequence and what the agent is doing (top
// left); a way back to power (top right); orange corners round the region the agent last
// sampled; and the summary of its last edit as a caption. Under it, a rail: play, the time,
// the sequence with its revisions placed where they edited and its pins, and the keys that
// summon the panels. Everything here reads the store; none of it is the document.

import { agentView, bear, setBear, watchAgent } from "../agent";
import { Cmd, run } from "../commands";
import { button, dragPointer, el } from "../dom";
import { icon } from "../icons";
import { panels, PANEL_KEYS } from "../panelhost";
import { fmtTime, store } from "../store";
import type { Activity, HistoryEntry } from "../types";

export function mountDen(host: HTMLElement): void {
  host.append(el("div", "den-scrim"));

  // ---- HUD
  const pet = bear("idle", 3);
  const seqName = el("span", "den-seq");
  const status = el("span", "den-status");
  const cursor = el("span", "den-cursor", "_");
  const hud = el("div", "den-hud");
  hud.append(pet, el("span", "den-chip", "den"), seqName, status, cursor);
  const power = button("power ↗", () => panels.setMode("power"), "Back to every panel, at the playhead (the selection stays)", "den-power");
  host.append(hud, power);
  watchAgent((v) => {
    setBear(pet, v.bear);
    const rev = store.state.project ? `r${store.state.project.revision}` : "";
    status.textContent = `${rev}${v.client ? ` · kuma ${v.doing}` : ""}`;
  });
  store.select((s) => [s.project, s.sequence], (s) => {
    seqName.textContent = s.project?.bin[s.sequence]?.name || s.sequence;
    const v = agentView(s);
    status.textContent = `${s.project ? `r${s.project.revision}` : ""}${v.client ? ` · kuma ${v.doing}` : ""}`;
  });

  // ---- viewfinder: the corners of what the agent last looked at
  const vf = el("div", "den-vf");
  for (const c of ["tl", "tr", "bl", "br"]) vf.appendChild(el("span", `vf-c ${c}`));
  host.appendChild(vf);
  const placeFinder = () => {
    const stage = document.querySelector<HTMLElement>("#preview .stage");
    const p = store.state.project;
    if (!stage || !p || store.state.mode !== "den" || !stage.offsetWidth) { vf.hidden = true; return; }
    const box = stage.getBoundingClientRect(), mine = host.getBoundingClientRect();
    const look = lastLook(store.state.activity, store.state.sequence, p.master);
    const r = look?.sample?.rect ?? { x: 0, y: 0, w: p.width, h: p.height };
    const sx = box.width / p.width, sy = box.height / p.height;
    // Inset a little when it is the whole frame, so the corners sit on the picture.
    const inset = look?.sample?.rect ? 0 : 14;
    Object.assign(vf.style, {
      left: `${box.left - mine.left + r.x * sx + inset}px`, top: `${box.top - mine.top + r.y * sy + inset}px`,
      width: `${r.w * sx - 2 * inset}px`, height: `${r.h * sy - 2 * inset}px`,
    });
    vf.hidden = false;
    vf.title = look ? `Where kuma last looked: sample_frames${look.sample?.rect ? ` of ${r.w}×${r.h} at ${r.x},${r.y}` : ""}` : "";
  };
  const observer = new ResizeObserver(() => placeFinder());
  observer.observe(host);
  const watchStage = () => { const st = document.querySelector("#preview .stage"); if (st) observer.observe(st); };
  watchStage();
  store.select((s) => [s.activity, s.sequence, s.mode, s.project], () => { watchStage(); requestAnimationFrame(placeFinder); });

  // ---- caption: the agent's last edit, in its own words
  const caption = el("div", "den-caption");
  const capRev = el("span", "rev");
  const capText = el("span", "text");
  caption.append(capRev, capText);
  host.appendChild(caption);
  store.select((s) => [s.history, s.activity], (s) => {
    const h = s.history.find((x) => x.author === "agent" && x.kind === "edit" && x.summary);
    const a = s.activity.find((x) => x.summary);
    const rev = h?.revision ?? a?.revision_after ?? null;
    const text = h?.summary ?? a?.summary ?? "";
    caption.hidden = !text;
    capRev.textContent = rev !== null ? `r${rev}` : "";
    capText.textContent = text;
  });

  // ---- the rail
  const rail = el("div", "den-rail");
  const play = button(icon("play"), () => run(Cmd.togglePlay), "Play/pause (Space)", "den-play");
  const now = el("span", "den-time");
  const end = el("span", "den-time end");
  const track = el("div", "den-track");
  const ticks = el("div", "dr-ticks");
  const marks = el("div", "dr-marks");
  const head = el("div", "dr-head");
  track.append(ticks, marks, head);
  const keys = el("div", "den-keys");
  const keyEls = PANEL_KEYS.map(([letter, id]) => {
    const b = button(letter, () => panels.toggle(id), `${panels.spec(id)?.title ?? id} (${letter.toUpperCase()})`, `den-key${id === "agent" ? " agent" : ""}`);
    b.dataset.panel = id;
    keys.appendChild(b);
    return b;
  });
  const pin = button("", () => run(Cmd.addPin), "Leave a pin at the playhead: a note on this frame for the agent (P)", "den-pin");
  pin.append(el("span", "dot"), "pin");
  rail.append(play, now, track, end, keys, pin);
  host.appendChild(rail);

  const x = (t: number) => `${(Math.min(Math.max(0, t), dur()) / (dur() || 1)) * 100}%`;
  const dur = () => Math.max(store.duration, 1);
  track.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const r = track.getBoundingClientRect();
    const seek = (cx: number) => store.seek(((cx - r.left) / r.width) * dur());
    seek(e.clientX);
    dragPointer(e, { axes: "x", move: (_dx, _dy, ev) => seek(ev.clientX), end: () => {} });
  });

  store.subscribe((s) => {
    now.textContent = fmtTime(s.playhead, store.fps);
    head.style.left = x(s.playhead);
    if (play.dataset.playing !== String(s.playing)) {
      play.dataset.playing = String(s.playing);
      play.replaceChildren(icon(s.playing ? "pause" : "play"));
    }
  });
  store.select((s) => [s.project, s.sequence, s.history, s.pins], (s) => {
    end.textContent = fmtTime(store.duration, store.fps);
    // A tick a second, a taller one every ten — denser and it is a texture, not a scale.
    const d = dur();
    const step = d > 600 ? 10 : d > 120 ? 5 : 1;
    ticks.replaceChildren();
    for (let t = 0; t <= d; t += step) {
      const tk = el("span", (t / step) % 10 === 0 ? "big" : "");
      tk.style.left = x(t);
      ticks.appendChild(tk);
    }
    marks.replaceChildren();
    // The last revisions that edited something on this sequence, where they edited it.
    const placed = s.history.filter((h) => h.revision !== null && h.kind !== "refused").map((h) => ({ h, t: editedAt(h) })).filter((p): p is { h: HistoryEntry; t: number } => p.t !== null).slice(0, 3).reverse();
    placed.forEach(({ h, t }, i) => {
      const m = el("div", `dr-rev${i === placed.length - 1 ? " latest" : ""}`);
      m.style.left = x(t);
      m.append(el("span", "", `r${h.revision}`));
      m.title = `r${h.revision} · ${h.summary}`;
      m.onclick = (e) => { e.stopPropagation(); store.seek(t); };
      m.onpointerdown = (e) => e.stopPropagation();
      marks.appendChild(m);
    });
    for (const m of store.markers.filter((m) => m.label.trim())) {
      const p = el("div", "dr-pin");
      p.style.left = x(m.t);
      p.title = m.label;
      p.onpointerdown = (e) => e.stopPropagation();
      p.onclick = () => store.seek(m.t);
      marks.appendChild(p);
    }
  });
  const markKeys = () => { for (const k of keyEls) k.classList.toggle("on", panels.isFloating(k.dataset.panel!)); };
  document.addEventListener("kuma:panels", markKeys);
  markKeys();
}

/** The agent's last look at what is on screen: a sample of the open sequence (the master
 *  when it named none), not of an overlay on its own. */
function lastLook(activity: Activity[], sequence: string, master: string): Activity | undefined {
  return activity.find((a) => a.tool === "sample_frames" && a.sample && !a.sample.overlay && (a.sample.sequence ?? master) === sequence);
}

/** Where on the open sequence a revision edited: the earliest start of the clips it
 *  touched there, as they are now. Null when it touched nothing on this timeline. */
function editedAt(h: HistoryEntry): number | null {
  const starts = h.targets.map((id) => store.tracks.flatMap((t) => t.clips).find((c) => c.id === id)?.start).filter((t): t is number => t !== undefined);
  return starts.length ? Math.min(...starts) : null;
}
