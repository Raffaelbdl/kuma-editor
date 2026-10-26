# kuma-editor — guidance for agents working in this repository

Hybrid video editor: the video half is MLT (the engine under Kdenlive/Shotcut), the
overlay half is HTML/CSS/TS baked with alpha by hyperframes, both driven from one JSON
project file and exposed to agents over MCP. Read `docs/manual.md` for the architecture and
`library/blueprints/CLAUDE.md` before authoring an overlay.

**Picking this up? Read `docs/decisions.md` first.** It records why the model looks the
way it does, including what was tried and rejected, and what is still open.

## Layout

```
crates/kuma-core      project model (seconds, exclusive ends, rational fps), schema, validation, JSON Patch edits
crates/kuma-mlt       JSON → MLT XML compiler (the only place MLT's rules live), melt driver, presets
crates/kuma-overlay   hyperframes bake driver, content-hash cache, ffprobe helpers
crates/kuma-daemon    HTTP/WS API, undo, jobs, overlay live-preview routes, preview engine transport;
                      the agent's activity, the history and markers' answers (session state, `activity.rs`/`history.rs`/`markers.rs`)
crates/kuma-mcp       rmcp stdio MCP server proxying the daemon
crates/kuma-cli       `kuma` binary: serve | mcp | render | bake | sample | lint | design-system | library | …
engine/               C++ libmlt++ preview engine (JPEG over stdout, SDL2-paced audio)
ui/                   Vite + TypeScript editor (no framework); den/power modes over one set of panels
                      (`panelhost.ts`), power's dock a tree of splits and tab groups (`dock.ts`)
app/                  Electron shell: project launcher, windows, daemon lifecycle
library/blueprints/   starter blueprints (folder = blueprint) + `_vendor/{gsap.min.js,kuma.js,kuma-motion.js,kuma-canvas.js,rough.min.js,kuma.css}`
library/presets/      starter effect presets (typed params + `$expr` substitution)
library/design-systems/ design systems (`DESIGN.md` + `tokens.css`) copied into a project; none ship yet
library/fonts/        starter font families (open licences) that `font` vars name
library/logs/         starter camera logs (`<id>.json`: the maker's curve and gamut, or their LUT)
                      that a grade's `log` names
library/skills/       the agent skill (`kuma-video`) that `kuma init` / `kuma mcp-config` write into a
                      project's `.claude/skills/`; embedded by `crates/kuma-cli/src/skill.rs`, whose
                      `FILES` must list every file (a test checks)
```

Blueprints, presets and fonts are user assets, not code: at runtime kuma reads
`$XDG_DATA_HOME/kuma` (default `~/.local/share/kuma`), the libraries the open project names
(`Project::libraries`; no other project sees them), and every directory in `KUMA_LIBRARY`,
later wins; presets also come from the project's own `presets/`, and
fonts and camera logs from the project's own `fonts/` and `logs/`, which win over every library.
Blueprints never come from a project. `library/` is only the starter set that
`kuma library` installs; until the user library has blueprints, and when `KUMA_LIBRARY` is
unset, every daemon reads it in place (`library_dirs`; the desktop shell only reports it).
Tests point `KUMA_LIBRARY` (or the baker) at it.

## Commands

```
npm run rebuild                           # binary + ui/dist + app/dist + engine + whisper.cpp, then `kuma doctor`
cargo build && cargo test                 # Rust; render/bake tests skip without melt/ffmpeg/hyperframes
npm test                                  # every suite: Rust, the shell, the UI. Runs all three whatever
                                          # each does, and marks a suite that skipped rather than passed
cmake -S engine -B engine/build && cmake --build engine/build -j
npm install && (cd ui && npm install && npm run build)
kuma library                              # install library/ into ~/.local/share/kuma once
kuma serve -p myvideo.kuma.json           # http://127.0.0.1:7455 (--bind / KUMA_BIND)
kuma mcp -p myvideo.kuma.json             # stdio MCP; starts the daemon in-process if none is running
npm run app                               # the desktop app: launcher, then a window per project
```

System deps (Debian/Ubuntu): `ffmpeg melt libmlt-dev libmlt++-dev libsdl2-dev libturbojpeg0-dev cmake xvfb`.
Subtitles run whisper.cpp as a subprocess: the rebuild's own `whisper.cpp/build/bin/whisper-cli`
(else `PATH`, or `KUMA_WHISPER_BIN`) and a ggml model in `~/.local/share/kuma/models` (the
rebuild downloads one; or `KUMA_WHISPER_MODEL`); tests stand a shell script in for it.
Headless machines: MLT's Qt module needs a display; melt and the engine are wrapped in
`xvfb-run -a` automatically when `DISPLAY` is unset. Set `HYPERFRAMES_BROWSER_PATH` to a
Chrome headless shell if hyperframes should not download one.

