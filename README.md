# kuma-editor

A hybrid video editor for people who animate in HTML and edit in a real NLE.

- **Video half:** [MLT](https://www.mltframework.org/) — the engine under Kdenlive and
  Shotcut. Cuts, transforms, crops, speed, dissolves/wipes, colour, blur, audio mixing,
  hundreds of filters, and an export that Kdenlive can open (`.mlt` *is* the Kdenlive
  format).
- **Overlay half:** HTML/CSS/TS templates (labels, tags, callouts, animated explanations)
  rendered as **live DOM** while you scrub, and **baked to alpha video** by
  [hyperframes](https://github.com/heygen-com/hyperframes) at export — any length, any
  resolution, supersampled, deterministic.
- **Agent half:** a JSON project file with a schema, and an MCP server so Claude Code can
  read the timeline, edit it with revision-checked patches, author overlay templates, and
  **look at the actual composited frames** (`sample_frames`).

Status: proof of concept. Linux only. See "What works" below before relying on anything.

## Why

Authoring animations as HTML is the right medium, but capturing them in a browser tab caps
you at ~20 s, samples at screen resolution and can't emit alpha; and every iteration means
a manual export/import round trip into Kdenlive. kuma removes the seam: one timeline, one
project file, one preview, and the overlay is never "exported" by hand.

## Architecture

```
                      ┌──────────────── kuma daemon (Rust, axum) ────────────────┐
  browser UI  ◄─WS/HTTP─►  project state · revisions · undo · jobs · templates    │
  (TS, Vite)          │       │                │                    │             │
                      │  kuma-mlt          kuma-overlay        preview engine     │
  Claude Code ◄─stdio─►  JSON → MLT XML    hyperframes bake     (C++ libmlt++)    │
  (MCP, rmcp)         │       │            PNG seq w/ alpha      JPEG frames       │
                      └───────┼──────────────────┼────────────────────┼───────────┘
                           melt (render)     Chrome headless      SDL2 audio clock
```

Same HTML, two consumers:

```
overlay.html ──seek(t)──┬── preview: live iframe in the editor (GPU, zero latency)
                        └── export:  hyperframes → RGBA PNG sequence → MLT producer
```

Details and the reasoning behind each choice are in `docs/plan.md`.

## Quick start

System packages (Debian/Ubuntu):

```
sudo apt install ffmpeg melt libmlt-dev libmlt++-dev libsdl2-dev libturbojpeg0-dev cmake xvfb
```

Build:

```
npm install                                  # hyperframes + gsap
cargo build --release
cmake -S engine -B engine/build && cmake --build engine/build -j
(cd ui && npm install && npm run build)
```

`cargo build --release` produces the `kuma` binary at `target/release/kuma` (it is the
`crates/kuma-cli` crate). Either run it from there or install it on your `PATH`:

```
cargo install --path crates/kuma-cli
```

`kuma` needs the checkout for `overlays/`, `node_modules/hyperframes` and
`engine/build/kuma-preview`. It finds it by walking up from the current directory or from
the executable's location, so from `target/release/` nothing is needed; if you installed
it into `~/.cargo/bin`, tell it where the repository is once:

```
export KUMA_REPO=/path/to/kuma-editor        # or pass --repo <path> on each call
```

A project is a directory with `<name>.kuma.json` and your media:

```
mkdir myvideo && cd myvideo
kuma init --project myvideo.kuma.json --fps 30          # or 29.97, 30000/1001, --width 1080 --height 1920
kuma add-asset media/screen.mp4
kuma serve                                               # open http://127.0.0.1:7455
```

Drag templates and assets onto the timeline, scrub, edit vars in the inspector, press
**Render**. Output lands in `renders/`.

### Claude Code

`kuma init` writes a **project-scoped** `.mcp.json` next to the project file, so running
`claude` in that directory offers the kuma server (Claude Code asks once to approve it).
Nothing is installed globally. To (re)generate it for an existing project, or to see what
it contains:

```
kuma mcp-config            # writes ./.mcp.json (merges into an existing one)
kuma mcp-config --print
```

The equivalent by hand, for any of Claude Code's scopes:

```
claude mcp add --scope project kuma -- kuma mcp --project myvideo.kuma.json   # .mcp.json, shareable
claude mcp add --scope local   kuma -- kuma mcp --project myvideo.kuma.json   # this directory, you only
claude mcp add --scope user    kuma -- kuma mcp --project /abs/path.kuma.json  # everywhere
```

`kuma mcp` starts the daemon in-process if none is running on the port, so the browser UI
and the agent always share one state. Tools: `get_project`, `get_schema`, `apply_edits`,
`set_project`, `undo`, `redo`, `validate_project`, `list_templates`, `list_presets`,
`add_asset`, `probe_asset`, **`sample_frames`**, `bake_overlay`, `render`, `job_status`,
`health`. The server's instructions and `overlays/CLAUDE.md` tell the agent how to author
overlays.

### CLI

```
kuma render --preset youtube-1080p         # bakes overlays, compiles, renders with melt
kuma sample 12.0 14.0 --count 4 --rect 400,300,800,400
kuma bake                                  # bake all overlay clips (cached by content)
kuma compile > timeline.mlt                # inspect the MLT graph (or open it in Kdenlive)
kuma validate | schema | presets | templates | thumbs
```

## The project file

```json
{
  "fps": { "num": 30000, "den": 1001 }, "width": 1920, "height": 1080,
  "assets": { "screen": { "path": "media/screen.mp4" } },
  "tracks": [
    { "id": "v1", "kind": "video", "clips": [
      { "id": "c1", "start": 0, "end": 12.5, "src": { "asset": "screen", "in": 3.2 },
        "effects": [
          { "type": "crop", "left": 0.05, "right": 0.05 },
          { "type": "transform", "keyframes": [ { "t": 0, "scale": 1 }, { "t": 3, "scale": 1.4, "x": -120 } ] } ] } ] },
    { "id": "o1", "kind": "overlay", "clips": [
      { "id": "o1c1", "start": 2, "end": 8, "src": { "overlay": "location-tag", "vars": { "label": "Paris" } },
        "speed": 0.5, "effects": [ { "type": "blend", "mode": "screen" } ] } ] }
  ]
}
```

Seconds everywhere, `end` exclusive, fps rational, tracks bottom-up. Overlay clips take
the same effects as video clips. The full schema: `kuma schema`.

## Overlay templates

`overlays/<id>/index.html` + `template.json`. The manifest's typed vars become the
properties panel, so templates are editable without an agent; the agent can author new
ones because they are plain folders. Starter set: `location-tag`, `lower-third`,
`callout-arrow`, `label-pin`, `section-title`, `countdown`, `watermark` (static).
Contract: `overlays/CLAUDE.md`.

Effect presets live in `presets/effects/*.json` (Ken Burns, zoom to region, fade, vignette,
picture-in-picture, dim) and expand to ordinary effects.

## What works (verified in this repo's tests and smoke runs)

- 60 s 1080p30 overlay bake to ProRes 4444 / PNG sequence with exact straight alpha;
  4K supersampling; byte-identical repeat bakes with one worker.
- Compiler: dissolves (A/B lanes + luma), wipes (generated PGM lumas), timewarp speed,
  crop, keyframed transform, fade, blend modes (discrete keyframes on `qtblend`), blur,
  colour, PiP stills, overlay PNG sequences; golden-file + DTD + real `melt` render tests.
- Daemon + MCP end to end: patch/undo/409 on stale revisions, `sample_frames` returning
  image blocks, background render jobs.
- Preview engine: seek/play at ~23 fps for a 960×540 composite on 4 CPU cores, audio as
  the clock; browser UI showing engine frames under live overlay iframes at the same
  playhead.

## Troubleshooting

- **"Preview engine unavailable: …"** — the toast carries the daemon's reason; `GET /api/health`
  shows `preview_engine_path` and `preview_error`. The engine is a separate build step:
  `cmake -S engine -B engine/build && cmake --build engine/build -j` (needs `libmlt++-dev
  libsdl2-dev libturbojpeg0-dev`). Without it the preview falls back to single-frame
  snapshots (scrubbing works, playback does not).
- **Preview looks soft** — the engine defaults to half resolution for speed; pick
  `preview ¾` or `preview full` in the transport bar. Snapshots are requested at the
  stage's device-pixel width.
- **No display** (SSH, CI) — MLT's Qt module needs X11; install `xvfb` and kuma wraps
  `melt`/the engine in `xvfb-run` automatically.
- **Bakes fail with "timelines did not become ready"** — a template without a GSAP timeline
  needs `data-no-timeline` on its composition root (see `overlays/CLAUDE.md`).

## Known limitations

- Headless MLT needs Xvfb (Qt module). The daemon and CLI wrap `melt`/the engine
  automatically when `DISPLAY` is unset.
- The live-DOM preview approximates MLT-side effects on overlays with CSS; bake or
  `sample_frames` to see the exact result. Blend modes on overlays are exact only in export.
- Audio in dissolves is summed (not crossfaded) on the main tractor; the A/B lane
  crossfade is applied within the transition range only.
- No proxies/preview chunk cache yet — heavy timelines will scrub below real time.
- `.mlt` output opens in Kdenlive as a clip/timeline import; Kdenlive's own project
  scaffolding (`main_bin`, `kdenlive:` properties) is not emitted.
- Overlay SFX (`sfx` in manifests) are parsed but not yet placed on an audio track.

## Licence

MIT. Runtime dependencies: MLT (LGPL, dynamically linked in `engine/`), ffmpeg/melt as
subprocesses, hyperframes (Apache-2.0), GSAP (its standard licence).
