# Footage

What a video films, records or generates. It goes in as media and MLT cuts it, which
is also what makes it cheap: nothing is baked.

## Bringing it in

- `add_media` each file (`probe_media` first to see its length, size, rate and audio).
  Keep originals under `media/` and never overwrite one a render or the preview is reading:
  write a new name and add that.
- One entry per source, placed as many times as it is cut. A clip is a placement: `in` is
  where it reads from in the source, `start`/`end` where it sits on the timeline.
- Mixed frame rates and sizes are fine; the project's `fps`, `width` and `height` are what
  renders.

## Cutting

- **Removing a stretch** is two clips of one entry: the second starts where the first ends
  and reads from further in (`in` advanced by the stretch you removed).
- **Speech**: `transcribe` a recording first. Its cues are one per word, in source seconds,
  so the gaps between consecutive cues are the pauses, and a stumble is a run of words to
  drop. Cut in the gaps, a few hundredths of a second after the last word and before the
  next.
- **Subtitles after cutting**: one `subtitles` entry per piece, holding that piece's cues,
  its `offset` the piece's `in`, placed over the piece. That is also the split into pieces
  of about a minute that keeps bakes short (`timeline_guide`).
- **Length**: `in + (end − start) × speed` must stay within the source. A clip that reads
  past the end shows its last frame or nothing.

## Speed, movement, colour

- `speed` on a clip plays it faster or slower, keeping the pitch (`timewarp`). A sequence
  cannot be sped up: render it and place the file.
- `transform` keyframes move the picture:
  - a **punch-in** on a talking head for emphasis: scale 1 → 1.08 held for the sentence,
    then back on the next cut; alternating framings hide jump cuts;
  - a **slow push** on a still (Ken Burns): scale 1 → 1.1 over the clip, linear;
  - hits and shakes: `pacing.md`.
  Keep the scale ≥ 1 wherever the picture moves, or the frame's edge shows.
- `crop` to reframe, `color` (brightness, contrast, saturation, gamma) to match clips shot
  in different light.

## Transitions

- A dissolve or a wipe joins two clips on one track (`transitions`). It overlaps them by
  its length, centred on the cut, so each source needs half of it beyond the cut: before
  the incoming clip's `in` and after the outgoing clip's end. Without it the compile
  refuses ("no handle before its in point").
- Use them for a change of place, time or topic. Between shots of one scene a straight cut
  reads better.

## Portrait (shorts)

- 9:16 is only `width`/`height` (1080×1920); every preset renders it.
- Landscape footage in a portrait frame: `transform` scale to fill and an `x` offset to
  keep the subject in frame, checked with `sample_frames`.
- Check every overlay in the orientation it will render: `sample_frames` with `overlay`
  and `frame: [1080, 1920]`.

## When there is no footage: generate it

When a video needs something MLT can cut, dissolve, speed or push into and there is no
footage, draw it. `scripts/pixel_footage_template.py` is a working starting point: a
pixel-art seascape in a dusk and a night variant.

- **A frame is a function of `t` only**: seeded generators, no state carried from frame to
  frame, so re-running reproduces the file byte for byte.
- **Draw small and upscale by an even integer** with nearest neighbour (320×180 × 6 =
  1920×1080), and pipe raw `rgb24` frames into ffmpeg (`libx264 -crf 14 -pix_fmt yuv420p
  -movflags +faststart`). A limited palette with ordered (Bayer) dithering is the whole
  retro look.
- **Longer than any placement reads into it**: 14 s was too short for a push-in reading
  from 2 s.
- **Variants share geometry**: dusk and night draw the same shapes in the same places with
  different palettes, so a dissolve between them lines up.
- **Look before encoding**: stills at the start, middle and end, checked for the obvious
  (the lit side faces the light, nothing clips at the edge).
