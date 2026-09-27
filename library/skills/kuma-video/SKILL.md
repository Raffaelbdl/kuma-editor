---
name: kuma-video
description: Make or edit a video in kuma-editor through its MCP — screen recordings and tutorials, talking heads, vlogs and montages, shorts, explainers and motion graphics — from footage, titles and overlays, subtitles, music and effects. Use when planning, assembling or editing a kuma project, cutting footage, authoring overlays, adding subtitles, fixing fonts or CJK text, mixing loudness, tightening pacing, or rendering and reviewing a kuma video.
---

# Making a video with kuma

The MCP's `timeline_guide` and `overlay_guide` are the contract. This skill is the craft
around them: how to organise a video, and the traps that each cost a render to find. The
largest project it was learned on is a 9½-minute motion-graphics explainer (~400 overlay
entries, ~600 clips), so its examples lean that way; the rules hold for any video. `kuma
init` and `kuma mcp-config` install it into the project and refresh it, so keep project
notes elsewhere.

## Every session, first

1. Read `timeline_guide` and `overlay_guide`; they change with kuma. A tool result that
   starts "kuma was rebuilt after this MCP server started" means your tools are the old
   build's: say so and ask for the MCP server to be restarted.
2. `health`, `list_presets` (note what is `available`, e.g. `youtube-nvenc`),
   `list_blueprints` (ready-made titles, lower thirds, subtitles, callouts…) and
   `list_fonts` (each family's `scripts`: one without `han`/`kana` cannot set Japanese).
3. `validate_project`. Besides errors, it warns about every entry whose font is not
   installed and every character an entry's fonts cannot set.

## Know what you are making

| Video | Built mostly from | Read first |
|---|---|---|
| Screen recording, tutorial, talking head | footage and speech; subtitles, lower thirds, zooms | `footage.md`, `audio.md` |
| Vlog, travel, montage | many short clips, music, titles | `footage.md`, `pacing.md` |
| Short (9:16) | reframed footage, large captions | `footage.md` (portrait) |
| Explainer, motion graphics | many small overlays, little or generated footage | `overlays.md`, `storyboard-as-code.md` |

Most videos mix these. The shape below holds for all of them.

## The shape of a video

- **A sequence per part** (chapter, segment, scene), and a master that places them end to
  end, with transitions and music crossing the cuts. A part can be sampled, measured and
  rendered on its own (`sequence`). A short video can live on the master alone.
- **Tracks by layer band**, bottom to top. Video: backgrounds < main footage < cutaways.
  Overlays: panels < titles and callouts < captions < transitions. Audio: voice, music,
  effects. Put each clip in the first free lane of its band, so the stacking follows the
  band, not the order you added things.
- **Media for what MLT should cut, dissolve, speed up or push into; overlays for text,
  graphics and motion.** Stills (a texture, a screenshot, a logo) are image media, far
  cheaper than an overlay.
- **One overlay, one job**: a title, the box behind it and an arrow are three entries and
  three clips. Start from a blueprint (`add_blueprint`, then set vars); write a component
  only for what none does, and configure it many times.

## Work as code past ~50 clips

Keep the video in a script that builds the project, generate the patch, and send only the
difference: `references/storyboard-as-code.md`. In short:

- A diff compares the script's project with the live one and writes the smallest JSON Patch
  to a file in the project. Apply it with `apply_edits` and `patch_path`: one call, exact,
  whatever its size, with the `base_revision` the diff read.
- **Run the diff again afterwards: it must print 0 ops.** That proves nothing was left out,
  and that nobody else's edit is being overwritten.
- Clip ids from content (`<entry>_<start ms>`); pin the ids of entries you insert later.
- Retime with cuts (a time warp applied when the script builds), never by editing every
  time by hand.

## Traps, and the rule each one taught

| Symptom | Cause | Rule |
|---|---|---|
| A clip's `in` runs past the source | media shorter than a placement reads into it | `in + (end − start) × speed ≤` the media's duration |
| A dissolve refuses to compile ("no handle") | the source has nothing before the incoming clip's `in` | leave half the transition's length of source on each side of the cut |
| "keyframe beyond clip end" after a trim | keyframes timed to the old length | compute keyframe times from the clip's actual length |
| A correction costs minutes of baking | one long overlay (a whole transcript) bakes as one clip | split subtitles and long overlays into pieces of about a minute |
| A tweak costs 20 minutes | a `_vendor/` or font change re-bakes every overlay | decide motion and type early, batch such changes, review with the `draft` preset |
| Kanji look Chinese, or odd | the machine's CJK fallback font under `lang="en"` (Noto CJK **SC** on Linux) | `lang="ja"`, a Japanese font var; `validate_project` names the characters → `references/japanese-type.md` |
| `<!--` drawn as an arrow | programming ligatures in a mono font | `font-feature-settings: "liga" 0, "calt" 0` on code |
| Inserting one entry renamed every later one | counter-based entry ids | pin ids of new entries; content-based clip ids |
| The mix is 10 dB too quiet | kuma has no limiter, and gains were guessed | `measure_audio` before rendering → `references/audio.md` |
| Every shot is fine but the video drags | an even rhythm with no beats | a hook, a beat on each change, trimmed holds → `references/pacing.md` |

## Review loop

- **Feedback arrives as markers.** A person reviewing in the editor presses M and types a
  comment; "see the markers" means every sequence's `markers` (`t`, `label`) in
  `get_project`. Sample each one's moment before changing anything, answer per marker,
  and leave them for the person to clear (`timeline_guide` has the details).
- **While building**, sample the part you are working on, or an overlay entry on its own,
  over a short range. Sampling the master bakes every changed overlay it shows:
  `sample_frames` with `times` bakes only what those frames show, and `tile` returns them
  as one contact sheet.
- **Before the final render**: `validate_project` (no font warnings), the diff at 0 ops if
  you work as code, `measure_audio` (loudness overall and per part, true peak, the loudest
  moments, silences), `sample_frames` with `times` at every cut ±0.3 s and `tile`, and
  `scripts/reading_audit.py` if the video shows text to read.
- **After it**, check:
  - duration (ffprobe), and the render result's `warnings`;
  - loudness and true peak of the file itself (ebur128), which should read what
    `measure_audio` did;
  - a contact sheet of every cut ±0.3 s and each key moment, from the file
    (`scripts/contact_sheet.py`).

  Look at the frames. Keep the previous render under another name before re-rendering to
  the same path.
- Final preset: `youtube-1080p` (x264 slow, CRF 18), or `youtube-nvenc` where available
  (much faster, somewhat larger). `threads`, `quality` and `speed` change one render only.

## Scripts

Run them from the project directory: `python3 .claude/skills/kuma-video/scripts/<name>`.
They need numpy, Pillow and fontTools; if pip refuses (an externally managed Python), make
a venv: `python3 -m venv .venv && .venv/bin/pip install numpy pillow fonttools`.

| Script | Does |
|---|---|
| `reading_audit.py` | every overlay clip showing text: when it can have been read, and how long it then stays |
| `contact_sheet.py` | frames of a rendered file at given times, tiled into one image (before rendering, `sample_frames` with `times` and `tile`) |
| `pixel_footage_template.py` | generated footage: a deterministic pixel-art seascape, encoded as real H.264; adapt it |
| `subset_jp_fonts.py` | only to shrink a project with Japanese text (`add_font` of the full collection is simpler) |

## References

- `references/footage.md`: bringing footage in, cutting it, speed, movement, transitions, portrait, generating it
- `references/overlays.md`: authoring overlays beyond the contract
- `references/audio.md`: loudness with gains only, voice and music, measuring the mix
- `references/pacing.md`: readable text, hooks, beats, punch-ins, trimming
- `references/storyboard-as-code.md`: a video as a script, stable ids, cuts, the diff, applying it
- `references/japanese-type.md`: CJK fonts, fallback, glyph forms, glossing words
- `references/render-and-review.md`: costs, presets, versions, the checklist with commands
