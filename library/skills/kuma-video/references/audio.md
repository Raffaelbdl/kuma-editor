# Audio: loudness with gains only

kuma mixes by summing clips with their `volume` and `audio_fade`. There is no compressor or
limiter, so loudness is whatever the gains make it, and the peak is whatever the loudest
coincidence of clips makes it. A first render came out at **-24 LUFS**, 10 dB under
YouTube's reference, because the gains had been set by ear against nothing.

## Measure, then render

1. **Measure each source** once:
   `ffmpeg -i x.wav -af ebur128=peak=true -f null -` gives integrated loudness and peak.
   Note each file's crest (peak minus loudness): a sparse, spooky cue can have 19 dB of
   crest against 11 for a busy one.
2. **Measure the mix** with `measure_audio`. It renders the master's sound through the real
   graph (every volume, fade and crossfade, nested sequences included; no overlay is baked)
   and returns integrated loudness, range and true peak, overall and per chapter (each clip
   of the master that places a sequence, or the `sections` you give), the five loudest
   moments and every silent stretch of a second or more. On the explainer it read -17.3 LUFS
   and -1.9 dBTP, exactly what its rendered file measured. To try gains, apply them,
   measure, and `undo` if they were wrong.
3. **Targets:**
   - about -16 to -17 LUFS integrated for a music-and-SFX explainer;
   - a measured true peak ≤ -1.5 dBTP;
   - chapters within ±1 LU of each other, except a deliberately quiet mood, and that only
     by choice.
4. Apply the gains, re-diff, render, and **measure the rendered file** with ebur128.

## Where the peaks come from

- `measure_audio`'s `hottest` lists the loudest moments, at least 2 s apart. Nearly always
  it is a hit (a taiko *don* on a title, a stamp, a "correct!" chime) landing on the
  music's own peak. Trim the few hottest effects' gains (don 0.75 → 0.6), not the music.
- Raise music by its cue gains on the master (0.3 → 0.66…0.78 took the mix up ~6 dB).
- A cue with a big crest (sparse plucks) can sit a few LU lower than the rest on purpose.

## Cues and silences

- Put music on the **master**, one cue per chapter or mood. Overlap neighbours by
  0.6–1.6 s around the cut, with `audio_fade` in 1.2 s and out 1.6 s.
- **Silences you did not mean** (a clip muted by mistake, music that ends early, a gap
  between cues) are in `measure_audio`'s `silences`. Read them against the storyboard: a
  pause is deliberate, the first seconds of a chapter usually are not.
- Generated music: write each cue at exactly the length the storyboard computes, to a
  **new file name**. Never overwrite a file a running render is reading.
- Long reading stretches want a pulse under them (a soft kick and backbeat at ~90 bpm)
  rather than a drumless pad. See `pacing.md`.
