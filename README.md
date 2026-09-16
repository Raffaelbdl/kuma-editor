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
  desktop app (Electron) ── starts and stops one daemon per open project
        │  launcher → window
        ▼
                      ┌──────────────── kuma daemon (Rust, axum) ────────────────┐
  editor UI   ◄─WS/HTTP─►  project state · revisions · undo · jobs · templates    │
  (TS, Vite)          │       │                │                    │             │
                      │  kuma-mlt          kuma-overlay        preview engine     │
  Claude Code ◄─stdio─►  JSON → MLT XML    hyperframes bake     (C++ libmlt++)    │
  (MCP, rmcp)         │       │            PNG seq w/ alpha      JPEG frames       │
                      └───────┼──────────────────┼────────────────────┼───────────┘
                           melt (render)     Chrome headless      SDL2 audio clock
```

The same UI runs in the desktop app and in a plain browser against `kuma serve`; the app
adds a launcher, native menus and a file picker, and owns the daemon's lifetime.

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
npm run rebuild                              # everything, in order, then `kuma doctor`
```

Three of the artifacts kuma runs from are not in git — the `kuma` binary, `ui/dist` (which
the daemon serves) and `app/dist` (the desktop shell) — so **run this after every pull**.
A stale one fails far from its cause: an old binary writes projects the editor cannot
open, an old `ui/dist` dies on the shape of a new one. Step by step, if you prefer:

```
npm install                                  # hyperframes + gsap
cargo build --release
cmake -S engine -B engine/build && cmake --build engine/build -j
(cd ui && npm install && npm run build)
```

The preview engine is the only optional piece; `npm run rebuild` reports it and carries on
if its system libraries are missing.

`cargo build --release` produces the `kuma` binary at `target/release/kuma` (it is the
`crates/kuma-cli` crate). Either run it from there or install it on your `PATH`:

```
cargo install --path crates/kuma-cli
```

`kuma` needs the checkout for `node_modules/hyperframes`, `ui/dist` and
`engine/build/kuma-preview`. It finds it by walking up from the current directory or from
the executable's location, so from `target/release/` nothing is needed; if you installed
it into `~/.cargo/bin`, tell it where the repository is once:

```
export KUMA_REPO=/path/to/kuma-editor        # or pass --repo <path> on each call
```

Overlay templates and effect presets are **assets shared across projects**, not part of
the codebase: they live in a user library at `~/.local/share/kuma/{overlays,presets}`
(`$XDG_DATA_HOME/kuma`), plus any directories in `KUMA_LIBRARY` (a `:`-separated path
list) and a project's own `overlays/` and `presets/` folders. Later entries win on an id
clash. The repository ships a starter set under `library/`; install it once:

```
kuma library                                 # copies library/{overlays,presets} → ~/.local/share/kuma (--force to overwrite)
```

A project is a directory with `<name>.kuma.json` and your media:

```
mkdir myvideo && cd myvideo
kuma init --project myvideo.kuma.json --fps 30          # or 29.97, 30000/1001, --width 1080 --height 1920
kuma add-media media/screen.mp4
kuma serve                                               # open http://127.0.0.1:7455 (--bind / KUMA_BIND to change)
```

Drag bin entries onto the timeline, scrub, edit vars in the inspector, press **Render**.
Output lands in `renders/`.

### Desktop app

`kuma serve` plus a browser tab is still the whole editor. The desktop app is the same UI
in a window, with the things a tab cannot give you: a launcher, an application menu, a
native file picker, and a daemon that starts and stops with the window.

```
npm run app                                  # installs its own deps on first run, builds, starts
```

It opens a **launcher**: recent projects on the right, *New project* and *Open project…*
on the left. You can also drop a `.kuma.json` on the window, or pass one on the command
line. *New project* asks for a name, a location, a resolution and a frame rate, then runs
`kuma init` — so a project created here is identical to one created from the CLI, down to
the `.mcp.json` that lets Claude Code join it.