Testing by hand — looking at the editor headless, driving it with a real pointer, faking the
preview engine, measuring a change — is written down in `docs/manual-testing.md`. Read it
before checking a UI change by eye: it lists the traps (stale `ui/dist`, leftover
`localStorage`, `pkill -f` killing its own shell).

## Rules that keep the design honest

- Times in the model are seconds; `end` is exclusive; fps is rational. Seconds → frames
  happens once, in `Fps::frame`. Never do frame math anywhere else.
- The `bin` holds everything placeable (media, configured overlays, colours, sequences); a
  clip is a placement referencing one entry and never carries a source. What is shown lives
  on the entry, when it is shown lives on the clip. The format has an upgrade path now
  (`kuma_core::parse_project`, one step per version, applied to the JSON on the way in):
  changing it means bumping `PROJECT_VERSION` and adding a step there.
- There is no project-level `tracks`: a timeline is a `Sequence` bin entry and `master`
  names the one that renders, so a clip lives at `/bin/<seq>/source/tracks/<i>/clips/<j>`.
  Track and clip ids are unique across the whole project. A clip's `group` is an editing
  relation and nothing else — clips sharing one are selected and moved together, the
  members are whichever clips carry the id, they must be on one sequence, and the compiler
  never reads it. A sequence compiles to one
  tractor however often it is placed; each placement is a wrapper playlist carrying its own
  cut and filters, and a nested sequence's background is transparent. The daemon holds the
  open sequence as session state — never in the document.
- The centre of the editor is tabbed: one tab per **open sequence**, and one per overlay
  entry open in the **workbench** (`ui/src/panels/workbench.ts`). Sequence tabs are a list
  of timelines one click away, never several at once — the daemon holds which one is open
  and the engine holds its compiled timeline, so clicking a tab is a round trip and the
  last tab cannot be closed. The workbench edits an entry,
  never a placement — it creates no clip, moves no playhead, and its length and frame are
  view state, not document values. Vars stay in the inspector: an entry has one editor for
  what it is, and it is not this panel. An agent sees the same thing through `sample_frames`
  with `overlay`: a synthetic one-clip timeline, compiled and never stored, so looking at
  an overlay is never an edit.
- **Render means the master** unless a sequence is named: `render` and `sample_frames`
  default to it on every surface, and no session state leaks into either. The editor's
  render dialog is the one place a person can choose otherwise, with the master
  preselected whatever is open.
- Renders are **queued**: one encodes at a time, first come first served
  (`jobs::in_render_lane`), the rest wait `queued`, and any of them can be cancelled (`POST /api/jobs/{id}/cancel`, MCP
  `cancel_job`). A cancel kills melt and removes the partial file. A render's sequence and
  output are settled when it is asked for, not when its turn comes. A job's `state` is
  what clients read; never infer one from `message`.
- Overlays are live DOM on the sequence you have open and **baked** in the sequences it
  nests, which is what puts them in the preview at all; the two sets are disjoint, so none
  is drawn twice. Preview bakes are best effort — an overlay that will not bake costs that
  overlay, never the preview — and run behind the compile (`preview-bake`), so opening a
  sequence never waits on hyperframes.
- An **overlay** is a folder the project owns (`overlays/<name>/`: `overlay.json` +
  `index.html`) and a bin entry names it (`"overlay": "overlays/<name>"`), always relative
  and inside the project. A **blueprint** is the same shape in the user's library,
  reusable across projects, and is never read through an entry: using one **copies** it
  into the project (`overlays.rs`), so editing a blueprint cannot re-style a finished
  video. An overlay folder reaches the bin only by being imported — writing one does not
  add it, and one outside the project is copied in first — and entries may share a folder; "make unique" copies it for one of them. Undoing
  an import leaves its folder on disk: nothing deletes a folder, it may hold someone's work.
- Clients ask for a preview reload on **every** edit and never decide whether one is
  needed: `preview::reload` compiles, compares the document against what the engine holds,
  and only then reloads it. Do not reintroduce a client-side model of what the compiler
  depends on — that is what `engineKey` was, and it drifted twice. This makes the compiler
  deterministic a requirement, not a nicety (`compile.rs` tests it).
- Every MLT detail (inclusive in/out, `<blank length>`, background track, `qtblend`/`mix`
  transitions, A/B lanes for dissolves) stays in `crates/kuma-mlt/src/compile.rs`.
  `tests/fixtures/reference.mlt` is a golden file: regenerate with
  `UPDATE_GOLDEN=1 cargo test -p kuma-mlt` and read the diff. Renders build several frames
  at once (`real_time`, `melt.rs`), and not every MLT service is thread-safe: a new effect
  goes into the reference project, where `parallel_frames_render_what_serial_frames_do`
  checks it renders the same in parallel.
- A **design system** is a document, not a runtime layer: `kuma design-system <id>` copies
  one into `<project>/design-system/` and an agent reads it there. Nothing injects it, so
  applying it is an ordinary edit to an overlay folder or to an entry's vars — do not add a
  cascade that would re-style finished videos behind the editor's back.
