// How the timeline lays its tracks out, in a real browser: overlays on top, audio at the
// bottom, and the document's own order untouched underneath.
//
// The order tracks are *drawn* in and the order they are *stored* in are two different
// things — the compiler reads the array, a person reads the screen — and the only way to
// check that the screen says one thing while the document still says the other is to load
// the page. Which is also the risk: a "tidy the display" change that quietly reordered the
// document would change every composite in the project.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const track = (id, kind, name) => ({ id, kind, name, muted: false, hidden: false, transitions: [], clips: [] });

/** Stored video, video, overlay, audio — the order a project written by hand or by an
 *  agent can perfectly well be in, and not the order it should be drawn in. */
const PROJECT = JSON.stringify({
  version: 1, name: "timeline", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    main: { name: "Main", source: { markers: [], tracks: [
      track("v1", "video", "Video"),
      track("v2", "video", "Video 2"),
      track("o1", "overlay", "Overlays"),
      track("a1", "audio", "Audio"),
    ] } },
  },
}, null, 2);

const why = missing();

describe("the timeline's tracks", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const DRAWN = `[...document.querySelectorAll("#timeline .tl-track")].map(r => r.dataset.tid).join(" ")`;
  const STORED = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.tracks.map(t => t.id).join(" "))`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-timeline-"));
    const projectFile = path.join(tmp, "timeline.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 4`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("draws overlays on top, then video, then audio", async () => {
    // Within a kind the later track is drawn higher, because that is the one that
    // composites over the other; audio, which composites in no order at all, sits out of
    // the way at the bottom.
    assert.equal(await page.eval(DRAWN), "o1 v2 v1 a1");
  });

  it("leaves the document's order alone", async () => {
    assert.equal(await page.eval(STORED), "v1 v2 o1 a1", "drawing is not an edit");
    // Every row still names its index in the document, which is what every track edit —
    // mute, hide, remove, a clip dragged onto another track — is addressed by.
    assert.deepEqual(await page.eval(`[...document.querySelectorAll("#timeline .tl-track")].map(r => r.dataset.tid + ":" + r.dataset.ti)`), ["o1:2", "v2:1", "v1:0", "a1:3"]);
  });

  it("puts a new track in its lane, wherever the patch adds it", async () => {
    // A video track is stored below the first overlay one (so it composites under it) and
    // is drawn at the top of the video group: one rule, seen from the two ends.
    await page.eval(`(() => { const s = [...document.querySelectorAll("#transport select")].find(s => s.title === "Add a track"); s.value = "video"; s.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 5`);
    assert.equal(await page.eval(STORED), "v1 v2 v3 o1 a1");
    assert.equal(await page.eval(DRAWN), "o1 v3 v2 v1 a1");
  });
});
