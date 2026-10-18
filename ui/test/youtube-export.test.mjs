// "For YouTube" in the render dialog: the subtitles of the sequence picked written as an
// SRT beside the renders, and its chapters shown (and copied where the browser lets it) as
// description timestamps. The daemon works both out; the page only asks and shows, so
// this checks the wiring and what a person reads, not the time mapping (Rust tests that).
//
// A real daemon and the shipped `subtitles` blueprint. No melt: nothing is rendered.
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { REPO, driver, missing, startDaemon } from "./harness.mjs";

const clip = (id, start, end, bin, extra = {}) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "", ...extra });
const track = (id, kind, clips) => ({ id, kind, name: id, muted: false, hidden: false, transitions: [], clips });
const PROJECT = JSON.stringify({
  version: 9, name: "yt", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 240, sample_rate: 48000,
  bin: {
    navy: { name: "Navy", source: { color: "#102030" } },
    subs: { name: "Subs", source: { overlay: "overlays/subtitles", vars: { words_per_line: 3, cues: [
      { start: 0.5, end: 1.0, text: "Hello" }, { start: 1.0, end: 1.5, text: "from" }, { start: 1.5, end: 2.0, text: "kuma." },
    ] } } },
    intro: { name: "Intro", source: { markers: [], tracks: [track("v1", "video", [clip("c1", 0, 12, "navy")]), track("o1", "overlay", [clip("s1", 0, 12, "subs")])] } },
    body: { name: "Body", source: { markers: [], tracks: [track("v2", "video", [clip("c2", 0, 20, "navy")])] } },
    main: { name: "Main", source: { markers: [], tracks: [track("v3", "video", [
      clip("m1", 0, 12, "intro"), clip("m2", 12, 32, "body", { label: "The middle" }), clip("m3", 32, 44, "body"),
    ])] } },
  },
}, null, 2);

const why = missing();

describe("exporting for YouTube from the render dialog", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const dialog = `document.querySelector("dialog.modal.render")`;
  const click = (label) => `[...${dialog}.querySelectorAll(".render-export button")].find(b => b.textContent === ${JSON.stringify(label)}).click()`;
  const note = `${dialog}.querySelector(".render-export-note").textContent`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-yt-"));
    const projectFile = path.join(tmp, "yt.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    fs.mkdirSync(path.join(tmp, "overlays/subtitles"), { recursive: true });
    for (const f of ["overlay.json", "index.html"]) fs.copyFileSync(path.join(REPO, "library/blueprints/subtitles", f), path.join(tmp, "overlays/subtitles", f));
    fs.mkdirSync(path.join(tmp, "library/blueprints"), { recursive: true });
    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelector("#topbar button.primary")`);
    await page.eval(`document.querySelector("#topbar button.primary").click()`);
    await page.until(`${dialog}?.querySelector(".render-export")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("writes the master's subtitles as an SRT beside the renders", async () => {
    await page.eval(click("Export subtitles (.srt)"));
    await page.until(`${note}.includes("written")`);
    assert.equal(await page.eval(note), "1 caption written to renders/yt.srt");
    assert.equal(fs.readFileSync(path.join(tmp, "renders/yt.srt"), "utf8"), "1\n00:00:00,500 --> 00:00:02,000\nHello from kuma.\n\n");
  });

  it("shows the chapters as description timestamps", async () => {
    await page.eval(click("Copy chapters"));
    const text = `${dialog}.querySelector(".render-chapters")`;
    await page.until(`!${text}.hidden && ${text}.value`);
    assert.equal(await page.eval(`${text}.value`), "0:00 Intro\n0:12 The middle\n0:32 Body\n");
    // Copied, or left selected to copy by hand where the clipboard is refused.
    assert.match(await page.eval(note), /^3 chapters/);    assert.deepEqual(await page.eval(`[...${dialog}.querySelectorAll(".render-export-warnings li")].map(l => l.textContent)`), []);
  });

  it("says so when the sequence picked shows no subtitles, and writes nothing", async () => {
    await page.eval(`(() => { const s = ${dialog}.querySelector(".render-pane select[title]"); s.value = "body"; s.dispatchEvent(new Event("change")); })()`);
    await page.eval(click("Export subtitles (.srt)"));
    await page.until(`${note} === "No subtitles to export."`);
    const warnings = `[...${dialog}.querySelectorAll(".render-export-warnings li")].map(l => l.textContent)`;
    assert.match((await page.eval(warnings))[0], /`subs` is not placed in anything `body` shows/);
    assert.ok(!fs.existsSync(path.join(tmp, "renders/yt-body.srt")));
    await page.eval(click("Copy chapters"));
    await page.until(`${warnings}.some(w => w.includes("places no sequence"))`);
    assert.equal(await page.eval(`${dialog}.querySelector(".render-chapters").hidden`), true);
  });
});
