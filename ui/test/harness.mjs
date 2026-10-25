// Shared plumbing for the browser tests: finding what they need, a daemon to point at,
// and a way to drive the page.
//
// The driver speaks CDP over Node's own WebSocket rather than depending on a browser
// automation library: the tests already require the browser the baker uses, and one
// screenshot-and-evaluate loop is cheaper than a dependency the rest of the repo does not
// have. `--screenshot` alone cannot click, and a tab strip is nothing but clicks.

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

export const REPO = path.resolve(import.meta.dirname, "..", "..");

/** The same browser the baker uses, so a machine that can bake can run these. */
export function findBrowser() {
  const env = process.env.HYPERFRAMES_BROWSER_PATH;
  if (env && fs.existsSync(env)) return env;
  for (const root of ["/opt/pw-browsers", path.join(os.homedir(), ".cache/ms-playwright")]) {
    let entries = [];
    try { entries = fs.readdirSync(root); } catch { continue; }
    for (const e of entries.filter((x) => x.startsWith("chromium"))) {
      for (const rel of ["chrome-linux/headless_shell", "chrome-linux/chrome"]) {
        const p = path.join(root, e, rel);
        if (fs.existsSync(p)) return p;
      }
    }
  }
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    for (const name of ["chromium", "chromium-browser", "google-chrome"]) {
      const p = path.join(dir, name);
      if (dir && fs.existsSync(p)) return p;
    }
  }
  return null;
}

export const findKuma = () =>
  ["target/release/kuma", "target/debug/kuma"].map((p) => path.join(REPO, p)).find((p) => fs.existsSync(p)) ?? null;

export const freePort = () =>
  new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });

/** Why these tests cannot run here, or null when they can. */
export const missing = () =>
  !findKuma() ? "no kuma binary (cargo build --release)"
  : !fs.existsSync(path.join(REPO, "ui/dist/index.html")) ? "ui/dist is not built"
  : !findBrowser() ? "no Chromium"
  : null;

/** The preview engine's socket, played by the page: ready at once, and every command the
 *  page sends kept in `window.__engine`. Every other socket is the real one. */
export const FAKE_ENGINE = `(() => {
  const Real = window.WebSocket;
  window.__engine = [];
  window.WebSocket = function (url, protocols) {
    if (!String(url).includes("/api/preview/ws")) return new Real(url, protocols);
    const ws = { readyState: 0, binaryType: "blob", onopen: null, onmessage: null, onclose: null, onerror: null,
      send(m) { window.__engine.push(String(m)); }, close() {} };
    setTimeout(() => {
      ws.readyState = 1;
      ws.onopen?.({});
      ws.onmessage?.({ data: JSON.stringify({ ready: true, scale: 0.5 }) });
    }, 50);
    return ws;
  };
  window.WebSocket.OPEN = 1;
})();`;

/** A daemon serving `projectFile`, answering on the returned url. */
export async function startDaemon(projectFile, library) {
  const port = await freePort();
  const url = `http://127.0.0.1:${port}`;
  const proc = spawn(findKuma(), ["serve", "--project", projectFile, "--bind", `127.0.0.1:${port}`], {
    // A test that names its library reads that one alone: an empty user library of its own,
    // or the one on this machine (and whatever libraries were added to it) would join in.
    env: { ...process.env, KUMA_REPO: REPO, ...(library ? { KUMA_LIBRARY: library, XDG_DATA_HOME: fs.mkdtempSync(path.join(os.tmpdir(), "kuma-data-")) } : {}) },
    stdio: "ignore",
  });
  const deadline = Date.now() + 30_000;
  for (;;) {
    if (await fetch(`${url}/api/health`).then((r) => r.ok).catch(() => false)) break;
    if (Date.now() >= deadline) throw new Error("the daemon never answered");
    await new Promise((r) => setTimeout(r, 200));
  }
  return { url, proc };
}

/**
 * A headless browser on `url`, driven over CDP. `eval(js)` runs in the page and resolves
 * with the value (awaiting a promise), which is all these tests need: read the DOM, click
 * something, read it again.
 */
