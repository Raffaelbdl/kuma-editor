# Testing by hand

How to check the editor beyond `npm test`: looking at it, driving it with a real pointer,
and measuring it. It is written for the next session that changes the UI — the recipes
here are the ones that caught real bugs, and the warnings are traps that were fallen into. Everything here runs headless — no
display, no GPU, no preview engine — so it also works on a CI-like machine or in a cloud
session. Scratch files go in a scratch directory, never in the repository.

## 1. Build what the test will load

```
cargo build                          # target/debug/kuma — the daemon the page talks to
(cd ui && npm run build)             # ui/dist — the page the daemon serves
(cd app && npm run build)            # app/dist — only for the launcher (§5)
```

The daemon serves `ui/dist`, not `ui/src`: a test of a source change without a rebuild
tests the old code.

## 2. A daemon on a throwaway project

Copy a project into a scratch directory — never point a test at a project you care
about, every edit is saved — and start a daemon on a port of its own:

```
mkdir -p "$SCRATCH/demo" && cp -r <some project folder>/* "$SCRATCH/demo/"
KUMA_LIBRARY=$PWD/library target/debug/kuma serve --repo $PWD \
  -p "$SCRATCH/demo/demo.kuma.json" --bind 127.0.0.1:7611 &
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7611/   # 200 when it is up
```

- `--repo` is required when the project file is outside the checkout (the daemon looks for
  `node_modules/hyperframes` and `ui/dist` there).
- A small project is a few lines of JSON: `ui/test/*.test.mjs` each build one inline (a
  `bin` with a colour entry and a `main` sequence of tracks), which needs no media on
  disk. For a big one, generate it — 8 tracks × 60 clips, 120 of them overlay clips — to
  see costs that a ten-clip project hides.
- Stop it by its PID, found through `/proc/*/cmdline` matching the port. **Not with
  `pkill -f <pattern>`**: the shell running the command contains the pattern too, and kills
  itself. Exclude `$$` when scanning.

## 3. Drive the page

Playwright's Chromium, headless, at a desktop size:

```js
const { chromium } = require("playwright");          // wherever it is installed
const b = await chromium.launch();
const p = await b.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
p.on("console", (m) => m.type() === "error" && errors.push(m.text()));
await p.goto("http://127.0.0.1:7611/");
await p.evaluate(() => localStorage.clear()); await p.reload();   // the default dock, mode, panels
await p.waitForTimeout(1500);
await p.screenshot({ path: "power.png" });
await p.keyboard.press("Backquote");                   // den
await p.keyboard.press("h");                            // summon the history
await p.screenshot({ path: "den.png" });
console.log(errors);                                    // read them: a 404 for a missing thumbnail is
await b.close();                                        // expected, a TypeError is not
```

- **Clear `localStorage` first.** The dock, the mode, what floats and where are kept per
  origin (`kuma.dock`, `kuma.mode`, `kuma.panels.*`); an earlier run's arrangement
  otherwise leaks into this one.
- **Look at the screenshots.** Most of what went wrong in this work showed there first:
  a group squeezed to nothing, a tab row overflowing, a style lost to a bad CSS merge.
- **Real pointer input for drags.** A synthetic `dispatchEvent` skips hit testing and
  pointer capture; use `page.mouse` (or `harness.mjs`' `page.mouse(type, x, y, {held})`,
  which goes through CDP), and move in several steps — `dragPointer` ignores travel under
  its dead zone. `ui/test/dock.test.mjs` is the pattern: press on a tab, move in eight
  steps, release over a group's middle or edge, read the arrangement back.
- **Read the arrangement as a string** rather than by eye: `ui/test/dock.test.mjs`' `GROUPS`
  turns the dock into `bin*+blueprints | inspector* | timeline*` (the front tab starred).
- `ui/test/harness.mjs` (`startDaemon`, `driver`, `page.until`) does §2 and §3 for a test
  file; for a one-off look, the bare script above is quicker to change.

### No preview engine

There is no engine binary in a headless session, so the preview falls back to snapshots
and playback cannot run. To exercise playback, replace the engine's socket before the page
loads (`page.addInitScript`, or `harness.mjs`' `page.onNewDocument`): a fake `WebSocket` for
`/api/preview/ws` that records what the page sends (`play`, `pause`, `seek …`) and pushes
`{ready: true}` and binary frames (8 bytes of little-endian frame number, then a JPEG) at
25 a second. `ui/test/playback.test.mjs` has one.

### Panel windows

A panel's own window is the same page at `?panel=<id>`. Two pages in one browser share the
`BroadcastChannel`, so open the editor and `/?panel=agent` side by side to test view sync
and the way back. To test a window that vanishes without a goodbye, play its side from a
second channel inside the editor's own page (`ui/test/panels.test.mjs`).

## 4. Measure

When a change is about speed, measure before and after on the same machine, interleaved,
on the big project — absolute numbers from a headless software renderer are higher than on
a desktop with a GPU; the ratios hold.

- **Freeze the builds you compare.** Copy `ui/dist` to `dist-before/` and `dist-after/`
  in the scratch directory and serve each through Playwright's request interception, so a
  rebuild during the run cannot change what is measured.
- **CDP metrics** (`Performance.getMetrics`): `TaskDuration`, `ScriptDuration`,
  `LayoutCount`, `RecalcStyleCount`, `JSHeapUsedSize` — deltas over a fixed scenario
  (ten seconds of playback at 25 frames a second; twenty edits; fifty agent calls; thirty
  resize steps).
- **`store.set` time**: wrap the store's `set` in the served bundle (the minified method
  is findable by its body) to time every state change and what changed in it.
- **DOM churn**: a `MutationObserver` on `document.body` counting records and added
  elements per tick or per edit.
- **Hotspots**: `Profiler.start/stop` around the scenario, self time per function, mapped
  back to `ui/src` through the bundle's source map.
- **Leaks**: heap after `HeapProfiler.collectGarbage` before and after a few hundred mode
  switches, panel toggles, edits and activity events; the store's listener count should
  stay flat.
- **Edits from outside** — the agent's path — are a `POST /api/project/patch` with an
  `origin` of `mcp: …`; the UI's own go through `store.patch`. Time both: they refresh
  differently.
- **Agent activity** without an agent: `POST /api/activity` with
  `{phase: "call", tool, client, args, ok, result, revision, started_at}`.
- **The daemon** is measured separately, with a throwaway release-mode bench that depends
  on `kuma-core` and `kuma-daemon` by path (edit, undo, restore latency; RSS after a few
  hundred edits). Keep it out of the workspace.

## 5. The launcher

The launcher is a page of the desktop shell, served from `app/dist/launcher`. It needs no
Electron to look at: serve that directory with any static server and stub its bridge
before the page loads (`page.addInitScript`), with `window.launcher` answering `info()`,
`recents()`, `previewPath()` and the rest from fixed data — including a `create()` that
throws, to see the error, and an `info()` with `kuma: null`, to see a missing binary. Then
screenshot each state: recents, empty, new project, broken, a file dragged over. The page's
Content-Security-Policy still applies, which is the point: a font or style it forbids
shows up as a missing face.

Run the screenshot script from the scratch directory, not from `app/`: a relative output
path written from `app/` lands in `app/launcher/` and is copied into the build.

## 6. Then the suites

```
npm test
```

Runs Rust, the shell and the UI whatever each does, and says which skipped. Without
`melt` the render paths skip; without the hyperframes player, `drawn-overlay.test.mjs`
fails on `main` as well — check a failure against `main` before chasing it. A UI test
that fails once under a full parallel run and passes alone and on a rerun is load, not a
regression; one that fails the same way twice is a regression.
