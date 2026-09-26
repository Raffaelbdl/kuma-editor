# Generating footage MLT can cut

When the video needs footage to demonstrate cuts, dissolves, speed or push-ins, and there is
none, draw it. `scripts/pixel_footage_template.py` is a working starting point: a pixel-art
seascape with a mountain, sun and sea, in a dusk and a night variant.

## Recipe

1. **Draw small.** 320×180 in a limited palette, with gradients quantised by a 4×4 Bayer
   matrix (ordered dithering). That is the whole "retro" look, and it is cheap in NumPy.
2. **Layers, back to front:**
   - sky, stars, clouds, sun or moon;
   - far hills, the hero shape (a mountain from a per-column ridge profile), near hills;
   - sea with scrolling wave dashes and a glint column under the light;
   - foreground props.
3. **A frame is a function of `t` only.** Seeded random generators, no state carried from
   frame to frame. The same `t` always draws the same frame, and re-running reproduces the
   file byte for byte.
4. **Upscale by an even integer** with nearest neighbour (×6 to 1920×1080): crisp pixels,
   and even sizes suit yuv420p.
5. **Pipe raw `rgb24` frames into ffmpeg**: `libx264 -preset slow -crf 14 -pix_fmt yuv420p
   -movflags +faststart`. Save the middle frame as a PNG still for panels that show the
   scene frozen.
6. `add_media` each file.

## Gotchas

- **Length**: longer than any clip will read into it. For every placement,
  `in + (end - start) × speed ≤ duration`. 14 s was too short for a push-in reading from
  2 s at 1.0×, which got a 0.9× timewarp as a workaround.
- **Variants share geometry.** Dusk and night draw the same mountain, torii and boat in the
  same places from the same functions, with different palettes, so a dissolve between
  them lines up.
- **Look before encoding.** Render stills at t = 0 / middle / end at ×3 and check the
  obvious: the lit side faces the light, stripes and bands sit where intended, nothing
  clips at the frame edge. The first pass needed two such fixes.
- **Never overwrite** media a running render or preview is reading; write a new name and
  `add_media` it.
