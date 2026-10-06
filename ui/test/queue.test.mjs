// Render and Queue are two buttons onto one dialog's two tabs, and the queue says what
// each job works on, whoever started it: here a sample asked of the daemon directly, as
// an agent or the CLI does.
//
// Skips, loudly, without a browser, a binary or `ui/dist`.

import { strict as assert } from "node:assert";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const why = missing();

describe("the render and queue buttons", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const HEADING = `document.querySelector("dialog.modal.render h3")?.textContent`;
  const button = (text) => `[...document.querySelectorAll("#topbar button")].find(b => b.firstChild?.textContent === ${JSON.stringify(text)})`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-queue-"));
    const projectFile = path.join(tmp, "q.kuma.json");
    fs.writeFileSync(projectFile, JSON.stringify({
      version: 8, name: "q", master: "main", fps: { num: 30, den: 1 }, width: 320, height: 180, sample_rate: 48000,
      bin: {
        red: { name: "Red", source: { color: "#ff0000" } },
        main: { name: "Main", source: { markers: [], tracks: [{ id: "v1", kind: "video", clips: [{ id: "c1", start: 0, end: 2, bin: "red" }] }] } },
      },
    }));
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!${button("queue")}`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("shows the render queue for one and the render dialog for the other", async () => {
    await page.eval(`${button("queue")}.click(); true`);
    await page.until(`document.querySelector('.dock-group .panel[data-panel="queue"]:not([hidden])') && document.querySelector('#rail .rail-key.on')?.dataset.panel === "queue"`);
    await page.eval(`${button("render")}.click(); true`);
    await page.until(`${HEADING} === "Render"`);
    await page.eval(`[...document.querySelectorAll("dialog.modal.render button.tab")].find(b => b.textContent === "Job queue").click()`);
    await page.until(`${HEADING} === "Job queue"`);
    await page.eval(`document.querySelector("dialog.modal.render .queue-pane .modal-actions button:last-child").click()`);
    await page.until(`!document.querySelector("dialog.modal.render")`);
  });

  it("lists a job started outside the editor, saying what it works on", async () => {
    await page.eval(`${button("queue")}.click(); true`);
    const { job } = await page.eval(`fetch("/api/sample/job", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ t0: 0, times: [0.5] }) }).then(r => r.json())`);
    await page.until(`[...document.querySelectorAll('.q-job[data-id="${job}"] .q-title')].some(t => t.textContent === "sample · Main")`);
  });
});
