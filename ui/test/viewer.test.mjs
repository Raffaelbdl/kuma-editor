// The viewer, in a real browser against a real daemon: a picture, a sound and a video open
// from the bin in a tab of their own, each shown on its own and none of them placed.
//
// What it pins is what a Rust test cannot see: that the bytes the daemon serves are a file
// the browser actually plays — a sound with a length, a picture with a size — that the
// file's own clock is what Space and the arrows drive there, and that a file the browser
// cannot decode says so rather than leaving a black box. Looking is never an edit.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do; the video
// case also wants ffmpeg with libvpx, and skips without it.

import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";
import { encodePng } from "./png.mjs";

const track = (id, kind, clips = []) => ({ id, kind, name: kind, muted: false, hidden: false, transitions: [], clips });

/** `seconds` of a quiet 440 Hz tone as 16-bit mono PCM WAV. */
function wav(seconds, rate = 8000) {
  const n = Math.round(seconds * rate);
  const data = Buffer.alloc(n * 2);
  for (let i = 0; i < n; i++) data.writeInt16LE(Math.round(4000 * Math.sin((2 * Math.PI * 440 * i) / rate)), i * 2);
  const head = Buffer.alloc(44);
  head.write("RIFF", 0); head.writeUInt32LE(36 + data.length, 4); head.write("WAVE", 8);
  head.write("fmt ", 12); head.writeUInt32LE(16, 16); head.writeUInt16LE(1, 20); head.writeUInt16LE(1, 22);
  head.writeUInt32LE(rate, 24); head.writeUInt32LE(rate * 2, 28); head.writeUInt16LE(2, 32); head.writeUInt16LE(16, 34);
  head.write("data", 36); head.writeUInt32LE(data.length, 40);
  return Buffer.concat([head, data]);
}

const why = missing();
/** Whether ffmpeg here can make a video the browser plays; decided before the cases are. */
const vp9 = spawnSync("ffmpeg", ["-hide_banner", "-encoders"], { encoding: "utf8" }).stdout?.includes("libvpx-vp9") ?? false;

