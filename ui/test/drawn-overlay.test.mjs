// A drawn overlay (`_vendor/kuma-canvas.js`) in the live preview: it redraws on every seek,
// whichever way the page is seeked. It first drew through an `onUpdate` callback, which
// `tl.seek(t)` suppresses, so the preview stayed blank while the bake looked fine.
//
// Skips without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { REPO, driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "drawn", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    wipe: { name: "Wipe", source: { overlay: "overlays/paint-wipe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [] } },
  },
}, null, 2);

const why = missing();

describe("a drawn overlay in the live preview", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-drawn-"));
    fs.cpSync(path.join(REPO, "library/blueprints/paint-wipe"), path.join(tmp, "overlays/paint-wipe"), { recursive: true });
    fs.writeFileSync(path.join(tmp, "drawn.kuma.json"), PROJECT);
    daemon = await startDaemon(path.join(tmp, "drawn.kuma.json"), path.join(REPO, "library"));
    page = await driver(`${daemon.url}/overlay-entry/wipe?duration=1.2&width=480&height=270&rev=1`);
    await page.until(`document.readyState === "complete" && !!window.__timelines?.main`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("redraws on every seek, however it is seeked", async () => {
    const covered = await page.eval(`(() => {
      const tl = window.__timelines.main, c = document.getElementById("paint"), ctx = c.getContext("2d");
      const share = () => { const d = ctx.getImageData(0, 0, c.width, c.height).data; let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n++; return n / (c.width * c.height); };
      const ways = { seek: (t) => tl.seek(t), time: (t) => tl.time(t), renderSeek: (t) => window.__player.renderSeek(t) };
      return Object.fromEntries(Object.entries(ways).map(([name, go]) => [name, [0, 0.6, 1.19, 0.6].map((t) => (go(t), share()))]));
    })()`);
    for (const [way, [start, mid, end, again]] of Object.entries(covered)) {
      assert.equal(start, 0, `${way}: nothing at t = 0`);
      assert.equal(mid, 1, `${way}: the whole frame at the midpoint`);
      assert.equal(end, 0, `${way}: nothing once it has dragged off`);
      assert.equal(again, 1, `${way}: seeking back draws the midpoint again`);
    }
  });
});
