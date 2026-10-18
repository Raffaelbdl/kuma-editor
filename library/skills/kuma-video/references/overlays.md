# Overlays: authoring gotchas beyond the contract

`overlay_guide` is the contract; `check_overlay` enforces most of it. These are what the
contract does not say, or what bit anyway.

## Structure

- **Blueprints first.** `list_blueprints` has titles (`title`, `title-subtitle`,
  `section-title`), a `lower-third`, `subtitles`, a `callout-arrow`, `label-pin` and
  `location-tag`, a `countdown`, a `watermark` and a `paint-wipe` transition, each with its
  looks as vars. `list_blueprints` names each one's vars; with its `id`, it gives their
  defaults and options. `add_blueprint` copies one into the project; set vars before touching markup.
- A **component** is a folder (`overlays/label/`) with var-driven markup; a use of it is an
  entry with vars. Write one only for what no blueprint does. A handful covers most videos
  (title, lower third, caption, callout, panel, list, code card, progress bar); a
  motion-graphics explainer used two dozen, from a narration box and speech bubbles to
  brush lettering and page-turn wipes.
- **Shared helpers** go in `overlays/_vendor/`: colour tokens and anchor classes (css), and
  `enter(tl, el, kind, at)` / `exit(tl, el, kind, duration)` so every component moves the
  same way. Changing them **re-bakes every overlay**, so settle motion and type early.
  A file there is used where an overlay asks for `_vendor/<file>`, and anything it does
  not hold still comes from the library's `_vendor/` (`kuma.js` and the rest), file by file.
- **Pictures in an overlay** (a layer, a thumbnail, a map) are files the overlay's folder
  holds: copy the media in (`overlays/timeline/thumb-03.jpg`) and reference it relatively. A
  path out of the folder (`../../media/x.png`) does not bake, since the bake renders a copy
  of the folder. A picture several overlays share can go in `overlays/_vendor/`, but every
  file there is in every overlay's bake key: adding or replacing one re-bakes them all, so
  keep that for small things that do not change. To choose the picture per entry, a string
  var holds its file name. A copy does not follow the original: when a picture is replaced
  on disk, copy it in again.
- **A graphic that must sit under a picture**: draw the picture in the overlay. Overlays
  always draw over video (a video track after an overlay track is refused), so a timeline
  rail with the thumbnails of earlier scenes in its slots is one overlay drawing both, not
  a rail full of holes over thumbnail clips, and a still shrinking into its slot is an
  `<img>` the same overlay animates. Only moving footage has to stay an MLT clip under the
  overlay: leave one hole for it.
- Anchor with normalised positions plus an `origin` (`top-left`, `center`, …), size in the
  unit kuma injects, and check an overlay in the other orientation (`frame: [1080, 1920]`).
- **Static** things: a texture or a logo as image media; a persistent frame (a HUD, a
  border) as a `data-no-timeline` overlay faded by an MLT `fade`. Neither costs a
  per-frame bake.
- One entry can be placed many times: a recurring bubble appeared in five chapters from one
  entry, and fixing its text fixed all five.

## What a bake costs

- **Every frame is a screenshot.** An empty 1080p frame costs about 45 ms to capture and
  encode: bake an empty overlay once to learn the floor on your machine, and compare. Each
  bake also starts a browser (2 to 3 s), so twice as many overlays over the same seconds
  is that much more: captions as a separate overlay per scene doubled the bakes of a
  98-scene video.
- **Shadows, blur and full-frame gradients are painted in software, on every frame**, and
  their soft alpha does not compress. Removing `box-shadow` and `text-shadow` and moving a
  vignette to a still took an 8 s scene from 57 s to 24 s, and its frames from 156 MB to
  19 MB. A vignette, a texture or a frame is an image clip (or a `static` overlay) that
  MLT composites, not something each overlay redraws.
- **Held frames are reused, moving ones are not.** Tweens tell the renderer where the
  picture changes; an overlay driven by `onFrame` changes everywhere (use `onFrames`,
  under Motion).
- **An overlay re-bakes when anything it reads changes**: its vars, its folder, `_vendor/`,
  its fonts, its clip's length. Give it only what its own scene knows. A progress bar over
  the whole video re-baked all 98 scenes whenever one was added; over the chapter, one.

## One engine for a whole video

A generated video can be one overlay folder holding an engine (nodes, arrows, code windows,
tables) and one entry per scene whose vars are that scene's data. It makes layout and
timing edits data edits, and auto-fit possible: measure the finished layout in the page and
scale it into the safe area, which removes most per-scene coordinates. Its costs:

- **Every scene shares the folder, so one engine edit re-bakes the whole video.** Batch
  engine changes; try one on a single scene (`sample_frames` with `overlay`) first.
- **A scene is one clip, so one word changed re-bakes the scene.** Captions inside the
  scene overlay save a browser start per scene and cost that.
- **Text kuma cannot see is text it cannot check.** `validate_project`'s font and glyph
  warnings, `reading_audit.py` and the inspector read `string`, `text` and `cues` vars. A
  scene packed into one base64 or JSON blob hides all of it. Keep what is read on screen
  in such vars (captions as a `cues` var), and the geometry in the blob if it must be one.
