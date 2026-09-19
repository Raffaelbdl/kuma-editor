// How the timeline lays its tracks out, in a real browser: overlays on top, audio at the
// bottom, and the document's own order untouched underneath.
//
// Drawn order and stored order are two different things: the compiler reads the array, a
// person reads the screen. Only a loaded page can check that the screen says one while the
// document still says the other — and a "tidy the display" change that quietly reordered
// the document would change every composite in the project.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const track = (id, kind, name, clips = []) => ({ id, kind, name, muted: false, hidden: false, transitions: [], clips });
const clip = (id, bin, start, end) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" });

/** Stored video, video, overlay, audio — the order a project written by hand or by an
 *  agent can perfectly well be in, and not the order it should be drawn in. The two clips
 *  on the first track put cuts at 0, 4, 6 and 10 seconds, which is what Alt+arrow walks. */
const PROJECT = JSON.stringify({
  version: 1, name: "timeline", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [
      track("v1", "video", "Video", [clip("c1", "red", 0, 4), clip("c2", "red", 6, 10)]),
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
  const TIME = `document.querySelector("#transport .time").textContent.split(" ")[0]`;
  /** A key, sent from `selector` (or the window), answering whether the page took it. A
   *  synthetic event runs no default action, so `defaultPrevented` is the only way to ask
   *  "would the browser have scrolled?" — which is the whole complaint about the arrows. */
  const key = (k, opts = {}, selector = null) => `(() => {
    const e = new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, bubbles: true, cancelable: true, ...${JSON.stringify(opts)} });
    (${selector ? `document.querySelector(${JSON.stringify(selector)})` : "window"}).dispatchEvent(e);
    return e.defaultPrevented;
  })()`;
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

  it("steps a frame on the arrows, and does not leave the key to the browser", async () => {
    assert.equal(await page.eval(key("Home")), true);
    assert.equal(await page.eval(TIME), "00:00.00");
    assert.equal(await page.eval(key("ArrowRight")), true, "an arrow the page ignores is an arrow that scrolls the timeline");
    assert.equal(await page.eval(TIME), "00:00.01");
    assert.equal(await page.eval(key("ArrowRight", { shiftKey: true })), true);
    assert.equal(await page.eval(TIME), "00:01.01", "Shift is the second, as it was");
    // The last thing clicked keeps the focus, and a track's mute button is a thing people
    // click. The arrows are still the timeline's afterwards; Space still presses the button.
    assert.equal(await page.eval(key("ArrowLeft", {}, ".tl-head .toggles button")), true);
    assert.equal(await page.eval(TIME), "00:01.00");
    assert.equal(await page.eval(key(" ", { code: "Space" }, ".tl-head .toggles button")), false, "Space belongs to the button under the cursor");
  });

  it("jumps to the next cut on Alt, and stops at the last one", async () => {
    await page.eval(key("Home"));
    for (const at of ["00:04.00", "00:06.00", "00:10.00", "00:10.00"]) {
      assert.equal(await page.eval(key("ArrowRight", { altKey: true })), true);
      assert.equal(await page.eval(TIME), at, "forward: every clip start and end, then nothing past the last");
    }
    for (const at of ["00:06.00", "00:04.00", "00:00.00", "00:00.00"]) {
      assert.equal(await page.eval(key("ArrowLeft", { altKey: true })), true);
      assert.equal(await page.eval(TIME), at, "and back the same way, down to the start");
    }
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
