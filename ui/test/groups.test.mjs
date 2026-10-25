// Moving clips as a block, in a real browser — and the two ways a block comes about: a
// selection you made, and a group that makes itself.
//
// The model's side of a group is one inert string on a clip. Everything a group *does*
// happens in the page: the selection expands, a drag gathers the members, the block keeps
// its shape and stops at 0 as one, and the move is a single patch. A multi-selection
// borrows that same block and differs in one way, which is why the two sit together here:
// it lasts only until you click elsewhere.
//
// The shortcut list is here too, because it is the same bindings this file presses.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const track = (id, kind, name, clips = []) => ({ id, kind, name, muted: false, hidden: false, transitions: [], clips });
const clip = (id, bin, start, end) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" });

/** `a` on video and `b` on audio are staggered on purpose: a block that stops at 0 has to
 *  stop when its *earliest* member gets there, not when the one under the pointer does.
 *  `c` sits well to the right on a second video track, ungrouped — the thing that must not
 *  move, and the second half of a block that changes track. `v3` is empty, so there is a
 *  track above for such a block to shift onto. Colour entries, so the fixture needs no
 *  media on disk. */
const PROJECT = JSON.stringify({
  version: 1, name: "groups", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [
      track("v1", "video", "Video", [clip("a", "red", 1, 5)]),
      track("v2", "video", "Video 2", [clip("c", "red", 6, 8)]),
      track("v3", "video", "Video 3"),
      track("a1", "audio", "Audio", [clip("b", "red", 0, 4)]),
    ] } },
  },
}, null, 2);

const why = missing();

