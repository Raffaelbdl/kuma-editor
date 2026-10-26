// The markers panel lists what `list_markers` gives the agent: the open sequence's markers,
// then every other sequence's under its name. A comment left inside a nested sequence was
// read by the agent and invisible from the master that nests it, and so taken for deleted.
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

/** The master nests "Scene", which holds a person's comment; the master holds the agent's note. */
const PROJECT = JSON.stringify({
  version: 9, name: "markers", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: {
    red: { name: "Red", source: { color: "#ff0000" } },
    scene: { name: "Scene", source: { markers: [{ t: 1, label: "zoom on the title", origin: "ui" }], tracks: [track("sv", "video", "Video", [clip("s1", "red", 0, 4)])] } },
    main: { name: "Main", source: { markers: [{ t: 0, label: "provisional cut", origin: "mcp" }], tracks: [track("v1", "video", "Video", [clip("c1", "scene", 0, 4)])] } },
  },
}, null, 2);

const why = missing();

describe("the markers panel", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const CARDS = `[...document.querySelectorAll(".mk-list > *")].map(e => e.matches(".mk-seq") ? "# " + e.textContent : e.querySelector(".mk-text")?.textContent ?? "-").join("|")`;
  const STORED = `fetch("/api/project").then(r => r.json()).then(p => p.bin.scene.source.markers.length)`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-markers-"));
    const projectFile = path.join(tmp, "markers.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`document.querySelectorAll("#timeline .tl-track").length === 1`);
    await page.eval(`(window.dispatchEvent(new KeyboardEvent("keydown", { key: "m", bubbles: true })), true)`);
    await page.until(`document.querySelector(".markers-panel") !== null`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("shows a nested sequence's comment from the master, opens it there, and removes it", async () => {
    await page.until(`${CARDS} === "provisional cut|# Scene|zoom on the title"`);
    assert.equal(await page.eval(`document.querySelectorAll("#timeline .tl-marker-flag").length`), 1, "the timeline still draws only the open sequence's");
    assert.equal(await page.eval(`document.querySelector('[data-panel="markers"]').classList.contains("flag")`), true, "and the rail says a comment is waiting");

    // A click on it opens its sequence at its moment.
    await page.eval(`([...document.querySelectorAll(".mk-card")].find(c => c.textContent.includes("zoom on the title")).click(), true)`);
    await page.until(`${CARDS} === "zoom on the title|# Main|provisional cut"`);
    await page.until(`[...document.querySelectorAll("#timeline .tl-marker-flag")].map(f => f.textContent).join("|") === "zoom on the title"`);

    // Back on the master, its ✕ removes it from the sequence it is on.
    await page.eval(`([...document.querySelectorAll(".mk-card")].find(c => c.textContent.includes("provisional cut")).click(), true)`);
    await page.until(`${CARDS} === "provisional cut|# Scene|zoom on the title"`);
    await page.eval(`([...document.querySelectorAll(".mk-card")].find(c => c.textContent.includes("zoom on the title")).querySelector(".mk-remove").click(), true)`);
    await page.until(`${CARDS} === "provisional cut"`);
    assert.equal(await page.eval(STORED), 0, "gone from the document the agent reads");
    assert.equal(await page.eval(`document.querySelector('[data-panel="markers"]').classList.contains("flag")`), false);
  });
});