Opening a project starts a daemon for it and shows the editor. Closing the window stops
that daemon; quitting stops all of them. Each project gets its own daemon and its own
window, so several can be open at once. The first one takes `127.0.0.1:7455`, which is
where `kuma mcp` looks — so Claude Code attaches to the project you have open without any
configuration.

The one daemon the app does *not* stop is one that was already serving that project file
when you opened it. Two daemons writing one `.kuma.json` would each think they own it, and
the one already there is usually a `kuma mcp` with a Claude Code session behind it, so the
app shares it instead and leaves it running.

The shell needs to find the `kuma` binary and the checkout. It looks for `KUMA_BIN`, then
`target/release/kuma`, then `target/debug/kuma`, then `PATH`; and for the repository the
same way `kuma` itself does (`KUMA_REPO`, else the nearest ancestor with
`node_modules/hyperframes`). The launcher's footer says what it found and what is missing.

Everything the app adds to the editor page sits behind one check in `ui/src/desktop.ts`,
so the UI keeps working unchanged in an ordinary browser.

### Claude Code

`kuma init` writes a **project-scoped** `.mcp.json` next to the project file, so running
`claude` in that directory offers the kuma server (Claude Code asks once to approve it).
Nothing is installed globally. To (re)generate it for an existing project, or to see what
it contains:

