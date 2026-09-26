# Pacing: readable, and still punchy

A video fails in two opposite ways: text or speech that leaves before it lands, and an even
rhythm that loses attention even when every shot is well timed. The second is the harder
one to see. The first cut of a 10-minute explainer had only ~45 s of dead holds in total,
and it still felt slow.

## Readable

- Budget **~14 characters a second** of on-screen text, plus ~0.4 s for it to arrive.
  `scripts/reading_audit.py` lists every overlay clip showing text: when it can have been
  read and how long it then stays. A negative hold means too much text: cut words, don't
  add seconds. A hold over ~2.5 s is a candidate for a cut.
- Shorter sentences and active verbs: "Hand it an XML file describing the edit" beats "You
  hand it an XML file that describes the edit".
- Subtitles: short lines (the `subtitles` blueprint groups words by `words_per_line`), and
  never under a lower third or a caption that shares the bottom of the frame.
- A recap at the end of a part may be dense: the viewer can pause.

## Punchy

In order of payoff:

1. **A hook in the first five seconds**: the result first, a question, or an open loop
   closed later ("this whole video hides a secret — I'll show you at the end").
2. **Trim**: dead air in speech (`footage.md`), holds after the last thing to read (keep
   ≥ 1 s), the second take of anything said twice.
3. **A beat on every change** of scene or topic: a short transition centred on the cut (a
   ~0.6 s wipe) and a soft whoosh (gain ~0.25). Heavier ones for the change of part only.
4. **Camera hits**: `transform` keyframes on the clip, footage or a whole part placed on the
   master.
   - Punch: scale 1 → 1.045 in 2 frames, then back via an intermediate key over ~0.4 s.
     Use linear keys: a `smooth` key can undershoot below 1 and bare the frame's edge.
   - Tap: 1.018, for a small emphasis.
   - Shake: ±14 px, decaying over 5 frames at scale ≥ 1.02, so the edges stay covered.
   - Derive them from the sound effects (a hit → punch, a click → tap), at most one per
     ~0.5 s.
5. **Snappy motion**: overlays arrive in ~0.3 s (`expo.out`, pops overshoot) and leave in
   0.2 s.
6. **Short title cards**: ~3.5 s for a part's title.
7. **Music with a pulse** under long talking or reading stretches (`audio.md`).
8. **A visible structure** (part titles, a progress bar or a chapter strip) so a viewer
   always knows where they are.

## Keep the craft invisible

- One accent colour for emphasis, and few others.
- Every foreign or technical word explained where it first appears (`japanese-type.md` for
  kanji).
- A recurring guide (a narrator's box, a mascot, the presenter) keeps one voice and one look.
