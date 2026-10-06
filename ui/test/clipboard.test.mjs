// The timeline clipboard, in a real browser against a real daemon: copy a clip, click
// another track, paste it at the playhead.
//
// What it pins is what no Rust test can: where a paste *lands*. That answer is three
// pieces of view state the daemon never sees — the selection, the playhead, the track last
// clicked — plus one document rule, a taken slot pushing the clip to the next free one.
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

/** `main` has two video tracks — one with a clip, one empty — plus an overlay track, so
 *  "paste onto the track I clicked" and "a track of the wrong kind cannot take it" are
 *  both askable. `intro` has an overlay track and no video one, which is where a paste
 *  into another sequence has to make the track it needs. Colour entries, so the fixture
 *  needs no media on disk. */
const PROJECT = JSON.stringify({
  version: 1, name: "clipboard", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    probe: { name: "Probe", source: { overlay: "overlays/probe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [
      track("v1", "video", "Video", [clip("c1", "red", 0, 4)]),
      track("v2", "video", "Video 2"),
      track("o1", "overlay", "Overlays"),
      track("a1", "audio", "Audio"),
    ] } },
    intro: { name: "Intro", source: { markers: [], tracks: [
      track("io1", "overlay", "Overlays", [clip("i1", "probe", 0, 4)]),
    ] } },
  },
}, null, 2);

const TEMPLATE = `<!doctype html><html><head></head><body>
  <div id="root" data-composition-id="probe" data-duration="4" data-width="1920" data-height="1080" data-no-timeline></div>
</body></html>`;

const why = missing();

