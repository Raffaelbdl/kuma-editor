# Footage

What a video films, records or generates. It goes in as media and MLT cuts it, which
is also what makes it cheap: nothing is baked.

## Bringing it in

- `add_media` with `paths` or a folder adds them in one edit and says what it skipped
  (`probe_media` first to see a file's length, size, rate and audio). Keep originals under
  `media/` and never overwrite one a render or the preview is reading: write a new name and
  add that. If a file was replaced anyway, `reprobe_media` reads it again.
- SVG is refused: rasterise it first (`rsvg-convert`, or ffmpeg where it has librsvg).
- One entry per source, placed as many times as it is cut. A clip is a placement: `in` is
  where it reads from in the source, `start`/`end` where it sits on the timeline.
- Mixed frame rates and sizes are fine; the project's `fps`, `width` and `height` are what
  renders.

## Finding moments

- **Contact sheets** of a source, coarse first, then dense around the candidates:
  `ffmpeg -i f.mp4 -vf "fps=1/5,scale=240:-2,drawtext=text='%{pts\:hms}':x=4:y=4:fontcolor=white,tile=8x4" -frames:v 1 sheet.png`
  is one frame every 5 s; then every 0.5–1 s over the stretch that matters.
- **Exact cuts** in edited material: `ffmpeg -i f.mp4 -vf "select='gt(scene,0.3)',showinfo" -f null -`
  and read each `pts_time`.
- **Long files** (a 24-minute episode, a 20-minute walk): hand the scouting to a sub-agent
  with the exact lines to cover and the lengths needed, then check its picks with
  `sample_frames`. Its times are right to the shot, not always to the frame.
- Keep every image you look at small: `width` 300–600 and `tile`. Full-size frames are
  expensive to read.

## Cutting

- **Removing a stretch** is two clips of one entry: the second starts where the first ends
  and reads from further in (`in` advanced by the stretch you removed).
- **Speech**: `transcribe` a recording first. Its cues are one per word, in source seconds,
  so the gaps between consecutive cues are the pauses, and a stumble is a run of words to
  drop. Cut in the gaps, a few hundredths of a second after the last word and before the
  next.
- **Subtitles after cutting**: one `subtitles` entry per piece, holding that piece's cues,
  its `offset` the piece's `in`, placed over the piece. `transcribe` from a clip does this
  itself, cut at pauses into entries of about a minute, which keeps bakes short.
- **Length**: `in + (end − start) × speed` must stay within the source. The frames a clip
  reads past the end show the tracks below and are silent; the validator counts them in
  whole frames, so a sound a fraction of a frame short of its clip is fine.

## Speed, movement, colour

- `speed` on a clip plays it faster or slower, keeping the pitch (`timewarp`). A sequence
  cannot be sped up: render it and place the file.
- `transform` keyframes move the picture:
  - a **punch-in** on a talking head for emphasis: scale 1 → 1.08 held for the sentence,
    then back on the next cut; alternating framings hide jump cuts;
  - a **slow push** on a still (Ken Burns): scale 1 → 1.1 over the clip, linear;
  - hits and shakes: `pacing.md`.
  Keep the scale ≥ 1 wherever the picture moves, or the frame's edge shows.
- `freeze` holds the frame a clip shows at `t` (seconds into the clip) for the whole clip,
  while its sound runs on: a scene that stops to become a picture (shrinking into a
  timeline, flying into a frame) is the scene's own clip, frozen, with a `transform`.
  Anything placeable freezes, a nested sequence included, so it never goes stale. It holds
  only what that clip shows: a title on a track above it is not in the frame, and vanishes
  when the frozen picture starts to move, so freeze at an instant with no title on screen.
- `crop` to reframe: fractions of the *frame*, not of a letterboxed still inside it, and a
  `radius` of 1 rounds the shorter side into semicircles (a circle on a square crop). Crop a
  still's letterbox away first, or the rounding lands in the empty corners.
- **A picture in a circle that moves** is `crop` then `transform` on one clip: effects run
  in list order, and a clip takes one of each kind. The crop cuts a square (in pixels)
  around the point (cx, cy) with radius r, anywhere in the frame — `left = (cx − r)/W`,
  `right = (W − cx − r)/W`, `top = (cy − r)/H`, `bottom = (H − cy − r)/H`, `radius: 1` — and
  the transform, which scales about the frame's centre, puts that circle's centre at
  (px, py) with `x = px − (W/2 + (cx − W/2)·s)`, `y = py − (H/2 + (cy − H/2)·s)`. The other
  way round, the crop's window stays put and cuts whatever the transform moved into it. A
  picture that moves inside a circle that stays (a push within a round frame) is the
  clip with its transform in a sequence, and the crop on the clip placing it.
- `color` (brightness, contrast, saturation, gamma) to match clips shot
  in different light.

## Stills

- A still is **fitted** into the frame (letterboxed or pillarboxed). To fill the frame with
  a `w`×`h` picture in a `W`×`H` frame: `transform` scale `max(W/w, H/h) / min(W/w, H/h)`.
- `transform` scales about the centre; `x` positive moves right, `y` positive moves down,
  in frame pixels. To centre a point (cx, cy) of a picture that fills the frame, at scale
  `s`: `x = (W/2 − cx)·s`, `y = (H/2 − cy)·s`. Keep `|x| ≤ (s − 1)·W/2` and
  `|y| ≤ (s − 1)·H/2`, or the edge shows.
- Ways to show one, each worth a helper in a script:
  - **cover and push**: fill the frame, slow scale-up;
  - **card**: the whole picture over a blurred, dimmed copy of itself that fills the frame
    (two clips of one entry);
  - **tilt**: a portrait picture filling the width, the frame travelling between two
    heights. Only when the picture is wide enough (≥ ~1000 px for a 1920 frame): a 400–700 px
    portrait upscaled 2.5–4× goes soft, and stays a card. A narrow travel keeps only the
    part you want, a title and faces say;
  - **pillar**: 4:3 archive footage over a blurred copy of itself;
  - **inset**: a picture over the main one, fading in and out.
- **Transparency shows what is below**, and with nothing below, the master's black: a map
  or a sketch drawn for a white page turns unreadable. Put a white colour clip under it, or
  flatten it first:
  `ffmpeg -i in.png -filter_complex "color=white:s=WxH[bg];[bg][0]overlay=format=auto:shortest=1" out.png`.

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
