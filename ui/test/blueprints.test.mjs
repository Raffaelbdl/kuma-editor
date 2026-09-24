// Blueprints and overlays, in a real browser against a real daemon: a blueprint reaches
// the video by being **copied** into the project, never by being pointed at, and two
// entries that share a folder are told so, next to the way to split them.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const track = (id, kind, clips = []) => ({ id, kind, name: kind, muted: false, hidden: false, transitions: [], clips });

/** Two entries on one folder — two wordings of one design — and an overlay track to
 *  drop on. */
const PROJECT = JSON.stringify({
  version: 7, name: "blueprints", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    probe: { name: "Probe", source: { overlay: "overlays/probe", vars: {} } },
    twin: { name: "Twin", source: { overlay: "overlays/probe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [track("mo1", "overlay")] } },
  },
}, null, 2);

const MARKUP = `<!doctype html><html><head></head><body>
  <div id="root" data-composition-id="main" data-duration="4" data-width="1920" data-height="1080" data-no-timeline></div>
</body></html>`;

const why = missing();

describe("blueprints and overlays", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const PROJECT_NOW = `fetch("/api/project").then(r => r.json())`;
  const buttonNamed = (scope, text) => `[...document.querySelectorAll(${JSON.stringify(scope)} + " button")].find(b => b.textContent === ${JSON.stringify(text)})`;

  const folder = (dir, name) => {
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "overlay.json"), JSON.stringify({ name, static: true, duration_default: 3 }));
    fs.writeFileSync(path.join(dir, "index.html"), MARKUP);
  };

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-blueprints-"));
    folder(path.join(tmp, "overlays/probe"), "Probe");
    folder(path.join(tmp, "library/blueprints/card"), "Card");
    const projectFile = path.join(tmp, "blueprints.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);

    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 1`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("lists the library's blueprints, and none of the project's overlays", async () => {
    await page.eval(`${buttonNamed("#library .tabs", "Blueprints")}.click()`);
    await page.until(`document.querySelectorAll("#library .lib-item").length > 0`);
    assert.deepEqual(await page.eval(`[...document.querySelectorAll("#library .lib-item .title")].map(t => t.firstChild.textContent)`), ["Card"]);
  });

  it("shows a clicked blueprint's details in the inspector, with a preview button", async () => {
    await page.eval(`document.querySelector("#library .lib-item .title").click()`);
    await page.until(`document.querySelector("#inspector h3")?.textContent.startsWith("Blueprint")`);
    const text = await page.eval(`document.querySelector("#inspector").textContent`);
    assert.match(text, /Card/);
    assert.ok(await page.eval(`!!${buttonNamed("#inspector", "Preview")}`));
    assert.ok(await page.eval(`!!${buttonNamed("#inspector", "Timeline")}`));
  });

  it("places a blueprint by copying it into the project, entry and clip in one edit", async () => {
    const before = (await page.eval(PROJECT_NOW)).revision;
    await page.eval(`${buttonNamed("#library .lib-item", "Timeline")}.click()`);
    await page.until(`document.querySelectorAll("#timeline .clip").length === 1`);
    const p = await page.eval(PROJECT_NOW);
    assert.equal(p.revision, before + 1, "the entry and its clip are one patch, one undo step");
    const [id, entry] = Object.entries(p.bin).find(([, i]) => i.name === "Card");
    assert.deepEqual(entry.source, { overlay: "overlays/card", vars: {} }, "the entry points into the project, not the library");
    assert.ok(fs.existsSync(path.join(tmp, "overlays/card/index.html")), "the blueprint was copied");
    const clip = p.bin.main.source.tracks[0].clips[0];
    assert.equal(clip.bin, id);
    assert.equal(clip.end - clip.start, 3, "at the length the blueprint was authored for");
  });

  it("says when an entry shares its folder, and makes it unique", async () => {
    await page.eval(`${buttonNamed("#library .tabs", "Bin")}.click()`);
    await page.until(`[...document.querySelectorAll("#library .bin-row")].some(r => r.title === "Twin")`);
    await page.eval(`[...document.querySelectorAll("#library .bin-row")].find(r => r.title === "Twin").click()`);
    await page.until(`!!${buttonNamed("#inspector", "Make unique")}`);
    assert.match(await page.eval(`document.querySelector("#inspector").textContent`), /Shares its folder with probe/);

    await page.eval(`${buttonNamed("#inspector", "Make unique")}.click()`);
    await page.until(`!${buttonNamed("#inspector", "Make unique")}`);
    const p = await page.eval(PROJECT_NOW);
    assert.equal(p.bin.twin.source.overlay, "overlays/twin");
    assert.equal(p.bin.probe.source.overlay, "overlays/probe", "the other entry keeps the folder it had");
    assert.ok(fs.existsSync(path.join(tmp, "overlays/twin/overlay.json")));
  });
});
