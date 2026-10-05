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
    fs.mkdirSync(path.join(tmp, "library/blueprints"), { recursive: true });
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
    assert.equal(await page.eval(`document.querySelector("#topbar .status").textContent`), "1 running · 1 queued");

    await page.eval(cancel(first));
    await page.until(`${status(first)} === "cancelled"`, 20_000);
    // Its turn comes as soon as the first is stopped.
    await page.until(`${status(second)} === "running"`, 20_000);
    await page.eval(cancel(second));
    await page.until(`${status(second)} === "cancelled"`, 20_000);
    await page.until(`document.querySelector("#topbar .status").textContent === "2 cancelled"`);

    // Nothing half-written is left behind, and the daemon agrees with the rows.
    assert.deepEqual(fs.readdirSync(path.join(tmp, "renders")).filter((f) => f.endsWith(".mp4")), []);
    const jobs = await page.eval(`fetch("/api/jobs?kind=render").then(r => r.json()).then(j => j.jobs.map(x => [x.id, x.state]))`);
    assert.deepEqual(jobs, [[first, "cancelled"], [second, "cancelled"]]);
    // The row names the file as a person would, not by its absolute path.
    assert.equal(await page.eval(`${row(second)}.querySelector(".job-path").textContent`), "renders/second.mp4");
    await page.eval(`${dialog}.querySelector(".queue-pane .modal-actions button:last-child").click()`);
    await page.until(`!${dialog}`);
  });

  it("warns before rendering over a file that is already there", async () => {
    fs.writeFileSync(path.join(tmp, "renders/queue-draft.mp4"), "");
    const before = (await page.eval(`fetch("/api/jobs?kind=render").then(r => r.json()).then(j => j.jobs.length)`));
    await page.eval(`document.querySelector("#topbar button.primary").click()`);
    await page.until(`${dialog}?.querySelector(".render-pane select:not([title]) option[value=draft]")`);
    await page.eval(`(() => { const p = ${dialog}.querySelector(".render-pane select:not([title])"); p.value = "draft"; p.dispatchEvent(new Event("change")); })()`);
    const warning = `${dialog}.querySelector(".render-warning")`;
    await page.until(`!${warning}.hidden`);
    assert.equal(await page.eval(`${warning}.textContent`), "queue-draft.mp4 already exists. Rendering replaces it.");

    // Another name clears it.
    await page.eval(`(() => { const o = ${dialog}.querySelector(".render-pane input"); o.value = "renders/fresh.mp4"; o.dispatchEvent(new Event("input")); })()`);
    await page.until(`${warning}.hidden`);
    await page.eval(`(() => { const o = ${dialog}.querySelector(".render-pane input"); o.value = ""; o.dispatchEvent(new Event("input")); })()`);
    await page.until(`!${warning}.hidden`);

    // The click asks, and saying no queues nothing.
    await page.eval(`${dialog}.querySelector(".render-pane button.primary").click()`);
    const ask = `[...document.querySelectorAll("dialog.modal")].find(d => d.textContent.includes("Overwrite?"))`;
    await page.until(ask);
    await page.eval(`[...${ask}.querySelectorAll("button")].find(b => b.textContent === "Cancel").click()`);
    await page.until(`!${ask}`);
    assert.equal(await page.eval(`fetch("/api/jobs?kind=render").then(r => r.json()).then(j => j.jobs.length)`), before);
    await page.eval(`${dialog}.querySelector(".render-pane .modal-actions button").click()`);
    fs.rmSync(path.join(tmp, "renders/queue-draft.mp4"));
  });

  it("the queue a window opens on is the daemon's, not only what it saw happen", async () => {
    await page.eval(`location.reload()`);
    await page.until(`document.querySelector("#topbar .status").textContent !== ""`);
    await page.eval(`document.querySelector("#topbar .queue-btn").click()`);
    await page.until(`!!document.querySelector('.panel[data-panel="queue"] .q-cache')`);
    await page.eval(`document.querySelector('.panel[data-panel="queue"] .q-cache').click()`);
    await page.until(`${dialog} && ${JOB_IDS}.length === 2 && ${dialog}.querySelector(".queue-pane").hidden === false`);
  });

  it("clears finished jobs, from the daemon as well as the list", async () => {
    const clear = `[...${dialog}.querySelectorAll(".queue-pane button")].find(b => b.textContent === "Clear finished")`;
    assert.equal(await page.eval(`${clear}.disabled`), false);
    await page.eval(`${clear}.click()`);
    await page.until(`${dialog}.querySelector(".job-list").textContent === "No jobs yet."`);
    assert.equal(await page.eval(`${clear}.disabled`), true);
    assert.equal(await page.eval(`document.querySelector("#topbar .status").textContent`), "");
    assert.deepEqual(await page.eval(`fetch("/api/jobs").then(r => r.json()).then(j => j.jobs)`), []);
  });

  it("asks for the threads, quality and speed chosen under Advanced, and only what the preset has", async () => {
    await page.eval(`document.querySelector("#topbar button.primary").click()`);
    await page.until(`${dialog}?.querySelector(".render-pane select:not([title]) option[value=draft]") && !${dialog}.querySelector(".render-pane").hidden`);
    const adv = `${dialog}.querySelector(".render-advanced")`;
    const field = (label) => `[...${adv}.querySelectorAll(".field")].find(f => f.querySelector("label").textContent === "${label}")`;
    const pick = (preset) => `(() => { const p = ${dialog}.querySelector(".render-pane select:not([title])"); p.value = "${preset}"; p.dispatchEvent(new Event("change")); })()`;
    const set = (label, value, event) => `(() => { const i = ${field(label)}.querySelector("input, select"); i.value = "${value}"; i.dispatchEvent(new Event("${event}")); })()`;

    // A preset this machine cannot run is listed greyed, and only such a one.
    const offered = await page.eval(`[...${dialog}.querySelectorAll(".render-pane select:not([title]) option")].map(o => [o.value, o.disabled])`);
    const presets = await page.eval(`fetch("/api/presets").then(r => r.json()).then(ps => ps.map(p => [p.id, !!p.unavailable]))`);
    assert.deepEqual(offered, presets);

    // Folded by default; opening it shows the machine's threads as the placeholder.
    assert.equal(await page.eval(`${adv}.classList.contains("closed")`), true);
    await page.eval(`${adv}.querySelector(".disclose").click()`);
    await page.until(`!${adv}.classList.contains("closed") && /^\\d+ \\(default\\)$/.test(${field("Threads")}.querySelector("input").placeholder)`);

    // ProRes has neither to change; draft has both, with its own values as the defaults.
    await page.eval(pick("prores-master"));
    await page.until(`${field("Quality")}.hidden && ${field("Speed")}.hidden && !${adv}.querySelector(".render-fixed").hidden`);
    await page.eval(pick("draft"));
    await page.until(`!${field("Quality")}.hidden && !${field("Speed")}.hidden`);
    assert.equal(await page.eval(`${field("Quality")}.querySelector("input").placeholder`), "26 (preset)");
    assert.equal(await page.eval(`${field("Speed")}.querySelector("select").value`), "ultrafast");

    await page.eval(set("Threads", "2", "input"));
    await page.eval(set("Quality", "30", "input"));
    await page.eval(set("Speed", "veryfast", "change"));
    assert.equal(await page.eval(`${adv}.querySelector(".render-advanced-note").textContent`), "CRF 30 · veryfast · 2 threads");
    await page.eval(`${dialog}.querySelector(".render-pane input").value = "renders/advanced.mp4"`);
    await page.eval(`${dialog}.querySelector(".render-pane button.primary").click()`);
    await page.until(`${JOB_IDS}.length === 1`);
    const [id] = await page.eval(JOB_IDS);

    // The daemon was asked for exactly those, and the queue says so.
    const params = await page.eval(`fetch("/api/jobs?kind=render").then(r => r.json()).then(j => j.jobs[0].params)`);
    assert.deepEqual([params.threads, params.quality, params.speed], [2, 30, "veryfast"]);
    assert.match(await page.eval(`${row(id)}.querySelector(".job-title").textContent`), /draft \(CRF 30 · veryfast · 2 threads\)$/);
    await page.until(`${status(id)} === "running" || ${status(id)} === "done"`, 60_000);
    if (await page.eval(`${status(id)} === "running"`)) {
      await page.eval(cancel(id));
      await page.until(`${status(id)} === "cancelled"`, 20_000);
    }
  });
});
