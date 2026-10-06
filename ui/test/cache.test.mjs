// The job queue's cache line: its size, and a button that prunes it and says what it freed.
// Skips without a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "cache", master: "main",
  fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
  bin: { main: { name: "Main", source: { markers: [], tracks: [] } } },
}, null, 2);

const why = missing();

describe("pruning the cache from the editor", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-cache-"));
    const projectFile = path.join(tmp, "cache.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    const stale = path.join(tmp, ".kuma/bakes/0123456789abcdef01234567/frames");
    fs.mkdirSync(stale, { recursive: true });
    fs.writeFileSync(path.join(stale, "frame_000001.png"), Buffer.alloc(2_500_000));
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector("#topbar .queue-btn")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("shows the cache's size in the job queue and prunes it", async () => {
    // The queue button shows the render queue; its head opens the whole job queue.
    await page.eval(`document.querySelector("#topbar .queue-btn").click()`);
    await page.until(`!!document.querySelector('.q-cache')`);
    await page.eval(`document.querySelector('.q-cache').click()`);
    const note = await page.until(`document.querySelector(".cache-size")?.textContent || ""`);
    assert.match(note, /^Cache 2\.5 MB · 1 bake, 0 frames$/);

    await page.eval(`[...document.querySelectorAll(".queue-pane button")].find(b => b.textContent === "Prune cache").click()`);
    await page.until(`[...document.querySelectorAll(".job .job-title")].some(t => t.textContent.endsWith("· prune"))`);
    await page.until(`document.querySelector(".cache-size").textContent.startsWith("Cache 0 B")`);
    assert.ok(await page.eval(`[...document.querySelectorAll(".toast")].some(t => t.textContent === "Freed 2.5 MB")`));
    assert.equal(fs.readdirSync(path.join(tmp, ".kuma/bakes")).length, 0);
  });
});