describe("clip groups", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  /** Every clip as `id@start-end`, in document order, with its group — the whole answer to
   *  "what moved, and what is held together" in one string. */
  const CLIPS = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks
    .flatMap(t => t.clips.map(c => c.id + "@" + c.start + "-" + c.end + (c.group ? ":" + c.group : ""))).sort().join(" "))`;
  const GROUP = (id) => `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks
    .flatMap(t => t.clips).find(c => c.id === ${JSON.stringify(id)})?.group ?? "")`;
  /** Every clip as `track/id@start`, sorted: where things are, tracks included. */
  const WHERE = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks
    .flatMap(t => t.clips.map(c => t.id + "/" + c.id + "@" + c.start)).sort().join(" "))`;
  const REVISION = `fetch("/api/project").then(r => r.json()).then(p => p.revision)`;
  const SELECTED = `[...document.querySelectorAll("#timeline .clip.selected")].map(c => c.dataset.id).sort().join(",")`;

  /** Wait for the timeline to read exactly `expected`, then say what it read instead. The
   *  comparison happens in the page because an edit is a round trip, not a delay. */
  const clipsAre = async (expected) => {
    try { await page.until(`${CLIPS}.then(s => s === ${JSON.stringify(expected)})`, 8000); }
    catch { assert.equal(await page.eval(CLIPS), expected); }
  };
  /** The same, for where the clips are rather than when. Then wait for the page to have
   *  redrawn them there: the next drag reads the row a clip is in to know which track it
   *  started on, so a stale box would measure the move from the wrong place. */
  const whereIs = async (expected) => {
    try { await page.until(`${WHERE}.then(s => s === ${JSON.stringify(expected)})`, 8000); }
    catch { assert.equal(await page.eval(WHERE), expected); }
    const on = expected.split(" ").map((s) => s.split("/"));
    for (const [tid, rest] of on) {
      await page.until(`document.querySelector('.clip[data-id="${rest.split("@")[0]}"]:not(.ghost)')?.closest(".tl-track")?.dataset.tid === ${JSON.stringify(tid)}`);
    }
  };

  const pointer = (selector, dx = 0, modifiers = {}) => `(() => {
    const e = document.querySelector(${JSON.stringify(selector)});
    const r = e.getBoundingClientRect();
    e.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + ${dx}, clientY: r.top + 2, ...${JSON.stringify(modifiers)} }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
    return true;
  })()`;
  const selectClip = (id) => pointer(`.clip[data-id="${id}"]`, 20);
  const addClip = (id) => pointer(`.clip[data-id="${id}"]`, 20, { ctrlKey: true });
  /** A button in the inspector's action row, by its label. */
  const ACTION = (label) => `([...document.querySelectorAll("#inspector .insp-actions button")].find(b => b.textContent.trim() === ${JSON.stringify(label)}) ?? null)`;
  const key = (k, mods = {}) => `window.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, ctrlKey: true, bubbles: true, ...${JSON.stringify(mods)} })) || true`;

  /** Press on a clip's body, travel `dx` pixels, let go — the gesture that moves it. The
   *  moves go to the window, as the real ones do once the pointer has left the clip. */
  const dragClip = (id, dx) => `(() => {
    const el = document.querySelector('.clip[data-id="${id}"]:not(.ghost)');
    const r = el.getBoundingClientRect();
    const x = r.left + 20, y = r.top + 6;
    el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, clientX: x, clientY: y }));
    for (const f of [0.5, 1]) window.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, clientX: x + ${dx} * f, clientY: y }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, clientX: x + ${dx}, clientY: y }));
    return true;
  })()`;

  /** Press a clip, travel `dx` pixels, and let go over another track's row. The moves go
   *  to that row, as the browser's own do — which is how the drag knows what it is over. */
  const dragToTrack = (id, dx, tid) => `(() => {
    const el = document.querySelector('.clip[data-id="${id}"]:not(.ghost)');
    const row = document.querySelector('.tl-track[data-tid="${tid}"]');
    const r = el.getBoundingClientRect(), rr = row.getBoundingClientRect();
    const x = r.left + 20, y = r.top + 6;
    el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, clientX: x, clientY: y }));
    for (const f of [0.5, 1]) row.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, clientX: x + ${dx} * f, clientY: rr.top + 5 }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, clientX: x + ${dx}, clientY: rr.top + 5 }));
    return true;
  })()`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-groups-"));
    const projectFile = path.join(tmp, "groups.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="a"]')`);
    // The default zoom, so a pixel of drag is a known number of seconds.
    await page.until(`document.querySelector("#timeline .tl-corner input.zoom").value === "60"`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("moves a selection of ungrouped clips as one", async () => {
    await page.eval(selectClip("a"));
    await page.eval(addClip("b"));
    await page.until(`${SELECTED} === "a,b"`);
    const before = await page.eval(REVISION);
    await page.eval(dragClip("a", 120)); // two seconds at 60px/s
    await clipsAre("a@3-7 b@2-6 c@6-8");
    assert.equal(await page.eval(REVISION), before + 1, "one patch, as a group's move is");
    // Pressing a clip that is already selected keeps the selection, so the block can be
    // moved again without rebuilding it. Back where it started.
    await page.until(`${SELECTED} === "a,b"`);
    await page.eval(dragClip("a", -120));
    await clipsAre("a@1-5 b@0-4 c@6-8");
  });

  it("collapses to the clip you pressed when the press turns out to be a click", async () => {
    // The other half of keeping the selection through a press: without this there would be
    // no way back to one clip except by clicking empty space first.
    await page.until(`${SELECTED} === "a,b"`);
    await page.eval(selectClip("a"));
    await page.until(`${SELECTED} === "a"`);
  });

  it("drags one clip onto another track", async () => {
    await page.eval(selectClip("a"));
    await page.until(`${SELECTED} === "a"`);
    await page.eval(dragToTrack("a", 0, "v2"));
    await whereIs("a1/b@0 v2/a@1 v2/c@6");
    // …and back. A track move is a move like any other, not a one-way door.
    await page.eval(dragToTrack("a", 0, "v1"));
    await whereIs("a1/b@0 v1/a@1 v2/c@6");
  });

  it("shifts a block by whole tracks, keeping the shape it has across them", async () => {
    await page.eval(selectClip("a"));
    await page.eval(addClip("c"));
    await page.until(`${SELECTED} === "a,c"`);
    const before = await page.eval(REVISION);
    // `a` is on v1 and `c` on v2, so dragging `a` onto v2 is an offset of one track and
    // `c` goes to v3. There is no single track for the pair to land on, and asking for one
    // is what the offset avoids.
    await page.eval(dragToTrack("a", 0, "v2"));
    await whereIs("a1/b@0 v2/a@1 v3/c@6");
    assert.equal(await page.eval(REVISION), before + 1, "one patch, tracks and all");
    // And back down, which is the same offset the other way.
    await page.eval(dragToTrack("a", 0, "v1"));
    await whereIs("a1/b@0 v1/a@1 v2/c@6");
    await page.eval(selectClip("a"));
    await page.until(`${SELECTED} === "a"`);
  });

  it("groups the selected clips, across the tracks they are on", async () => {
    await page.eval(selectClip("a"));
    await page.eval(addClip("b"));
    await page.until(`${SELECTED} === "a,b"`);
    // Ctrl+G is not a way to find out the feature exists, and a browser tab has no menu.
    assert.equal(await page.eval(`${ACTION("Group")} !== null`), true, "the inspector offers it");
    await page.eval(key("g"));
    await page.until(`${GROUP("a")}.then(g => !!g)`);
    const g = await page.eval(GROUP("a"));
    assert.equal(await page.eval(GROUP("b")), g, "one group, whatever tracks its clips are on");
    assert.equal(await page.eval(GROUP("c")), "", "and nothing else joins it");
    // It says so where you look at it, or the first surprise is two clips moving.
    assert.equal(await page.eval(`document.querySelectorAll("#timeline .clip.grouped").length`), 2);
  });

  it("selects the whole group when you click any of it", async () => {
    // Off the clips first: the assertion is that *one* click brings both back.
    await page.eval(`(() => { const lane = document.querySelector('.tl-track[data-tid="v2"] .tl-lane'); const r = lane.getBoundingClientRect();
      lane.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.right - 10, clientY: r.top + 5 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`${SELECTED} === ""`);
    await page.eval(selectClip("b"));
    await page.until(`${SELECTED} === "a,b"`);
    // The one clicked is still the one the inspector edits — the group joins in front of
    // it, so "the last one clicked" survives.
    assert.match(await page.eval(`document.querySelector("#inspector h3").textContent`), /Clip\s*b/);
  });

  it("moves the block when you drag one of it, in one patch", async () => {
    const before = await page.eval(REVISION);
    await page.eval(dragClip("a", 120)); // two seconds at 60px/s
    await clipsAre("a@3-7:g1 b@2-6:g1 c@6-8");
    assert.equal(await page.eval(REVISION), before + 1, "a block is one edit: half a group having moved is not a state");
  });

  it("stops the block when its earliest clip reaches 0, not the one you are dragging", async () => {
    // `a` is at 3 and `b` at 2, so a four-second drag left would put `b` at -1 — which the
    // daemon would refuse, taking the whole move with it. The block travels 2 instead and
    // keeps its shape.
    await page.eval(dragClip("a", -240));
    await clipsAre("a@1-5:g1 b@0-4:g1 c@6-8");
  });

  it("gives a pasted copy a group of its own", async () => {
    await page.eval(selectClip("a"));
    await page.until(`${SELECTED} === "a,b"`);
    const had = await page.eval(`[...document.querySelectorAll("#timeline .clip:not(.ghost)")].map(c => c.dataset.id)`);
    await page.eval(key("c"));
    await page.eval(key("v"));
    await page.until(`(() => { const s = document.querySelector("#timeline .clip.selected"); return !!s && !${JSON.stringify(had)}.includes(s.dataset.id); })()`);
    const groups = await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks
      .flatMap(t => t.clips).filter(c => !${JSON.stringify(had)}.includes(c.id)).map(c => c.group))`);
    assert.equal(groups.length, 2, "the copy is the whole group, because selecting one selected both");
    assert.equal(groups[0], groups[1], "and it is a group");
    assert.notEqual(groups[0], "g1", "its own, or the copy would move with the original forever");
  });

  it("lists the shortcuts, on ? and from the top bar", async () => {
    await page.eval(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "?", bubbles: true })) || true`);
    await page.until(`!!document.querySelector("dialog.modal .shortcuts")`);
    const rows = await page.eval(`[...document.querySelectorAll("dialog.modal .sc-row")].map(r => r.querySelector(".sc-keys").textContent + " = " + r.querySelector(".sc-what").textContent)`);
    // The list is the bindings, so what this really asks is that they carry their own
    // documentation — a key bound and undescribed simply would not be here.
    assert.ok(rows.some((r) => r.startsWith("Ctrl+G ")), `grouping is listed: ${rows.join(" | ")}`);
    assert.ok(rows.some((r) => r.startsWith("Space ")), "and so is the transport");
    // The gestures too: half of what the editor does is done with the mouse.
    assert.ok(rows.some((r) => r.startsWith("Drag a clip =")), `and the gestures: ${rows.join(" | ")}`);
    assert.deepEqual(await page.eval(`[...document.querySelectorAll("dialog.modal h4")].map(h => h.textContent)`), ["Panels", "Transport", "Editing", "Workbench and viewer"]);
    await page.eval(`document.querySelector("dialog.modal .modal-actions button").click()`);
    await page.until(`!document.querySelector("dialog.modal")`);
    // ⌘K names the same list.
    await page.eval(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true }))`);
    await page.until(`!!document.querySelector(".palette input")`);
    await page.eval(`(() => { const i = document.querySelector(".palette input"); i.value = "shortcuts"; i.dispatchEvent(new Event("input")); i.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })); return true; })()`);
    await page.until(`!!document.querySelector("dialog.modal .shortcuts")`);
    await page.eval(`document.querySelector("dialog.modal .modal-actions button").click()`);
    await page.until(`!document.querySelector("dialog.modal")`);
  });

  it("ungroups, and leaves nothing behind in the document", async () => {
    await page.eval(selectClip("a"));
    await page.until(`${SELECTED} === "a,b"`);
    await page.until(`${ACTION("Ungroup")} !== null`);
    await page.eval(key("g", { shiftKey: true }));
    await page.until(`${GROUP("a")}.then(g => g === "")`);
    assert.equal(await page.eval(GROUP("b")), "", "the whole group goes, since selecting one selects them all");
    // Not as `"group": ""` — an ungrouped project is the one it was before.
    assert.equal(await page.eval(`fetch("/api/project").then(r => r.json()).then(p => "group" in p.bin.main.source.tracks[0].clips[0])`), false);
  });
});