- An overlay is **checked, not discovered to be broken**: `Baker::lint` reads the staged
  document (contract injected, `_vendor/` in place) and adds hyperframes' own linter, and
  `kuma new-overlay` writes a skeleton that passes it. A test lints every shipped
  blueprint and the scaffold, so the starter library cannot drift from the contract it
  documents. New contract rule → a rule in `lint.rs`, or nothing enforces it.
- `_vendor/` is **append-only in behaviour**. Every project's overlays read it from the
  user's library, and it is hashed into every bake key, so a helper that starts returning
  something else re-draws finished videos the next time they bake. Add a new helper and
  keep the old one (`kumaCanvas.rand` beside `hash`); pin what helpers return in a test.
- A `font` var is a family name, never a file: kuma reads family, weight and italic from
  the font files (`kuma-overlay/src/fonts.rs`), writes the `@font-face` rules into the
  contract and stages the files; `Baker::fonts` is the one place fonts are resolved, for
  the bake, the lint and the live preview alike. The bake key hashes the font bytes. An overlay says `font-family:
  var(--var-<name>), <fallback>` and nothing more. Adding a font is a file, not an edit.
- The bin's **New** menu is manifests saying `create`, not a list in the UI: a blueprint
  offers itself there, and creating one is the same copy as adding any blueprint — one
  folder per entry, so no two titles share markup.
- Overlay bakes run hyperframes with **one worker**; multi-worker capture was measured
  to be non-deterministic on GSAP-driven transforms. Separate one-worker processes side by
  side were measured byte-identical, so clips bake in parallel: `bake_clips` runs a pool,
  and `Baker::concurrency` (`KUMA_BAKE_JOBS`) caps the processes across every caller.
- Subtitles are not a model concept: a transcript is the `cues` var of a `subtitles`
  overlay entry, in source seconds, edited and baked like any other var. Portrait is not
  one either: it is `width`/`height`, and nothing else in the model may depend on which
  way round they are.
- `.kuma/` is pruned only when asked (`cache.rs`), keeping the bakes `jobs::bake_request`
  yields — build a clip's `BakeRequest` nowhere else. Readers of bakes or samples hold
  `cache_users` shared; writers name scratch `.tmp-…` (`is_in_flight`) until renamed.
- Alpha intermediates are PNG sequences (straight alpha). Assert `pix_fmt` on any encode.
- All state lives in the daemon. The UI and MCP are thin clients; every mutation is a
  revision-checked JSON Patch that is validated atomically and undoable.
- One project, one daemon. The desktop shell starts one per project it opens and kills it
  when the window closes, but **attaches** to a daemon already serving that file rather
  than starting a rival writer — that one is usually a `kuma mcp`. The editor page is the
  same page a browser gets: everything the shell adds is behind `desktop()` in
  `ui/src/desktop.ts` and absent in a tab. Do not make the UI require the shell — and
  note the converse, which is easy to miss because it fails silently: Electron does not
  implement `prompt`, `alert` or `confirm`, so UI code that calls them works in a browser
  and throws in the app. Ask for text with `askText` in `ui/src/modal.ts`.
- **A panel is one module in any container** (`ui/src/panelhost.ts`): a tab in power's dock
  (`dock.ts`: splits and tab groups round the frame, rearranged by dragging a tab once the rail's
  padlock lets panels move — locked by default), floating,
  or a window of its own (`?panel=<id>`), and the panel never knows which. The frame is the
  dock's one leaf that is not a panel; it is never re-parented except by a drop that moves
  it (that reloads its overlay iframes), and den lifts it with CSS rather than moving it.
  The dock, summoned panels and where they float are view state in `localStorage`; a panel's
  window is the same page, kept in step by `viewsync.ts` (view state only — the document
  comes from the daemon in every window). The letters `B I T A H M R` and `P` are commands,
  so ⌘K, the rail, den's keys and the shell's menus all reach a panel the same way.
- **What the agent did is the daemon's, never a client's model**: tool calls reach the UI as
  `/api/activity` — a call that starts a job is that job's record, opened and closed by the
  daemon with the job; every job says who asked (`asked_by`, read off its request's `origin`
  as an edit's author is), and only an agent's opens a record; any other call the MCP server
  reports once it has ended, and a report may never slow or fail a tool —
  steps and refused patches as `/api/history`, markers and their answers as `/api/markers`.
  All session state: none of it is in the project file, and a marker's done-ness never edits
  it. Who left a marker is in the document (`origin`), and the daemon stamps it as the edit
  that added it lands — never a client — so the agent's own markers are not read back to it
  as requests.
- Do not add a dependency on `mlt-sys`, `ffmpeg-next` or `wgpu` without a reason written
  in `docs/manual.md`; the PoC deliberately uses `melt`/`ffmpeg` as subprocesses.