describe("the timeline clipboard", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  /** Every track's clips as `track:bin@start-end`, in document order — the whole answer
   *  to "where did it land" in one string. The comparison happens inside the page: an
   *  edit is a round trip, so the test waits on the document rather than on a delay. */
  const CLIPS = (seq) => `fetch("/api/project").then(r => r.json()).then(p => p.bin[${JSON.stringify(seq)}].source.tracks
    .flatMap(t => t.clips.map(c => t.id + ":" + c.bin + "@" + c.start + "-" + c.end)).join(" "))`;
  /** Wait for the timeline to read exactly `expected`, then say what it read instead. The
   *  comparison happens in the page because `until` cannot await a promise for its
   *  answer; the re-read on timeout is only there to make the failure legible. */
  const clipsAre = async (expected, seq = "main") => {
    try {
      await page.until(`${CLIPS(seq)}.then(s => s === ${JSON.stringify(expected)})`, 8000);
    } catch {
      assert.equal(await page.eval(CLIPS(seq)), expected);
    }
  };
  const REVISION = `fetch("/api/project").then(r => r.json()).then(p => p.revision)`;

  /** Drive the page the way a person does: a pointer on the timeline, keys on the window. */
  const pointer = (selector, dx = 0, modifiers = {}) => `(() => {
    const e = document.querySelector(${JSON.stringify(selector)});
    const r = e.getBoundingClientRect();
    e.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + ${dx}, clientY: r.top + 2, ...${JSON.stringify(modifiers)} }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
    return true;
  })()`;
  const selectClip = (id) => pointer(`.clip[data-id="${id}"]`, 20);
  /** The one clip on a track — Ctrl-clicked, which is how a selection of several is built. */
  const addClipOn = (track) => pointer(`.tl-track[data-tid="${track}"] .clip`, 20, { ctrlKey: true });
  const clickTrack = (id) => pointer(`.tl-track[data-tid="${id}"] .tl-head`);
  /** Put the playhead at `t` seconds by clicking the ruler, at the zoom the page is on. */
  const seek = (t) => `(() => {
    const tl = document.querySelector(".tl"), ruler = document.querySelector(".tl-ruler");
    const zoom = Number(document.querySelector("#timeline .tl-corner input.zoom").value);
    const r = tl.getBoundingClientRect();
    ruler.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 150 + ${t} * zoom, clientY: r.top + 2 }));
    window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
    return true;
  })()`;
  const key = (k) => `window.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, ctrlKey: true, bubbles: true })) || true`;
  const TIME = `document.querySelector("#transport .time").textContent`;
  const CLIP_IDS = `[...document.querySelectorAll("#timeline .clip:not(.ghost)")].map(c => c.dataset.id)`;

  /** Paste, and wait until the page has finished with it — a pasted clip becomes the
   *  selection, and that lands one round trip after the patch. A person cannot click
   *  inside that window; a test sending its next gesture immediately loses the race half
   *  the time, and would then cut the clip it had just pasted. */
  const paste = async () => {
    const had = await page.eval(CLIP_IDS);
    await page.eval(key("v"));
    await page.until(`(() => { const s = document.querySelector("#timeline .clip.selected"); return !!s && !${JSON.stringify(had)}.includes(s.dataset.id); })()`);
  };

  /** Switch the sequence being edited, the way the topbar picker does. */
  const openSequence = (id) => `(() => { document.querySelector('#library .bin-row[data-sel="bin:' + ${JSON.stringify(id)} + '"]').dispatchEvent(new MouseEvent("dblclick", { bubbles: true })); return true; })()`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-clipboard-"));
    const tpl = path.join(tmp, "overlays/probe");
    fs.mkdirSync(tpl, { recursive: true });
    fs.writeFileSync(path.join(tpl, "overlay.json"), JSON.stringify({ name: "Probe", static: true }));
    fs.writeFileSync(path.join(tpl, "index.html"), TEMPLATE);
    const projectFile = path.join(tmp, "clipboard.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);

    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 4`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("copies onto the track that was clicked, at the playhead", async () => {
    const before = await page.eval(REVISION);
    await page.eval(selectClip("c1"));
    await page.eval(key("c"));
    // A copy is not an edit: it must cost no revision, and so no undo step.
    assert.equal(await page.eval(REVISION), before, "copying must not touch the document");

    await page.eval(seek(5));
    await page.until(`${TIME}.startsWith("00:05")`);
    await page.eval(clickTrack("v2"));
    await paste();
    await clipsAre("v1:red@0-4 v2:red@5-9");
  });

  it("pastes into the first free slot when the playhead is inside a clip", async () => {
    // Same track, same clipboard, playhead back inside what is already there: the paste
    // names a position, so an occupied one is answered with the next free one.
    await page.eval(seek(6));
    await page.until(`${TIME}.startsWith("00:06")`);
    await page.eval(clickTrack("v2"));
    await paste();
    await clipsAre("v1:red@0-4 v2:red@5-9 v2:red@9-13");
  });

  it("will not paste onto a track of the wrong kind", async () => {
    // A video clip on the overlay track is not a paste anyone asked for: it goes to the
    // first video track instead, the rule a drop from the bin already follows.
    await page.eval(seek(20));
    await page.until(`${TIME}.startsWith("00:20")`);
    await page.eval(clickTrack("o1"));
    await paste();
    await clipsAre("v1:red@0-4 v1:red@20-24 v2:red@5-9 v2:red@9-13");
  });

  it("cut takes the clip away and the paste puts it back where you ask", async () => {
    await page.eval(selectClip("c1"));
    await page.eval(key("x"));
    await clipsAre("v1:red@20-24 v2:red@5-9 v2:red@9-13");
    await page.until(`document.querySelector('#timeline .clip[data-id="c1"]') === null`);
    await page.eval(seek(30));
    await page.until(`${TIME}.startsWith("00:30")`);
    await page.eval(clickTrack("v1"));
    await paste();
    await clipsAre("v1:red@20-24 v1:red@30-34 v2:red@5-9 v2:red@9-13");
  });

  it("pastes into another sequence, making the track it needs", async () => {
    // The clipboard outlives the switch — that is what makes "copy here, paste there" one
    // gesture — and `intro` has no video track, so the paste has to add one. It goes in
    // below the overlay track, and the clip lands at the playhead: the overlay clip
    // sitting across that moment is on another track and is not in its way.
    await page.eval(openSequence("intro"));
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 1`);
    await page.eval(seek(1));
    await page.until(`${TIME}.startsWith("00:01")`);
    await paste();
    await clipsAre("v3:red@1-5 io1:probe@0-4", "intro");
    // …and the sequence it was copied from is untouched by any of it.
    await clipsAre("v1:red@20-24 v1:red@30-34 v2:red@5-9 v2:red@9-13");
  });

  it("copies several clips as a shape: their own tracks, their own spacing", async () => {
    // `intro` now holds an overlay at 0–4 and a video clip at 1–5, one second apart on two
    // tracks of different kinds — which is the whole question a group paste answers.
    await page.eval(selectClip("i1"));
    await page.eval(addClipOn("v3"));
    await page.until(`document.querySelectorAll("#timeline .clip.selected").length === 2`);
    // The one place the editor says how many: two outlines are countable, ten are not.
    assert.equal(await page.eval(`document.querySelector("#statusline .sel-count").textContent`), "2 clips selected");
    await page.eval(key("c"));

    await page.eval(seek(10));
    await page.until(`${TIME}.startsWith("00:10")`);
    // The clicked track is deliberately the wrong one for half of the group: several clips
    // keep the tracks they came from, so it must not come into it at all.
    await page.eval(clickTrack("v3"));
    await paste();
    await clipsAre("v3:red@1-5 v3:red@11-15 io1:probe@0-4 io1:probe@10-14", "intro");

    // Again, with the playhead inside what was just pasted: the *group* moves to where all
    // of it fits, so the second copy keeps the same one second between its halves.
    await page.eval(seek(12));
    await page.until(`${TIME}.startsWith("00:12")`);
    await paste();
    await clipsAre("v3:red@1-5 v3:red@11-15 v3:red@15-19 io1:probe@0-4 io1:probe@10-14 io1:probe@14-18", "intro");

    // A paste selects what it pasted, so Delete takes the whole group back off in one
    // patch — and leaves the clips it was pasted from alone.
    await page.until(`document.querySelectorAll("#timeline .clip.selected").length === 2`);
    await page.eval(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "Delete", bubbles: true })) || true`);
    await clipsAre("v3:red@1-5 v3:red@11-15 io1:probe@0-4 io1:probe@10-14", "intro");
  });
});
