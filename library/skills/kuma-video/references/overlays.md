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
- Anchor with normalised positions plus an `origin` (`top-left`, `center`, …), size in the
  unit kuma injects, and check an overlay in the other orientation (`frame: [1080, 1920]`).
- **Static** things: a texture or a logo as image media; a persistent frame (a HUD, a
  border) as a `data-no-timeline` overlay faded by an MLT `fade`. Neither costs a
  per-frame bake.
- One entry can be placed many times: a recurring bubble appeared in five chapters from one
  entry, and fixing its text fixed all five.

## Motion

- Arrivals: ~0.3 s, `expo.out` for rises and slides, `back.out(2.6)` from scale 0.8 for
  pops, 0.2 s scale-down "slams" for impact lettering. Departures: 0.2 s fades anchored to
  the clip's end (`duration - d`), so any clip length works.
- To show an animation frozen at a moment (the same overlay at t = 0.04 / 0.15 / 1.0 side
  by side), give it an `at` var and compute the pose directly (`gsap.parseEase`). A second
  private timeline fails the lint.
- Draw on canvas through `kumaCanvas` with seeded Rough.js (`g.rough(key)`), never an
  unseeded `rough.canvas`.

## Text

- **Ligatures**: mono fonts with programming ligatures draw `<!--`, `->` and `!=` as
  symbols. Put `font-variant-ligatures: none; font-feature-settings: "liga" 0, "calt" 0`
  on code.
- **Overflow**: a box sized by `max-width` grows to it when the text gets longer, and it
  can run off the frame (a bottom-left box at x = 0.44 with width 120 did). Whenever you
  lengthen text, check its right and bottom edges against the frame, in both orientations
  if the video has a portrait cut.
- Inline markup in text vars (`*emphasis*`, `` `code` ``) is parsed by one shared helper
  in `_vendor/`, not by each component, so it means the same everywhere.
- CJK text: see `japanese-type.md`. In short, `lang="ja"` on the page and a `jp` fallback
  font var in every font stack.

## Lint messages worth knowing

- "fallback font family without @font-face": name families only through font vars,
  `font-family: var(--var-font), var(--var-jp, serif), serif`.
- "private gsap.timeline not registered": register the one timeline via `timeline()`.
- "invalid inline script": regex literals in inline scripts trip the parser; use
  `new RegExp("…", "y")`.

## Looking at one

- `sample_frames` with `overlay` renders the entry on its own, before any clip places it.
- Sample over **white and black** backdrops if it can sit over footage.
