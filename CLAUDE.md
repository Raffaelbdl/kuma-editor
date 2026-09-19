# kuma-editor — guidance for agents working in this repository

Hybrid video editor: the video half is MLT (the engine under Kdenlive/Shotcut), the
overlay half is HTML/CSS/TS baked with alpha by hyperframes, both driven from one JSON
project file and exposed to agents over MCP. Read `README.md` for the architecture and
`library/overlays/CLAUDE.md` before authoring an overlay template.

**Picking this up? Read `docs/decisions.md` first.** It records why the model looks the
way it does, including what was tried and rejected, and what is still open.

## Layout

```
crates/kuma-core      project model (seconds, exclusive ends, rational fps), schema, validation, JSON Patch edits
crates/kuma-mlt       JSON → MLT XML compiler (the only place MLT's rules live), melt driver, presets
crates/kuma-overlay   hyperframes bake driver, content-hash cache, ffprobe helpers
crates/kuma-daemon    HTTP/WS API, undo, jobs, overlay live-preview routes, preview engine transport
crates/kuma-mcp       rmcp stdio MCP server proxying the daemon
crates/kuma-cli       `kuma` binary: serve | mcp | render | bake | sample | lint | design-system | library | …
engine/               C++ libmlt++ preview engine (JPEG over stdout, SDL2-paced audio)
ui/                   Vite + TypeScript editor (no framework)
app/                  Electron shell: project launcher, windows, daemon lifecycle
library/overlays/     starter overlay templates (folder = template) + `_vendor/{gsap.min.js,kuma.js,kuma.css}`
library/presets/      starter effect presets (typed params + `$expr` substitution)
library/design-systems/ design systems (`DESIGN.md` + `tokens.css`) copied into a project; none ship yet
```

Templates and presets are user assets, not code: at runtime kuma reads
`$XDG_DATA_HOME/kuma` (default `~/.local/share/kuma`), every directory in `KUMA_LIBRARY`,
then the project's own `overlays/` and `presets/`; later wins. `library/` is only the
starter set that `kuma library` installs. Tests point `KUMA_LIBRARY` (or the baker) at it.

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
  Render button is the one place a person can choose otherwise, and only once the open
  sequence differs from the master.
- Overlays are live DOM on the sequence you have open and **baked** in the sequences it
  nests, which is what puts them in the preview at all; the two sets are disjoint, so none
  is drawn twice. Preview bakes are best effort — a template that will not bake costs that
  overlay, never the preview.
- Clients ask for a preview reload on **every** edit and never decide whether one is
  needed: `preview::reload` compiles, compares the document against what the engine holds,
  and only then reloads it. Do not reintroduce a client-side model of what the compiler
  depends on — that is what `engineKey` was, and it drifted twice. This makes the compiler
  deterministic a requirement, not a nicety (`compile.rs` tests it).
- Every MLT detail (inclusive in/out, `<blank length>`, background track, `qtblend`/`mix`
  transitions, A/B lanes for dissolves) stays in `crates/kuma-mlt/src/compile.rs`.
  `tests/fixtures/reference.mlt` is a golden file: regenerate with
  `UPDATE_GOLDEN=1 cargo test -p kuma-mlt` and read the diff.
- A **design system** is a document, not a runtime layer: `kuma design-system <id>` copies
  one into `<project>/design-system/` and an agent reads it there. Nothing injects it, so
  applying it is an ordinary edit to a template or to an entry's vars — do not add a
  cascade that would re-style finished videos behind the editor's back.
- A template is **checked, not discovered to be broken**: `Baker::lint` reads the staged
  document (contract injected, `_vendor/` in place) and adds hyperframes' own linter, and
  `kuma new-template` writes a skeleton that passes it. A test lints every shipped
  template and the scaffold, so the starter library cannot drift from the contract it
  documents. New contract rule → a rule in `lint.rs`, or nothing enforces it.
- Overlay bakes run hyperframes with **one worker**; multi-worker capture was measured
  to be non-deterministic on GSAP-driven transforms.
- Subtitles are not a model concept: a transcript is the `cues` var of a `subtitles`
  overlay entry, in source seconds, edited and baked like any other var. Portrait is not
  one either: it is `width`/`height`, and nothing else in the model may depend on which
  way round they are.
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
- Do not add a dependency on `mlt-sys`, `ffmpeg-next` or `wgpu` without a reason written
  in the README; the PoC deliberately uses `melt`/`ffmpeg` as subprocesses.
