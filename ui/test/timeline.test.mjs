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

  it("adds a marker with a comment on M, into the document an agent reads", async () => {
    const MARKERS = `fetch("/api/project").then(r => r.json()).then(p => JSON.stringify(p.bin.main.source.markers))`;
    const FLAGS = `[...document.querySelectorAll("#timeline .tl-marker-flag")].map(f => f.textContent).join("|")`;
    const DIALOG = `document.querySelector("dialog.modal textarea.comment")`;
    /** Type into the open comment dialog and send it with Ctrl+Enter, as a person would. */
    const answer = (text) => `(() => {
      const t = ${DIALOG};
      t.value = ${JSON.stringify(text)};
      t.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
      return true;
    })()`;

    await page.eval(key("Home"));
    await page.eval(key("ArrowRight", { shiftKey: true }));
    await page.eval(key("ArrowRight", { shiftKey: true }));
    await page.eval(key("m"));
    await page.until(DIALOG);
    await page.eval(answer("the title comes in too early\nwait for the beat"));
    await page.until(`${FLAGS} === "the title comes in too early"`);
    assert.deepEqual(JSON.parse(await page.eval(MARKERS)), [{ t: 2, label: "the title comes in too early\nwait for the beat", color: null }],
      "the comment is in the project file, whole, at the playhead's time");

    // A second one earlier on stays in time order in the document, whatever order they came in.
    await page.eval(key("Home"));
    await page.eval(key("m"));
    await page.until(DIALOG);
    await page.eval(answer("cold open"));
    await page.until(`${FLAGS} === "cold open|the title comes in too early"`);

    // M on a frame that has one edits it rather than stacking a second note on the same frame.
    await page.eval(key("m"));
    await page.until(DIALOG);
    assert.equal(await page.eval(`${DIALOG}.value`), "cold open");
    await page.eval(answer("cold open: cut the first second"));
    await page.until(`${FLAGS} === "cold open: cut the first second|the title comes in too early"`);
    assert.equal(JSON.parse(await page.eval(MARKERS)).length, 2);

    // Right-click → Delete marker.
    await page.eval(`(() => {
      const f = document.querySelector('#timeline .tl-marker-flag[data-i="0"]');
      f.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
      [...document.querySelectorAll(".popup-menu .popup-item")].find(b => b.textContent === "Delete marker").click();
      return true;
    })()`);
    await page.until(`${FLAGS} === "the title comes in too early"`);
    assert.equal(await page.eval(`document.querySelectorAll("#timeline .tl-marker").length`), 1, "the line goes with its flag");
    // Every marker edit is one undoable patch.
    await page.eval(`(${key("z", { ctrlKey: true })})`);
    await page.until(`${FLAGS} === "cold open: cut the first second|the title comes in too early"`);
  });

  it("moves a marker by its flag, and edits it on a double-click", async () => {
    const MARKERS = `fetch("/api/project").then(r => r.json()).then(p => p.bin.main.source.markers.map(m => m.t).join(" "))`;
    const flagAt = (i) => page.eval(`(() => { const r = document.querySelector('#timeline .tl-marker-flag[data-i="${i}"]').getBoundingClientRect(); return [r.left + 6, r.top + r.height / 2]; })()`);
    const pxPerSecond = await page.eval(`Number(document.querySelector("#transport input[type=range]").value)`);
    assert.equal(await page.eval(MARKERS), "0 2");

    // Drag the marker at 2s one second right: the document moves it, and nothing else.
    const [x, y] = await flagAt(1);
    await page.mouse("mousePressed", x, y);
    for (let k = 1; k <= 5; k++) await page.mouse("mouseMoved", x + (pxPerSecond * k) / 5, y, { held: true });
    await page.mouse("mouseReleased", x + pxPerSecond, y);
    await page.until(`document.querySelector('#timeline .tl-marker-flag[data-i="1"]').title.includes("3.00s")`);
    assert.equal(await page.eval(MARKERS), "0 3");

    // A double-click on a flag opens its comment — the flag's own pointer handling must
    // not swallow it.
    const [x2, y2] = await flagAt(1);
    for (const clickCount of [1, 2]) {
      await page.mouse("mousePressed", x2, y2, { clickCount });
      await page.mouse("mouseReleased", x2, y2, { clickCount });
    }
    await page.until(`document.querySelector("dialog.modal textarea.comment")?.value === "the title comes in too early\\nwait for the beat"`);
    await page.eval(`(document.querySelector("dialog.modal .modal-actions button").click(), true)`);
    assert.equal(await page.eval(MARKERS), "0 3", "a double-click is not a drag");
  });

  it("puts a new track in its lane, wherever the patch adds it", async () => {
    // A video track is stored below the first overlay one (so it composites under it) and
    // is drawn at the top of the video group: one rule, seen from the two ends.
    await page.eval(`(() => { const s = [...document.querySelectorAll("#transport select")].find(s => s.title === "Add a track"); s.value = "video"; s.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 5`);
    assert.equal(await page.eval(STORED), "v1 v2 v3 o1 a1");
    assert.equal(await page.eval(DRAWN), "o1 v3 v2 v1 a1");
  });

  it("marks a clip muted on its own, whose track's M stays off", async () => {
    // As an agent mutes one: a patch to the clip, from outside the page.
    const CLIP = `document.querySelector('#timeline .clip[data-id="c1"]')`;
    const BADGE = `${CLIP}.querySelectorAll(".cname svg.mute-badge").length`;
    assert.equal(await page.eval(BADGE), 0);
    const ok = await page.eval(`fetch("/api/project").then(r => r.json()).then(p => fetch("/api/project/patch", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ base_revision: p.revision, patch: [{ op: "replace", path: "/bin/main/source/tracks/0/clips/0/muted", value: true }] }),
    })).then(r => r.ok)`);
    assert.equal(ok, true);
    await page.until(`${BADGE} === 1`);
    assert.equal(await page.eval(`${CLIP}.classList.contains("silenced")`), true);
    assert.match(await page.eval(`${CLIP}.title`), /muted — this clip only/);
    assert.equal(await page.eval(`document.querySelectorAll('#timeline .clip[data-id="c2"] .mute-badge').length`), 0, "its neighbour still sounds");
    assert.equal(await page.eval(`document.querySelector('#timeline .tl-track[data-tid="v1"] .toggles button[title="Mute"]').className`), "", "the track is not muted, and its button says so");
    // Undone, the badge goes with it.
    await page.eval(key("z", { ctrlKey: true }));
    await page.until(`${BADGE} === 0`);
  });
});