```
kuma mcp-config            # writes .mcp.json next to the project file (merges into an existing one)
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
`undo`, `redo`, `validate_project`, `list_templates`, `template_guide`, `list_presets`,
`add_media`, `probe_media`, **`sample_frames`**, `bake_overlay`, `render`, `job_status`,
`health`. `template_guide` returns the overlay authoring contract
(`library/overlays/CLAUDE.md`) so the agent can write templates into the library or the
project.

### CLI

```
kuma render --preset youtube-1080p         # bakes overlays, compiles, renders with melt
kuma sample 12.0 14.0 --count 4 --rect 400,300,800,400
kuma bake                                  # bake all overlay clips (cached by content)
kuma compile > timeline.mlt                # inspect the MLT graph (or open it in Kdenlive)
kuma render --sequence intro               # render one nested sequence instead of the master
kuma validate | thumbs | library | doctor
```

## The project file

```json
{
  "fps": { "num": 30000, "den": 1001 }, "width": 1920, "height": 1080,
  "master": "main",
  "bin": {
    "screen": { "name": "Screen recording", "source": { "path": "media/screen.mp4", "kind": "video" } },
    "paris":  { "name": "Paris tag", "source": { "template": "location-tag", "vars": { "label": "Paris" } } },
    "navy":   { "name": "Navy card", "source": { "color": "#102030" } },

    "intro":  { "name": "Intro", "source": { "tracks": [
      { "id": "iv1", "kind": "video", "clips": [
        { "id": "i1", "start": 0, "end": 6, "bin": "screen", "in": 20 } ] } ] } },

    "main":   { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "clips": [
        { "id": "s1", "start": 0, "end": 6, "bin": "intro" },
        { "id": "c1", "start": 6, "end": 18.5, "bin": "screen", "in": 3.2,
          "effects": [
            { "type": "crop", "left": 0.05, "right": 0.05 },
            { "type": "transform", "keyframes": [ { "t": 0, "scale": 1 }, { "t": 3, "scale": 1.4, "x": -120 } ] } ] } ] },
      { "id": "o1", "kind": "overlay", "clips": [
        { "id": "o1c1", "start": 2, "end": 8, "bin": "paris",
          "speed": 0.5, "effects": [ { "type": "blend", "mode": "screen" } ] } ] }
    ] } }
  }
}
```

**The bin holds everything placeable** — media files, configured overlays, colours and
*sequences* — each with an id, a name and optional probed metadata. A clip is a
*placement*: it says which entry, when, how fast and with which effects, and never carries
a source of its own. So an overlay's text lives on its bin entry and every placement of it
follows; a second wording is a second entry. Overlay clips take the same effects as video
clips.

**A timeline is a bin entry too.** There is no project-level `tracks`: a sequence entry
holds the tracks, and `master` names the one that gets rendered. A sequence placed on
another sequence is a nested sequence — edit a part separately, place it in the final cut,
trim into it and put effects on this placement without touching the other. The editor
opens one sequence at a time (the picker in the topbar); the preview compiles whichever is
open, since the engine loads one timeline. Overlays on that sequence stay live DOM so edits
show as you type; overlays inside the sequences it nests are baked into the preview, which
is content you are not editing and is what the render will show. A sequence cannot contain itself, and cannot be
sped up — MLT's `timewarp` takes a resource path, not a tractor, so render the part out
and place the result as media.

Seconds everywhere, `end` exclusive, fps rational, tracks bottom-up, `in` meaningful for
video, audio and sequence entries. Track and clip ids are unique across the whole project,
not per sequence. The full schema: `GET /api/schema` or the `get_schema` tool.

## Overlay templates

A template is a folder `<library>/overlays/<id>/` with `index.html` + `template.json`. The
manifest's typed vars become the properties panel, so templates are editable without an
agent; the agent can author new ones because they are plain folders. Starter set
(`library/overlays/`, installed by `kuma library`): `location-tag`, `lower-third`,
`callout-arrow`, `label-pin`, `section-title`, `countdown`, `watermark` (static).
Contract: `library/overlays/CLAUDE.md`.

Effect presets are `<library>/presets/*.json` (Ken Burns, zoom to region, vignette,
picture-in-picture, dim) and expand to ordinary effects; `$expr` values may use the
project's `width`/`height`.

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

- **Just pulled? Run `npm run rebuild`.** The binary, `ui/dist` and `app/dist` are not in
  git, so a pull leaves them behind, and each fails in a way that does not name the build
  step: `kuma init` writing a project the editor refuses to open, or the editor window
  failing to start with something like `t.tracks is not iterable`. The desktop app warns
  about the first two and refuses to open a window against a stale `ui/dist`.
- **Start with `kuma doctor --project <file>`** — it reports the binary, repo root, tools,
  engine, display, whether a daemon answers on the port (and for which project), and the
  MCP config.
- **Claude Code says `kuma … Failed to connect ("Connection closed")`** — the `kuma mcp`
  process exited at startup. Run the exact command from `.mcp.json` by hand
  (`kuma mcp --project <file>`); the reason is printed on stderr. `kuma mcp` reuses a
  running `kuma serve` for the same project, starts a private daemon on a free port if the
  running one serves another project, and never routes loopback requests through
  `http_proxy`. Then `/mcp` → reconnect in Claude Code.
- **"Preview engine unavailable: …"** — the toast carries the daemon's reason; `GET /api/health`
  shows `preview_engine` and `preview_error`. The engine is a separate build step:
  `cmake -S engine -B engine/build && cmake --build engine/build -j` (needs `libmlt++-dev
  libsdl2-dev libturbojpeg0-dev`). Without it the preview falls back to single-frame
  snapshots (scrubbing works, playback does not).
- **Preview looks soft** — the engine defaults to half resolution for speed; pick
  `preview ¾` or `preview full` in the transport bar. Snapshots are requested at the
  stage's device-pixel width.
- **No display** (SSH, CI) — MLT's Qt module needs X11; install `xvfb` and kuma wraps
  `melt`/the engine in `xvfb-run` automatically.
- **A template is missing from the library panel** — `GET /api/health` lists
  `template_dirs` (where kuma looked) and `template_errors` (manifests that failed to load
  and why). No library at all: run `kuma library`.
- **Bakes fail with "timelines did not become ready"** — a template without a GSAP timeline
  needs `data-no-timeline` on its composition root (see `library/overlays/CLAUDE.md`).

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
- Overlays bake at full frame size; there is no per-template bake region yet.
- Transitions are refused on overlay clips: bakes carry no handle frames yet.
- Clip and track ids must match `[A-Za-z_][A-Za-z0-9_-]*` (they double as XML ids and URL segments).

## Licence

MIT. Runtime dependencies: MLT (LGPL, dynamically linked in `engine/`), ffmpeg/melt as
subprocesses, hyperframes (Apache-2.0), GSAP (its standard licence).
