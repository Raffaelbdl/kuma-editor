# Writing a design system

A design system here is **a document an agent reads**, not code the daemon loads. One is
copied into a project (`kuma design-system <id>` → `<project>/design-system/`) and from
then on it is the answer to "what should this look like" for every overlay in that video.
Nothing is injected at runtime: applying it is an edit to a template or to an entry's
vars, which keeps a template readable on its own and keeps the values where the rest of
the project's decisions live.

```
design-systems/<id>/
  DESIGN.md     the prose: what it is for, colour, type, geometry, motion, legibility
  tokens.css    the same values as a `:root { --ds-… }` block, to copy into a template
```

The id is the folder name. `DESIGN.md`'s first `# heading` is the name shown by
`kuma design-system`. Both files are required; anything else in the folder (fonts, a
reference frame) is copied with them.

## What belongs in `DESIGN.md`

Write it for someone who has to make a decision in the next minute, and give numbers.

- **What it is for** — the footage it sits on, and the kind of video. A system that does
  not say where it does not belong gets used everywhere.
- **Colour** — a table of roles (accent, surface, ink, secondary ink, scrim), each with a
  value and *where it is used*. Say how many hues are allowed; one accent is usually the
  rule, and saying so stops a video growing three.
- **Type** — a stack that needs no download (the bake is offline and deterministic), then
  sizes in `u`, weights and tracking per role. Give a floor: text below ~`1.6u` is
  unreadable on a phone.
- **Geometry** — radius, padding, stroke widths, the gap between a mark and its label.
- **Motion** — durations and easings for the entrance and the exit, what staggers and by
  how much, and what is forbidden. Anchor exits to the clip's real length, never to a
  constant.
- **Legibility over footage** — safe margins, when a scrim is required, how long a line
  must stay on screen to be read.
- **Applying it in kuma** — which of the template vars (`accent`, `size`, `at`/`anchor`)
  carry which value.

Lengths are in kuma's `--u` (`1vh × size`), not pixels: that is what makes a template
survive 1920×1080 and 1080×1920 without edits. A value in `px` is a value that will be
wrong in portrait.

## What does not belong

Anything that is really a template: markup, a layout, a component. A design system says
how things look, not what they are — two videos with the same system have different
overlays in them.
