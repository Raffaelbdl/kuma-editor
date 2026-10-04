// Files dropped from the desktop, in a real browser, with the shell's `pathForFile` stood
// in for: a drop anywhere adds to the bin, on a folder into it, one edit per file; the bin
// is framed while a file is over the window, grey until the drag says it carries files.
//
// Skips, loudly, without a browser, a binary, `ui/dist` or ffmpeg to make the media.

import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "drop", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    clips: { name: "Clips", source: { folder: true } },
    main: { name: "Main", source: { markers: [], tracks: [] } },
  },
});

const why = missing() ?? (spawnSync("ffmpeg", ["-version"]).status === 0 ? null : "no ffmpeg");

describe("dropping files on the bin", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  /** Every media entry as `id>parent`, sorted. */
  const MEDIA = `fetch("/api/project").then(r => r.json()).then(p => Object.entries(p.bin)
    .filter(([, e]) => e.source.path).map(([id, e]) => id + ">" + (e.parent ?? "")).sort().join(" "))`;
  const REVISION = `fetch("/api/project").then(r => r.json()).then(p => p.revision)`;
  const HINT = `document.querySelector(".bin-drop-hint")`;
  const FOLDER_ROW = '.bin-row[title="Clips"]';
  const toasted = (text) => page.until(`[...document.querySelectorAll("#toasts .toast.err")].some(t => t.textContent.includes(${JSON.stringify(text)}))`);

  /** Drag `names` in through the browser's own drag pipeline, over each of `over` in turn,
   *  and drop on the last. Returns the hint's text and the lit row just before the drop. */
  const dragIn = async (names, ...over) => {
    const files = names.map((n) => path.join(tmp, n));
    const points = [];
    for (const sel of over) points.push(await page.eval(`(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`));
    await page.dragFiles("dragEnter", ...points[0], files);
    for (const [x, y] of points) {
      await page.dragFiles("dragOver", x, y, files);
      await page.dragFiles("dragOver", x + 3, y + 1, files);
    }
    const shown = await page.eval(`({ hint: ${HINT}?.textContent ?? "", row: document.querySelector(".bin-row.drop")?.title ?? "" })`);
    await page.dragFiles("drop", ...points.at(-1), files);
    return shown;
  };

  /** A synthetic drag event on the timeline, carrying no types, as GNOME often sends. */
  const empty = (type) => page.eval(`document.querySelector("#timeline").dispatchEvent(new DragEvent("${type}", { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() }))`);

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-drop-"));
    for (const n of ["one", "two", "three"]) {
      const r = spawnSync("ffmpeg", ["-v", "error", "-f", "lavfi", "-i", "sine=duration=1", path.join(tmp, `${n}.wav`)]);
      assert.equal(r.status, 0, String(r.stderr));
    }
    const card = path.join(tmp, "overlays/card");
    fs.mkdirSync(card, { recursive: true });
    fs.writeFileSync(path.join(card, "overlay.json"), JSON.stringify({ name: "Card" }));
    fs.writeFileSync(path.join(card, "index.html"), "<div data-composition-id='main'></div>");
    const projectFile = path.join(tmp, "drop.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector(${JSON.stringify(FOLDER_ROW)})`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("says why it cannot in a browser tab, and adds nothing", async () => {
    const before = await page.eval(REVISION);
    const shown = await dragIn(["one.wav"], "#library");
    assert.match(shown.hint, /desktop app/);
    await toasted("browser cannot");
    assert.equal(await page.eval(REVISION), before);
  });

  it("adds files dropped on the timeline to the bin, in one edit", async () => {
    await page.eval(`globalThis.kumaApp = { pathForFile: (f) => ${JSON.stringify(tmp + "/")} + f.name }; true`);
    const before = await page.eval(REVISION);
    const shown = await dragIn(["one.wav", "two.wav"], "#library", "#timeline");
    assert.match(shown.hint, /^Drop to add/);
    await page.until(`${MEDIA}.then(s => s === "one> two>")`);
    assert.equal(await page.eval(REVISION), before + 1);
    assert.equal(await page.eval(`!${HINT}`), true, "the frame outlived the drop");
  });

  it("adds files dropped on a folder into it", async () => {
    const shown = await dragIn(["three.wav"], "#timeline", FOLDER_ROW);
    assert.equal(shown.row, "Clips");
    await page.until(`${MEDIA}.then(s => s === "one> three>clips two>")`);
  });

  it("imports a dropped overlay.json as an overlay entry", async () => {
    await page.eval(`globalThis.kumaApp = { pathForFile: (f) => ${JSON.stringify(tmp + "/")} + (f.name === "overlay.json" ? "overlays/card/" : "") + f.name }; true`);
    fs.copyFileSync(path.join(tmp, "overlays/card/overlay.json"), path.join(tmp, "overlay.json"));
    await dragIn(["overlay.json"], "#timeline", FOLDER_ROW);
    const entry = `fetch("/api/project").then(r => r.json()).then(p => JSON.stringify([p.bin.card?.source, p.bin.card?.parent]))`;
    await page.until(`${entry}.then(s => s === ${JSON.stringify(JSON.stringify([{ overlay: "overlays/card", vars: {} }, "clips"]))})`);
  });

  it("takes a drag with no types, grey, and says so if it lands empty", async () => {
    await empty("dragenter");
    assert.equal(await empty("dragover"), false, "the dragover was left alone, so the drop would be refused");
    assert.ok(await page.eval(`${HINT}?.classList.contains("waiting")`), "not grey");
    const before = await page.eval(REVISION);
    await empty("drop");
    await toasted("without its files");
    assert.equal(await page.eval(REVISION), before);
  });

  it("keeps the frame through a stray leave, and drops it when the drag leaves", async () => {
    for (const t of ["dragenter", "dragleave", "dragover"]) await empty(t);
    await new Promise((r) => setTimeout(r, 400));
    assert.ok(await page.eval(`!!${HINT}`), "a stray leave took the frame away");
    await empty("dragleave");
    await page.until(`!${HINT}`, 2000);
  });
});
