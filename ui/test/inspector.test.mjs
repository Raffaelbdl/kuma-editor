// The inspector's property fields, in a real browser: a number you drag, a vector wearing
// its axis colours, and one patch per drag rather than one per pixel.
//
// The drag is the part no unit test can stand in for — it is three pointer events, a dead
// zone, a modifier and a commit that must happen exactly once, against a daemon that
// refuses a patch built on a stale revision. What it pins beyond "the value changed" is
// the bargain in `props.ts`: the field moves while you drag it and the document changes
// when you let go.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const clip = (effects) => ({ id: "c1", start: 0, end: 8, bin: "red", in: 0, speed: 1, volume: 1, muted: false, label: "", effects });

const PROJECT = JSON.stringify({
  version: 1, name: "props", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
        clip([{ type: "transform", x: 0, y: 0, scale: 1, rotation: 0, opacity: 1, keyframes: [] }]),
      ] },
    ] } },
  },
}, null, 2);

const why = missing();

describe("the inspector's property fields", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const EFFECT = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects[0])`;
  const REVISION = `fetch("/api/project").then(r => r.json()).then(p => p.revision)`;
  /** What a field reads right now — which during a drag is ahead of the document. */
  const reads = (sel) => `document.querySelector(${JSON.stringify(sel)} + " input").value`;

  /** Press, move, release — the three events a drag is, with the modifiers it is held
   *  with. `steps` intermediate moves, because a drag is not one jump. */
  const drag = (sel, dx, mods = {}) => `(() => {
    const el = document.querySelector(${JSON.stringify(sel)});
    const r = el.getBoundingClientRect();
    const x = r.left + 8, y = r.top + 5;
    el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, clientX: x, clientY: y }));
    for (const f of [0.5, 1]) window.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, clientX: x + ${dx} * f, clientY: y, ...${JSON.stringify(mods)} }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, clientX: x + ${dx}, clientY: y }));
    return true;
  })()`;

  /** Wait for the *page* to have caught up with the document: the panel rebuilds from the
   *  refreshed project, so a field reading the committed value means the next drag will
   *  build its patch on the current revision. A drag sent before that is refused as stale
   *  — which is the daemon doing its job, and a race only a test is fast enough to lose. */
  const fieldIs = (sel, text) => page.until(`document.querySelector(${JSON.stringify(sel)} + " input").value === ${JSON.stringify(text)}`);

  /** Wait for a field of the effect to read `value` in the document. */
  const effectIs = async (key, value) => {
    try {
      await page.until(`${EFFECT}.then(e => e[${JSON.stringify(key)}] === ${JSON.stringify(value)})`, 8000);
    } catch {
      assert.deepEqual(await page.eval(`${EFFECT}.then(e => e[${JSON.stringify(key)}])`), value, `${key} never reached ${value}`);
    }
  };

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-props-"));
    const projectFile = path.join(tmp, "props.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="c1"]')`);
    // Select the clip: the inspector shows a clip's effects, and that is what is under test.
    await page.eval(`(() => { const c = document.querySelector('.clip[data-id="c1"]'); const r = c.getBoundingClientRect();
      c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 20, clientY: r.top + 2 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`!!document.querySelector(".effect .num.axis-x")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("gives the transform a position row wearing its axis colours", async () => {
    // X red, Y green — the reason the row is readable at a glance, and the one thing about
    // it a person cannot recover from the numbers.
    assert.deepEqual(await page.eval(`[...document.querySelectorAll(".effect .vec .num")].map(n => n.className + "|" + n.querySelector(".tag").textContent)`),
      ["num axis-x|X", "num axis-y|Y"]);
    assert.equal(await page.eval(`getComputedStyle(document.querySelector(".effect .num.axis-x .tag")).color`), "rgb(217, 87, 107)");
  });

  it("drags a number, and commits once when the drag ends", async () => {
    const before = await page.eval(REVISION);
    // 20 pixels, less the 3 a press takes to become a drag, at one step each.
    await page.eval(drag(".effect .num.axis-x", 20));
    await effectIs("x", 17);
    await fieldIs(".effect .num.axis-x", "17");
    assert.equal(await page.eval(REVISION), before + 1, "a drag is one edit, not one per pixel");
  });

  it("is fine with Shift and coarse with Alt", async () => {
    // 20 pixels, less the 3 the drag takes to be recognised, at a tenth of a step each.
    await page.eval(drag(".effect .num.axis-x", 20, { shiftKey: true }));
    await effectIs("x", 18.7);
    await fieldIs(".effect .num.axis-x", "18.7");
    // Ten steps a pixel, and landing on a multiple of ten: a coarse drag snaps to the
    // grid it is dragging on, which is what makes it useful for getting back to round.
    await page.eval(drag(".effect .num.axis-x", -20, { altKey: true }));
    await effectIs("x", -150);
    await fieldIs(".effect .num.axis-x", "-150");
  });

  it("stops at the bounds the model has", async () => {
    // The validator refuses a scale of 0, so the field stops one step above it: a drag
    // must not be able to compose a patch the daemon will reject.
    await page.eval(drag(`[data-prop="scale"] .num`, -500, { altKey: true }));
    await effectIs("scale", 0.01);
    await fieldIs(`[data-prop="scale"] .num`, "0.01");
  });

  it("takes a typed value, and a click that only puts a caret in costs nothing", async () => {
    const before = await page.eval(REVISION);
    // A press that never moves is a click: it focuses the field and changes nothing.
    await page.eval(drag(".effect .num.axis-y", 0));
    assert.equal(await page.eval(`document.activeElement === document.querySelector(".effect .num.axis-y input")`), true);
    assert.equal(await page.eval(REVISION), before, "a caret is not an edit");

    await page.eval(`(() => { const i = document.querySelector(".effect .num.axis-y input"); i.value = "-42.5"; i.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
    await effectIs("y", -42.5);
    assert.equal(await page.eval(reads(".effect .num.axis-y")), "-42.5");
  });
});

// ---------------------------------------------------------------------------------
// Keyframes. The clip starts at 2s, so every assertion here is also a check that the
// panel counts keyframe time from the clip rather than from the timeline.

const KEYED = JSON.stringify({
  version: 1, name: "keys", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
        { id: "c1", start: 2, end: 10, bin: "red", in: 0, speed: 1, volume: 1, muted: false, label: "", effects: [
          { type: "transform", x: 0, y: 0, scale: 1, rotation: 0, opacity: 1, keyframes: [
            { t: 0, x: 0, y: 0, scale: 1, rotation: 0, opacity: 1 },
            { t: 4, x: 100, y: 0, scale: 1, rotation: 0, opacity: 1 },
          ] },
        ] },
      ] },
    ] } },
  },
}, null, 2);

describe("keyframes", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const EFFECT = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects[0])`;
  /** The keys as `t:x`, which is the whole question: when, and what. */
  const KEYS = `${EFFECT}.then(e => e.keyframes.map(k => k.t + ":" + k.x).join(" "))`;
  const X = `document.querySelector(".effect .num.axis-x input").value`;
  const button = (text) => `[...document.querySelectorAll(".effect button")].find(b => b.textContent.trim() === ${JSON.stringify(text)})`;

  const keysAre = async (expected) => {
    try {
      await page.until(`${KEYS}.then(s => s === ${JSON.stringify(expected)})`, 8000);
    } catch {
      assert.equal(await page.eval(KEYS), expected);
    }
  };
  /** Move the playhead in timeline time; the clip starts at 2s, so 4s is 2s into it. */
  const seek = (t) => `(() => {
    const tl = document.querySelector(".tl"), ruler = document.querySelector(".tl-ruler");
    const zoom = Number(document.querySelector("#transport input[type=range]").value);
    const r = tl.getBoundingClientRect();
    ruler.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 136 + ${t} * zoom, clientY: r.top + 2 }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
    return document.querySelector("#transport .time").textContent;
  })()`;
  const type = (value) => `(() => { const i = document.querySelector(".effect .num.axis-x input"); i.value = ${JSON.stringify(value)}; i.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-keys-"));
    const projectFile = path.join(tmp, "keys.kuma.json");
    fs.writeFileSync(projectFile, KEYED);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="c1"]')`);
    await page.eval(`(() => { const c = document.querySelector('.clip[data-id="c1"]'); const r = c.getBoundingClientRect();
      c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 20, clientY: r.top + 2 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`!!document.querySelector(".effect .kfs-key")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("draws a key per keyframe, in the panel and on the clip", async () => {
    assert.equal(await page.eval(`document.querySelectorAll(".effect .kfs-key").length`), 2);
    // …and on the clip itself, so an animated clip says so without opening anything.
    assert.equal(await page.eval(`document.querySelectorAll('.clip[data-id="c1"] .kf-lane .kf').length`), 2);
  });

  it("shows the value at the playhead, not a static one", async () => {
    await page.eval(seek(4)); // two seconds into the clip: halfway between the keys
    await page.until(`${X} === "50"`);
    await page.eval(seek(6)); // on the second key
    await page.until(`${X} === "100"`);
    await page.eval(seek(2)); // the clip's own start, where the first key is
    await page.until(`${X} === "0"`);
  });

  it("writes into the key under the playhead", async () => {
    await page.eval(seek(6));
    await page.until(`${X} === "100"`);
    await page.eval(type("140"));
    await keysAre("0:0 4:140");
  });

  it("makes a key where there is none, carrying every field", async () => {
    await page.eval(seek(4));
    await page.until(`${X} === "70"`); // halfway to the edited key
    await page.eval(type("10"));
    await keysAre("0:0 2:10 4:140");
    // A new key is a full sample — a key that left `scale` out would mean "the static
    // value" to the compiler, which would yank it.
    assert.deepEqual(await page.eval(`${EFFECT}.then(e => e.keyframes[1])`), { t: 2, x: 10, y: 0, scale: 1, rotation: 0, opacity: 1 });
  });

  it("adds a key that does not move the picture", async () => {
    await page.eval(seek(3)); // one second in: between the keys at 0 and 2
    await page.until(`${X} === "5"`);
    await page.eval(`${button("+ key")}.click()`);
    await keysAre("0:0 1:5 2:10 4:140");
    assert.equal(await page.eval(X), "5", "adding a key must leave the value where it was");
  });

  it("drags a key to another time", async () => {
    // The strip spans the clip, so the time a pixel is worth is the test's to compute.
    const to = await page.eval(`(() => {
      const keys = [...document.querySelectorAll(".effect .kfs-key")];
      const key = keys[1], track = document.querySelector(".effect .kfs-track");
      const r = key.getBoundingClientRect(), box = track.getBoundingClientRect();
      const dx = 30, x = r.left + 4, y = r.top + 4;
      key.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, clientX: x, clientY: y }));
      for (const f of [0.5, 1]) window.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, clientX: x + dx * f, clientY: y }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, clientX: x + dx, clientY: y }));
      // Where it was let go, in the clip's own time, on the frame grid — the same sum the
      // strip makes, rather than an assumption about where a rotated diamond's box starts.
      return Math.round((((x + dx) - box.left) / box.width) * 8 * 30) / 30;
    })()`);
    await keysAre(`0:0 ${to}:5 2:10 4:140`);
  });

  it("clears the keys and keeps the value where the playhead is", async () => {
    await page.eval(seek(6));
    await page.until(`${X} === "140"`);
    await page.eval(`${button("Clear")}.click()`);
    await keysAre("");
    assert.equal(await page.eval(`${EFFECT}.then(e => [e.x, e.scale])`).then(JSON.stringify), JSON.stringify([140, 1]));
    // …and the row goes back to offering animation rather than a strip.
    await page.until(`!document.querySelector(".effect .kfs-key") && !!${button("◆ Animate")}`);
  });

  it("starts keyframing from what the effect already does", async () => {
    await page.eval(seek(5)); // three seconds in
    await page.eval(`${button("◆ Animate")}.click()`);
    await keysAre("3:140");
    assert.deepEqual(await page.eval(`${EFFECT}.then(e => e.keyframes[0])`), { t: 3, x: 140, y: 0, scale: 1, rotation: 0, opacity: 1 });
  });
});
