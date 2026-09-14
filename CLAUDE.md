# kuma-editor — guidance for agents working in this repository

Hybrid video editor: the video half is MLT (the engine under Kdenlive/Shotcut), the
overlay half is HTML/CSS/TS baked with alpha by hyperframes, both driven from one JSON
project file and exposed to agents over MCP. Read `README.md` for the architecture and
`overlays/CLAUDE.md` before authoring an overlay template.

## Layout

```
crates/kuma-core      project model (seconds, exclusive ends, rational fps), schema, validation, JSON Patch edits
crates/kuma-mlt       JSON → MLT XML compiler (the only place MLT's rules live), melt driver, presets
crates/kuma-overlay   hyperframes bake driver, content-hash cache, ffprobe helpers
crates/kuma-daemon    HTTP/WS API, undo, jobs, overlay live-preview routes, preview engine transport
crates/kuma-mcp       rmcp stdio MCP server proxying the daemon
crates/kuma-cli       `kuma` binary: serve | mcp | render | bake | sample | …
engine/               C++ libmlt++ preview engine (JPEG over stdout, SDL2 audio clock)
ui/                   Vite + TypeScript editor (no framework)
overlays/             overlay templates (folder = template) + `_vendor/gsap.min.js`
presets/effects/      effect presets (typed params + `$expr` substitution)
```

## Commands

```
cargo build && cargo test                 # Rust; render/bake tests skip without melt/ffmpeg/hyperframes
cmake -S engine -B engine/build && cmake --build engine/build -j
npm install && (cd ui && npm install && npm run build)
kuma serve -p myvideo.kuma.json           # http://127.0.0.1:7455
kuma mcp -p myvideo.kuma.json             # stdio MCP; starts the daemon in-process if none is running
```

System deps (Debian/Ubuntu): `ffmpeg melt libmlt-dev libmlt++-dev libsdl2-dev libturbojpeg0-dev cmake xvfb`.
Headless machines: MLT's Qt module needs a display; melt and the engine are wrapped in
`xvfb-run -a` automatically when `DISPLAY` is unset. Set `HYPERFRAMES_BROWSER_PATH` to a
Chrome headless shell if hyperframes should not download one.

## Rules that keep the design honest

- Times in the model are seconds; `end` is exclusive; fps is rational. Seconds → frames
  happens once, in `Fps::frame`. Never do frame math anywhere else.
- Every MLT detail (inclusive in/out, `<blank length>`, background track, `qtblend`/`mix`
  transitions, A/B lanes for dissolves) stays in `crates/kuma-mlt/src/compile.rs`.
  `tests/fixtures/reference.mlt` is a golden file: regenerate with
  `UPDATE_GOLDEN=1 cargo test -p kuma-mlt` and read the diff.
- Overlay bakes run hyperframes with **one worker**; multi-worker capture was measured
  to be non-deterministic on GSAP-driven transforms.
- Alpha intermediates are PNG sequences (straight alpha). Assert `pix_fmt` on any encode.
- All state lives in the daemon. The UI and MCP are thin clients; every mutation is a
  revision-checked JSON Patch that is validated atomically and undoable.
- Do not add a dependency on `mlt-sys`, `ffmpeg-next` or `wgpu` without a reason written
  in the README; the PoC deliberately uses `melt`/`ffmpeg` as subprocesses.
