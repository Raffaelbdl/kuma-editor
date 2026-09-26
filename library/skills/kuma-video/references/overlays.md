# Overlays: authoring gotchas beyond the contract

`overlay_guide` is the contract; `check_overlay` enforces most of it. These are what the
contract does not say, or what bit anyway.

## Structure

- A **component** is a folder (`overlays/label/`) with var-driven markup; a use of it is an
  entry with vars. Two dozen components covered a 10-minute video: narration box, label,
  panel (koma), box, arrow, code card, list, bubble, JRPG window, menu, kakimoji (sound
  effect lettering), seal (hanko), brush lettering, focus lines, screentone, wipe, mini
  timeline, ruler, flipbook, characters.
- **Shared helpers** go in `overlays/_vendor/`: colour tokens and anchor classes (css), and
  `enter(tl, el, kind, at)` / `exit(tl, el, kind, duration)` so every component moves the
  same way. Changing them **re-bakes every overlay**, so settle motion and type early.
- Anchor with normalised positions plus an `origin` (`top-left`, `center`, …), size in the
  unit kuma injects, and check an overlay in the other orientation (`frame: [1080, 1920]`).
- **Static** things: a paper texture as image media on the bottom track; a HUD as a
  `data-no-timeline` overlay faded by an MLT `fade`. Neither costs a per-frame bake.
- One entry can be placed many times: the example bubble appeared in five chapters from one
  entry, and fixing its text fixed all five.

## Motion

- Arrivals: ~0.3 s, `expo.out` for rises and slides, `back.out(2.6)` from scale 0.8 for
  pops, 0.2 s scale-down "slams" for impact lettering. Departures: 0.2 s fades anchored to
  the clip's end (`duration - d`), so any clip length works.
- A "frozen at t" variant (the same overlay shown at t = 0.04 / 0.15 / 1.0 side by side)
  takes an `at` var and computes the pose directly (`gsap.parseEase`). A second private
  timeline fails the lint.
- Draw on canvas through `kumaCanvas` with seeded Rough.js (`g.rough(key)`), never an
  unseeded `rough.canvas`.

## Text

- **Quotes in string vars**: at the time of writing kuma writes string vars unquoted into
  one `:root { … }` line, so a lone `'` or `"` opens a CSS string that swallows every custom
  property after it. The overlay then renders with missing vars, often tiny in a corner,
  with no error. Use typographic apostrophes (’), and assert balanced quotes in the DSL.
  Check whether a var containing `Let's` still breaks before relying on the workaround.
- **Ligatures**: mono fonts with programming ligatures draw `<!--`, `->` and `!=` as
  symbols. Put `font-variant-ligatures: none; font-feature-settings: "liga" 0, "calt" 0`
  on code.
- **Overflow**: a box sized by `max-width` grows to it when the text gets longer, and it
  can run off the frame (a bottom-left narration box at x = 0.44 with width 120 did).
  Whenever you lengthen text, check its right and bottom edges against the frame.
- Markup in text vars (`*emphasis*`, `_second colour_`, `` `code` ``) is parsed by the
  shared helper, not by each component.
- CJK text: see `japanese-type.md`. In short, `lang="ja"` on the page and a `jp` fallback
  font var in every font stack.

## Lint messages worth knowing

- "fallback font family without @font-face": name families only through font vars,
  `font-family: var(--var-font), var(--var-jp, serif), serif`.
- "private gsap.timeline not registered": register the one timeline via `timeline()`.
- "invalid inline script": regex literals in inline scripts trip the parser; use
  `new RegExp("…", "y")`.

## Looking at one

- `sample_frames` with `overlay` renders the entry on its own. At the time of writing it
  fails for entries that have a `parent` folder ("parent … is not in the bin"); sample the
  chapter sequence over the clip's range instead.
- Sample over **white and black** backdrops if it can sit over footage.
