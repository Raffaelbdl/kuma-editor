// Matching a grade from the inspector, in a real browser against a real daemon: drag a
// region over the frame, and the grade's white balance and exposure are measured into place
// (`measure_color` through melt). Needs melt too; skips without it, as its neighbours skip
// without a browser.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const hasMelt = (() => { try { execFileSync("melt", ["-version"], { stdio: "ignore" }); return true; } catch { return false; } })();
const why = missing() ?? (hasMelt ? null : "no melt");

const clip = (id, start, end, bin, effects) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, label: "", effects });
const PROJECT = JSON.stringify({
  version: 11, name: "match", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
  bin: {
    grey: { name: "Grey", source: { color: "#808080" } },
    warm: { name: "Warm", source: { color: "#a08060" } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
        clip("w", 0, 2, "warm", [{ type: "grade", saturation: 0.9 }]),
        clip("g", 2, 4, "grey", []),
      ] },
    ] } },
  },
}, null, 2);

/** Select a clip the way a click does: press on it, and let go. */
const selectClip = (page, id) => page.eval(`(() => { const c = document.querySelector('.clip[data-id="${id}"]'); const r = c.getBoundingClientRect();
  c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 20, clientY: r.top + 2 }));
  window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);

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
});
