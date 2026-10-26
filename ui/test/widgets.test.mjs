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

  it("keeps the grade's other curves a tab away, and a hue curve wraps with no ends", async () => {
    const tab = (key) => `document.querySelector('#inspector .curve-tabs [data-curve="${key}"]')`;
    assert.equal(await page.eval(`${tab("curve")}.classList.contains("on")`), true, "the tone curve first");
    await page.eval(`${tab("hue_sat")}.click(), true`);
    assert.equal(await page.eval(`!!document.querySelector("#inspector .curve-plot.curve-hue")`), true);
    assert.equal(await page.eval(`document.querySelectorAll("#inspector .curve-pt").length`), 0, "no corners round a circle");
    // Pressed at hue 0.3, a quarter of the way down: an amount of 0.5.
    await page.eval(drag("#inspector .curve-plot", [0.3, 0.25], [0.3, 0.25]));
    await page.until(`${GRADE}.then(e => e.hue_sat?.length === 1)`);
    assert.deepEqual(await page.eval(`${GRADE}.then(e => e.hue_sat)`), [[0.3, 0.5]]);
    await settled(page);
    // The edit rebuilt the inspector, and the tab it was made in is still the one open; the
    // tone curve's tab says it bends something.
    await page.until(`${tab("hue_sat")}?.classList.contains("on")`);
    assert.equal(await page.eval(`${tab("curve")}.classList.contains("set")`), true);
    // Flat takes the curve out of the document, rather than leaving an empty list.
    await page.eval(`[...document.querySelectorAll("#inspector .curve-reset")].find(b => b.offsetParent).click(), true`);
    await page.until(`${GRADE}.then(e => !("hue_sat" in e))`);
    await settled(page);
    await page.eval(`${tab("curve")}.click(), true`);
  });

  it("names the camera log from a list, and none leaves the field out", async () => {
    const pick = (v) => `(() => { const s = document.querySelector('#inspector [data-prop="log"] select'); s.value = ${JSON.stringify(v)}; s.dispatchEvent(new Event("change")); return true; })()`;
    assert.equal(await page.eval(`document.querySelector('#inspector [data-prop="log"] select').value`), "");
    await page.eval(pick("v-log"));
    await page.until(`${GRADE}.then(e => e.log === "v-log")`);
    await settled(page);
    await page.until(`!!document.querySelector('#inspector [data-prop="log"] select')`);
    await page.eval(pick(""));
    await page.until(`${GRADE}.then(e => !("log" in e))`);
    await settled(page);
    // A log is a file: one put in the project's `logs/` is offered once the list is opened.
    fs.mkdirSync(path.join(tmp, "logs"), { recursive: true });
    fs.writeFileSync(path.join(tmp, "logs", "my-camera.json"), JSON.stringify({ name: "My camera", lut: "mine.cube" }));
    await page.eval(`document.querySelector('#inspector [data-prop="log"] select').dispatchEvent(new FocusEvent("focus")), true`);
    await page.until(`[...document.querySelectorAll('#inspector [data-prop="log"] option')].some(o => o.value === "my-camera" && o.textContent === "My camera")`);
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

describe("copying an effect onto other clips", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const clip = (id, start, end, effects) => ({ id, start, end, bin: "card", in: 0, speed: 1, volume: 1, muted: false, label: "", effects });
  const COPIES = JSON.stringify({
    version: 11, name: "copies", master: "main",
    fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
    bin: {
      card: { name: "Card", source: { color: "#808080" } },
      main: { name: "Main", source: { markers: [], tracks: [
        { id: "v1", kind: "video", name: "Video", muted: false, hidden: false, transitions: [], clips: [
          clip("a", 0, 1, [{ type: "grade", temperature: 0.4, saturation: 0.8 }]),
          clip("b", 1, 2, [{ type: "grade", exposure: 1 }, { type: "blur", radius: 2 }]),
          clip("c", 2, 3, []),
        ] },
      ] } },
    },
  }, null, 2);
  const effects = (i) => `fetch("/api/project").then(r => r.json()).then(p => JSON.stringify(p.bin.main.source.tracks[0].clips[${i}].effects))`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-copies-"));
    const projectFile = path.join(tmp, "copies.kuma.json");
    fs.writeFileSync(projectFile, COPIES);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="a"]')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("pastes a grade onto every selected clip as one edit: replacing a grade, adding one, leaving the rest", async () => {
    await selectClip(page, "a");
    await page.until(`!!document.querySelector("#inspector .fx-copy")`);
    await page.eval(`document.querySelector("#inspector .fx-copy").click(), true`);
    // Select b and c (Shift adds to a selection), then Ctrl + Alt + V.
    await selectClip(page, "b");
    await page.eval(`(() => { const c = document.querySelector('.clip[data-id="c"]'); const r = c.getBoundingClientRect();
      c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, shiftKey: true, clientX: r.left + 20, clientY: r.top + 2 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);
    await page.until(`document.querySelectorAll(".clip.selected").length === 2`);
    const before = await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.revision)`);
    await page.eval(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "v", ctrlKey: true, altKey: true, bubbles: true })), true`);
    await page.until(`fetch("/api/project").then(r => r.json()).then(p => p.revision > ${before})`);
    const grade = { type: "grade", temperature: 0.4, saturation: 0.8 };
    assert.deepEqual(JSON.parse(await page.eval(effects(1))), [grade, { type: "blur", radius: 2 }], "b's grade replaced, its blur kept");
    assert.deepEqual(JSON.parse(await page.eval(effects(2))), [grade], "c given one");
    assert.equal(await page.eval(`fetch("/api/project").then(r => r.json()).then(p => p.revision)`), before + 1, "one edit");
  });
});

describe("a mask: where an effect acts", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const EFFECTS = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks[0].clips[0].effects)`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-mask-"));
    const projectFile = path.join(tmp, "mask.kuma.json");
    fs.writeFileSync(projectFile, PROJECT.replace(`"effects": [{ "type": "grade", "curve": [[0.5, 0.6]] }]`, `"effects": [{ "type": "grade" }]`));
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.clip[data-id="c1"]')`);
    await selectClip(page, "c1");
    await page.until(`!!document.querySelector('#inspector [data-prop="where"] select')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("puts a shape in the middle of the frame, and previews the grade inside it alone", async () => {
    await page.eval(`(() => { const s = document.querySelector('#inspector [data-prop="where"] select'); s.value = "rectangle"; s.dispatchEvent(new Event("change")); return true; })()`);
    await page.until(`${EFFECTS}.then(e => !!e[0].mask)`);
    assert.deepEqual(await page.eval(`${EFFECTS}.then(e => e[0].mask)`), { shape: "rectangle", x: 80, y: 45, w: 160, h: 90, feather: 9 });
    await settled(page);
    await page.until(`!!document.querySelector('#inspector [data-prop="where"] .vec')`);
    // An exposure dragged: brighter inside the rectangle, the corner as it was.
    await page.eval(`(() => { const r = document.querySelector('#inspector [data-prop="exposure"] input[type=range]'); r.value = "2"; r.dispatchEvent(new Event("input", { bubbles: true })); return true; })()`);
    await page.until(`(() => { const c = document.querySelector("#preview .grade-live"); return c ? !c.hidden : false; })()`, 30000);
    const at = (x, y) => `(() => { const c = document.querySelector("#preview .grade-live"); const k = document.createElement("canvas"); k.width = c.width; k.height = c.height;
      const g = k.getContext("2d"); g.drawImage(c, 0, 0); return [...g.getImageData(Math.round(c.width * ${x}), Math.round(c.height * ${y}), 1, 1).data.slice(0, 3)]; })()`;
    const [inside, corner] = [await page.eval(at(0.5, 0.5)), await page.eval(at(0.05, 0.05))];
    assert.ok(inside[0] > 170 && Math.abs(corner[0] - 128) < 6, `inside ${inside}, corner ${corner}`);
    await page.eval(`window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })), true`);
  });

  it("keys the shape at the playhead, writes into the key, and holds where it was when the last key goes", async () => {
    const where = `document.querySelector('#inspector [data-prop="where"]')`;
    const press = (label) => `[...${where}.querySelectorAll("button")].find(b => b.textContent === ${JSON.stringify(label)}).click(), true`;
    await page.eval(press("+ key"));
    await page.until(`${EFFECTS}.then(e => e[0].mask.keyframes?.length === 1)`);
    assert.deepEqual(await page.eval(`${EFFECTS}.then(e => e[0].mask.keyframes[0])`), { t: 0, x: 80, y: 45, w: 160, h: 90, rotation: 0 });
    await settled(page);
    await page.until(`${where}.textContent.includes("1 key")`);
    // X typed with the mask keyed: the key changes, the static shape does not.
    await page.eval(`(() => { const i = ${where}.querySelector(".axis-x input"); i.focus(); i.value = "200"; i.dispatchEvent(new Event("change", { bubbles: true })); i.blur(); return true; })()`);
    await page.until(`${EFFECTS}.then(e => e[0].mask.keyframes[0].x === 200)`);
    assert.equal(await page.eval(`${EFFECTS}.then(e => e[0].mask.x)`), 80);
    await settled(page);
    await page.eval(press("− key"));
    await page.until(`${EFFECTS}.then(e => !e[0].mask.keyframes)`);
    assert.equal(await page.eval(`${EFFECTS}.then(e => e[0].mask.x)`), 200, "the last key gone, the shape stays where it was");
    await settled(page);
  });

  it("offers keys on a grade's light", async () => {
    await page.until(`!!document.querySelector('#inspector .kf-toggle')`);
    await page.eval(`document.querySelector('#inspector .kf-toggle').click(), true`);
    await page.until(`${EFFECTS}.then(e => (e[0].keyframes ?? []).length > 0)`);
    const key = await page.eval(`${EFFECTS}.then(e => e[0].keyframes[0])`);
    assert.deepEqual(Object.keys(key).sort(), ["exposure", "t", "temperature", "tint"]);
    await settled(page);
  });

  it("adds a second window of the same effect, and the whole picture takes the mask away", async () => {
    await page.eval(`[...document.querySelectorAll('#inspector [data-prop="where"] button')].find(b => b.textContent === "+ window").click(), true`);
    await page.until(`${EFFECTS}.then(e => e.length === 2)`);
    const second = await page.eval(`${EFFECTS}.then(e => e[1])`);
    assert.equal(second.type, "grade");
    assert.equal(second.mask.shape, "ellipse");
    await settled(page);
    await page.until(`document.querySelectorAll('#inspector [data-prop="where"] select').length === 2`);
    await page.eval(`(() => { const s = document.querySelector('#inspector [data-prop="where"] select'); s.value = ""; s.dispatchEvent(new Event("change")); return true; })()`);
    await page.until(`${EFFECTS}.then(e => !("mask" in e[0]))`);
    await settled(page);
  });
});

describe("a grade on the source: every placement's", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  // A one-pixel grey PNG: an image entry to grade.
  const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNoAAAAggCBd81ytgAAAABJRU5ErkJggg==", "base64");
  const ENTRY = `fetch("/api/project").then(r => r.json()).then(p => p.bin.still)`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-entry-"));
    fs.writeFileSync(path.join(tmp, "still.png"), PNG);
    const projectFile = path.join(tmp, "entry.kuma.json");
    const p = JSON.parse(PROJECT);
    p.bin.still = { name: "Still", source: { path: "still.png", kind: "image" } };
    fs.writeFileSync(projectFile, JSON.stringify(p));
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.bin-row[data-sel="bin:still"]')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("adds a grade to an image entry, with no shape or match of its own", async () => {
    await page.eval(`document.querySelector('.bin-row[data-sel="bin:still"]').click(), true`);
    await page.until(`[...document.querySelectorAll("#inspector h3")].some(h => h.textContent === "On the source")`);
    await page.eval(`(() => { const s = [...document.querySelectorAll("#inspector select")].find(s => s.options[0]?.textContent === "+ add effect…"); s.value = "grade"; s.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
    await page.until(`${ENTRY}.then(e => e.effects?.[0]?.type === "grade")`);
    await settled(page);
    await page.until(`!!document.querySelector('#inspector [data-prop="where"]')`);
    assert.match(await page.eval(`document.querySelector('#inspector [data-prop="where"]').textContent`), /a shape goes on a clip's own grade/);
    assert.match(await page.eval(`document.querySelector('#inspector [data-prop="match"]').textContent`), /match there/);
    assert.equal(await page.eval(`!!document.querySelector('#inspector .kf-toggle')`), false, "an entry's grade holds still");
  });
});
