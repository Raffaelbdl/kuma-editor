# Audio: gains, a master stage, and measuring

kuma mixes by summing clips with their `volume`, `volume` keyframes and `audio_fade`, then
through the master's own effects: a `volume` and a `limiter` on the master sequence
(`/bin/<master>/source/effects`) are the master stage, played in the preview, measured by
`measure_audio` and rendered. Loudness is whatever the gains make it: one first render came
out at **-24 LUFS**, 10 dB under what YouTube plays at, because the gains had been set
against nothing.

## Measure, then render

1. **Measure each source** once with `measure_media` (a bin entry or a file in the
   project): integrated loudness, range and true peak, a loudness `curve`, and its pauses.
   Note each file's crest (peak minus loudness): a sparse cue can have 19 dB of crest
   against 11 for a busy one, and needs less gain to sound as loud. A gain to reach a
   target: `10 ** ((target − L) / 20)`.
2. **Measure the mix** with `measure_audio`. It renders a sequence's sound through the real
   graph (every volume, fade and crossfade, nested sequences included, the master stage
   last; no overlay is baked) and returns integrated loudness, range and true peak, overall
   and per part (each clip that places a sequence, or the `sections` you give), the five
   loudest moments and every silent stretch of a second or more. **`solo` and `mute`** (track
   ids, nested ones too) measure the voice alone or the music alone, for that measurement
   only: never mute tracks by an edit to measure, and never `undo` to restore one, since
   undo takes back the last edit whoever made it, a person's included.
3. **Targets:**
   - -14 to -16 LUFS integrated for an upload (YouTube turns louder videos down to about
     -14, and does not turn quieter ones up);
   - a measured true peak ≤ -1.5 dBTP, which leaves room for the encoder;
   - parts within ±1 LU of each other, unless a quiet one is quiet on purpose.
4. **The master stage** brings the whole to the target: `volume` from the integrated figure,
   then a `limiter` to hold the peaks it raises. The limiter holds *sample* peaks at its
   `ceiling`, and true peaks land up to about a dB over it: a ceiling of -2.5 dB gave
   -1.7 dBTP on a narrated mix raised by 18 dB, where -2.0 gave -1.3. Measure again with
   `measure_audio` after setting it.
5. Render, and **measure the rendered file** (`ffmpeg -i out.mp4 -vn -af ebur128=peak=true -f null -`).
   Without a master stage, the same fix after the render is a fixed gain and a limiter with
   the picture copied:
   `ffmpeg -i in.mp4 -c:v copy -af "volume=10.2dB,alimiter=limit=0.75:attack=5:release=60:level=false" -c:a aac -b:a 384k out.mp4`.
   Not `loudnorm`: needing more gain than the headroom allows, it switches to its adaptive
   mode and rides the level, which undoes a careful balance between parts.

## Voice and music

- In a video with speech, the voice carries the loudness: set it first (`narration.md`
  for levelling it part by part), then the music under it.
- **The bed**: about **15 LU under the voice**, 18 for a busy track. Measured afterwards,
  beds set that way sat within 0.5 dB of their targets.
- **Ducking** is `volume` keyframes on the music clip: kuma has no sidechain. Down over
  ~0.3–0.5 s before speech starts, back up over ~0.8 s after it ends; hold keys
  (`ease: "hold"`) keep it flat in between. Under a person talking to camera, duck **deeper**
  than under voice-over: 5 dB more as a rule, 10 in a tense part, 16 under a spoken warning.
  Where nobody speaks (a chapter card), lift it: +8 dB over 0.8 s, and back with the card.
- **A track that drifts** (3 to 14 dB over a minute is common) makes a level bed impossible
  by fixed gain. Put `auto_level` on the music clip (a target, a window of a few seconds,
  bounds on the gain): it keeps the bed level, at the cost of flattening the track's own
  build; it settles over about one window after a seek or a sudden jump. Where the build
  matters, key the clip's `volume` from `measure_media`'s curve instead (`step: 10`).
- Speech recorded quietly is raised by the clip's `volume`; check its peaks with
  `hottest`.

## Cues

- Put music on the **master**, one cue per part or mood, and overlap neighbours by
  0.6–1.6 s around the cut on two lanes. `audio_fade` in 0.6–1.2 s, out 1.6 s. A cue can also
  stop on an event (a 0.25 s fade ending on a shutter's snap).
- Skip a sparse intro with the clip's `in`. Reusing a track later in the video, start it at
  a point where its loudness dips and rebuilds (`measure_media`'s curve shows where).
- Let a clip that brings its own music play alone: the next cue waits for it to end. A
  silent card can be a choice, not a gap.
- **Silences you did not mean** (a clip muted by mistake, music that ends early, a gap
  between cues) are in `measure_audio`'s `silences`. Only near-silence counts (under
  -70 LUFS): a pause in a recording is room tone, which is louder.
- Generated music: write each cue at exactly the length it will be placed at, to a **new
  file name**. Never overwrite a file a running render is reading.
- Long talking or reading stretches want a pulse under them (a soft kick and backbeat at
  ~90 bpm) rather than a drumless pad. See `pacing.md`.

## Effects and peaks

- **Align a sound effect's onset**, not its file's first sample, with the picture event: a
  click's press can be 0.04 s in, a shutter's snap 0.2 s. `measure_media` with a low
  `silence_db` (about -50) gives the onset as the end of the first silence.
- `measure_audio`'s `hottest` lists the loudest moments, at least 2 s apart. Nearly always
  it is a hit (a drum, a stamp, a chime) landing on the music's own peak, or a shout. Keep
  effects under about -5 dBTP (a click raised 1.6× reached -0.7) and trim the few hottest
  effects' gains, not the music.
- **The same effect repeated quickly stacks**: five thumps 0.18 s apart peaked 3 dB under
  full scale, each one quiet. When events come closer than about 0.45 s, sound one in two
  or three.
- A clip's own sound (footage with its room or its music) is ducked with `volume` keyframes
  and brought up for the one moment that matters.
