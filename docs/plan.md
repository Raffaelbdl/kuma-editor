# kuma-editor — hybrid video editor PoC

## Context

The problem is a workflow seam, not a missing feature. Animated overlays get authored as
HTML/CSS/TS (Claude Design), which is the right medium for tags, labels, callouts and
animated explanations — but browser-tab realtime capture caps out around 20s, samples at
screen resolution, and cannot emit alpha. Kdenlive handles the heavy video work well, but
every overlay iteration means a manual export/import round trip, and that round trip is
where the time goes.

kuma-editor closes the seam: one editor where the video half is a real native engine and
the overlay half stays HTML/CSS/TS, with both driven from a JSON project file that an
agent can edit and an MCP surface that lets Claude Code look at the actual pixels.

**Verdict: viable.** Both halves already exist as proven components; the PoC is integration
plus a compiler, not new research. The two load-bearing facts:

1. **MLT is reusable standalone.** It is the engine under Kdenlive *and* Shotcut; Shotcut
   renders by generating `.mlt` XML and shelling out to `melt`. `.kdenlive` **is** MLT XML.
   So emitting MLT XML gives us hundreds of filters (avfilter, frei0r, qtblend, OpenFX),
   correct audio mixing, and Kdenlive-openable projects — for free.
2. **Deterministic HTML→alpha-video baking is solved.** OpenDesign does it in production
   today: CDP frame capture against a *paused* timeline, `seek(t)` → await paint → RGBA
   screenshot → ffmpeg. Its runtime, `hyperframes` (Apache-2.0), additionally ships seek
   adapters that make GSAP, **CSS keyframes**, WAAPI, Lottie, Three.js and Anime.js
   frame-accurate.

Primary delivery target: **YouTube**, both landscape (16:9) and Shorts (9:16).

Non-goals for the PoC: full Kdenlive parity, Windows/macOS, GPU effects, collaborative
editing, packaging, automatic reframing of one timeline into two aspect ratios.

## Decisions taken

| Decision | Choice | Why |
| --- | --- | --- |
| Render backend | JSON → MLT XML → `melt` | Inherit MLT's filter library + Kdenlive interop. |
| Agent-facing format | Compact JSON, **not** MLT XML | See "Why not MLT XML directly". |
| Overlay runtime | `hyperframes` (Apache-2.0) | Already makes CSS/GSAP/Lottie seekable — the hard part. |
| Preview | C++ `libmlt++` sidecar + live DOM overlay | WYSIWYG; MLT has no Rust bindings. |
| UI | Rust daemon + browser UI (TS) | Real-time preview needs a UI; overlay layer is web tech anyway. |
| Platform | Linux | Matches existing Kdenlive workflow. |
| Agent scope | Full read/write via `apply_edits` | Schema-validated, atomic; git is the undo. |

### Why Rust core + a C++ sidecar (not all-C++)

`libmlt++` is needed in **one component only** — the preview frame-puller — because MLT has
no maintained Rust bindings (`mlt-sys` dead since 2018). Nothing else touches libmlt, and the
sidecar talks to the daemon over a **process boundary** (WebSocket/JSON), not FFI: no
bindgen, no unsafe glue, no ABI matching. Same shape as Kdenlive/Shotcut (app + `melt`).

