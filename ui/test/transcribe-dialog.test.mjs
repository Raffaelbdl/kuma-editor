// The Transcribe dialog offers what the daemon takes: the language, a model when several
// are installed, snapping word edges to pauses, and how long each subtitles entry runs.
// What it sends is read off the request; whisper itself never runs here.
//
// Skips, loudly, without a browser, a binary, `ui/dist` or ffmpeg to make the media.

import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { after, before, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { driver, missing, startDaemon } from "./harness.mjs";

const why = missing() ?? (spawnSync("ffmpeg", ["-version"]).status === 0 ? null : "no ffmpeg");

describe("the transcribe dialog", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const LABELS = `[...document.querySelectorAll("dialog.modal label")].map(l => l.textContent)`;
  const field = (label) => `[...document.querySelectorAll("dialog.modal label")].find(l => l.textContent === ${JSON.stringify(label)}).parentElement.querySelector("input, select")`;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-transcribe-"));
    const r = spawnSync("ffmpeg", ["-v", "error", "-y", "-f", "lavfi", "-i", "sine=duration=1", path.join(tmp, "talk.wav")]);
    assert.equal(r.status, 0, String(r.stderr));
    // Two models in the user library's `models/`: names are all the dialog reads.
    const models = path.join(tmp, "data/kuma/models");
    fs.mkdirSync(models, { recursive: true });
    for (const m of ["ggml-base.bin", "ggml-large-v3.bin"]) fs.writeFileSync(path.join(models, m), "");
    const projectFile = path.join(tmp, "t.kuma.json");
    fs.writeFileSync(projectFile, JSON.stringify({
      version: 8, name: "t", master: "main", fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
      bin: {
        talk: { name: "Talk", source: { path: "talk.wav", kind: "audio" }, meta: { duration: 1, has_audio: true } },
        main: { name: "Main", source: { markers: [], tracks: [] } },
      },
    }));
    process.env.XDG_DATA_HOME = path.join(tmp, "data");
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    // Every request the dialog's job sends, with its body.
    await page.eval(`(() => { const f = window.fetch; window.sent = []; window.fetch = (u, init) => { if (String(u).includes("/api/transcribe")) window.sent.push(JSON.parse(init.body)); return f(u, init); }; return true; })()`);
    await page.until(`!!document.querySelector('.bin-row[title="Talk"]')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    delete process.env.XDG_DATA_HOME;
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("asks for the model, snapping and the split, and sends them", async () => {
    const models = await page.eval(`fetch("/api/health").then(r => r.json()).then(h => h.whisper_models)`);
    assert.deepEqual(models, ["ggml-base.bin", "ggml-large-v3.bin"], "the daemon reads the test's models");
    await page.eval(`document.querySelector('.bin-row[title="Talk"]').click(); true`);
    await page.until(`[...document.querySelectorAll("#inspector button")].some(b => b.textContent === "Transcribe → subtitles")`);
    await page.eval(`[...document.querySelectorAll("#inspector button")].find(b => b.textContent === "Transcribe → subtitles").click(); true`);
    await page.until(`document.querySelector("dialog.modal") && ${LABELS}.includes("Snap to pauses")`);
    assert.deepEqual(await page.eval(LABELS), ["Language", "Model", "Snap to pauses", "Split every (s)"]);
    assert.equal(await page.eval(`${field("Model")}.value`), "ggml-base.bin", "the daemon's default first");
    assert.equal(await page.eval(`${field("Snap to pauses")}.checked`), true);
    assert.equal(await page.eval(`${field("Split every (s)")}.value`), "60");
    await page.eval(`(() => {
      const m = ${field("Model")}; m.value = "ggml-large-v3.bin";
      ${field("Snap to pauses")}.checked = false;
      ${field("Split every (s)")}.value = "2";
      document.querySelector("dialog.modal form").requestSubmit(); return true; })()`);
    await page.until(`window.sent.length === 1`);
    const [body] = await page.eval(`window.sent`);
    assert.deepEqual({ model: body.model, snap: body.snap, split: body.split, bin: body.bin }, { model: "ggml-large-v3.bin", snap: false, split: 5, bin: "talk" },
      "under 5 s a split is 5 s, the finest the daemon cuts");
  });
});