- **Escape in one place.** An engine that escapes HTML and a storyboard that writes
  `<span>` print the markup on screen. Give text a small markup of its own
  (`{warn:text}`, `` `code` ``) that the engine alone turns into elements.
- The engine can be a file beside `index.html` (`engine.js`), checked like inline code.
  Hand values between scripts on `window`, and draw with `onFrames` where scenes hold.
- Hand-placed elements collide when a text gets longer: the layout bugs of that video were
  a title over a grid, a note over a row, code lines cut at the window's edge. Sample
  every scene at full width after any change to sizes.

## Motion

- Arrivals: ~0.3 s, `expo.out` for rises and slides, `back.out(2.6)` from scale 0.8 for
  pops, 0.2 s scale-down "slams" for impact lettering. Departures: 0.2 s fades anchored to
  the clip's end (`duration - d`), so any clip length works.
- To show an animation frozen at a moment (the same overlay at t = 0.04 / 0.15 / 1.0 side
  by side), give it an `at` var and compute the pose directly (`gsap.parseEase`). A second
  private timeline fails the lint.
- An overlay whose script draws each frame itself (`onFrame`) bakes every frame of its
  clip, held ones included. If it holds still for stretches, drive it with
  `onFrames(spans, fn)` and name the spans where it changes: the bake reuses the frames
  between them (half the frames of a typical explainer scene, in the field). Keep continuous
  motion (a bar that grows all scene long, a pulsing ring, a blinking caret) out of the
  holds, or there are none.
- Draw on canvas through `kumaCanvas` with seeded Rough.js (`g.rough(key)`), never an
  unseeded `rough.canvas`.
- Few easings for a whole video (an out-cubic, an in-out-cubic and one soft overshoot is
  enough), and flash or shake only on percussive moments.

## Transition overlays

A wipe, a shutter, a page turn drawn by an overlay over a cut:

- The clip is a window **centred on the cut**: placed at `[cut − d/2, cut + d/2]`, a cover
  phase, then a reveal phase, and **the frame is fully covered at the midpoint**, which is
  where the cut goes. Then the cut never shows, whatever is on either side.
- Everything is a function of progress through the window alone, and nothing is drawn
  outside it, so the clip can be lengthened or moved without re-authoring it.

## Geometry

- Author in the pixels of a 1920-wide frame and scale once: `--px: calc(100vw / 1920)`,
  then `calc(24 * var(--px))`. Numbers then match what you measure on a sampled frame.
- A geometry an overlay and a script both need (the slots of a timeline graphic that
  footage shrinks into, say) is written in one place and read by both, never typed twice.
- Marks drawn over footage (an arrow, a ring) need a dark line under the light one, or
  they vanish on a white wall.

## Text

- **A font is `add_font`, never a file in a folder**: it takes `.ttf`, `.otf`, `.ttc` and the
  web fonts (`.woff`, `.woff2`) a design system often ships as, and every overlay names the
  family through a `font` var. A face bundled with `@font-face` in one folder works, but
  has to be copied into every folder that uses it.
- **Ligatures**: mono fonts with programming ligatures draw `<!--`, `->` and `!=` as
  symbols. Put `font-variant-ligatures: none; font-feature-settings: "liga" 0, "calt" 0`
  on code.
- **Overflow**: a box sized by `max-width` grows to it when the text gets longer, and it
  can run off the frame (a bottom-left box at x = 0.44 with width 120 did). Whenever you
  lengthen text, check its right and bottom edges against the frame, in both orientations
  if the video has a portrait cut.
- Inline markup in text vars (`*emphasis*`, `` `code` ``) is parsed by one shared helper
  in `_vendor/`, not by each component, so it means the same everywhere.
- CJK text: see `japanese-type.md`. In short, the entry's `lang` var set to `ja` (or
  `lang="ja"` on the page, or on the one element that shows Japanese inside a line in
  another language) and a `jp` fallback
  font var in every font stack.

## Lint messages worth knowing

- "fallback font family without @font-face": name families only through font vars,
  `font-family: var(--var-font), var(--var-jp, serif), serif`.
- "private gsap.timeline not registered": register the one timeline via `timeline()`.
- "invalid inline script": regex literals in inline scripts trip the parser; use
  `new RegExp("…", "y")`, or put the code in a file beside `index.html` (`engine.js`).
  A linked script is checked like an inline one, so it can register the timeline and
  read the vars with nothing inline. A library goes in `_vendor/` (the overlay's own
  will do), which is not held to the rules about clocks and vars.

## Looking at one

- **A frame with nothing on it**: read the `warnings` of the sample, bake or render. A
  script that throws does not fail a bake: the warning quotes the error. "baked fully
  transparent" with no error means the page drew nothing for the whole clip. One cause met in the field (the page worked in a browser and baked blank): a top-level
  `const` of an inline `<script>` was not visible to a file loaded after it when baked;
  values handed over on `window` were.

- `sample_frames` with `overlay` renders the entry on its own, before any clip places it.
- Sample over **white and black** backdrops if it can sit over footage.
