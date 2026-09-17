# Broadcast

A light, editorial overlay language for interviews, documentary and news-style cuts:
type does the work, the accent is a rule rather than a fill, and nothing bounces.

Category: interviews, talking heads, field footage, corporate and conference video.

## What it is for

Footage of people and places — mid-tone, moving, often graded. Overlays here are captions
in the journalistic sense: they name, they attribute, and they leave. The restraint is the
style; a "designed" overlay on an interview reads as an advert.

## Colour

| Role | Value | Where |
|---|---|---|
| Ink | `#0f1115` | Primary text, on a light surface. |
| Surface | `rgba(255, 255, 255, 0.94)` | The band or card behind the text. |
| Ink-2 | `#565b66` | Role, title, location — the second line. |
| Accent | `#c8102e` | A rule, an underline or a single word. Never a fill behind text. |
| Scrim | `linear-gradient(transparent, rgba(0, 0, 0, 0.45))` | Under a full-width band on dark footage. |
| Hairline | `rgba(15, 17, 21, 0.12)` | Dividers inside a card. |

## Type

System serif for names, system sans for everything else:
`Georgia, "Iowan Old Style", serif` and `"Segoe UI", system-ui, sans-serif`.

- Name: `3u`, serif, weight 600, tracking `-0.005em`.
- Role: `1.9u`, sans, weight 500, Ink-2, tracking `0.02em`.
- Caption / location: `1.8u`, sans, weight 600, uppercase, tracking `0.08em`.
- Never below `1.6u`.

ALL CAPS always takes at least `0.06em` of tracking; without it the letters collide and
the line reads as a block rather than as words.

## Geometry

- Square corners. Radius `0` is the signature; a rounded corner belongs to the other
  system.
- Accent rule `0.35u` on the leading edge, full height of the name line only.
- Band padding `1.2u` / `2u`. Two lines maximum.
- Hairline between name and role when they share a card.

## Motion

Nothing overshoots. Everything wipes or fades along the reading direction.

- Entrance `0.5s`, `power2.out`: a `clip-path` wipe from the leading edge, or opacity
  with `x: -12px`.
- The second line follows the first by `0.12s`, never simultaneously.
- Exit `0.4s`, `power1.in`, anchored to `duration - 0.4`.
- No scale, no rotation, no bounce, no loop.

## Legibility over footage

- Safe area 6% on every side; a lower third sits with its baseline at 78–82% of frame
  height, clear of burned-in captions.
- The band is opaque enough to carry Ink at 4.5:1 over any footage — that is what the
  0.94 alpha is for. Do not thin it to "let the picture through".
- A name is on screen for at least `2.5s`. Below that, the viewer reads it or watches the
  person, not both.

## Applying it in kuma

`accent` takes the accent above; `size` scales the type; `at`/`anchor` place the band.
`tokens.css` carries the same values as custom properties — copy its `:root` block into
the template's first `<style>` and reference `var(--ds-*)` from there.
