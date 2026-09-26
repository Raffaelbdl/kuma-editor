# Audio: loudness with gains only

kuma mixes by summing clips with their `volume` and `audio_fade`. There is no compressor or
limiter, so loudness is whatever the gains make it, and the peak is whatever the loudest
coincidence of clips makes it. A first render came out at **-24 LUFS**, 10 dB under
YouTube's reference, because the gains had been set by ear against nothing.

## Measure, then model, then render

1. **Measure each source** once:
   `ffmpeg -i x.wav -af ebur128=peak=true -f null -` gives integrated loudness and peak.
   Note each file's crest (peak minus loudness): a sparse, spooky cue can have 19 dB of
   crest against 11 for a busy one.
2. **Model the mix** with `scripts/mixsim.py`. It sums every audio clip in the master,
   recursing into nested sequences, with volumes and fades, and writes a WAV.
   `--lufs` measures it overall and per master clip (chapter); `--gain bin=vol` tries gains
   without editing anything. It matched the real render within 0.5 LU. AAC adds about
   0.5–0.8 dB of true peak on top of the model.
3. **Targets:**
   - about -16 to -17 LUFS integrated for a music-and-SFX explainer;
   - a modelled true peak ≤ -1.5 dBTP;
   - chapters within ±1 LU of each other, except a deliberately quiet mood, and that only
     by choice.
4. Apply the gains, re-diff, render, and **measure the render** the same way.

## Where the peaks come from

- Nearly always a hit (a taiko *don* on a title, a stamp, a "correct!" chime) landing on the
  music's own peak. Trim the few hottest effects' gains (don 0.75 → 0.6), not the music.
- Raise music by its cue gains on the master (0.3 → 0.66…0.78 took the mix up ~6 dB).
- A cue with a big crest (sparse plucks) can sit a few LU lower than the rest on purpose.

## Cues, fades and the silence trap

- Put music on the **master**, one cue per chapter or mood. Overlap neighbours by
  0.6–1.6 s around the cut, with `audio_fade` in 1.2 s and out 1.6 s.
- **A fade must be at least 2 frames.** At the time of writing, a fade that rounds to one
  frame on a clip whose own range starts at frame 0 compiles to a filter with `in="0"
  out="0"`, which MLT reads as unbounded. A 0.05 s fade-in on the opening music silenced all
  93 s of it. After every render, check the opening in 2 s windows:
  `ffmpeg -ss 0 -t 2 -i out.mp4 -af volumedetect -f null -` (look for ~-91 dB).
- Generated music: write each cue at exactly the length the storyboard computes, to a
  **new file name**. Never overwrite a file a running render is reading.
- Long reading stretches want a pulse under them (a soft kick and backbeat at ~90 bpm)
  rather than a drumless pad. See `pacing.md`.
