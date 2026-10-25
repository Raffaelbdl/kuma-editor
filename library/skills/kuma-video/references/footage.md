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
- `grade` to match clips shot in different light, and to give a video one look. Fix before
  you style: white balance (`temperature`, `tint`) and `exposure` first, so skin and greys
  look right shot to shot; then the wheels — `lift` for the shadows, `gain` for the
  highlights, `gamma` for the middle, as `{r, g, b}` (warm shadows: `lift {r: 0.02, b: -0.02}`)
  — and `contrast`, a gentle S `curve` (`[[0.25, 0.22], [0.75, 0.8]]`) or `saturation` last.
  Small numbers: 0.05 on a wheel is already visible. To recover a part of the range rather
  than the whole: `shadows: 0.15` opens dark faces, `highlights: -0.2` brings a bright sky
  back, `blacks`/`whites` set the ends. `vibrance` (0.2) livens dull colours without pushing
  skin; for one colour only, the hue curves — `hue_sat: [[0.6, -0.4]]` greys that hue,
  `hue_hue` turns it towards its neighbours, `hue_luma` darkens it (a sky) — with hue in
  turns round the vectorscope (skin near 0.34). `luma_sat: [[0.1, -0.5]]` takes colour out of
  the shadows, where noise lives. Log footage (flat, grey, washed out
  straight from the camera) wants converting before anything else — on the bin entry
  (`/bin/<id>/effects: [{"type": "grade", "log": …}]`), so every clip of it converts and a
  split cannot lose it: `log` names the camera's
  (`s-log3-cine`, `v-log`, `logc3`, `apple-log`, `d-log`, … — kuma does the maker's maths, mid
  grey to mid grey, highlights rolled off), or the maker's `.cube` as `input_lut`; a creative `.cube` goes in `look`,
  softened by `look_amount`. Sample a frame of each clip side by side after grading, and
  compare a neutral in each (a white wall, a grey shirt) rather than the whole picture.
  `color` (brightness, contrast, saturation, gamma) still works and can be keyframed. A
  grade's light can be keyed (`keyframes: [{t, exposure?, temperature?, tint?}]`) for light
  that changes in the shot; `match` on such a grade answers a key at the moment measured,
  so measure the start and the end of the change and match each. Anything more than the
  light changing is a split.
- **Part of the picture**: a `mask` on `grade`, `color` or `blur` limits it to a shape. Read
  the region off a `sample_frames` frame (composition pixels, top-left and size, as `region`)
  and give it as `mask: {x, y, w, h, feather}` — an ellipse unless `shape: "rectangle"`. A
  face to hide: `blur` `radius: 24` with the face's region and `feather: 20`. A vignette:
  `grade` `exposure: -0.7` with an ellipse a little inside the frame, `invert: true`,
  `feather` a fifth of the height. A sky too bright: a rectangle over it, `exposure` down. Each
  window is its own effect (a clip takes any number masked). A subject that moves: sample
  frames where it turns (`times`), read its region off each, and give the mask a key per
  frame, `keyframes: [{t, x, y}, …]` (`t` from the clip's start; straight lines between, so
  a curved path wants more keys). Check frames *between* the keys before rendering.
- **Grade by measuring.** `measure_color` with a `region` on something that should match in
  life (a face, a white wall, a grey shirt — never the whole frame, whose content differs)
  gives its `exposure` and `balance` as grade numbers; compare shots by those. To fix one,
  `match: {clip, reference}` with `{"neutral": true}` (the region is white or grey) or
  `{"t": <a moment of the hero shot>, "region": …}` returns the clip's grade and a `patch`
  for `apply_edits`. `reached: false` means the reference is past the grade's ranges or
  clipped highlights hold it back: say so rather than push. `clipped` above a few percent on
  a face or sky is lost detail no grade brings back.
- **One grade on many clips** (a look over the whole video, one camera's correction): the
  same `grade` patched onto each, in one `apply_edits`; then match the shots that still differ.
- **Look at before and after**, at several moments of every clip you graded:
  `sample_frames` with `times` across each clip, `tile: true` and `before: {"without":
  {"kinds": ["grade"]}}` (or `{"revision": <the one before your edit>}`) gives one sheet, a
  row a moment, before on the left. A grade that fixes the moment you measured and spoils
  another — a face that leaves the light, a window that comes into shot — shows up there.

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
