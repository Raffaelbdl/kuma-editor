// What an overlay template can measure in the preview, and when.
//
// A template is allowed to measure itself — fitting a line of text to the box it sits in
// is the ordinary way to do it, and the bake loads the page full-size so it works there.
// The preview has to agree, and it did not: the stage hid a clip that is not under the
// playhead with `display: none`, which gives the iframe a 0×0 viewport, so a template
// measuring as it loaded read zeros and kept whatever it had. The editor and the render
// then disagreed about the same clip.
//
// The clip here starts at 6s with the playhead at 0, which is the ordinary case: nothing
// is under the playhead when a project opens. Only a real browser answers this — a 0×0
// layout is something only a layout engine produces.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

/** Records what it could measure while its own scripts ran — the only moment a template
 *  that fits itself to its box gets. The reset is inline so the fixture does not depend on
 *  a `_vendor/` being installed. */
const TEMPLATE = `<!doctype html><html><head><style>
  html, body { margin: 0; width: 100%; height: 100%; background: transparent; }
  #root { position: absolute; inset: 0; }
  #col { width: 50%; height: 10%; }
</style></head><body>
  <div id="root" data-composition-id="probe" data-duration="4" data-width="1920" data-height="1080" data-no-timeline>
    <div id="col"></div>
  </div>
  <script>
    window.__atLoad = { w: innerWidth, h: innerHeight, col: document.getElementById("col").clientWidth };
  </script>
</body></html>`;

const PROJECT = JSON.stringify({
  version: 1, name: "ovl", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    probe: { name: "Probe", source: { template: "probe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [
      { id: "o1", kind: "overlay", name: "O", muted: false, hidden: false, transitions: [],
        clips: [{ id: "oc1", start: 6, end: 10, bin: "probe", in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" }] },
    ] } },
  },
}, null, 2);

const why = missing();

describe("what an overlay can measure in the preview", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const OV = `document.querySelector("#preview .ov")`;
  const seen = (prop) => `getComputedStyle(${OV}).${prop}`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-ovl-"));
    const tpl = path.join(tmp, "library/overlays/probe");
    fs.mkdirSync(tpl, { recursive: true });
    fs.writeFileSync(path.join(tpl, "template.json"), JSON.stringify({ id: "probe", name: "Probe", static: true }));
    fs.writeFileSync(path.join(tpl, "index.html"), TEMPLATE);
    const projectFile = path.join(tmp, "ovl.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector("#preview iframe")?.contentWindow?.__atLoad`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("hands it the project's frame while its scripts run, off screen or not", async () => {
    // The playhead is at 0 and the clip starts at 6, so this page has never been shown.
    // The measurement is asserted first, because it is the defect; the visibility below is
    // only how it is achieved, and a failure should name the zeros.
    assert.deepEqual(await page.eval(`document.querySelector("#preview iframe").contentWindow.__atLoad`), { w: 1920, h: 1080, col: 960 },
      "a hidden overlay still lays out at the project frame; zeros here mean it was hidden with `display: none`");
    assert.equal(await page.eval(seen("visibility")), "hidden");
  });

  it("lays the hidden wrapper out rather than collapsing it", async () => {
    // The distinction the fix turns on: hidden by visibility is still a box, hidden by
    // display is nothing at all, and it is the box the iframe takes its viewport from.
    assert.equal(await page.eval(`${OV}.getBoundingClientRect().width > 0 && ${OV}.getBoundingClientRect().height > 0`), true);
  });

  it("shows it when the playhead reaches the clip, and hides it again after", async () => {
    const seek = (t) => `(() => {
      const tl = document.querySelector(".tl"), ruler = document.querySelector(".tl-ruler");
      const zoom = Number(document.querySelector("#transport input[type=range]").value);
      const r = tl.getBoundingClientRect();
      ruler.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 136 + ${t} * zoom, clientY: r.top + 2 }));
      window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
      return true;
    })()`;
    await page.eval(seek(7));
    await page.until(`${seen("visibility")} === "visible"`);
    await page.eval(seek(2));
    await page.until(`${seen("visibility")} === "hidden"`);
  });
});
