// Power's dock, in a real browser with real pointer input: panels dragged by their tabs
// into another group, beside one, beside the frame; a seam dragged; a panel closed and
// called back by its letter; and the arrangement kept across a reload.
//
// The groups are read back as `tabs | tabs`, in the order the dock draws them, the panel in
// front marked `*` — the whole answer to "what is where" in one string.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "dock", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: { main: { name: "Main", source: { markers: [], tracks: [] } } },
});

const why = missing();

describe("the dock", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const GROUPS = `[...document.querySelectorAll(".dock-group:not([hidden])")].map(g => [...g.querySelectorAll(".dock-tab")].map(t => t.dataset.panel + (t.classList.contains("on") ? "*" : "")).join("+")).join(" | ")`;
  const box = (sel) => page.eval(`(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return [r.left, r.top, r.width, r.height]; })()`);
  const key = (k) => page.eval(`(window.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, bubbles: true })), true)`);
  /** Press at `from`, move there in steps, let go: a drag the page cannot tell from a hand's. */
  const drag = async ([x0, y0], [x1, y1]) => {
    await page.mouse("mouseMoved", x0, y0);
    await page.mouse("mousePressed", x0, y0);
    for (let i = 1; i <= 8; i++) await page.mouse("mouseMoved", x0 + ((x1 - x0) * i) / 8, y0 + ((y1 - y0) * i) / 8, { held: true });
    await page.mouse("mouseReleased", x1, y1);
  };
  const tab = async (panel) => { const [x, y, w, h] = await box(`.dock-tab[data-panel="${panel}"]`); return [x + w / 2, y + h / 2]; };
  const group = (panel) => box(`.dock-group:has(.dock-tab[data-panel="${panel}"])`);

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-dock-"));
    const projectFile = path.join(tmp, "dock.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector(".dock-tab")`);
    // Whatever an earlier run left in this origin's storage, start from the default.
    await page.eval(`(localStorage.clear(), location.reload(), true)`);
    await page.until(`!!document.querySelector(".dock-tab")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("starts as power always was: the side panels as tabs, the inspector, the timeline", async () => {
    assert.equal(await page.eval(GROUPS), "bin*+blueprints+fonts+markers+history+scopes+queue | inspector* | timeline*");
  });

  it("holds still until the panels are let move, from the foot of the rail", async () => {
    // Locked, the default: a drag is only a press on the tab, which shows it.
    const [x, y, w, h] = await group("timeline");
    await drag(await tab("queue"), [x + w / 2, y + h / 2]);
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(await page.eval(GROUPS), "bin+blueprints+fonts+markers+history+scopes+queue* | inspector* | timeline*");
    assert.equal(await page.eval(`!!document.querySelector("#app.dock-locked")`), true);
    await page.eval(`document.querySelector("#rail .rail-key.lock").click()`);
    await page.until(`!document.querySelector("#app.dock-locked") && document.querySelector("#rail .rail-key.lock.free")`);
    await page.eval(`document.querySelector('.dock-tab[data-panel="bin"]').dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0 })); window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); true`);
    await page.until(`${GROUPS}.startsWith("bin*")`);
  });

  it("joins a group when a tab is dropped on its middle", async () => {
    const [x, y, w, h] = await group("timeline");
    await drag(await tab("queue"), [x + w / 2, y + h / 2]);
    await page.until(`${GROUPS} === "bin*+blueprints+fonts+markers+history+scopes | inspector* | timeline+queue*"`);
  });

  it("splits a group when a tab is dropped on its edge", async () => {
    const [x, y, w, h] = await group("inspector");
    await drag(await tab("history"), [x + w / 2, y + h - 8]);
    await page.until(`${GROUPS} === "bin*+blueprints+fonts+markers+scopes | inspector* | history* | timeline+queue*"`);
    // Under the inspector, sharing its column: both on screen, one above the other.
    const [, iy, , ih] = await group("inspector");
    const [, hy, , hh] = await group("history");
    assert.ok(ih > 40 && hh > 40, `both have room: ${ih}, ${hh}`);
    assert.ok(hy >= iy + ih - 2, "history is below the inspector");
  });

  it("goes beside the frame when dropped on the frame's edge", async () => {
    const [x, y, , h] = await box("#center");
    await drag(await tab("markers"), [x + 12, y + h / 2]);
    await page.until(`${GROUPS} === "bin*+blueprints+fonts+scopes | markers* | inspector* | history* | timeline+queue*"`);
    const [px, , pw] = await group("markers");
    const [cx] = await box("#center");
    assert.ok(px + pw <= cx + 2, "markers is left of the frame");
  });

  it("is resized by the seam beside it", async () => {
    const [, , w0] = await group("bin");
    const [gx, gy, , gh] = await box('[data-group="g-side"] + .dock-grip');
    await drag([gx, gy + gh / 2], [gx + 60, gy + gh / 2]);
    const [, , w1] = await group("bin");
    assert.ok(Math.abs(w1 - w0 - 60) < 3, `${w0} → ${w1}`);
  });

  it("comes back where it was closed from, by its letter", async () => {
    await page.eval(`document.querySelector('.dock-group:has(.dock-tab[data-panel="inspector"]) .dock-head button[title^="Close"]').click()`);
    await page.until(`${GROUPS} === "bin*+blueprints+fonts+scopes | markers* | history* | timeline+queue*"`);
    await key("i");
    await page.until(`${GROUPS}.includes("inspector*")`);
    // Its group was gone, so it goes beside the frame on the right, where it lives.
    const [ix] = await group("inspector");
    const [cx, , cw] = await box("#center");
    assert.ok(ix >= cx + cw - 2, "the inspector is right of the frame");
  });

  it("docks a floating panel dragged by its title bar onto a group", async () => {
    // The agent has no place in the dock to start with: its letter floats it.
    await key("a");
    await page.until(`!!document.querySelector('.panel.floating[data-panel="agent"]')`);
    const [hx, hy] = await box('.panel.floating[data-panel="agent"] .panel-head');
    const [x, y, w, h] = await group("timeline");
    await drag([hx + 70, hy + 14], [x + w / 2, y + h / 2]);
    await page.until(`!document.querySelector('.panel.floating[data-panel="agent"]') && ${GROUPS}.includes("timeline+queue+agent*")`);
  });

  it("keeps the arrangement across a reload, and the rail brings a tab to the front", async () => {
    const before = await page.eval(GROUPS);
    await page.eval(`(location.reload(), true)`);
    await page.until(`!!document.querySelector(".dock-tab")`);
    assert.equal(await page.eval(GROUPS), before);
    await page.eval(`document.querySelector('#rail .rail-key[data-panel="blueprints"]').click()`);
    await page.until(`${GROUPS}.startsWith("bin+blueprints*+fonts")`);
  });
});