Rust earns its place in everything else because (a) the plumbing libraries are best-in-class
for exactly this work — `rmcp` (official MCP SDK), `serde`+`schemars` (JSON model *and* its
schema from one type definition, which is the agent's contract), `axum`/`tokio`; and (b) the
bug class this project will die of is data-modelling bugs — off-by-one frames, a missing
transition, an unhandled effect variant — which exhaustive `match`, `Option`/`Result` and
no implicit conversions turn into compile errors. All-C++ + TypeScript MCP was a legitimate
alternative and was declined.

### Why not MLT XML directly

MLT XML is a good *render* target and a bad *authoring* format for an agent:

- `in`/`out` are **inclusive frame indices**; `<blank length=N>` is a **duration**. Mixing
  these up is the classic MLT bug and an LLM will hit it constantly.
- One Kdenlive track = **two** MLT tracks (same-track mixes are a transition between halves).
- Flat id-reference graph — producers declared up top, referenced from `<entry>`/`<track>` —
  so most edits touch several disjoint places.
- Effect parameters have **no schema**; service names and properties come from per-module
  YAML metadata, so an agent cannot validate its own edit.
- Compositing/audio mixing are not implicit: every overlaid video track needs an explicit
  `<transition mlt_service="qtblend">`, audio needs `mix`, plus a conventional black
  background track.
- MLT's own `xml` consumer does not reliably round-trip a full multitrack graph
  ([mlt#464](https://github.com/mltframework/mlt/issues/464)), so XML can't be the source of truth.

So: **JSON is the source of truth; one Rust compiler owns every MLT sharp edge.**

### Why the overlay is live DOM in preview and baked video in export

MLT has **no HTML producer**. The module that did — `webvfx` — is archived and marked
DEPRECATED; it died with QtWebKit in 2015. Embedding Chromium into MLT would repeat that
mistake. So the overlay must be an out-of-process bake step. Which frees preview to do
something better than baking: render the overlay HTML as a real DOM layer in the editor UI,
positioned over the video preview and driven by `seek(t)` from the same playhead.

Same HTML, same contract, two consumers:

```
overlay.html ──seek(t)──┬── preview: live DOM layer in editor UI (GPU, zero latency)
                        └── export:  hyperframes → ProRes 4444 → MLT producer
```

Known limitation, accept it: MLT-side effects applied *to* the overlay (blend modes, colour)
do not appear in the live-DOM preview. Approximate with CSS, and provide a "bake this range"
button for exactness.

## Architecture

```
kuma-editor/
  crates/
    kuma-core/      project model, JSON schema, validation, rational time math
    kuma-mlt/       JSON → MLT XML compiler; melt driver
    kuma-overlay/   hyperframes driver; bake cache keyed by content hash
    kuma-daemon/    axum HTTP/WS: project CRUD, preview stream, bake jobs, frame sampling
    kuma-mcp/       rmcp stdio server → daemon HTTP
    kuma-cli/       kuma serve | render | bake | mcp
  engine/           C++ kuma-preview: libmlt++ frame puller → JPEG/WS, SDL2 audio clock
  ui/               TS + Vite: timeline, preview canvas, live overlay DOM layer
  overlays/         HTML overlay compositions (hyperframes conventions)
  projects/         *.kuma.json
```

Pinned versions (all verified live on crates.io today): `rmcp` 3.3, `axum` 0.8,
`ffmpeg-sidecar` 2.5 (proxies/thumbnails/waveforms only), `serde`/`serde_json`.
MLT 7.22 via `libmlt-dev` (apt). **No `ffmpeg-next`, no `wgpu`, no `mlt-sys`** in the PoC —
`mlt-sys` has been abandoned since 2018.

### Project model (`crates/kuma-core`)

Times in **seconds (f64)**, ends **exclusive**, **fps rational** (29.97 = 30000/1001 — an
integer fps drifts one frame every ~30 s and MLT's profile is `frame_rate_num/den` anyway),
ids stable and human-readable. Tracks also carry `transitions[]` between adjacent clips
(`{ "type": "dissolve" | "wipe", "duration": 0.5, "luma": "..." }`), which the compiler
emits as MLT `luma` transitions with the required dual-track structure.

```json
{
  "version": 1,
  "fps": { "num": 30000, "den": 1001 }, "width": 1920, "height": 1080, "sample_rate": 48000,
  "assets": { "a1": { "path": "media/screen.mp4", "kind": "video" } },
  "tracks": [
    { "id": "v1", "kind": "video", "clips": [
      { "id": "c1", "asset": "a1", "start": 0.0, "end": 12.5, "source_in": 3.2,
        "effects": [
          { "type": "crop", "l": 0, "r": 0, "t": 0, "b": 0 },
          { "type": "transform", "keyframes": [
            { "t": 0.0, "scale": 1.0, "x": 0, "y": 0 },
            { "t": 3.0, "scale": 1.4, "x": -120, "y": -80 } ] } ] } ] },
    { "id": "o1", "kind": "overlay", "clips": [
      { "id": "o_c1", "overlay": "overlays/lower-third", "start": 2.0, "end": 8.0,
        "vars": { "title": "kuma" }, "speed": 0.5,
        "effects": [
          { "type": "transform", "x": 40, "y": -30, "scale": 0.9, "opacity": 0.85 },
          { "type": "blend", "mode": "screen" },
          { "type": "blur", "radius": 2 } ] } ] }
  ]
}
```

Overlay clips carry the **same `effects[]` as video clips** and are trimmed/moved/split the
same way. Once baked, an overlay is just another producer in the tractor, so every MLT
filter applies to it.

### Editing overlay clips after the agent makes them

| Edit | Where it's applied | Preview fidelity |
| --- | --- | --- |
| Position/scale/rotation/opacity/crop | MLT filter on the baked producer | Exact — CSS transform/opacity/clip-path on the DOM layer |
| **Speed / reverse** | **At bake time**: time-remap the seek loop (`seek(t × speed)`) — no frame duplication or interpolation, unlike `timewarp` | Exact — same remapped seek |
| Colour, blur | MLT filter (`avfilter.*`, `frei0r.*`) | Approximation via CSS `filter` |
| Blend mode | `qtblend` mode on the transition | Approximation via `mix-blend-mode` |
| Anything else | MLT filter | "Bake this range" button for exactness |
| The animation itself | Edit the HTML directly, ask the agent, or tweak `vars` (exposed in the UI inspector) | Exact — live DOM |

Native video clips keep using MLT `timewarp` for speed; only overlays get the bake-time remap.

Validated by a JSON schema shipped in-repo so the agent gets a machine-checkable contract.

### MLT compiler (`crates/kuma-mlt`)

The only place that knows about MLT's edges. Responsibilities:

- Convert seconds → frames exactly once: `in = round(start*fps)`, `out = round(end*fps) - 1`.
- Emit `LC_NUMERIC="C"`, `root=`, and all required `<profile>` attributes.
- Flat serialization form (what MLT's own `xml` consumer writes).
- Gaps as `<blank length="N">` (duration, not index).
- Black background track as track 0.
- One `<transition mlt_service="qtblend">` per overlaid video track; `mix` per audio pair.
- Keyframes → MLT animation strings (`0=1.0;90=1.4`).
- Speed on video clips → `timewarp:<factor>:<path>` producer with rescaled in/out; speed on
  overlay clips is **not** emitted — it is consumed by `kuma-overlay` at bake time.
- Effects on overlay clips → same filter emission as video clips, attached to the baked producer.
- Handle both `<producer>` and `<chain>` on any future read path.

Output `project.mlt` is both the render input and a Kdenlive-openable project — which
doubles as the correctness oracle.

### Overlay bake (`crates/kuma-overlay`)

Drives the `hyperframes` CLI per overlay clip, cached on
`hash(html + css + js + vars + fps + resolution + range)` so nothing re-bakes unnecessarily.

- Intermediate: **ProRes 4444 `yuva444p10le`** (`prores_ks -profile:v 4444`), with
  PNG sequence as the lossless debug fallback (MLT reads it via the `pixbuf` producer).
- **Do not** use VP9-alpha as an intermediate: it needs `-auto-alt-ref 0` or alpha is
  silently stripped, and alpha can depend on previous frames, which produces flicker at
  chunk boundaries under parallel rendering. VP9/WebM `yuva420p` is a *delivery* format only.
- Supersample via `deviceScaleFactor` 2–4, then downscale — this is the fix for sampling quality.
- Assert the negotiated `pix_fmt` after every encode. Encoders silently pick a non-alpha
  format when `-pix_fmt` is absent and drop alpha with no error.
- Internal convention, written down once: **straight (non-premultiplied) RGBA** end to end.
  Chrome's PNG alpha is already straight; do not premultiply anywhere.
- Also emit an optional **fill + matte pair** for dropping into Kdenlive by hand.

### Preview (`engine/` + `ui/`)

C++ sidecar, ~400 lines, isolated:

- Build the MLT tractor from the compiled XML; use a half-resolution profile (960×540).
- Pull frames, encode JPEG (q80 ≈ 40 KB), push over WebSocket — ~1 MB/s at 25fps on
  localhost. Shared-memory ring is a later optimization, not PoC work.
- **Audio is the clock**, not video: SDL2 output stream, consumed-sample count is the
  playhead, video frames selected to match and dropped when late.
- Commands in: `seek(t)`, `play`, `pause`, `reload`.

Borrow both of Kdenlive's performance mechanisms, because they matter more than compositor
speed: **proxy clips** (fixes decode cost) and a **chunked, disk-backed composited preview
cache invalidated on edit** (fixes composite/effect cost). Ship the chunk cache for ranges
that can't hit realtime.

### MCP surface (`crates/kuma-mcp`)

`rmcp` stdio server proxying to the daemon, so `claude mcp add kuma -- kuma mcp` works.
Tools:

- `get_project` / `apply_edits` — read and patch the JSON (schema-validated, rejected atomically).
- `sample_frames { t0, t1, count, rect?, resolution? }` — **the feature you asked for.**
  Renders those exact times through MLT, crops to `rect`, writes PNGs, returns paths.
  Claude Code reads PNGs natively, so region analysis is just: select area → sample → look.
- `bake_overlay { overlay, range }` — force a bake, return the file + `pix_fmt` assertion.
- `render { range?, preset }` — final render via `melt`.
- `list_assets`, `probe_asset` — duration, fps, resolution, audio layout.

### Agent authoring loop (what "hybrid + MCP" means in practice)

The agent never writes hyperframes internals; hyperframes is a dependency we call. The agent
writes **overlay HTML/CSS/TS following hyperframes' conventions**:

1. User selects a range: *"add an animated callout pointing at the terminal here."*
2. Claude calls `sample_frames` over the range and **looks at the footage** — where the
   terminal is, what's on screen, what moves.
3. Claude writes `overlays/callout-terminal/index.html`: root with `data-composition-id` /
   `data-duration` / `data-fps`; children with `data-start` / `data-duration` /
   `data-track-index`; animation in CSS keyframes or GSAP. Ordinary web code — the only
   constraint is seekability, which hyperframes' adapters provide for CSS/WAAPI/GSAP/Lottie.
4. Claude calls `apply_edits` to add an overlay clip referencing that folder with
   `start` / `end` / `vars`.
5. The UI's live DOM layer shows it immediately on scrub — no bake.
6. Export bakes it to ProRes 4444 and the compiler wires it into MLT.

Timing responsibility is split cleanly: **kuma's JSON decides *when* an overlay is on screen;
hyperframes' `data-*` attributes decide the choreography *inside* it.**

Deliverable: **`overlays/CLAUDE.md`** documenting this contract (composition root attributes,
clip attributes, the seekable-animation rule, `vars` injection, transparent-root
requirement, forbidden patterns like `setTimeout`-driven state) so Claude authors correct
compositions by default rather than being re-taught each session.

### Template library (overlays and effect presets, no agent required)

An overlay template is a folder with a manifest. The manifest is what makes agent-free
editing possible: the UI generates a properties panel from it, and `kuma-core` validates a
clip's `vars` against it.

```
overlays/
  location-tag/
    index.html          hyperframes composition; reads vars, transparent root
    template.json       manifest: name, default duration, typed vars
    thumb.png           auto-generated (bake one mid-frame), cached
presets/
  effects/
    zoom-to-rect.json   named effects[] bundle with exposed parameters
```

```json
{
  "id": "location-tag", "name": "Location tag", "duration_default": 4.0,
  "vars": {
    "label":  { "type": "string", "default": "Paris, France", "label": "Place" },
    "accent": { "type": "color",  "default": "#ff4f00" },
    "x":      { "type": "number", "default": 0.7, "min": 0, "max": 1, "label": "Anchor X" },
    "y":      { "type": "number", "default": 0.3, "min": 0, "max": 1, "label": "Anchor Y" },
    "style":  { "type": "enum",   "options": ["pin", "chip", "bracket"], "default": "pin" }
  }
}
```

Var types: `string`, `text`, `number` (min/max/step), `color`, `boolean`, `enum`, `asset`
(image/video path), `point` (x/y, draggable on the preview). Vars are injected three ways so
any authoring style works: CSS custom properties (`--var-accent`), `window.kuma.vars`, and
hyperframes' `data-composition-variables`.

- **Instances.** A timeline clip references a template id plus `vars` overrides; one template,
  many instances (five location tags, five sets of vars). Editing a var in the properties
  panel updates the live DOM layer instantly and invalidates only that instance's bake.
- **Effect presets.** A preset is a named `effects[]` bundle with exposed parameters — e.g.
  `zoom-to-rect { rect, duration, ease }` expands to keyframed crop + transform. Applied to
  any clip (video or overlay) from the same properties panel. Presets are plain JSON, so the
  agent can author them too.
- **Library panel** in the UI: grid of templates/presets with thumbnails, drag onto the
  timeline, "duplicate as new template" to fork one.
- **Agent authoring is additive**: because templates are ordinary folders, "make me a
  location tag variant with a compass" produces a new template that then lives in the library
  for agent-free reuse. `overlays/CLAUDE.md` documents manifest authoring as well.
- **Starter set** shipped in-repo: overlays `location-tag`, `lower-third`, `callout-arrow`,
  `label-pin`, `section-title`, `countdown`; presets `ken-burns`, `zoom-to-rect`,
  `fade-in-out`, `vignette`, `slide-wipe`.

### Desktop shell later (detachable windows, native dialogs)

Designed for, not built in the PoC. Two rules make the switch packaging work rather than a
rewrite:

1. **All UI-relevant state lives in the daemon** (playhead, selection, project, preview
   stream). The UI is a thin client. Consequence available immediately in browser mode:
   open `/#/preview` or `/#/timeline` in a second window and it is already a detached,
   in-sync panel — there is nothing to sync. A desktop shell is N native windows loading the
   same bundle at different routes, with the daemon as a sidecar.
2. **A `platform` interface** in `ui/` (`openFile`, `saveAs`, `revealInFolder`,
   `dropPaths`, global shortcuts) with a browser implementation (daemon-served file browser,
   drop → upload) now and a native one later.

Shell choice, when it comes: **Electron over Tauri on Linux.** Tauri's Linux webview is
WebKitGTK, while the overlay bake runs in Chromium — a preview that doesn't match the export
would undercut the whole point. Electron is Chromium and matches. Tauri only if the
WebKitGTK difference proves negligible for the overlays in use.

### Formats: inputs, intermediates, exports

Three distinct places, three different answers — do not conflate them:

| Stage | Format | Notes |
| --- | --- | --- |
| **Inputs** | Anything ffmpeg reads: MP4, MOV, MKV, WebM, image sequences | Proxies generated as light H.264 for scrubbing. |
| **Overlay intermediate** (bake → MLT) | Default **ProRes 4444** `yuva444p10le`; options `vp9-alpha` (WebM `yuva420p`, `-auto-alt-ref 0`) and `png-seq` | Needs alpha, cached locally, region-cropped, never shipped. Size is a non-issue. |
| **Final export** | **MP4/H.264** by default; WebM/VP9, MOV/H.265 as presets | `melt` avformat consumer + a preset table. |
| **Kdenlive handoff** | VP9-alpha WebM, or fill + matte H.264 MP4 pair | Kdenlive reads VP9 alpha more reliably than ProRes 4444. |

Export presets shipped: `youtube-1080p` (H.264 High, `yuv420p`, CRF 18 / ~12 Mbps, AAC
384 kbps, `+faststart`), `youtube-4k` (~40 Mbps), `youtube-shorts` (1080×1920, same codec
settings), `vp9-hq`, `prores-master`. YouTube re-encodes everything; the goal is a clean
high-bitrate H.264 upload, not a small file.

### Desktop and mobile (16:9 and 9:16)

- Project-level **format presets**: `1920×1080`, `3840×2160`, **`1080×1920` (Shorts)**;
  fps rational as above.
- **Templates are responsive.** They are CSS, so a lower-third *reflows* to 9:16 instead of
  being a fixed raster like a Kdenlive title. Template authoring rule: size and position
  relative to the composition (`vw`/`vh`/`%`, `point` vars for anchors), never absolute px
  for layout. `overlays/CLAUDE.md` states this; the starter templates demonstrate it.
- Preview shows **safe-area guides** for Shorts (top/bottom UI chrome).
- "**Duplicate as vertical**" helper: copies the project into a 9:16 format keeping clips,
  overlays and audio; you re-frame video clips with crop/transform (Ken Burns presets
  help). Overlays need no work. Automatic reframing is post-PoC.

### Gaps to design for from day one

- **Undo stack in the daemon.** Every mutation is a JSON patch; undo is the inverse patch.
  Agent edits are undoable entries labelled by origin ("Claude: added callout"). Git is
  history, not an undo button.
- **Revision-based concurrency.** The project carries a revision counter; `apply_edits`
  (and UI mutations) include `base_revision` and are rejected on mismatch, so the agent
  cannot clobber an edit made while it was thinking.
- **Fonts bundled per template.** `@font-face` with relative paths inside the template
  folder; no Google-Fonts links (network dependency + nondeterministic bake); bake awaits
  `document.fonts.ready`. Preview and bake then use identical fonts.
- **Bake only the region that matters.** Manifest `region` (or auto-probe one frame for the
  alpha bounding box); compiler positions the small bake at the offset. A lower-third is
  1920×200, not 1920×1080. **Static templates** (slides, title cards — common with
  generated content) bake to a single PNG placed as a still, not N identical frames.
- **Overlay SFX.** Manifest declares audio assets with offsets; kuma places them on an
  audio track. The bake is video-only.
- **Banding.** Overlays stay 10-bit (ProRes 4444); profile colorspace 709; final
  compositing is 8-bit in the PoC — known and accepted.
- **hyperframes bus factor.** Templates use only root attrs, `data-start`/`data-duration`
  and the seek contract; the bake driver is ours. Replacing the runtime is small if needed.
- **Post-PoC MCP tools the model must not preclude:** timeline-aligned transcript
  (Whisper) for "add a tag when I say *deploy*"; `sample_audio` for waveform/level queries.
- **Licensing is clean:** MLT is LGPL (dynamically linked in the sidecar), ffmpeg/melt used
  as subprocesses, hyperframes Apache-2.0. Nothing viral touches our code.
- **CI without a display:** SDL2 `SDL_AUDIODRIVER=dummy` for the sidecar; Chromium headless.

## Build order

Each step is independently verifiable; stop early if a spike fails.

1. **Spike A — overlay alpha (half a day, highest information).** Install `hyperframes`,
   bake a transparent CSS-keyframe + GSAP comp at 1080p/30 for 60s to `png-sequence` and
   `mov`. Assert: real alpha, 60s works, supersampling improves edges, two runs are
   byte-identical. *This validates the half of the thesis that your current workflow blocks on.*
2. **Spike B — MLT render.** `apt install melt libmlt-dev`. Hand-write a minimal `.mlt` with
   two video tracks + `qtblend` + the baked ProRes overlay; render with `melt`. Open it in
   Kdenlive to confirm interop.
3. **`kuma-core` + `kuma-mlt`.** Model, schema, compiler. Golden-file tests for JSON→XML,
   property test for the seconds↔frames conversion, `xmllint --dtdvalid` against
   `mlt-xml.dtd`.
4. **`kuma-overlay`.** Bake driver + content-hash cache + `pix_fmt` assertions.
5. **`kuma-cli` + `kuma-daemon`.** `kuma render` end to end first (no UI), then the HTTP API.
6. **`kuma-mcp`.** Wire up MCP; `sample_frames` before the GUI exists — it's usable immediately.
7. **`engine/kuma-preview`.** libmlt++ frame puller + SDL2 clock + WS transport.
8. **`ui/`.** Timeline, preview canvas, live overlay DOM layer, scrub.
9. **Template library.** Manifest schema + validation in `kuma-core`, properties panel
   generated from manifests, effect presets, thumbnail generation, library panel, starter set.

## Verification

- `cargo test` — golden MLT XML files, frame-conversion property tests, schema round-trip.
- `xmllint --dtdvalid src/modules/xml/mlt-xml.dtd project.mlt` in CI.
- Alpha assertion script: `ffprobe -show_entries stream=pix_fmt` equals `yuva444p10le`, plus
  sample a known-transparent pixel and assert `a == 0`.
- Determinism: bake twice, `cmp` the PNG frames byte for byte.
- Interop: open the emitted `.mlt` in Kdenlive (manual, local).
- End-to-end acceptance for the PoC: a 60s 1080p timeline with one screen-recording clip
  (crop + keyframed zoom), one HTML overlay with baked alpha, and audio — rendered by
  `melt`, previewed at 25fps with synced audio, and with Claude Code able to sample a
  selected region and describe it.

## Honest risks

- **`hyperframes` alpha flags are documented but unverified by me.** Two independent reads
  agree the CLI has `--format webm|mov|png-sequence` with transparency and RGBA frame output,
  but I have not run it. Spike A exists to settle this on day one; if alpha is missing, the
  fallback is our own CDP capture loop plus a reimplemented seek-adapter layer — a real cost,
  which is exactly why this spike goes first.
- **Preview through MLT has no Rust path.** `mlt-sys` is abandoned (2018) and MLT's SWIG
  bindings cannot properly consume its event system. Hence the C++ sidecar. It is small and
  isolated, but it does add C++ and `libmlt-dev` to the build.
- **Live-DOM preview diverges from baked output** for MLT-side effects on overlays. Mitigated
  by a bake-range button, not eliminated.
- **MLT's Movit/GLSL GPU path is optional at build time and commonly missing in distro
  packages.** Assume CPU `qtblend` compositing; this is why the proxy + chunk cache matter.
- **Long-GOP screen recordings make seeking expensive** — a seek can mean decoding hundreds
  of frames. Proxies are not optional for scrub feel.
- **ProRes 4444 alpha is sometimes lost on the Kdenlive side** (reported as solid black).
  PNG sequence is the bulletproof fallback; keep it wired.
