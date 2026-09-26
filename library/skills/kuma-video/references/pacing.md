# Pacing: readable, and still punchy

A text-dense explainer fails in two opposite ways: text that leaves before it can be read,
and a steady reading rhythm that loses attention even when every scene is well timed. The
second is the harder one to see. The first cut of a 10-minute video had only ~45 s of dead
holds in total, and it still felt slow.

## Readable

- Budget **~14 characters a second** of on-screen text, plus ~0.4 s for it to arrive. A
  typed JRPG window: its typing time, plus ~1.2 s.
- `scripts/reading_audit.py` lists, per scene, when the last text can have been read and
  how long the scene then holds. A negative hold means too much text: cut words, don't add
  seconds. A hold over ~2 s is a candidate for a cut.
- Shorter sentences and active verbs: "Hand it an XML file describing the edit" beats "You
  hand it an XML file that describes the edit".
- A summary (まとめ) at each chapter's end may be dense: it is a recap, and the viewer can
  pause.

## Punchy

In order of payoff:

1. **A hook in the first five seconds**: a cold open with an open loop, closed later ("Psst…
   this whole video hides a secret. I'll show you at the twist." → at the twist, a 秘密 seal
   and the reveal).
2. **A beat on every scene change**: a light screentone "page turn" (a 0.6 s wipe centred
   on the cut, sweeping right to left like manga pages) and a soft whoosh (gain ~0.26).
   Heavier ink wipes are for chapter cuts only.
3. **Camera hits** from the sound effects: transform keyframes on the chapter's clip on the
   master.
   - Punch: scale 1 → 1.045 in 2 frames, then back via an intermediate key over ~0.4 s.
     Use linear keys: a `smooth` key can undershoot below 1 and bare the frame's edge.
   - Tap: 1.018, for stamps and correct answers.
   - Shake: ±14 px, decaying over 5 frames at scale ≥ 1.02, so the edges stay covered; for
     tears, slashes, a refused edit.
   - Derive them from the SFX list (don → punch, stamp → tap, slash → shake), at most one
     per 0.55 s.
4. **Snappy motion**: arrivals in ~0.3 s (`expo.out`, pops overshoot), exits in 0.2 s.
5. **Short chapter cards**: ~3.4 s. A brush kanji, focus lines, a *don*, the name, the
   episode line.
6. **Trim holds** with cuts (`storyboard-as-code.md`), keeping ≥ 1 s after the last read.
7. **Music with a pulse** under long stretches (`audio.md`).
8. A visible structure (a HUD with the chapter lit up) so a viewer always knows where they
   are.

## Keep the craft invisible

- One accent colour for emphasis, one per engine or actor, and no more.
- Every foreign word glossed (`japanese-type.md`).
- Characters as guides, each with their own voice: a narrator's box, a mascot's round
  balloons, the agent's JRPG window.
