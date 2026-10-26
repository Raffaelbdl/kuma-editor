// Matching a grade from the inspector, in a real browser against a real daemon: drag a
// region over the frame, and the grade's white balance and exposure are measured into place
// (`measure_color` through melt). Needs melt too; skips without it, as its neighbours skip
// without a browser.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, have, missing, selectClip, startDaemon } from "./harness.mjs";

const why = missing() ?? (have("melt") ? null : "no melt");

const clip = (id, start, end, bin, effects) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, label: "", effects });
const PROJECT = JSON.stringify({
  version: 11, name: "match", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
  bin: {
    grey: { name: "Grey", source: { color: "#808080" } },
    warm: { name: "Warm", source: { color: "#a08060" } },
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
        clip("w", 0, 2, "warm", [{ type: "grade", saturation: 0.9 }]),
        clip("g", 2, 4, "grey", []),
      ] },
      // A small red picture in the top-left corner, so the frame is not one colour and the
      // live preview has a top and a bottom to get the right way round.
      { id: "v2", kind: "video", name: "PiP", muted: false, hidden: false, transitions: [], clips: [
        clip("pip", 0, 2, "red", [{ type: "transform", x: -120, y: -60, scale: 0.25 }]),
      ] },
    ] } },
  },
}, null, 2);

/** Drag across the middle of the frame on the pick sheet, from a quarter in to three. */
const DRAG = `(() => { const s = document.querySelector(".region-pick"); const r = s.getBoundingClientRect();
  const at = (fx, fy) => ({ bubbles: true, pointerId: 1, clientX: r.left + r.width * fx, clientY: r.top + r.height * fy });
  s.dispatchEvent(new PointerEvent("pointerdown", at(0.25, 0.25)));
  s.dispatchEvent(new PointerEvent("pointermove", at(0.75, 0.75)));
  s.dispatchEvent(new PointerEvent("pointerup", at(0.75, 0.75))); return true; })()`;

