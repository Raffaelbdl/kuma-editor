// Panels in windows of their own coming back to the editor window, in a real browser.
//
// A panel's window (`?panel=<id>`) talks to the editor window over a BroadcastChannel. The
// editor window hides a panel while it is out, so a panel whose window went without being
// heard — its goodbye posted while the page unloaded and lost, as the desktop shell's were
// — stayed hidden, and its letter only tried to focus a window that was gone: the panel was
// unreachable until a reload. This test plays the panel's window with a second channel in
// the same page, which needs no popup and shows every way back.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "panels", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: { main: { name: "Main", source: { markers: [], tracks: [] } } },
});

const why = missing();

describe("a panel's own window", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const SHOWN = `!!document.querySelector('#floating .panel[data-panel="agent"]')`;
  const key = (k) => `(window.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, bubbles: true })), true)`;
  /** The agent panel's window, played from inside the editor's page: it says hello and beats. */
  const OPEN = `(() => {
    const ch = new BroadcastChannel("kuma-view");
    const beat = () => ch.postMessage({ type: "alive", from: "test-window", panel: "agent" });
    ch.postMessage({ type: "hello", from: "test-window", panel: "agent" });
    beat();
    window.__panelWindow = { ch, timer: setInterval(beat, 500) };
    return true;
  })()`;
  /** …and stops, with nothing said: closed by the window manager, crashed, its goodbye lost. */
  const VANISH = `(clearInterval(window.__panelWindow.timer), true)`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-panels-"));
    const projectFile = path.join(tmp, "panels.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector(".modes button")`);
    await page.eval(`(localStorage.clear(), document.querySelector(".modes button").click(), true)`);
    await page.until(`document.getElementById("app").classList.contains("den")`);
    await page.eval(key("a"));
    await page.until(SHOWN);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("is resized from any edge or corner, and stays that size", async () => {
    const BOX = `(() => { const r = document.querySelector('#floating .panel[data-panel="agent"]').getBoundingClientRect(); return [r.left, r.top, r.width, r.height].map(Math.round); })()`;
    const drag = async ([x0, y0], [x1, y1]) => {
      await page.mouse("mouseMoved", x0, y0);
      await page.mouse("mousePressed", x0, y0);
      for (let i = 1; i <= 6; i++) await page.mouse("mouseMoved", x0 + ((x1 - x0) * i) / 6, y0 + ((y1 - y0) * i) / 6, { held: true });
      await page.mouse("mouseReleased", x1, y1);
    };
    const [x, y, w, h] = await page.eval(BOX);
    // The bottom-right corner: wider and taller, the top-left where it was.
    await drag([x + w - 4, y + h - 4], [x + w + 56, y + h + 44]);
    assert.deepEqual(await page.eval(BOX), [x, y, w + 60, h + 48]);
    // The left edge: the right edge stays put.
    await drag([x + 2, y + 100], [x + 42, y + 100]);
    assert.deepEqual(await page.eval(BOX), [x + 40, y, w + 20, h + 48]);
    // The top edge: the bottom stays put.
    await drag([x + 150, y + 2], [x + 150, y + 32]);
    assert.deepEqual(await page.eval(BOX), [x + 40, y + 30, w + 20, h + 18]);
    // Never smaller than a panel can be used at.
    await drag([x + 40 + 2, y + 200], [x + 900, y + 200]);
    assert.equal((await page.eval(BOX))[2], 220);
    // And kept: summoned again, it comes back at the size it was left.
    const kept = await page.eval(BOX);
    await page.eval(key("a"));
    await page.until(`!${SHOWN}`);
    await page.eval(key("a"));
    await page.until(SHOWN);
    assert.deepEqual(await page.eval(BOX), kept);
  });

  it("is hidden here while it is out, and comes back when its window says goodbye", async () => {
    await page.eval(OPEN);
    await page.until(`!${SHOWN}`);
    await page.eval(`(window.__panelWindow.ch.postMessage({ type: "bye", from: "test-window", panel: "agent" }), ${VANISH})`);
    await page.until(SHOWN);
  });

  it("comes back when its window stops beating, with no goodbye", async () => {
    await page.eval(OPEN);
    await page.until(`!${SHOWN}`);
    await page.eval(VANISH);
    await page.until(SHOWN, 10_000);
    // A beat that was already on its way does not hide it again.
    await new Promise((r) => setTimeout(r, 1500));
    assert.equal(await page.eval(SHOWN), true);
  });

  it("is called back by its letter, and its window is told to close", async () => {
    await page.eval(OPEN);
    await page.until(`!${SHOWN}`);
    await page.eval(`(() => { window.__recalled = false; window.__panelWindow.ch.onmessage = (e) => { if (e.data?.type === "recall" && e.data.panel === "agent") window.__recalled = true; }; return true; })()`);
    await page.eval(key("a"));
    await page.until(SHOWN);
    await page.until(`window.__recalled`);
    await page.eval(VANISH);
    // And the letter is a toggle again.
    await page.eval(key("a"));
    await page.until(`!${SHOWN}`);
    await page.eval(key("a"));
    await page.until(SHOWN);
  });
});
