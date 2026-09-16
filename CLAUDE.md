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
crates/kuma-cli       `kuma` binary: serve | mcp | render | bake | sample | library | …
engine/               C++ libmlt++ preview engine (JPEG over stdout, SDL2-paced audio)
ui/                   Vite + TypeScript editor (no framework)
app/                  Electron shell: project launcher, windows, daemon lifecycle
library/overlays/     starter overlay templates (folder = template) + `_vendor/{gsap.min.js,kuma.js,kuma.css}`
library/presets/      starter effect presets (typed params + `$expr` substitution)
```

Templates and presets are user assets, not code: at runtime kuma reads
`$XDG_DATA_HOME/kuma` (default `~/.local/share/kuma`), every directory in `KUMA_LIBRARY`,
then the project's own `overlays/` and `presets/`; later wins. `library/` is only the
starter set that `kuma library` installs. Tests point `KUMA_LIBRARY` (or the baker) at it.

## Commands

```
npm run rebuild                           # binary + ui/dist + app/dist + engine, then `kuma doctor`
cargo build && cargo test                 # Rust; render/bake tests skip without melt/ffmpeg/hyperframes
cmake -S engine -B engine/build && cmake --build engine/build -j
npm install && (cd ui && npm install && npm run build)
kuma library                              # install library/ into ~/.local/share/kuma once
kuma serve -p myvideo.kuma.json           # http://127.0.0.1:7455 (--bind / KUMA_BIND)
kuma mcp -p myvideo.kuma.json             # stdio MCP; starts the daemon in-process if none is running
npm run app                               # the desktop app: launcher, then a window per project
```

System deps (Debian/Ubuntu): `ffmpeg melt libmlt-dev libmlt++-dev libsdl2-dev libturbojpeg0-dev cmake xvfb`.
Headless machines: MLT's Qt module needs a display; melt and the engine are wrapped in
`xvfb-run -a` automatically when `DISPLAY` is unset. Set `HYPERFRAMES_BROWSER_PATH` to a
Chrome headless shell if hyperframes should not download one.

## Rules that keep the design honest

- Times in the model are seconds; `end` is exclusive; fps is rational. Seconds → frames
  happens once, in `Fps::frame`. Never do frame math anywhere else.
- The `bin` holds everything placeable (media, configured overlays, colours, sequences); a
  clip is a placement referencing one entry and never carries a source. What is shown lives
  on the entry, when it is shown lives on the clip. Nothing has shipped yet, so the format
  has no upgrade path: changing it means bumping `PROJECT_VERSION` and writing one.
- There is no project-level `tracks`: a timeline is a `Sequence` bin entry and `master`
  names the one that renders, so a clip lives at `/bin/<seq>/source/tracks/<i>/clips/<j>`.
  Track and clip ids are unique across the whole project. A sequence compiles to one
  tractor however often it is placed; each placement is a wrapper playlist carrying its own
  cut and filters, and a nested sequence's background is transparent. The daemon holds the
  open sequence as session state — never in the document.
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
- Overlay bakes run hyperframes with **one worker**; multi-worker capture was measured
  to be non-deterministic on GSAP-driven transforms.
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
