// Playback stops when the timeline it plays is no longer the one open, in a real browser.
//
// Opening another sequence (a double-click in the bin, a tab) used to leave the engine
// playing the one before, under a transport that now drove the new one: only the play
// button's own toggle ever told the engine to pause. This machine may have no preview
// engine, so the page is given a stand-in for its socket that says it is ready and keeps
// what the page tells it.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, FAKE_ENGINE, missing, startDaemon } from "./harness.mjs";

const track = (id, clips = []) => ({ id, kind: "video", name: id, muted: false, hidden: false, transitions: [], clips });
const clip = (id, start, end) => ({ id, start, end, bin: "red", in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" });
const PROJECT = JSON.stringify({
  version: 1, name: "playback", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    main: { name: "Main", source: { markers: [], tracks: [track("v1", [clip("a", 0, 20)])] } },
    intro: { name: "Intro", source: { markers: [], tracks: [track("v2", [clip("b", 0, 5)])] } },
  },
});


const why = missing();

describe("playback", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const SENT = `window.__engine.filter(m => m === "play" || m === "pause").join(" ")`;
  const PLAYING = `document.querySelector("#transport button.icon[data-playing]")?.dataset.playing`;
  const key = (k, code) => page.eval(`(window.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(k)}, code: ${JSON.stringify(code ?? k)}, bubbles: true })), true)`);

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-playback-"));
    const projectFile = path.join(tmp, "playback.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.onNewDocument(FAKE_ENGINE);
    await page.eval(`(location.reload(), true)`);
    await page.until(`!!window.__engine && document.querySelectorAll("#library .bin-row").length === 3`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("plays and pauses on Space", async () => {
    await key(" ", "Space");
    await page.until(`${PLAYING} === "true" && ${SENT} === "play"`);
    await key(" ", "Space");
    await page.until(`${PLAYING} === "false" && ${SENT} === "play pause"`);
  });

  it("stops when another sequence is opened from the bin", async () => {
    await page.eval(`(window.__engine.length = 0, true)`);
    await key(" ", "Space");
    await page.until(`${PLAYING} === "true" && ${SENT} === "play"`);
    await page.eval(`(document.querySelector('#library .bin-row[data-sel="bin:intro"]').dispatchEvent(new MouseEvent("dblclick", { bubbles: true })), true)`);
    await page.until(`fetch("/api/sequence").then(r => r.json()).then(j => j.current === "intro")`);
    // The engine is told, and the transport says so: what it drives now is not playing.
    await page.until(`${SENT} === "play pause" && ${PLAYING} === "false"`);
  });

  it("stops when a sequence tab is clicked", async () => {
    await page.eval(`(window.__engine.length = 0, true)`);
    await key(" ", "Space");
    await page.until(`${PLAYING} === "true" && ${SENT} === "play"`);
    await page.eval(`[...document.querySelectorAll("#editor-tabs .etab")].find(t => t.textContent.includes("Main")).click()`);
    await page.until(`fetch("/api/sequence").then(r => r.json()).then(j => j.current === "main")`);
    await page.until(`${SENT} === "play pause" && ${PLAYING} === "false"`);
  });

  // Play used to flip the button to "pause" over a still picture for as long as the engine
  // took to start — seconds behind a reload — which read as a frozen editor.
  const STARTING = `document.querySelector("#transport button.icon[data-playing]")?.dataset.starting`;
  const BADGE = `document.querySelector(".stage .badge").textContent`;
  const say = (j) => page.eval(`(window.__engineSocket.onmessage({ data: ${JSON.stringify(JSON.stringify(j))} }), true)`);
  /** A real JPEG at `frame`, framed as the daemon frames one. */
  const frame = (f) => page.eval(`(async () => {
    const c = document.createElement("canvas"); c.width = 16; c.height = 9;
    const jpeg = await new Promise((r) => c.toBlob(r, "image/jpeg")).then((b) => b.arrayBuffer());
    const buf = new Uint8Array(8 + jpeg.byteLength);
    new DataView(buf.buffer).setBigInt64(0, BigInt(${f}), true);
    buf.set(new Uint8Array(jpeg), 8);
    window.__engineSocket.onmessage({ data: buf.buffer });
    return true;
  })()`);

  it("says it is starting, and what on, until the first frame", async () => {
    await key(" ", "Space");
    await page.until(`${PLAYING} === "true" && ${STARTING} === "true" && ${BADGE}.startsWith("starting")`);
    await say({ waiting: "for the timeline to load" });
    await page.until(`${BADGE}.includes("for the timeline to load")`);
    await say({ playing: true });
    await page.until(`${BADGE}.includes("rendering the first frame")`);
    await frame(3);
    await page.until(`${STARTING} === "false" && ${PLAYING} === "true" && ${BADGE}.startsWith("engine")`);
    await key(" ", "Space");
    await page.until(`${PLAYING} === "false"`);
  });

  it("turns back off when the engine refuses to play, and says why", async () => {
    await key(" ", "Space");
    await page.until(`${PLAYING} === "true" && ${STARTING} === "true"`);
    await say({ refused: "no timeline loaded" });
    await page.until(`${PLAYING} === "false" && ${STARTING} === "false"`);
    await page.until(`[...document.querySelectorAll("#toasts .toast")].some((t) => t.textContent.includes("no timeline loaded"))`);
  });
});
