# Authoring kuma overlay templates

An overlay template is a folder under `overlays/` (the library) or `<project>/overlays/`
(project-local, wins on id clash):

```
overlays/<id>/
  template.json   manifest: id, name, description, duration_default, entry, vars, static, region
  index.html      the composition (or whatever `entry` names, inside this folder)
  fonts/…         any fonts, referenced with relative @font-face paths
  thumb.png       generated (`kuma thumbs`); never part of a bake, do not hand-edit
```

`_vendor/` holds shared files, referenced as `_vendor/<file>` from the template. A
template's own `_vendor/` wins, then the one next to the templates, then the library's;
the bake stages and the daemon serves exactly the same folder.

- `_vendor/kuma.js` — `kumaTemplate(defaults)` applies the vars, returns `{ vars, width,
  height, duration, text(id, value), timeline() }` and works standalone.
- `_vendor/kuma.css` — the transparent reset and the `--u` unit (`1vh × --var-size`).
- `_vendor/gsap.min.js`.

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
  "region": { "x": 0, "y": 0, "w": 1920, "h": 1080 },   // optional: crop the bake to this
  "vars": {
    "label":  { "type": "string", "default": "Paris", "label": "Place" },
    "accent": { "type": "color",  "default": "#ff4f00" },
    "size":   { "type": "number", "default": 1, "min": 0.5, "max": 2.5, "step": 0.1 },
    "style":  { "type": "enum",   "options": ["pin", "chip"], "default": "pin" },
    "anchor": { "type": "point",  "default": { "x": 0.68, "y": 0.28 } },
    "on":     { "type": "boolean", "default": true },
    "note":   { "type": "text",   "default": "multi-line" },
    "logo":   { "type": "asset",  "default": "" }
  },
  "sfx": [ { "path": "whoosh.mp3", "at": 0.1, "gain": 0.8 } ],   // parsed, not yet placed on a track
  "tags": ["label"]
}
```

Var types → UI controls: `string` input, `text` textarea, `number` slider (min/max/step),
`color` picker, `boolean` checkbox, `enum` select, `point` x/y (fraction of frame),
`asset` path. The manifest is checked on load (defaults must fit their specs, enums need
options, `entry` stays in the folder, `duration_default` > 0); clip `vars` are validated
against it and unknown keys are rejected.

## Timing responsibilities

- **kuma's project JSON decides *when* the overlay is on screen** (clip `start`/`end`/`speed`).
- **The template decides the choreography inside** (`data-start`/`data-duration` on
  children, the GSAP timeline).

Speed on an overlay clip is applied at bake time as a time remap of the seek (no frame
duplication). Position/scale/opacity/blur/blend/crop on the clip are MLT effects applied to
the baked producer and approximated with CSS in the live preview.

## Testing a template

```
kuma templates                       # lists it (id, name, vars)
kuma thumbs                          # bakes thumb.png for the library panel
npx hyperframes preview overlays/<id>   # hyperframes' own studio, if you want a scrubber
```

Add an instance to a project (`apply_edits` or drag it on the timeline), scrub the
preview, then `sample_frames` on its range to check the baked result matches.

## Adding a template as an agent

1. `sample_frames` on the target range to see what the overlay must sit over.
2. Create `overlays/<id>/template.json` and `index.html` following this contract.
3. Call `POST /api/templates` (or the `list_templates` tool) so the daemon reloads.
4. Add a clip: `{ "id": "…", "start": t, "end": t + d, "src": { "overlay": "<id>", "vars": {…} } }`
   on an overlay track via `apply_edits`.
5. `sample_frames` again on that range to verify.
