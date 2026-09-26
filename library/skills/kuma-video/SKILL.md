---
name: kuma-video
description: Make or edit a whole video in kuma-editor through its MCP — explainers and motion-graphics videos built from many small HTML overlays plus MLT footage. Use when planning or storyboarding a kuma video, authoring its overlays, editing a large kuma project with apply_edits, fixing fonts or Japanese/CJK text, mixing audio loudness, tightening pacing, or rendering and reviewing a kuma video.
---

# Making a video with kuma

The MCP's `timeline_guide` and `overlay_guide` are the contract. This skill is the craft
around them: how to organise a video of hundreds of clips, and the traps that each cost a
render to find while making one (a 9½-minute explainer: 26 overlay components, ~400 entries,
~600 clips). `kuma init` and `kuma mcp-config` install it into the project and refresh it,
so keep project notes elsewhere.

## Every session, first

1. Read `timeline_guide` and `overlay_guide`; they change with kuma. A tool result that
   starts "kuma was rebuilt after this MCP server started" means your tools are the old
   build's: say so and ask for the MCP server to be restarted.
2. `health`, `list_presets` (note what is `available`, e.g. `youtube-nvenc`), `list_fonts`
   (note each family's `scripts`: a family without `han`/`kana` cannot set Japanese).
3. `validate_project`. Besides errors, it warns about every entry whose font is not
   installed (`health`'s `blueprint_dirs` say which libraries the daemon reads; `kuma
   library` installs the starter fonts) and every character an entry's fonts cannot set.

## The shape of a video

- **One sequence per chapter**, and a master that places them end to end, with the
  wipes on the cuts, the music cues crossfading under them and the whoosh on each cut.
  Chapters can be sampled and rendered on their own (`sequence`).
- **One overlay, one job.** A title, the box it sits in and the arrow beside it are three
  entries and three clips, each on its own track. Write a few var-driven components
  (label, narration, panel, arrow, bubble…) in `overlays/` and configure them many times.
  House-style helpers go in `overlays/_vendor/`.
- **Tracks by layer band.** Video: paper < page < footage. Overlays: tone < panels <
  content < text < characters < narration < wipes. Audio: sfx, music. Allocate tracks per
  band (first free lane), so the stacking follows the band, not the order you wrote things.
- Anything MLT should cut, dissolve, speed-ramp or push into is **media**, not an overlay:
  generate it if you have to (`references/footage.md`). Stills (paper texture, screenshots)
  are image media on the bottom tracks, which is far cheaper than an overlay.

## Work as code past ~50 clips

Keep the whole video in a `storyboard.py` with a small DSL, generate the patches, and
send only the difference: `references/storyboard-as-code.md`. In short:

- `diff.py` compares the storyboard with the live project and writes the smallest JSON
  Patch to a file in the project. Apply it with `apply_edits` and `patch_path`: one call,
  exact, whatever its size, with the `base_revision` the diff read.
- **Run the diff again afterwards: it must print 0 ops.** That proves nothing was left out,
  and that nobody else's edit is being overwritten.
- Clip ids are stable (`<entry>_<start ms>`); pin the ids of entries you insert later.
- Retime with cuts (a time warp applied at build), never by editing every time by hand.

## Traps, and the rule each one taught

| Symptom | Cause | Rule |
|---|---|---|
| Kanji look Chinese, or odd | a Latin-font label fell back to the machine's CJK font under `lang="en"` (Noto CJK **SC** on Linux) | `lang="ja"`, a `jp` fallback font var, JP-face fonts with full JIS coverage; `validate_project` names the characters and the page → `references/japanese-type.md` |
| Viewers don't know what 承 means | only the on'yomi ("SHŌ") was given | gloss kanji with the kun'yomi verb and its English sense (承ける *ukeru*, "take it up") |
| `<!--` drawn as an arrow | programming ligatures in the mono font | `font-feature-settings: "liga" 0, "calt" 0` on code |
| A clip's `in` runs past the source | generated footage too short | make footage longer than any clip reads into it |
| "keyframe beyond clip end" after a trim | keyframes were timed to the old length | compute keyframe times from the clip's actual length |
| Inserting one entry renamed every later one | counter-based entry ids | pin ids of new entries; content-based clip ids |
| The mix is 10 dB too quiet | kuma has no limiter, and gains were guessed | `measure_audio` before rendering → `references/audio.md` |
| Scenes read fine but the video drags | a steady reading rhythm with no beats | hook, page turns, camera hits, snappy motion → `references/pacing.md` |
| A tweak costs 20 minutes | a `_vendor/` or font change re-bakes every overlay | decide motion and type early, batch such changes, review with the `draft` preset |

## Review loop

- **While drawing**, sample the overlay entry on its own, or its sequence over a short
  range. Sampling the master bakes every changed overlay it shows: `sample_frames` with
  `times` bakes only what those frames show, and `tile` returns them as one contact sheet.
- **Before the final render**: `validate_project` (no font warnings), diff at 0 ops,
  `measure_audio` (loudness overall and per chapter, true peak, the loudest moments,
  silences), `sample_frames` with `times` at every cut ±0.3 s and `tile`, and
  `scripts/reading_audit.py` (text on screen long enough to read, holds not too long).
- **After it**, check:
  - duration (ffprobe), and the render result's `warnings`;
  - loudness and true peak of the file itself (ebur128), which should read what
    `measure_audio` did;
  - a contact sheet of every cut ±0.3 s and each key scene, from the file
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
| `subset_jp_fonts.py` | only to shrink a project (`add_font` of the full collection is simpler): the JP face, JIS X 0208 and the project's text |
| `reading_audit.py` | per scene: when the last text can have been read, and how long the scene then holds |
| `contact_sheet.py` | frames of a rendered file at given times, tiled into one image (before rendering, `sample_frames` with `times` and `tile`) |
| `pixel_footage_template.py` | a deterministic pixel-art seascape, encoded as real H.264 footage; adapt it |

## References

- `references/storyboard-as-code.md`: the DSL, stable ids, cuts, the diff, applying it
- `references/overlays.md`: authoring gotchas beyond the contract
- `references/japanese-type.md`: fonts, fallback, glyph forms, glossing kanji
- `references/audio.md`: loudness with gains only, measuring the mix, SFX peaks, music cues
- `references/pacing.md`: reading speed, hooks, page turns, camera hits, motion
- `references/footage.md`: generating footage MLT can cut
- `references/render-and-review.md`: costs, presets, versions, the checklist with commands