const GRADE = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects[0])`;
const click = (sel) => `(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`;
const button = (label) => `[...document.querySelectorAll('#inspector [data-prop="match"] button')].find(b => b.textContent === ${JSON.stringify(label)})`;

describe("matching a grade from the inspector", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-match-"));
    const projectFile = path.join(tmp, "match.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="w"]') && !!document.querySelector("#preview .stage")`);
    await selectClip(page, "w");
    await page.until(`!!document.querySelector('#inspector [data-prop="match"]')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("offers matching to a reference only once there is one", async () => {
    assert.equal(await page.eval(`${button("Match…")}.disabled`), true);
    assert.equal(await page.eval(`document.querySelector('#inspector .match-note').textContent`), "No reference yet");
  });

  it("picks nothing, and measures nothing, when Escape ends the drag", async () => {
    await page.eval(`${button("Neutral…")}.click()`);
    await page.until(`!!document.querySelector(".region-pick")`);
    await page.eval(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })), true`);
    await page.until(`!document.querySelector(".region-pick")`);
    assert.equal(await page.eval(`fetch("/api/jobs").then(r => r.json()).then(j => (Array.isArray(j) ? j : j.jobs ?? []).length)`), 0);
  });

  it("makes a warm region neutral, keeping the rest of the grade", async () => {
    await page.eval(`${button("Neutral…")}.click()`);
    await page.until(`!!document.querySelector(".region-pick")`);
    await page.eval(DRAG);
    await page.until(`${GRADE}.then(e => (e.temperature ?? 0) < -0.5)`, 30000);
    const g = await page.eval(`${GRADE}`);
    assert.equal(g.saturation, 0.9, "the rest of the grade stays");
    assert.equal(g.exposure ?? 0, 0, "neutral leaves the light");
    // The region it measured was the one dragged: the middle half of the 320×180 frame.
    const params = await page.eval(`fetch("/api/jobs").then(r => r.json()).then(j => (Array.isArray(j) ? j : j.jobs).find(x => x.kind === "measure-color").params)`);
    assert.deepEqual(params.region, { x: 80, y: 45, w: 160, h: 90 });
  });

  it("shows a slider's grade on the frame while it is dragged, and the frame without the grade while Before is held", async () => {
    const live = `(() => { const c = document.querySelector("#preview .grade-live"); return c ? !c.hidden : false; })()`;
    // A drag the way a range input reports one: input events, and change on release.
    await page.eval(`(() => { const r = document.querySelector('#inspector [data-prop="temperature"] input[type=range]'); r.value = "0.8"; r.dispatchEvent(new Event("input", { bubbles: true })); return true; })()`);
    await page.until(live, 30000);
    // The right way up: the red corner is at the top left, the warm clip at the bottom right.
    const at = (x, y) => `(() => { const c = document.querySelector("#preview .grade-live"); const k = document.createElement("canvas"); k.width = c.width; k.height = c.height;
      const g = k.getContext("2d"); g.drawImage(c, 0, 0); return [...g.getImageData(Math.round(c.width * ${x}), Math.round(c.height * ${y}), 1, 1).data.slice(0, 3)]; })()`;
    const [corner, rest] = [await page.eval(at(0.06, 0.12)), await page.eval(at(0.9, 0.9))];
    assert.ok(corner[0] > 150 && corner[1] < 70 && rest[1] > 90, `top left ${corner}, bottom right ${rest}`);
    // Before, held: the frame without the grade; let go, and it is gone.
    await page.eval(`window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })), true`);
    const before = `document.querySelector("#inspector .grade-before")`;
    await page.eval(`${before}.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })), true`);
    await page.until(`${before}.classList.contains("on") && ${live}`, 30000);
    await page.eval(`${before}.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })), true`);
    await page.until(`!${live}`);
    // Previewing is not editing.
    assert.equal(await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects[0].temperature ?? 0)`) < 0, true, "the dragged value was never written");
  });

  it("sets a reference where the playhead is, and says so on every grade", async () => {
    // The grey clip, at 3 s: the playhead moves by clicking the ruler 3 s in.
    await page.eval(`(() => { const tl = document.querySelector(".tl"), ruler = document.querySelector(".tl-ruler"); const r = tl.getBoundingClientRect();
      ruler.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 150 + 3 * 60, clientY: r.top + 2 })); window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`document.querySelector("#transport .time").textContent.startsWith("00:03")`);
    await page.eval(`${button("Set reference…")}.click()`);
    await page.until(`!!document.querySelector(".region-pick")`);
    await page.eval(DRAG);
    await page.until(`!${button("Match…")}.disabled`);
    assert.match(await page.eval(`document.querySelector('#inspector .match-note').textContent`), /^Reference: 160×90 px at 00:03\.00 in Main$/);
  });

  it("matches every selected clip to the reference as one edit", async () => {
    // The reference is the grey clip's middle, set just above. Select the grey clip, then add
    // the warm one: the inspector shows the last clicked, and it is the one with a grade.
    await selectClip(page, "g");
    await page.eval(`(() => { const c = document.querySelector('.clip[data-id="w"]'); const r = c.getBoundingClientRect();
      c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, shiftKey: true, clientX: r.left + 20, clientY: r.top + 2 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`!!${button("Match 2 selected")}`);
    const before = await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.revision)`);
    await page.eval(`${button("Match 2 selected")}.click(), true`);
    await page.until(`fetch("/api/project").then(r => r.json()).then(p => p.revision > ${before})`, 60000);
    const doc = await page.eval(`fetch("/api/project").then(r => r.json())`);
    const [w, g] = doc.bin.main.source.tracks[0].clips.map((c) => c.effects.find((e) => e.type === "grade"));
    assert.equal(doc.revision, before + 1, "one edit for both");
    assert.ok(w && g, "both graded");
    assert.equal(w.saturation, 0.9, "the rest of a grade is kept");
  });
});
