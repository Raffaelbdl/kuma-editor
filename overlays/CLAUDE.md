# Authoring kuma overlay templates

An overlay template is a folder under `overlays/` (the library) or `<project>/overlays/`
(project-local, wins on id clash):

```
overlays/<id>/
  template.json   manifest: id, name, description, duration_default, vars, static, region, sfx
  index.html      the composition
  fonts/…         any fonts, referenced with relative @font-face paths
  thumb.png       generated (`kuma thumbs`); do not hand-edit
```

`_vendor/` next to the templates holds shared libraries (`gsap.min.js`). Reference them
as `_vendor/gsap.min.js` from the template — the bake copies that folder in and the
daemon serves it under every template.

## The contract (what kuma guarantees, what the template must do)

**kuma injects, before your `<head>` content:**

- `window.kuma = { vars, fps: {num, den}, width, height, duration, speed, bake }` —
  `vars` are the manifest defaults overlaid with the clip's overrides; `duration` is the
  clip's internal duration in seconds (clip length × |speed|).
- CSS custom properties `--var-<name>` on `:root` for every var (strings raw, objects
  JSON-stringified).
- At the end of `<body>`: the root `[data-composition-id]` element gets
  `data-duration`, `data-width`, `data-height` set from the clip/project. **Never hard-code
  the duration in logic; read `window.kuma.duration`.**

**The template must:**

1. Be transparent all the way to the root: `html, body, #root { background: transparent }`.
   Any opaque wrapper becomes an opaque video.
2. Have one root: `<div id="root" data-composition-id="main" data-start="0" data-duration="4"
   data-width="1920" data-height="1080">`. Timed children carry `class="clip"`,
   `data-start`, `data-duration` (seconds, relative to the root).
3. Be a **pure function of time**. Animation must be seekable:
   - GSAP: one paused timeline registered as `window.__timelines["main"] = gsap.timeline({ paused: true })`.
   - **No GSAP timeline at all** (static templates, pure CSS animation): put `data-no-timeline`
     on the composition root, or the renderer waits 45 s for a timeline and fails the bake.
   - CSS `@keyframes`/transitions and WAAPI are fine — the renderer scrubs them.
   - Lottie, Anime.js, Three.js are supported through the hyperframes adapters.
   - **Forbidden:** `setTimeout`/`setInterval`/`requestAnimationFrame`-driven state,
     `Date.now()`, `Math.random()` without a fixed seed, autoplaying `<video>`. Anything
     wall-clock driven renders wrong or frozen.
4. Read vars with a fallback so the page also works standalone:
   `const vars = Object.assign({}, defaults, (window.kuma && window.kuma.vars) || {})`.
   Apply them to the DOM at load (text content, `style.setProperty("--var-…")`).
5. Anchor the out-animation to the end: `tl.to(el, {...}, Math.max(minIn, duration - 0.45))`.
6. Be **responsive**: size in `vh`/`vw`/`%` (use a `--u: calc(1vh * var(--var-size))` unit),
   positions as fractions via `point` vars. A template must look right at 1920×1080 and
   1080×1920 without edits.
7. Bundle fonts (`@font-face` with relative `src`). No Google Fonts links: the bake is
   headless and must be deterministic.
8. Keep `_vendor`, `data-*` timing and the `__timelines` contract as the only
   hyperframes-specific surface, so the runtime stays replaceable.

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
  "sfx": [ { "path": "whoosh.mp3", "at": 0.1, "gain": 0.8 } ],
  "tags": ["label"]
}
```

Var types → UI controls: `string` input, `text` textarea, `number` slider (min/max/step),
`color` picker, `boolean` checkbox, `enum` select, `point` x/y (fraction of frame),
`asset` path. Clip `vars` are validated against this manifest; unknown keys are rejected.

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
