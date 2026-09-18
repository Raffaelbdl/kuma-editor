// The editor's tab strip, in a real browser against a real daemon.
//
// What it pins is the thing a Rust test cannot see: which sequence the daemon has open is
// *the* state a sequence tab switches, so a strip that disagrees with it is showing a
// timeline nobody is editing. And closing a tab has to close it — the × sits inside the
// tab, and a click that also reaches the tab reopens what it just closed, which is how
// this test came to exist.
//
// Skips, loudly, without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, REPO, startDaemon } from "./harness.mjs";

const track = (id, kind, clips = []) => ({ id, kind, name: kind, muted: false, hidden: false, transitions: [], clips });
const clip = (id, bin, start, end) => ({ id, start, end, bin, in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" });

/** Two sequences with a clip each, so "the timeline followed the tab" is checkable, and
 *  one overlay entry, so both kinds of tab share the strip. */
const PROJECT = JSON.stringify({
  version: 1, name: "tabs", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    probe: { name: "Probe", source: { template: "probe", vars: {} } },
    main: { name: "Main", source: { markers: [], tracks: [track("mo1", "overlay", [clip("m1", "probe", 0, 4)])] } },
    intro: { name: "Intro", source: { markers: [], tracks: [track("io1", "overlay", [clip("i1", "probe", 0, 2)])] } },
  },
}, null, 2);

const TEMPLATE = `<!doctype html><html><head></head><body>
  <div id="root" data-composition-id="probe" data-duration="4" data-width="1920" data-height="1080" data-no-timeline></div>
</body></html>`;

const why = missing();

describe("editor tabs", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  /** The tabs as `name` / `name*` for the active one. */
  const TABS = `[...document.querySelectorAll("#editor-tabs .etab")].map(t => t.querySelector(".etab-name").textContent + (t.classList.contains("on") ? "*" : "")).join(" | ")`;
  const OPEN_SEQUENCE = `fetch("/api/sequence").then(r => r.json()).then(j => j.current)`;
  const clickTab = (name) => `[...document.querySelectorAll("#editor-tabs .etab")].find(t => t.textContent.includes(${JSON.stringify(name)})).click()`;
  const closeTab = (name) => `[...document.querySelectorAll("#editor-tabs .etab")].find(t => t.textContent.includes(${JSON.stringify(name)})).querySelector(".x").click()`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-tabs-"));
    const tpl = path.join(tmp, "library/overlays/probe");
    fs.mkdirSync(tpl, { recursive: true });
    fs.writeFileSync(path.join(tpl, "template.json"), JSON.stringify({ id: "probe", name: "Probe", static: true }));
    fs.writeFileSync(path.join(tpl, "index.html"), TEMPLATE);
    const projectFile = path.join(tmp, "tabs.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);

    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#editor-tabs .etab").length > 0`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("starts on the open sequence, with nothing to close", async () => {
    assert.equal(await page.eval(TABS), "Main*");
    // The daemon always has one sequence open, so the last tab cannot be closed: a strip
    // with no sequence tab would misrepresent what the preview is showing.
    assert.equal(await page.eval(`[...document.querySelectorAll("#editor-tabs .etab")].some(t => t.querySelector(".x"))`), false);
  });

  it("opening another sequence gives it a tab and makes it the one being edited", async () => {
    await page.eval(`(() => { const s = document.querySelector("#topbar select"); s.value = "intro"; s.dispatchEvent(new Event("change", { bubbles: true })); })()`);
    await page.until(`${TABS} === "Main | Intro*"`);
    assert.equal(await page.eval(OPEN_SEQUENCE), "intro", "the daemon must hold what the strip shows as open");
    // The timeline is the open sequence's, not the tab's neighbour's.
    assert.equal(await page.eval(`[...document.querySelectorAll("#timeline .clip")].map(c => c.dataset.id).join()`), "i1");
  });

  it("clicking a sequence tab switches back, timeline and all", async () => {
    await page.eval(clickTab("Main"));
    await page.until(`${TABS} === "Main* | Intro"`);
    assert.equal(await page.eval(OPEN_SEQUENCE), "main");
    assert.equal(await page.eval(`[...document.querySelectorAll("#timeline .clip")].map(c => c.dataset.id).join()`), "m1");
  });

  it("closing the tab being edited falls back to another, and does not reopen it", async () => {
    await page.eval(clickTab("Intro"));
    await page.until(`${TABS} === "Main | Intro*"`);
    await page.eval(closeTab("Intro"));
    await page.until(`${TABS} === "Main*"`);
    assert.equal(await page.eval(OPEN_SEQUENCE), "main", "the daemon must follow the tab that took over");
  });

  it("holds sequence and overlay tabs together, and shows one editor at a time", async () => {
    await page.eval(`[...document.querySelectorAll(".bin-row")].find(r => r.textContent.includes("Probe")).dispatchEvent(new MouseEvent("dblclick", { bubbles: true }))`);
    await page.until(`${TABS} === "Main | Probe*"`);
    assert.deepEqual(await page.eval(`[document.getElementById("preview").hidden, document.getElementById("workbench").hidden]`), [true, false]);

    await page.eval(clickTab("Main"));
    await page.until(`${TABS} === "Main* | Probe"`);
    assert.deepEqual(await page.eval(`[document.getElementById("preview").hidden, document.getElementById("workbench").hidden]`), [false, true]);
    // The overlay tab closes the same way, and stays closed.
    await page.eval(closeTab("Probe"));
    await page.until(`${TABS} === "Main*"`);
  });
});
