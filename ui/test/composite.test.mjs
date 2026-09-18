// Does the editor composite an overlay over the stage, or paint over it? A dark
// `color-scheme` on the root once stopped Chromium compositing the overlay iframes, so a
// template's white canvas covered the video and nothing failed — only a real browser
// drawing a real iframe can see that.
//
// The assertions are position-independent on purpose: half the overlay is red, so red
// must reach the page and nothing on it may be white. Where the stage sits depends on the
// window and the panel widths, and a test that had to know would break on every layout
// change without ever finding a compositing fault.
//
// Skips, loudly, without a browser or `ui/dist` — as the render tests skip without melt.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { count, decodePng } from "./png.mjs";
import { findBrowser, missing, startDaemon } from "./harness.mjs";

/** A half-red, half-transparent overlay. The reset is inline so the fixture does not
 *  depend on a `_vendor/` being installed; what is under test is the compositing. */
const TEMPLATE = `<!doctype html><html><head><style>
  html, body { margin: 0; width: 100%; height: 100%; background: transparent; }
  #root { position: absolute; inset: 0; }
  #half { position: absolute; left: 0; top: 0; width: 50%; height: 100%; background: #ff0000; }
</style></head><body>
  <div id="root" data-composition-id="probe" data-duration="4" data-width="1920" data-height="1080" data-no-timeline>
    <div id="half"></div>
  </div>
</body></html>`;

const PROJECT = JSON.stringify({
  version: 1, name: "composite", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    probe: { name: "Probe", source: { template: "probe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "o1", kind: "overlay", name: "O", muted: false, hidden: false, transitions: [],
        clips: [{ id: "o1c1", start: 0, end: 4, bin: "probe", in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" }] },
    ] } },
  },
}, null, 2);

const why = missing();

describe("preview compositing", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, url, shot;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-composite-"));
    const tpl = path.join(tmp, "library/overlays/probe");
    fs.mkdirSync(tpl, { recursive: true });
    fs.writeFileSync(path.join(tpl, "template.json"), JSON.stringify({ id: "probe", name: "Probe", static: true }));
    fs.writeFileSync(path.join(tpl, "index.html"), TEMPLATE);
    const projectFile = path.join(tmp, "composite.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);

    ({ url, proc: daemon } = await startDaemon(projectFile, path.join(tmp, "library")));

    shot = path.join(tmp, "shot.png");
    execFileSync(findBrowser(), [
      "--headless", "--disable-gpu", "--no-sandbox", "--hide-scrollbars",
      "--window-size=1280,800", "--virtual-time-budget=15000",
      `--screenshot=${shot}`, url,
    ], { stdio: "ignore", timeout: 120_000 });
  });

  after(() => {
    daemon?.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("draws the overlay", () => {
    const img = decodePng(fs.readFileSync(shot));
    const red = count(img, ([r, g, b]) => r > 170 && g < 80 && b < 80);
    // Without this the next assertion would also pass on a page where the overlay never
    // loaded at all, which is not the thing being checked.
    assert.ok(red > 5000, `expected the overlay's red half on the stage, found ${red} red pixels`);
  });

  it("leaves the rest of the stage transparent", () => {
    const img = decodePng(fs.readFileSync(shot));
    const white = count(img, ([r, g, b]) => r > 240 && g > 240 && b > 240);
    // A template declares no colour scheme, so it is a light-scheme document; give the
    // embedder a dark one and Chromium stops compositing the iframe and paints that
    // document's white canvas over the whole stage — tens of thousands of pixels. Text
    // antialiasing contributes a few hundred at most.
    assert.ok(white < 2000, `${white} near-white pixels: the overlay iframe is painting over the stage`);
  });
});
