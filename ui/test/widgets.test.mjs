// The inspector's curve, colour wheels and colour picker, in a real browser against a real
// daemon: each gesture is one patch, and the document says what the widget showed. Skips,
// loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, selectClip, settled, startDaemon } from "./harness.mjs";

const why = missing();

const PROJECT = JSON.stringify({
  version: 11, name: "widgets", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
  bin: {
    card: { name: "Card", source: { color: "#808080" } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
        { id: "c1", start: 0, end: 4, bin: "card", in: 0, speed: 1, volume: 1, muted: false, label: "", effects: [{ type: "grade", curve: [[0.5, 0.6]] }] },
      ] },
    ] } },
  },
}, null, 2);

const GRADE = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects[0])`;
const CARD = `fetch("/api/project").then(r => r.json()).then(p => p.bin.card.source.color)`;

/** Press at a fraction of `sel`'s box, drag to another, and let go — a pointer the way
 *  `dragPointer` reads one: down on the element, moves and up on the window. */
const drag = (sel, [fx0, fy0], [fx1, fy1], index = 0) => `(() => {
  const t = document.querySelectorAll(${JSON.stringify(sel)})[${index}]; const r = t.getBoundingClientRect();
  const at = (fx, fy) => ({ bubbles: true, button: 0, pointerId: 1, clientX: r.left + r.width * fx, clientY: r.top + r.height * fy });
  t.dispatchEvent(new PointerEvent("pointerdown", at(${fx0}, ${fy0})));
  window.dispatchEvent(new PointerEvent("pointermove", at(${(fx0 + fx1) / 2}, ${(fy0 + fy1) / 2})));
  window.dispatchEvent(new PointerEvent("pointermove", at(${fx1}, ${fy1})));
  window.dispatchEvent(new PointerEvent("pointerup", at(${fx1}, ${fy1}))); return true; })()`;

describe("the inspector's curve, wheels and colour picker", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-widgets-"));
    const projectFile = path.join(tmp, "widgets.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="c1"]')`);
    await selectClip(page, "c1");
    await page.until(`!!document.querySelector("#inspector .curve-plot") && !!document.querySelector("#inspector .wheel-disc")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("draws the curve's points with the corners it passes through", async () => {
    assert.equal(await page.eval(`document.querySelectorAll("#inspector .curve-pt").length`), 3);
    assert.equal(await page.eval(`document.querySelectorAll("#inspector .curve-pt.end").length`), 2);
  });

  it("adds a point where it is pressed and moves it while held, as one edit", async () => {
    const before = await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.revision)`);
    // Pressed at in 0.25, out 0.5, and dragged up to 0.6 before letting go.
    await page.eval(drag("#inspector .curve-plot", [0.25, 0.5], [0.25, 0.4]));
    await page.until(`${GRADE}.then(e => e.curve.length === 2)`);
    assert.deepEqual(await page.eval(`${GRADE}.then(e => e.curve)`), [[0.25, 0.6], [0.5, 0.6]]);
    assert.equal(await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.revision)`), before + 1, "one gesture, one patch");
    await settled(page);
  });

  it("keeps a point between its neighbours, and takes it out on a double-click", async () => {
    // The point at 0.5 dragged far left stops just past the one at 0.25.
    await page.eval(drag("#inspector .curve-pt", [0.5, 0.5], [-100, 0.5], 2));
    await page.until(`${GRADE}.then(e => e.curve[1][0] < 0.5)`);
    assert.equal((await page.eval(`${GRADE}.then(e => e.curve)`))[1][0], 0.26);
    await settled(page);
    await page.eval(`document.querySelectorAll("#inspector .curve-pt")[1].dispatchEvent(new MouseEvent("dblclick", { bubbles: true })), true`);
    await page.until(`${GRADE}.then(e => e.curve.length === 1)`);
    await settled(page);
  });

  it("turns a wheel's colour without moving its brightness, and a double-click takes the colour out", async () => {
    // The lift wheel, dragged from its centre to the right: towards red.
    await page.eval(drag("#inspector .wheel-disc", [0.5, 0.5], [0.8, 0.5], 0));
    await page.until(`${GRADE}.then(e => (e.lift?.r ?? 0) > 0)`);
    const { r, g, b } = await page.eval(`${GRADE}.then(e => e.lift)`);
    assert.ok(r > 0 && g < 0 && b < 0 && Math.abs(g - b) < 1e-9, JSON.stringify({ r, g, b }));
    assert.ok(Math.abs(r + g + b) < 0.003, "the shared part — brightness — stays where it was");
    await settled(page);
    await page.eval(`document.querySelectorAll("#inspector .wheel-disc")[0].dispatchEvent(new MouseEvent("dblclick", { bubbles: true })), true`);
    await page.until(`${GRADE}.then(e => !e.lift || (e.lift.r === e.lift.g && e.lift.g === e.lift.b))`);
    await settled(page);
  });

  it("picks a colour with alpha, and stays open while it is used", async () => {
    await page.eval(`document.querySelector('[data-sel="bin:card"]').click(), true`);
    await page.until(`!!document.querySelector("#inspector .color-swatch")`);
    await page.eval(`document.querySelector("#inspector .color-swatch").click(), true`);
    await page.until(`!!document.querySelector("#inspector .color-alpha")`);
    // Half alpha: the browser's own colour input would have dropped it.
    await page.eval(drag("#inspector .color-alpha", [0.5, 0.5], [0.5, 0.5]));
    await page.until(`${CARD}.then(c => c === "#80808080")`);
    await settled(page);
    assert.equal(await page.eval(`!!document.querySelector("#inspector .color-alpha")`), true, "the edit rebuilt the panel, and the picker is still open");
    // Typed, in any form the model takes.
    await page.eval(`(() => { const i = document.querySelector("#inspector .color-hex"); i.value = "f00"; i.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
    await page.until(`${CARD}.then(c => c === "#ff0000")`);
  });
});
