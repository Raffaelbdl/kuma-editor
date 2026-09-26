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

1. Read `timeline_guide` and `overlay_guide`; they change with kuma. If the guide or
   `list_presets` names an option your `render` tool does not take (say `speed`), the MCP
   process is older than the daemon: say so and ask for it to be restarted.
2. `health`, `list_presets` (note what is `available`, e.g. `youtube-nvenc`), `list_fonts`
   (note each family's `scripts`: a family without `han`/`kana` cannot set Japanese).
3. `check_overlay` on one overlay. `font_missing` for JetBrains Mono, Caveat, etc. means
   the daemon cannot see kuma's font library: `kuma library` once, or a daemon started with
   `KUMA_LIBRARY=<kuma-editor>/library`.

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
  Patch. Send it with `apply_edits`, split into chunks of ≤ ~25 KB in an order that keeps
  every intermediate state valid.
- **Run the diff again afterwards: it must print 0 ops.** That proves the transcription was
  exact, and that nobody else's edit is being overwritten.
- Clip ids are stable (`<entry>_<start ms>`); pin the ids of entries you insert later.
- Retime with cuts (a time warp applied at build), never by editing every time by hand.

## Traps, and the rule each one taught

| Symptom | Cause | Rule |
|---|---|---|
| A box renders tiny in a corner, no error | a lone `'` or `"` in a string var broke every CSS var after it | typographic apostrophes (’); assert balanced quotes in the DSL |
| Kanji look Chinese, or odd | a Latin-font label fell back to the machine's CJK font under `lang="en"` (Noto CJK **SC** on Linux) | `lang="ja"`, a `jp` fallback font var, JP-face fonts with full JIS coverage, `scripts/fontcheck.py` → `references/japanese-type.md` |
| Viewers don't know what 承 means | only the on'yomi ("SHŌ") was given | gloss kanji with the kun'yomi verb and its English sense (承ける *ukeru*, "take it up") |
| 93 s of music silent | a fade that rounds to 1 frame, on a clip starting at frame 0, compiles to `in=0 out=0`, which MLT reads as the whole clip | fades ≥ 2 frames; check the first seconds of every render for silence |
| `<!--` drawn as an arrow | programming ligatures in the mono font | `font-feature-settings: "liga" 0, "calt" 0` on code |
| `sample_frames` with `overlay` refuses: parent not in the bin | the one-clip sample copies the entry, not its folder | sample the sequence instead |
| A clip's `in` runs past the source | generated footage too short | make footage longer than any clip reads into it |
| "keyframe beyond clip end" after a trim | keyframes were timed to the old length | compute keyframe times from the clip's actual length |
| Inserting one entry renamed every later one | counter-based entry ids | pin ids of new entries; content-based clip ids |
| The mix is 10 dB too quiet | kuma has no limiter, and gains were guessed | model the mix offline before rendering (`scripts/mixsim.py`) → `references/audio.md` |
| Scenes read fine but the video drags | a steady reading rhythm with no beats | hook, page turns, camera hits, snappy motion → `references/pacing.md` |
| A tweak costs 20 minutes | a `_vendor/` or font change re-bakes every overlay | decide motion and type early, batch such changes, review with the `draft` preset |

The first, fourth and sixth rows are kuma bugs at the time of writing. Check whether they
still hold (a var containing `Let's`, a 0.05 s `audio_fade` at 0) before working around them.

## Review loop

- **While drawing**, sample the overlay entry on its own, or its sequence over a short
  range. Sampling the master bakes every changed overlay in it: do that once, on purpose,
  or render `draft` and pull frames from the file (`scripts/contact_sheet.py`).
- **Before the final render**: `validate_project`, diff at 0 ops, `scripts/fontcheck.py`,
  `scripts/mixsim.py` (loudness and peak), and `scripts/reading_audit.py` (text on screen
  long enough to read, holds not too long).
- **After it**, check:
  - duration (ffprobe);
  - loudness overall and per chapter, and true peak (ebur128);
  - 2 s volume windows over the opening (silence);
  - a contact sheet of every cut ±0.3 s and each key scene.
  
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
| `mixsim.py` | models the project's audio mix; prints the peak, and with `--lufs` the loudness overall and per chapter; `--gain bin=vol` tries gains without editing |
| `fontcheck.py` | every CJK character in every text var is drawn by a project font (the overlay's own, or its `jp` fallback) |
| `subset_jp_fonts.py` | only to shrink a project (`add_font` of the full collection is simpler): the JP face, JIS X 0208 and the project's text |
| `reading_audit.py` | per scene: when the last text can have been read, and how long the scene then holds |
| `contact_sheet.py` | frames of a video at given times, tiled into one image |
| `pixel_footage_template.py` | a deterministic pixel-art seascape, encoded as real H.264 footage; adapt it |

## References

- `references/storyboard-as-code.md`: the DSL, stable ids, cuts, the diff, chunked applying
- `references/overlays.md`: authoring gotchas beyond the contract
- `references/japanese-type.md`: fonts, fallback, glyph forms, glossing kanji
- `references/audio.md`: loudness with gains only, the mix model, SFX peaks, music cues
- `references/pacing.md`: reading speed, hooks, page turns, camera hits, motion
- `references/footage.md`: generating footage MLT can cut
- `references/render-and-review.md`: costs, presets, versions, the checklist with commands
