# Authoring kuma overlay templates

Templates are assets shared across projects, not code. kuma reads them from every
library's `overlays/` and from the project's own `overlays/`, later wins on an id clash:

1. `$XDG_DATA_HOME/kuma/overlays` (default `~/.local/share/kuma/overlays`; `kuma library`
   installs the starter set there),
2. each directory in `KUMA_LIBRARY` (`:`-separated), as `<dir>/overlays`,
3. `<project dir>/overlays` — the place for one-off templates written for that video.

`GET /api/health` (the `health` tool) lists `template_dirs` and `template_errors`. A folder
is only a template if it holds a `template.json`, and loading refuses one without an
`index.html` beside it, so write both before rescanning or the template reports as broken
rather than appearing.

```
overlays/<id>/
  template.json   manifest: id, name, description, duration_default, static, vars, tags
  index.html      the composition (always this name)
  fonts/…         any fonts, referenced with relative @font-face paths
  thumb.png       generated (`kuma thumbs`); never part of a bake, do not hand-edit
```

`_vendor/` holds shared files, referenced as `_vendor/<file>` from the template. A
template's own `_vendor/` wins, then the one next to it, then the shared one from the
highest-precedence library that has it; the bake stages and the daemon serves exactly the
same folder.

- `_vendor/kuma.js` — `kumaTemplate(defaults)` applies the vars, returns `{ vars, width,
  height, duration, text(id, value), timeline() }` and works standalone.
- `_vendor/kuma.css` — the transparent reset and the `--u` unit (`1vh × --var-size`).
- `_vendor/gsap.min.js`.

## Start from the scaffold, and check your work

Do not write a template from an empty file. `kuma new-template <id>` (the `new_template`
tool) writes a working one into the project's `overlays/`: the root's timing attributes, a
registered paused timeline, typed vars of three kinds, `--u` sizing that survives both
orientations, and an exit anchored to the clip's real length. Edit that.

`kuma lint <id>` (the `check_template` tool, **Check** in the overlay workbench) reads the
staged template — the injection applied, `_vendor/` in place — and reports what would
otherwise surface as a bake that times out after 45 seconds or a frame that is quietly
wrong: a missing timeline registration, a wall clock, a Google Fonts URL, a var the markup
reads that the manifest does not declare, a missing asset. It runs hyperframes' own linter
too when it is installed. It renders nothing and writes nothing, so run it after every
edit; a bake is the slow way to learn the same thing.

## The contract (what kuma guarantees, what the template must do)

**kuma injects:**

- At the top of `<head>`: `window.kuma = { vars, fps: {num, den}, width, height,
  duration, speed, bake, clip }` — `vars` are the manifest defaults overlaid with the
  clip's overrides; `duration` is the clip's internal duration in seconds (clip length ×
  |speed|).
- At the end of `<head>` (so they win over the template's own `:root` fallbacks): CSS
  custom properties `--var-<name>` on `:root` for every scalar var, and
  `--var-<name>-x` / `--var-<name>-y` for `point` vars.
- The root `[data-composition-id]` element's `data-duration` (and `data-width`/`data-height`)
  are **rewritten in the markup** to the clip's real length before the renderer sees the
  file — hyperframes sizes the render from the attribute as written, before any script runs.
  The value in your template is only the standalone default; do not try to set it from a
  script (it has no effect on the render). **Never hard-code the duration in logic; read
  `window.kuma.duration`.**
- Timed children are **mounted only for their own `data-start`/`data-duration`** — a child
  that keeps `data-duration="4"` disappears after 4 s even in a 30 s clip. kuma stretches
  every child whose end coincides with the root's default (`start + duration == root
  default`) to the clip's real end; a child that ends earlier keeps its timing. So: give
  "whole clip" children the same end as the root, and shorter ones their real length.

**The template must:**

1. Be transparent all the way to the root (`_vendor/kuma.css` does it). Any opaque
   wrapper becomes an opaque video.
2. Have one root: `<div id="root" data-composition-id="main" data-start="0" data-duration="4"
   data-width="1920" data-height="1080">`. Timed children carry `class="clip"`,
   `data-start`, `data-duration` (seconds, relative to the root).
3. Be a **pure function of time**. Animation must be seekable:
   - GSAP: one paused timeline, `const tl = timeline()` from `kumaTemplate` (it registers
     `window.__timelines["main"]`).
   - **No GSAP timeline at all** (static templates, pure CSS animation): put `data-no-timeline`
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
   (`left: calc(var(--var-anchor-x) * 100%)`). A template must look right at 1920×1080
   and 1080×1920 without edits.
7. Bundle fonts (`@font-face` with relative `src`) or use a system stack. No Google Fonts
   links: the bake is headless and must be deterministic.
8. Keep `_vendor`, `data-*` timing and the `__timelines` contract as the only
   hyperframes-specific surface, so the runtime stays replaceable.

Transitions (dissolve/wipe) are refused on overlay clips: bakes carry no handle frames.

## Manifest (`template.json`)

```json
{
  "id": "location-tag",             // must equal the folder name
  "name": "Location tag",
  "description": "…",
  "duration_default": 4.0,
  "static": false,                  // true → bakes a single still frame
  "vars": {
    "label":  { "type": "string", "default": "Paris", "label": "Place" },
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
one editable row per `{start, end, text}` (seconds; `end` after `start`). A `cues` value
reaches the template only through `window.kuma.vars` — a list has no CSS property. The
manifest is checked on load (`id` equals the folder, defaults fit their specs, enums have
options, `duration_default` > 0); clip `vars` are validated against it and unknown keys
are rejected. Bakes are always the full frame; a still (`static`) is one PNG.

## Timing responsibilities

- **kuma's project JSON decides *when* the overlay is on screen** (clip `start`/`end`/`speed`).
- **The template decides the choreography inside** (`data-start`/`data-duration` on
  children, the GSAP timeline).

Speed on an overlay clip is applied at bake time as a time remap of the seek (no frame
duplication). Position/scale/opacity/blur/blend/crop on the clip are MLT effects applied to
the baked producer and approximated with CSS in the live preview.

## Testing a template

```
kuma thumbs                              # bakes thumb.png for the library panel
npx hyperframes preview <dir>/overlays/<id>   # hyperframes' own studio, if you want a scrubber
```

`list_templates` (or `GET /api/templates`) rescans every library and lists what loaded.
Add an instance to a project (`apply_edits` or drag it on the timeline), scrub the
preview, then `sample_frames` on its range to check the baked result matches.

## Adding a template as an agent

1. `template_guide` returns this file; `sample_frames` on the target range shows what the
   overlay must sit over.
2. Create `<dir>/overlays/<id>/template.json` and `index.html` following this contract —
   in the project's `overlays/` for a one-off, in the user library
   (`~/.local/share/kuma/overlays/`) for something reusable.
3. Call `list_templates` so the daemon reloads (it reports manifests that failed to load).
4. Add a bin entry and place it, in one `apply_edits`: add
   `{ "name": "…", "source": { "template": "<id>", "vars": {…} } }` under `/bin/<binId>`,
   then a clip `{ "id": "…", "start": t, "end": t + d, "bin": "<binId>" }` on an overlay
   track. Two instances with different vars are two bin entries.
5. `sample_frames` again on that range to verify.
