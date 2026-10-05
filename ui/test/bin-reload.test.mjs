// A media file replaced on disk is read again from its row's right-click: the entry's
// probed metadata follows the file in one edit, and the toast says what changed. Rows that
// are not files are not offered it.
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

/** `seconds` of sound at `file`. */
const beep = (file, seconds) => {
  const r = spawnSync("ffmpeg", ["-v", "error", "-y", "-f", "lavfi", "-i", `sine=duration=${seconds}`, file]);
  assert.equal(r.status, 0, String(r.stderr));
};

describe("reloading a media entry from its file", { skip: why ? `skipping: ${why}` : false }, () => {
  let tmp, daemon, page;
  const DURATION = `fetch("/api/project").then(r => r.json()).then(p => p.bin.talk.meta.duration)`;
  const REVISION = `fetch("/api/project").then(r => r.json()).then(p => p.revision)`;
  /** Right-click the row titled `title` and list the menu it opens. */
  const menuOn = (title) => page.eval(`(() => {
    const row = document.querySelector('.bin-row[title=${JSON.stringify(title)}]');
    const r = row.getBoundingClientRect();
    row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.left + 10, clientY: r.top + 5 }));
    return [...document.querySelectorAll(".popup-menu .popup-item")].map(b => b.textContent);
  })()`);

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-reload-"));
    beep(path.join(tmp, "talk.wav"), 1);
    const projectFile = path.join(tmp, "reload.kuma.json");
    fs.writeFileSync(projectFile, JSON.stringify({
      version: 7, name: "reload", master: "main",
      fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000,
      bin: {
        talk: { name: "Talk", source: { path: "talk.wav", kind: "audio" }, meta: { duration: 1, has_audio: true } },
        red: { name: "Red", source: { color: "#f00" } },
        main: { name: "Main", source: { markers: [], tracks: [] } },
      },
    }));
    daemon = await startDaemon(projectFile);
    page = await driver(daemon.url);
    await page.until(`!!document.querySelector('.bin-row[title="Talk"]')`);
  });

  after(() => {
    page?.close();
    daemon?.proc.kill("SIGTERM");
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("is offered on a media row and nowhere else", async () => {
    assert.ok((await menuOn("Red")).every((l) => !l.includes("Reload")));
    await page.eval(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); true`);
    assert.equal((await menuOn("Talk"))[0], "Reload from file");
  });

  it("writes what the file says now, in one edit, and says what changed", async () => {
    beep(path.join(tmp, "talk.wav"), 2);
    const rev = await page.eval(REVISION);
    await page.eval(`[...document.querySelectorAll(".popup-menu .popup-item")].find(b => b.textContent === "Reload from file").click(); true`);
    await page.until(`${DURATION}.then(d => d === 2)`);
    assert.equal(await page.eval(REVISION), rev + 1);
    await page.until(`[...document.querySelectorAll("#toasts .toast.ok")].some(t => t.textContent.includes("duration 1 → 2"))`);
  });
});
