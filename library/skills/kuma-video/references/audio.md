# Audio: loudness with gains only

kuma mixes by summing clips with their `volume`, `volume` keyframes and `audio_fade`. There
is no compressor or limiter, so loudness is whatever the gains make it, and the peak is
whatever the loudest coincidence of clips makes it. One first render came out at
**-24 LUFS**, 10 dB under what YouTube plays at, because the gains had been set against
nothing.

## Measure, then render

1. **Measure each source** once:
   `ffmpeg -i x.wav -af ebur128=peak=true -f null -` gives integrated loudness and peak.
   Note each file's crest (peak minus loudness): a sparse cue can have 19 dB of crest
   against 11 for a busy one, and needs less gain to sound as loud.
2. **Measure the mix** with `measure_audio`. It renders the master's sound through the real
   graph (every volume, fade and crossfade, nested sequences included; no overlay is baked)
   and returns integrated loudness, range and true peak, overall and per part (each clip
   of the master that places a sequence, or the `sections` you give), the five loudest
   moments and every silent stretch of a second or more. On the explainer it read the same
   -17.3 LUFS and -1.9 dBTP as the rendered file. To try gains, apply them, measure, and
   `undo` if they were wrong.
3. **Targets:**
   - -14 to -16 LUFS integrated for an upload (YouTube turns louder videos down to about
     -14, and does not turn quieter ones up);
   - a measured true peak ≤ -1.5 dBTP, which leaves room for the encoder;
   - parts within ±1 LU of each other, unless a quiet one is quiet on purpose.
4. Apply the gains, render, and **measure the rendered file** with ebur128.

## Voice and music

- In a video with speech, the voice carries the loudness: set it first, then the music
  under it, typically 15 LU or more below the voice while someone talks.
- **Measure them apart** by muting: mute the music tracks, `measure_audio`, `undo`; then the
  other way round. A muted track contributes nothing to the compile.
- **Ducking** is `volume` keyframes on the music clip: down over ~0.3 s before speech
  starts, back up over ~0.8 s after it ends. Hold keys (`ease: "hold"`) keep it flat in
  between.
- Speech recorded quietly is raised by the clip's `volume`; check its peaks with
  `hottest`, since nothing limits them.

## Where the peaks come from

- `measure_audio`'s `hottest` lists the loudest moments, at least 2 s apart. Nearly always
  it is a hit (a drum, a stamp, a chime) landing on the music's own peak, or a shout. Trim
  the few hottest effects' gains, not the music.
- Raise music by its clip gains on the master; a cue with a big crest can sit a few LU
  lower than the rest on purpose.

## Cues and silences

- Put music on the **master**, one cue per part or mood. Overlap neighbours by 0.6–1.6 s
  around the cut, with `audio_fade` in 1.2 s and out 1.6 s.
- **Silences you did not mean** (a clip muted by mistake, music that ends early, a gap
  between cues) are in `measure_audio`'s `silences`. Only near-silence counts (under
  -70 LUFS): a pause in a recording is room tone, which is louder.
- Generated music: write each cue at exactly the length it will be placed at, to a **new
  file name**. Never overwrite a file a running render is reading.
- Long talking or reading stretches want a pulse under them (a soft kick and backbeat at
  ~90 bpm) rather than a drumless pad. See `pacing.md`.
