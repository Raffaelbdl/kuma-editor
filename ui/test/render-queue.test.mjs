// The render queue, end to end: the dialog asks the daemon for renders, the daemon runs
// them one at a time, and Cancel stops the one encoding and the one waiting alike.
//
// Real melt against a real daemon — a queue is ordering between processes, which a
// stand-in cannot show. The timeline is one long colour clip at a tiny size, so an encode
// lasts long enough to be caught running and costs nothing to stop.
//
// Skips, loudly, without melt, a browser, a binary or `ui/dist`, as its neighbours do.

import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const PROJECT = JSON.stringify({
  version: 1, name: "queue", master: "main",
  fps: { num: 30, den: 1 }, width: 320, height: 240, sample_rate: 48000,
  bin: {
    navy: { name: "Navy", source: { color: "#102030" } },
    main: { name: "Main", source: { markers: [], tracks: [{ id: "v1", kind: "video", name: "V", muted: false, hidden: false, transitions: [],
      clips: [{ id: "c1", start: 0, end: 3600, bin: "navy", in: 0, speed: 1, volume: 1, muted: false, effects: [], label: "" }] }] } },
  },
}, null, 2);

const why = missing() ?? (spawnSync("melt", ["-version"]).status === 0 ? null : "no melt");

describe("the render queue", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const dialog = `document.querySelector("dialog.modal.render")`;
  const row = (id) => `${dialog}.querySelector('.job[data-id="${id}"]')`;
  const status = (id) => `(${row(id)}?.className ?? "").replace("job ", "")`;
  const cancel = (id) => `[...${row(id)}.querySelectorAll("button")].find(b => b.textContent === "Cancel").click()`;
  const JOB_IDS = `[...${dialog}.querySelectorAll(".job")].map(j => Number(j.dataset.id))`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-queue-"));
    const projectFile = path.join(tmp, "queue.kuma.json");
    fs.writeFileSync(projectFile, PROJECT);
    fs.mkdirSync(path.join(tmp, "library/overlays"), { recursive: true });
    daemon = await startDaemon(projectFile, path.join(tmp, "library"));
    page = await driver(daemon.url);
    await page.until(`document.querySelector("#topbar button.primary")`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("runs one render at a time, and cancels the running one and the queued one", async () => {
    await page.eval(`document.querySelector("#topbar button.primary").click()`);
    await page.until(`${dialog}?.querySelector(".render-pane select:not([title]) option[value=draft]")`);
    await page.eval(`(() => { const p = ${dialog}.querySelector(".render-pane select:not([title])"); p.value = "draft"; p.dispatchEvent(new Event("change")); })()`);
    const go = `${dialog}.querySelector(".render-pane button.primary").click()`;
    const toRender = `[...${dialog}.querySelectorAll("button.tab")].find(b => b.textContent === "Render").click()`;

    await page.eval(go);
    await page.until(`${JOB_IDS}.length === 1`);
    await page.eval(`${toRender}; ${dialog}.querySelector(".render-pane input").value = "renders/second.mp4"`);
    await page.eval(go);
    await page.until(`${JOB_IDS}.length === 2`);
    const [second, first] = await page.eval(JOB_IDS);

    await page.until(`${status(first)} === "running" && ${row(first)}.textContent.includes("frame")`, 60_000);
    assert.equal(await page.eval(status(second)), "queued", "a second render waits for the first");
    // The top bar counts rather than narrates.
    assert.equal(await page.eval(`document.querySelector("#topbar .status").textContent`), "1 job running, 1 job queued");

    await page.eval(cancel(first));
    await page.until(`${status(first)} === "cancelled"`, 20_000);
    // Its turn comes as soon as the first is stopped.
    await page.until(`${status(second)} === "running"`, 20_000);
    await page.eval(cancel(second));
    await page.until(`${status(second)} === "cancelled"`, 20_000);
    await page.until(`document.querySelector("#topbar .status").textContent === "2 jobs cancelled"`);

    // Nothing half-written is left behind, and the daemon agrees with the rows.
    assert.deepEqual(fs.readdirSync(path.join(tmp, "renders")).filter((f) => f.endsWith(".mp4")), []);
    const jobs = await page.eval(`fetch("/api/jobs?kind=render").then(r => r.json()).then(j => j.jobs.map(x => [x.id, x.cancelled]))`);
    assert.deepEqual(jobs, [[first, true], [second, true]]);
  });

  it("the queue a window opens on is the daemon's, not only what it saw happen", async () => {
    await page.eval(`location.reload()`);
    await page.until(`document.querySelector("#topbar .status.link")`);
    await page.eval(`document.querySelector("#topbar .status").click()`);
    await page.until(`${dialog} && ${JOB_IDS}.length === 2 && ${dialog}.querySelector(".queue-pane").hidden === false`);
  });
});
