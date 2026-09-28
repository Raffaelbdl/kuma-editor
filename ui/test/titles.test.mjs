// Titles, made the way Kdenlive's "Add Title Clip" makes them: from the bin, with no
// detour through the Blueprints tab. The New menu is every blueprint whose manifest says
// `create`, so this runs against the starter library, which is where `title` lives — and
// against its fonts, which the title's font picker lists.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { REPO, driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 7, name: "titles", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    intro: { name: "Intro", source: { folder: true } },
    main: { name: "Main", source: { markers: [], tracks: [{ id: "mo1", kind: "overlay", name: "Overlays", muted: false, hidden: false, transitions: [], clips: [] }] } },
  },
}, null, 2);

const why = missing();

describe("titles from the bin", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  const PROJECT_NOW = `fetch("/api/project").then(r => r.json())`;
  const menuItems = `[...document.querySelectorAll(".popup-menu .popup-item")].map(b => b.textContent)`;
  const menuItem = (text) => `[...document.querySelectorAll(".popup-menu .popup-item")].find(b => b.textContent === ${JSON.stringify(text)})`;
  const sections = `[...document.querySelectorAll("#inspector .var-group .disclose")].map(b => b.textContent)`;
  const section = (name) => `[...document.querySelectorAll("#inspector .var-group")].find(g => g.querySelector(".disclose").textContent === ${JSON.stringify(name)})`;
  const fieldLabels = (name) => `[...${section(name)}.querySelectorAll(".field > label")].map(l => l.textContent)`;
  const fieldInput = (name, label, sel) => `[...${section(name)}.querySelectorAll(".field")].find(f => f.querySelector("label").textContent === ${JSON.stringify(label)}).querySelector(${JSON.stringify(sel)})`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-titles-"));
    const projectFile = path.join(tmp, "titles.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile, path.join(REPO, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#library .bin-row").length === 2`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("offers the blueprints that say `create`, then the other things a bin makes", async () => {
    await page.eval(`document.querySelector('#library .lib-tools button[title^="New —"]').click()`);
    await page.until(`document.querySelector(".popup-menu")`);
    const items = await page.eval(menuItems);
    assert.deepEqual(items.slice(0, 2), ["Title", "Title + subtitle"]);
    assert.ok(items.includes("Sequence…") && items.includes("Folder…"), items.join(", "));
    await page.eval(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }))`);
    await page.until(`!document.querySelector(".popup-menu")`);
  });

  it("makes a title inside the folder it was right-clicked on, and opens it", async () => {
    await page.eval(`(() => { const r = [...document.querySelectorAll("#library .bin-row")].find(r => r.title === "Intro"); const b = r.getBoundingClientRect(); r.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: b.left + 10, clientY: b.top + 5 })); })()`);
    await page.until(`document.querySelector(".popup-menu .popup-heading")?.textContent === "New in Intro"`);
    await page.eval(`${menuItem("Title")}.click()`);
    const p = await page.until(`${PROJECT_NOW}.then(p => p.bin.title ? p : null)`);
    const { name, parent, source } = p.bin.title;
    assert.deepEqual({ name, parent, source }, { name: "Title", parent: "intro", source: { overlay: "overlays/title", vars: {} } });
    assert.ok(fs.existsSync(path.join(tmp, "overlays/title/index.html")), "the blueprint was copied into the project");
    // On the workbench, its vars in the inspector: what is shown, not yet when.
    await page.until(`document.querySelector("#editor-tabs")?.textContent.includes("Title")`);
    await page.until(`document.querySelectorAll("#inspector .var-group").length > 0`);
    assert.equal(p.bin.main.source.tracks[0].clips.length, 0, "nothing is placed");
  });

  it("lists the vars in the manifest's order, in its sections", async () => {
    assert.deepEqual(await page.eval(sections), ["Text", "Layout", "Outline", "Shadow", "Box", "Animation"]);
    assert.deepEqual((await page.eval(fieldLabels("Text"))).slice(0, 3), ["Text", "Font", "Weight"]);
  });

  it("shows a control only while its `when` holds", async () => {
    assert.deepEqual(await page.eval(fieldLabels("Box")), ["Box"], "the box's colour means nothing with the box off");
    await page.eval(`${fieldInput("Box", "Box", "input")}.click()`);
    await page.until(`${fieldLabels("Box")}.length > 1`);
    assert.deepEqual(await page.eval(fieldLabels("Box")), ["Box", "Colour", "Opacity", "Padding", "Corner radius"]);
    assert.equal((await page.eval(PROJECT_NOW)).bin.title.source.vars.box, true);
  });

  it("picks a font from the installed families", async () => {
    const families = await page.eval(`[...${fieldInput("Text", "Font", "select")}.options].map(o => o.value)`);
    assert.ok(families.includes("Inter") && families.includes("Bebas Neue"), families.join(", "));
    await page.eval(`(() => { const s = ${fieldInput("Text", "Font", "select")}; s.value = "Bebas Neue"; s.dispatchEvent(new Event("change")); })()`);
    await page.until(`${PROJECT_NOW}.then(p => p.bin.title.source.vars.font === "Bebas Neue")`);
    if (process.env.KUMA_SCREENSHOT) await page.screenshot(process.env.KUMA_SCREENSHOT);
  });

  it("adds a font by a typed path in a browser tab, and uses it", async () => {
    const font = path.join(REPO, "library/fonts/Anton/Anton-Regular.ttf");
    await page.eval(`${fieldInput("Text", "Font", "button")}.click()`);
    await page.until(`document.querySelector("dialog.modal input[type=text]")`);
    await page.eval(`(() => { const i = document.querySelector("dialog.modal input[type=text]"); i.value = ${JSON.stringify(font)}; document.querySelector("dialog.modal form").requestSubmit(); })()`);
    await page.until(`${PROJECT_NOW}.then(p => p.bin.title.source.vars.font === "Anton")`);
    assert.ok(fs.existsSync(path.join(tmp, "fonts/Anton/Anton-Regular.ttf")), "copied into the project's fonts/");
    const fonts = await page.eval(`fetch("/api/fonts").then(r => r.json())`);
    assert.equal(fonts.find((f) => f.name === "Anton").origin, "project", "the project's copy wins over the library's");
  });

  it("names the next one after it, in a folder of its own", async () => {
    await page.eval(`document.querySelector('#library .lib-tools button[title^="New —"]').click()`);
    await page.until(`document.querySelector(".popup-menu")`);
    await page.eval(`${menuItem("Title")}.click()`);
    const p = await page.until(`${PROJECT_NOW}.then(p => p.bin["title-2"] ? p : null)`);
    assert.equal(p.bin["title-2"].name, "Title 2");
    assert.equal(p.bin["title-2"].parent, undefined, "the button makes it at the top level");
    assert.equal(p.bin["title-2"].source.overlay, "overlays/title-2", "each title has its own copy");
  });
});
