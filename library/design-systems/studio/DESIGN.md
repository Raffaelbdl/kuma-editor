# Studio

A dark, high-contrast overlay language for screencasts, tutorials and product demos —
the look kuma's starter templates already have, written down so a video made of several
overlays reads as one thing.

Category: software demos, tutorials, launch videos.

## What it is for

Footage that is mostly a screen: bright, busy, and unpredictable behind any overlay. So
every element carries its own dark surface rather than trusting the frame, one saturated
accent does all the pointing, and nothing decorative competes with the thing being
demonstrated.

## Colour

| Role | Value | Where |
|---|---|---|
| Accent | `#ff4f00` | One per frame: a pin, a rule, a ring, a highlighted word. Never body text. |
| Surface | `rgba(12, 12, 18, 0.82)` | The chip or card every text element sits on. Translucent, so the footage stays legible under it. |
| Ink | `#ffffff` | Primary text on surface. |
| Ink-2 | `rgba(255, 255, 255, 0.72)` | Secondary line: coordinates, roles, timestamps. |
| Scrim | `linear-gradient(transparent, rgba(0,0,0,0.65))` | Behind a lower third that spans the frame. |
| Shadow | `0 0.6u 2u rgba(0, 0, 0, 0.4)` | Separates a surface from busy footage; never a border for its own sake. |

The accent is the only hue. A second one (a "success" green, a "warning" amber) is the
fastest way to make a video look like a dashboard.

## Type

System stack: `"Segoe UI", system-ui, -apple-system, sans-serif`. It is on every machine,
bundles nothing, and renders identically in the bake and the preview.

- Title: `3.2u`, weight 800, tracking `-0.01em`.
- Label: `2.6u`, weight 600 — the default for a tag or a pin.
- Secondary: `1.9u`, weight 500, Ink-2.
- Never below `1.6u`: at 1080p that is ~17px, the floor for a viewer on a phone.

`u` is kuma's `--u`: `1vh × size`, so every number above scales with the `size` var and
survives 1920×1080 and 1080×1920 alike.

## Geometry

- Radius `0.7u` on chips, `50%` on badges and rings.
- Padding `0.9u` vertical, `1.6u` horizontal.
- Accent rule: `0.5u` wide, full height, on the leading edge of a card.
- Gap between a mark and its label: `1.2u`.

## Motion

Entrances are fast and from the direction the eye is already travelling; exits are faster
and get out of the way.

- Entrance `0.4–0.45s`, `power3.out`, from `y: +24px, opacity: 0` (or `x: -20px` for
  something anchored to the left edge).
- A mark that wants attention: `scale` from 0.85 with `back.out(2.5)`, `0.45s`.
- Exit `0.3–0.35s`, `power2.in`, anchored to the clip's real length
  (`Math.max(0.5, duration - 0.35)`), never to a constant.
- Nothing loops except a deliberate pulse on a pointer, and that stays under 20% opacity
  travel.

## Legibility over footage

- Keep overlays inside 5% of the frame on every side; a lower third's text starts at 6%.
- Anything over live footage sits on Surface or Scrim. White text straight on video is
  legible until the moment it is not, and that moment is a frame you will not catch.
- One line of a label must stay on screen for at least `1.2s` after it has finished
  animating in; two lines, `2s`.

## Applying it in kuma

Overlay entries expose these as vars: `accent` takes the accent above, `size` scales `u`,
`at`/`anchor` place the element as a fraction of the frame. The values in `tokens.css`
are the same numbers, as custom properties, for a template that wants to declare them
once — copy the `:root` block into the template's first `<style>` and reference
`var(--ds-accent)` from there.