describe("the viewer", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const TABS = `[...document.querySelectorAll("#editor-tabs .etab")].map(t => t.querySelector(".etab-name").textContent + (t.classList.contains("on") ? "*" : "")).join(" | ")`;
  const KINDS = `[...document.querySelectorAll("#editor-tabs .etab .kind-sq")].map(s => s.title).join()`;
  const SHOWN = `["preview", "workbench", "viewer"].filter(id => !document.getElementById(id).hidden).join()`;
  const open = (id) => `document.querySelector('#library .bin-row[data-sel="bin:${id}"]').dispatchEvent(new MouseEvent("dblclick", { bubbles: true }))`;
  const closeTab = (name) => `[...document.querySelectorAll("#editor-tabs .etab")].find(t => t.textContent.includes(${JSON.stringify(name)})).querySelector(".x").click()`;
  const PROJECT = `fetch("/api/project").then(r => r.json())`;
  const REVISION = `${PROJECT}.then(p => p.revision)`;
  const AUDIO = `document.querySelector("#viewer audio")`;
  const VIDEO = `document.querySelector("#viewer video")`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-viewer-"));
    fs.mkdirSync(path.join(tmp, "media"));
    fs.writeFileSync(path.join(tmp, "media/still.png"), encodePng(64, 48, [200, 40, 40]));
    fs.writeFileSync(path.join(tmp, "media/tone.wav"), wav(2));
    // Named like a video and nothing like one: what a codec the browser lacks looks like.
    fs.writeFileSync(path.join(tmp, "media/broken.mp4"), Buffer.from("not a video at all"));
    const bin = {
      main: { name: "Main", source: { markers: [], tracks: [track("v1", "video"), track("a1", "audio")] } },
      still: { name: "Still", source: { path: "media/still.png", kind: "image" }, meta: { width: 64, height: 48, has_audio: false } },
      tone: { name: "Tone", source: { path: "media/tone.wav", kind: "audio" }, meta: { duration: 2, has_audio: true } },
      broken: { name: "Broken", source: { path: "media/broken.mp4", kind: "video" }, meta: { duration: 2, width: 160, height: 90, has_audio: false } },
    };
    if (vp9) {
      const ff = spawnSync("ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=160x90:rate=10:duration=3", "-c:v", "libvpx-vp9", "-b:v", "200k", path.join(tmp, "media/clip.webm")], { encoding: "utf8" });
      assert.equal(ff.status, 0, `ffmpeg could not make the video: ${ff.stderr}`);
      bin.clip = { name: "Clip", source: { path: "media/clip.webm", kind: "video" }, meta: { duration: 3, width: 160, height: 90, fps: { num: 10, den: 1 }, has_audio: false } };
    }
    const projectFile = path.join(tmp, "viewer.kuma.json");
    fs.writeFileSync(projectFile, JSON.stringify({ version: 7, name: "viewer", master: "main", fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000, bin }, null, 2));

    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#editor-tabs .etab").length > 0`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("opens a picture from the bin in a tab of its own, and decodes it", async () => {
    const revision = await page.eval(REVISION);
    await page.eval(open("still"));
    await page.until(`${TABS} === "Main | Still*"`);
    assert.equal(await page.eval(KINDS), "sequence,image", "the tab wears its kind, as the bin row does");
    assert.equal(await page.eval(SHOWN), "viewer", "one editor at a time");
    const size = await page.until(`(() => { const i = document.querySelector("#viewer img.vw-picture"); return i && i.complete && i.naturalWidth ? [i.naturalWidth, i.naturalHeight] : null; })()`);
    assert.deepEqual(size, [64, 48]);
    assert.equal(await page.eval(`document.querySelector("#viewer .wb-foot").hidden`), true, "a still has no clock to drive");
    // Opening it selects it, so the inspector is on the entry being looked at.
    await page.until(`document.querySelector("#inspector").textContent.includes("still")`);
    assert.equal(await page.eval(REVISION), revision, "looking is not an edit");
  });

  it("plays a sound on its own clock, which the keys drive", async () => {
    await page.eval(open("tone"));
    await page.until(`${TABS} === "Main | Still | Tone*"`);
    const duration = await page.until(`${AUDIO}?.readyState >= 1 && ${AUDIO}.duration`);
    assert.ok(Math.abs(duration - 2) < 0.05, `the file's own length, not ${duration}`);
    // Its waveform is drawn: the daemon's peaks, across the whole file.
    assert.ok(await page.eval(`!!document.querySelector("#viewer .vw-wave canvas")`));

    await page.press("ArrowRight");
    await page.press("ArrowRight");
    // Two frames at the project's rate, since a sound has none of its own.
    await page.until(`Math.abs(${AUDIO}.currentTime - 2 / 30) < 1e-3`);
    assert.match(await page.eval(`document.querySelector("#viewer .wb-foot .time").textContent`), /^00:00\.02 \/ 00:02\.00$/);
    // Space is the viewer's play here, not the sequence's: the playhead does not move.
    const playhead = await page.eval(`document.querySelector("#transport .time")?.textContent`);
    await page.press(" ", "Space");
    await page.until(`!${AUDIO}.paused || ${AUDIO}.ended || ${AUDIO}.currentTime > 0.2`);
    await page.press(" ", "Space");
    await page.until(`${AUDIO}.paused`);
    assert.equal(await page.eval(`document.querySelector("#transport .time")?.textContent`), playhead);
  });

  it("keeps where each entry was left, and lets go of a file it is not showing", async () => {
    const t = await page.eval(`${AUDIO}.currentTime`);
    await page.eval(open("still"));
    await page.until(`${TABS} === "Main | Still* | Tone"`);
    assert.equal(await page.eval(`document.querySelectorAll("#viewer audio").length`), 0);
    await page.eval(open("tone"));
    await page.until(`${AUDIO}?.readyState >= 1`);
    const back = await page.eval(`${AUDIO}.currentTime`);
    assert.ok(Math.abs(back - t) < 0.05, `back at ${t}, not ${back}`);
  });

  it("says when the browser cannot play a file, rather than showing a black box", async () => {
    await page.eval(open("broken"));
    await page.until(`${TABS} === "Main | Still | Tone | Broken*"`);
    const hint = await page.until(`document.querySelector("#viewer .wb-hint").textContent`);
    assert.match(hint, /cannot play media\/broken\.mp4/);
    assert.match(hint, /not affected/, "and that the timeline, preview and render are another matter");
    assert.equal(await page.eval(`document.querySelectorAll("#viewer video").length`), 0);
  });

  it("plays a video the browser can decode, stepping its own frames", { skip: !vp9 && "no ffmpeg with libvpx-vp9" }, async () => {
    await page.eval(open("clip"));
    await page.until(`${VIDEO}?.readyState >= 2 && ${VIDEO}.videoWidth === 160`);
    assert.equal(await page.eval(`document.querySelector("#viewer .wb-hint").textContent`), "");
    await page.press("ArrowRight");
    // Shift is a second; a frame is a tenth of one, at the file's own rate.
    await page.press("ArrowRight", "ArrowRight", { shift: true });
    await page.until(`Math.abs(${VIDEO}.currentTime - 1.1) < 1e-3`);
    await page.eval(closeTab("Clip"));
  });

  it("places what it shows at the playhead, and stays on it", async () => {
    await page.eval(open("tone"));
    await page.until(`${TABS}.includes("Tone*")`);
    const revision = await page.eval(REVISION);
    await page.eval(`[...document.querySelectorAll("#viewer .wb-head button")].find(b => b.textContent === "Place at playhead").click()`);
    await page.until(`${REVISION}.then(r => r > ${revision})`);
    const clips = await page.eval(`${PROJECT}.then(p => p.bin.main.source.tracks.flatMap(t => t.clips.map(c => [t.kind, c.bin, c.start, c.end])))`);
    assert.deepEqual(clips, [["audio", "tone", 0, 2]]);
    assert.equal(await page.eval(SHOWN), "viewer", "placing does not change what you are looking at");
  });

  it("closes like any tab, and falls back to the next one", async () => {
    for (const name of ["Broken", "Tone", "Still"]) await page.eval(closeTab(name));
    await page.until(`${TABS} === "Main*"`);
    assert.equal(await page.eval(SHOWN), "preview");
  });
});