export async function driver(url) {
  const port = await freePort();
  // `--headless` explicitly: `findBrowser` may hand back a full Chrome, which without it
  // looks for an X server and exits before it ever opens the debugging port.
  const chrome = spawn(findBrowser(), [
    "--headless", `--remote-debugging-port=${port}`, "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
    "--window-size=1600,950", "about:blank",
  ], { stdio: "ignore" });

  const deadline = Date.now() + 30_000;
  let target = null;
  for (;;) {
    target = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json()).then((l) => l.find((t) => t.type === "page")).catch(() => null);
    if (target) break;
    if (Date.now() >= deadline) { chrome.kill("SIGKILL"); throw new Error("the browser never opened a page"); }
    await new Promise((r) => setTimeout(r, 200));
  }

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let id = 0;
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  };
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  const send = (method, params = {}) => new Promise((res) => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.navigate", { url });

  return {
    /** Run `source` in every document this page loads from now on, before its own scripts —
     *  for standing something in for what this machine lacks (the preview engine). Reload
     *  the page to have it in the one already loaded. */
    onNewDocument(source) {
      return send("Page.addScriptToEvaluateOnNewDocument", { source });
    },
    async eval(expression) {
      const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
      const failed = r.result?.exceptionDetails;
      if (failed) throw new Error(`${expression}\n${failed.exception?.description ?? JSON.stringify(failed)}`);
      return r.result?.result?.value;
    },
    /** Poll `expression` until it is truthy, so a test waits on the page rather than on a
     *  guessed delay — an edit here is a round trip to the daemon and back. A throw is "not
     *  yet" too: after a reload the page is rebuilt under the poll, and what it waits for is
     *  briefly not there. The last one is reported if time runs out. */
    async until(expression, ms = 15_000) {
      const end = Date.now() + ms;
      for (;;) {
        let v, threw;
        try {
          v = await this.eval(expression);
        } catch (e) {
          threw = e;
        }
        if (v) return v;
        if (Date.now() > end) throw new Error(`timed out waiting for: ${expression}${threw ? `\nlast: ${threw.message}` : ""}`);
        await new Promise((r) => setTimeout(r, 150));
      }
    },
    /** A drag of `files` from outside the page through the browser's own drag pipeline:
     *  `type` is dragEnter, dragOver, drop or dragCancel, at viewport `x`, `y`. */
    dragFiles(type, x, y, files) {
      return send("Input.dispatchDragEvent", { type, x, y, data: { items: [], files, dragOperationsMask: 1 } });
    },
    /** A real mouse event through the browser's input pipeline — `type` is mousePressed,
     *  mouseReleased or mouseMoved, at viewport `x`, `y` — for a gesture a synthetic event
     *  cannot prove: a drag, or a double-click the page's pointer handlers might swallow. */
    mouse(type, x, y, opts = {}) {
      return send("Input.dispatchMouseEvent", { type, x, y, button: type === "mouseMoved" && !opts.held ? "none" : "left", buttons: opts.held || type === "mousePressed" ? 1 : 0, clickCount: opts.clickCount ?? 1 });
    },
    /** A real key press through the browser's input pipeline — what a page sees as the
     *  person's gesture, which a synthetic `KeyboardEvent` is not (`play()` refuses one). */
    async press(key, code = key, opts = {}) {
      const vk = { " ": 32, ArrowLeft: 37, ArrowRight: 39 }[key] ?? key.toUpperCase().charCodeAt(0);
      const base = { key, code, windowsVirtualKeyCode: vk, modifiers: opts.shift ? 8 : 0 };
      await send("Input.dispatchKeyEvent", { type: "keyDown", ...base, ...(key.length === 1 ? { text: key } : {}) });
      await send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
    },
    /** A PNG of the viewport, written to `file` — for looking at a page while writing a
     *  test, not for asserting on. */
    async screenshot(file) {
      const r = await send("Page.captureScreenshot", { format: "png" });
      fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
    },
    close() {
      try { ws.close(); } catch { /* already gone */ }
      chrome.kill("SIGTERM");
    },
  };
}

/** Whether `bin` runs here: `melt`, `ffmpeg`. A test that needs one skips without it. */
export const have = (bin) => spawnSync(bin, ["-version"], { stdio: "ignore" }).status === 0;

/** Select a clip the way a click does: press on it, and let go. */
export const selectClip = (page, id) => page.eval(`(() => { const c = document.querySelector('.clip[data-id="${id}"]'); const r = c.getBoundingClientRect();
  c.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + 20, clientY: r.top + 2 }));
  window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true })); return true; })()`);

/** Wait for the page to have caught up with the daemon: the status line shows the revision
 *  it last loaded, so a gesture after this builds its patch on the current one. */
export const settled = (page) => page.until(`fetch("/api/project").then(r => r.json()).then(p => p.revision).then(rev => document.querySelector("#statusline .rev").textContent.startsWith("r" + rev))`);
