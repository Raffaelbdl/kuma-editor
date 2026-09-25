# Authoring kuma overlays

An **overlay** is a project folder, `overlays/<name>/`, that a bin entry names
(`"overlay": "overlays/<name>"`). A **blueprint** is the same folder in the user's library
(`~/.local/share/kuma/blueprints/`, then each `KUMA_LIBRARY` dir's `blueprints/`); using one
copies it into the project, so the copy is yours to edit. A project's `overlays/` is never a
library. `health` lists `blueprint_dirs` and `blueprint_errors`.

```
overlays/<name>/        (a blueprint has the same shape)
  overlay.json    manifest: name, description, duration_default, static, vars, tags
  index.html      the composition (always this name)
  fonts/…         any fonts, referenced with relative @font-face paths
  thumb.png       generated for blueprints (`kuma thumbs`); never baked or copied
```

The folder name is the identity (the manifest has no `id`). A folder without both files
does not load. Entries may share a folder with different `vars`; editing it changes all of
them, so `make_overlay_unique` first to change one.

`_vendor/` holds shared files, referenced as `_vendor/<file>` from the overlay. An
overlay's own `_vendor/` wins, then the one next to it, then the shared one from the
highest-precedence library that has it (`<library>/blueprints/_vendor/`); the bake stages
and the daemon serves exactly the same folder. It is not copied into projects.

- `_vendor/kuma.js` — `kumaTemplate(defaults)` applies the vars, returns `{ vars, width,
  height, duration, text(id, value), timeline(), onFrame(fn) }` and works standalone.
- `_vendor/kuma.css` — the transparent reset and the `--u` unit (`1vh × --var-size`).
- `_vendor/kuma-motion.js` — entrances and exits by name, for an overlay whose motion is a
  var: `kumaMotion.enter(tl, el, vars.enter, { at, duration, text })` returns when it
  settles, `kumaMotion.exit(tl, el, vars.exit, { at, duration, after })`. The names at the
  top of the file are the enum options to list; `title` shows the whole pattern.
- `_vendor/kuma-canvas.js` — drawn overlays: a `<canvas>` painted as a function of time
  (see *Drawn overlays* below).
- `_vendor/rough.min.js` — Rough.js 4.6.6 (MIT), the hand-drawn look; use it through
  `kuma-canvas.js`.
- `_vendor/gsap.min.js`.

## Read the project's design system first

If the project directory has a `design-system/` folder, read `design-system/DESIGN.md`
before authoring or restyling anything, and take the colours, type, geometry and motion
from it rather than inventing them. `design-system/tokens.css` is the same values as a
`:root` block: copy it into the overlay's first `<style>` and reference `var(--ds-*)`.
That is the whole mechanism — nothing is injected at runtime, so what an overlay ends up
with is what you wrote into it, and an overlay stays readable on its own.

A project gets one with `kuma design-system <id>` (`kuma design-system` lists what is
installed; `kuma library` installs the starter set). A project without one is not a
mistake: use the overlay's own defaults and keep them consistent across the video.

## Start from the scaffold, and check your work

Do not write an overlay from an empty file. `kuma new-overlay <name>` (the `new_overlay`
tool) writes a working one into the project's `overlays/<name>/`: the root's timing
attributes, a registered paused timeline, typed vars of three kinds, `--u` sizing that
survives both orientations, and an exit anchored to the clip's real length. Edit that —
or `add_blueprint` one that is close.

`kuma lint overlays/<name>` (the `check_overlay` tool, **Check** in the overlay workbench)
reads the staged overlay — the injection applied, `_vendor/` in place — and reports what
would otherwise surface as a bake that times out after 45 seconds or a frame that is
quietly wrong: a missing timeline registration, a wall clock, a Google Fonts URL, a var the
markup reads that the manifest does not declare, a missing asset. It runs hyperframes' own
linter too when it is installed. It renders nothing and writes nothing, so run it after
every edit; a bake is the slow way to learn the same thing.

Writing a folder does not put it in the bin: `import_overlay` (`kuma import-overlay
overlays/<name>`) does, optionally with `vars`, once it checks clean.

## The contract (what kuma guarantees, what the overlay must do)

**kuma injects:**

- At the top of `<head>`: `window.kuma = { vars, fps: {num, den}, width, height,
  duration, speed, bake, clip }` — `vars` are the manifest defaults overlaid with the
  clip's overrides; `duration` is the clip's internal duration in seconds (clip length ×
  |speed|).
- At the end of `<head>` (so they win over the overlay's own `:root` fallbacks): CSS
  custom properties `--var-<name>` on `:root` for every scalar var, and
  `--var-<name>-x` / `--var-<name>-y` for `point` vars.
- The root `[data-composition-id]` element's `data-duration` (and `data-width`/`data-height`)
  are **rewritten in the markup** to the clip's real length before the renderer sees the
  file — hyperframes sizes the render from the attribute as written, before any script runs.
  The value in your markup is only the standalone default; do not try to set it from a
  script (it has no effect on the render). **Never hard-code the duration in logic; read
  `window.kuma.duration`.**
- Timed children are **mounted only for their own `data-start`/`data-duration`** — a child
  that keeps `data-duration="4"` disappears after 4 s even in a 30 s clip. kuma stretches
  every child whose end coincides with the root's default (`start + duration == root
  default`) to the clip's real end; a child that ends earlier keeps its timing. So: give
  "whole clip" children the same end as the root, and shorter ones their real length.

**The overlay must:**

1. Be transparent all the way to the root (`_vendor/kuma.css` does it). Any opaque
   wrapper becomes an opaque video.
2. Have one root: `<div id="root" data-composition-id="main" data-start="0" data-duration="4"
   data-width="1920" data-height="1080">`. Timed children carry `class="clip"`,
   `data-start`, `data-duration` (seconds, relative to the root).
3. Be a **pure function of time**. Animation must be seekable:
   - GSAP: one paused timeline, `const tl = timeline()` from `kumaTemplate` (it registers
     `window.__timelines["main"]`). Per-frame JS that GSAP does not tween (a canvas, a
     computed path) runs from `onFrame(t => …)`, never from `onUpdate`, which `tl.seek(t)`
     suppresses.
   - **No GSAP timeline at all** (static overlays, pure CSS animation): put `data-no-timeline`
     on the composition root, or the renderer waits 45 s for a timeline and fails the bake.
   - CSS `@keyframes`/transitions and WAAPI are fine — the renderer scrubs them.
   - Lottie, Anime.js, Three.js are supported through the hyperframes adapters.
   - **Forbidden:** `setTimeout`/`setInterval`/`requestAnimationFrame`-driven state,
     `Date.now()`, `Math.random()` without a fixed seed, autoplaying `<video>`. Anything
     wall-clock driven renders wrong or frozen.
4. Start with `const { vars, duration, text, timeline } = kumaTemplate({ …defaults })`,
   with the manifest defaults repeated so the page works standalone; then apply `vars`
   to the DOM (`text("id", vars.label)` hides the element when the value is empty).
5. Anchor the out-animation to the end: `tl.to(el, {...}, Math.max(minIn, duration - 0.45))`.
6. Be **responsive**: size in `--u`, `vh`/`vw`/`%`, positions as fractions via `point` vars
   (`left: calc(var(--var-anchor-x) * 100%)`). An overlay must look right at 1920×1080
   and 1080×1920 without edits.
7. Take a font as a `font` var and write `font-family: var(--var-<name>), <fallback stack>`:
   kuma supplies the `@font-face` rules for the family it names and stages the files, in
   the bake and the live preview alike. `list_fonts` says which families exist. A fixed
   font can also be bundled in the folder (`@font-face` with a relative `src`), or left to
   a system stack. No Google Fonts links: the bake is headless and must be deterministic.
8. **Re-measure rather than measure once.** Prefer CSS that needs no measuring at all; if
   the page must measure itself — fitting a line of text to its box is the usual reason —
   run it again on `resize` (a `ResizeObserver` on the element is the reliable one) and
   after `document.fonts.ready`, since fallback metrics differ. Never leave a temporary
   style behind on an early return: a measure that reads `0` and gives up must put back
   what it borrowed, or the frame renders at the probe's size.
9. Keep `_vendor`, `data-*` timing and the `__timelines` contract as the only
   hyperframes-specific surface, so the runtime stays replaceable.

Transitions (dissolve/wipe) are refused on overlay clips: bakes carry no handle frames.

## Manifest (`overlay.json`)

```json
{
  "name": "Location tag",
  "description": "…",
  "duration_default": 4.0,
  "static": false,                  // true → bakes a single still frame
  "create": "Location tag",         // optional: offered in the bin's New menu under this label
  "vars": {
    "label":  { "type": "string", "default": "Paris", "label": "Place" },
    "font":   { "type": "font",   "default": "Inter", "group": "Text" },
    "box":    { "type": "boolean", "default": false, "group": "Box" },
    "box_color": { "type": "color", "default": "#111111", "group": "Box", "when": { "box": true } },
    "accent": { "type": "color",  "default": "#ff4f00" },
    "size":   { "type": "number", "default": 1, "min": 0.5, "max": 2.5, "step": 0.1 },
    "style":  { "type": "enum",   "options": ["pin", "chip"], "default": "pin" },
    "anchor": { "type": "point",  "default": { "x": 0.68, "y": 0.28 } },
    "on":     { "type": "boolean", "default": true },
    "note":   { "type": "text",   "default": "multi-line" },
    "cues":   { "type": "cues",   "default": [ { "start": 0, "end": 1.5, "text": "line" } ] }
  },
  "tags": ["label"]
}
```

Var types → UI controls: `string` input, `text` textarea, `number` slider (min/max/step),
`color` picker, `boolean` checkbox, `enum` select, `point` x/y (fraction of frame), `cues`
one editable row per `{start, end, text}` (seconds; `end` after `start`), `font` a family
picker (its `--var-<name>` is the quoted family name). A `cues` value
reaches the overlay only through `window.kuma.vars` — a list has no CSS property.

The inspector lists vars **in the order the manifest writes them**. `group` puts a var in a
foldable section of that name (sections in the order their first var appears, ungrouped
vars first); `when` shows a control only while other vars hold the given value, or one of
a list of values (`{ "enter": ["fade", "zoom"] }`) — presentation only, the value still
reaches the overlay. A blueprint with `create` is offered by the bin's **New** menu, which
is how a person makes a title without opening the Blueprints tab: put it on a blueprint
that is a starting point, whose every look is a var. The
manifest is checked on load (defaults fit their specs, enums have options,
`duration_default` > 0); an entry's `vars` are validated against it and unknown keys are
rejected. Bakes are always the full frame; a still (`static`) is one PNG.

## Timing responsibilities

- **kuma's project JSON decides *when* the overlay is on screen** (clip `start`/`end`/`speed`).
- **The overlay decides the choreography inside** (`data-start`/`data-duration` on
  children, the GSAP timeline).

Speed on an overlay clip is applied at bake time as a time remap of the seek (no frame
duplication). Position/scale/opacity/blur/blend/crop on the clip are MLT effects applied to
the baked producer and approximated with CSS in the live preview.

## Motion discipline

An overlay is not a user interface. Nobody hovers it, nothing can be interrupted, there is
no reduced-motion setting to honour once the frames are baked, and the viewer cannot pause
to re-read a line that left too fast. What that leaves is one budget — the clip's length —
and one competitor: the footage, which is the thing people came for.

**Animate arrival, departure and change. Nothing else.** A tag arrives, points, and
leaves. A counter changes because the number is the content. Motion that decorates,
signals "premium" or fills a silence is motion that takes attention away from the video
and gives nothing back. An element that moves continuously while the presenter talks is
the worst of these, because it never stops asking.

House numbers, for overlays that should sit beside the shipped blueprints:

| | |
|---|---|
| Entrance | 0.3–0.5 s, an out-curve (`power3.out`); the viewer sees the end of a move, so the deceleration is the part that reads |
| Exit | 0.25–0.4 s, an in-curve (`power2.in`), anchored to `duration`, never to a constant |
| Stagger | 0.08–0.15 s between a line and its subline. Two things arriving together read as one thing arriving badly |
| Hold | ≥ 1.2 s on screen after the entrance settles for one line, ≥ 2 s for two, ≥ 2.5 s for a person's name |
| Anything longer than 0.6 s | only when the motion *is* the content: a countdown, a chart drawing itself, a line being traced |
| Loops | a pointer's pulse and nothing else, under 20% travel in scale or opacity |

- Scale from 0.9, never from 0: a thing that grows from nothing reads as a glitch at
  speed, and its first frames are unreadable anyway.
- Overshoot (`back.out`) belongs on a mark — a pin, a badge, a ring. On text it reads as
  cheap, and at 1080p the wobble is legible frame by frame.
- One thing moves at a time. If two must, stagger them.
- Blur, large shadows and filters over a whole frame are paid for on every baked frame;
  a 30 s overlay is 900 renders of them. Cheap where it does not show.

## Drawn overlays

For marks that are drawn rather than laid out (a wipe, particles, a hand-drawn underline, a
generative texture), paint a `<canvas>` with `_vendor/kuma-canvas.js`. Text and layout stay
DOM + GSAP. `paint-wipe` is the worked example.

```js
const tpl = kumaTemplate({ … });
kumaCanvas(tpl, "#paint", (g, t) => {
  g.rough("mark").circle(g.w / 2, g.h / 2, 20 * g.u, { fill: tpl.vars.accent, fillStyle: "hachure" });
});
```

- `draw(g, t)` paints the whole frame from `t` alone; the canvas is cleared and the context
  restored around it. `g` holds `ctx`, `w`, `h`, `u` (the canvas twin of `--u`), and three
  seeded sources: `rough(key)` (a Rough.js canvas), `jitter(key)` (a value in [0, 1)) and
  `seed(key)` (for any other seedable library). Each holds for one *drawing*, 12 a second,
  so wobble "boils" like redrawn animation, and keys are independent, so a moving thing
  does not make still things jitter. `kumaCanvas.rand(key)` is a value in [0, 1) that never
  changes: positions, sizes, delays. Keys as alike as `star1`, `star2` give unrelated
  values, so build them from an index freely. (`kumaCanvas.hash` and `g.rand` are the
  first versions of these two, whose values for alike keys are related; the lint warns
  on them, and they are kept only so overlays that use them keep drawing what they drew.)
- Helpers on `kumaCanvas`, pure functions of `t`: `kf`, `spring`, `ring`, `jump`, `take`,
  `arcPt`, `onTwos`, `noise`. Eases are GSAP's: `gsap.parseEase("power3.out")`, so the
  house numbers above apply unchanged.
- 2D only: a bake has no GPU. Cost is `draw`, on every frame; hundreds of shapes are fine.
- Lay out along the frame's axes in `g.u` and fractions of `g.w`/`g.h`, and check 1080×1920
  with `sample_frames` and `frame`.

## Checking your work

The contract check and the frames are two different questions: whether it will render, and
whether it is any good. Both have an answer that costs seconds.

1. **`check_overlay`** (`kuma lint overlays/<name>`) — the contract. Do this after every edit to the
   markup; it is the difference between knowing now and a bake timing out in 45 seconds.
2. **`sample_frames` with `overlay: "<bin id>"`** — the entry on its own, no placement
   needed, times counted from the start of the overlay. Take 5 or 6 across the clip, and
   include `t0: 0`: an overlay that is already fully visible in its first frame has no
   entrance, which is the most common defect and invisible in a still.
3. **Both orientations.** Pass `frame: [1080, 1920]` and look again. Text that fits in
   landscape overflows in portrait, and anchors that look centred move. The contract
   requires an overlay to survive both; this is how you find out that it does not.
4. **Both extremes of backdrop.** `backdrop: "#ffffff"` and `backdrop: "#000000"`. An
   overlay is transparent and will sit over footage you have not seen; if it is legible on
   white and on black, it is legible.
5. **Read the frames, not just the log.** Is the text inside the safe area (5–6% on every
   side)? Is the accent used once? Does the exit finish before the clip ends — sample at
   `duration - 0.05` and check that what should be gone is gone?

The editor's overlay workbench is the same five checks for a person: its own clock, a
frame picker, a backdrop picker and **Check**, on an entry that nothing places.

## Testing an overlay

```
kuma lint overlays/<name>                     # the contract, without baking
kuma thumbs                                   # bakes thumb.png for the blueprint browser
npx hyperframes preview overlays/<name>       # hyperframes' own studio, if you want a scrubber
```

Import it (or add a blueprint), scrub the preview, then `sample_frames` on its range to
check the baked result matches.

## Adding an overlay as an agent

1. `sample_frames` on the target range shows what the overlay sits over; `list_blueprints`
   shows what the library has (`add_blueprint` is often quicker than a scaffold).
2. `new_overlay` (or write the two files yourself), then edit until `check_overlay` is clean.
3. `import_overlay` with `path: "overlays/<name>"` and your `vars`. A second wording is a
   second import of the same folder. Run *Checking your work* on the entry — no clip needed.
4. Place it with `apply_edits`: a clip `{ "id": "…", "start": t, "end": t + d, "bin":
   "<binId>" }` on an overlay track, then `sample_frames` that range.
5. `save_blueprint` only when the user asks: the library is theirs.
