# Narrated videos: voice-over and to-camera

For a video carried by a voice: an essay, a documentary, a tutorial read over pictures, with
or without the narrator on camera. The voice is the clock and the loudness reference;
pictures, titles and music are placed against it. Learned on a 13-minute essay of about 470
clips: voice-over over stills and footage, to-camera takes between them, music beds under
both.

## Shape: a sequence per paragraph

- **Three levels**: the master places one sequence per chapter, and a chapter places one
  sequence per paragraph of the script (a *scene*). A scene holds its own slice of the
  voice-over with its shots and titles, so re-recording, replacing or retiming one paragraph
  never touches its neighbours, and each can be sampled and measured on its own.
- What spans scenes sits on the chapter, above the scene clips: a transition across two of
  them, a running graphic. Music sits on the master (`audio.md`).
- **Scene times are recording seconds.** A scene is a stretch `[t0, t1)` of the chapter's
  voice-over file; inside it, shots are appended in order (`shot(entry, src_in, dur)`), and
  a shot can end on a word (`until=<recording time>`). A line of the script is then found
  by ear in the recording, and nothing is recomputed when a neighbour changes.
- **Floor each scene's length to whole frames**, so a slice never reads past the end of its
  recording and the next one starts exactly where it ends.

## Timing from the voice

- `transcribe` the voice-over (pass `language`) for sentence and word times. The default
  model is small: outside English, ask for a larger one (`model: "large-v3"`, if `health`
  lists it), or expect wrong words with good times, which are what you need here.
- **Boundaries go in real pauses.** Whisper's words run into each other, and taken
  straight from its times, 2 of 19 scene boundaries were audibly wrong in one video (a slice
  held the first syllable of the next sentence, another clipped a first word). `transcribe`
  moves cue edges onto the pauses it hears, so a gap between two cues is a pause; check
  a boundary that matters against `measure_media`'s silences for the recording.
- End a scene in the middle of the pause after its last word; start the next just before
  its first word.

## Takes to camera

- A recording often holds several takes. Transcribe it, and find each take's start and end
  from its pauses (`measure_media`'s silences, which are in the file's own seconds); keep
  the last complete one unless told otherwise.
- Cut from **0.25 s before the first word to 0.2 s after the last**. By 0.4 s after it the
  speaker's hand is already reaching for the camera.
- Picture from the camera file and voice from a cleaned audio file: compare the two files'
  silences (`measure_media` on each) before trusting that they line up; they should agree to
  a few milliseconds.
- **Removing a pause inside a take**: two clips of the same take, about 0.12 s kept either
  side of the cut, the second one punched in a few percent, so the cut reads as a change of
  framing rather than a jump.
- **Movement**: a wide framing pushes in slowly (scale 1.00 → 1.035 over the take); a tight
  one a little more (1.10 → 1.13, lifting `y` a few pixels to keep the eyes placed). At most
  one change of framing per take, on a pause between two sentences, while the voice runs on.
- A 20 ms `audio_fade` in and out on every voice clip keeps cuts from clicking.

## Levelling the voice

- Level the to-camera takes to the voice-over: one gain per take, from the take's own
  loudness: `gain = 10 ** ((target − L) / 20)`.
- One recording session can still vary from paragraph to paragraph (−22 to −27 LUFS across
  scenes in one video). Measure the voice alone (`measure_audio` with the voice tracks in
  `solo` and one section per scene) and trim each slice towards the target, capped at about
  ±3.5 dB so the performance keeps its shape.
- Then put the music under it: `audio.md`.

## A generated voice

A synthesised narration (an offline TTS model, say) has no recording to cut: the script
makes the sound, so timing comes from the sound it made.

- **A beat is a sentence or two, and one audio file.** A scene is its beats in order with
  a gap between them and a tail; its length is their sum. Visuals attach to a beat's start
  (`beat.t0`), so changing a sentence means synthesising it again and rebuilding, and
  nothing is retimed by hand. Word-level alignment was not needed.
- **Captions** split a beat's text across its length by character count.
- **Normalise every beat to one loudness** (RMS) before placing it: 98 scenes then sat
  within ±0.4 LU of each other after mastering, with no per-scene trim.
- **You cannot hear it.** Acronyms, product names and numbers are where a voice goes wrong.
  Keep a respelling table applied only at synthesis ("nginx" → "engine x"), so captions
  keep the real spelling, and write numbers that matter as words ("twenty twenty-five").
- **Key the audio cache on everything that changes the sound**: the text, the voice, the
  speed and the respelling table. Keyed on the text alone, a fixed pronunciation plays the
  old file.
- **Pace**: about 150 to 165 words a minute, gaps included, suits technical content; 180
  reads as rushed.
- Hand over the list of words you respelled or doubt, and the longest scenes, as things for
  a person to listen to: nothing measures a mispronunciation.

## Checking a narrated cut

- Listen-free checks first: `measure_audio` per chapter, the silences you did not mean, and
  `sample_frames` with `times` at every scene boundary ±0.3 s.
- Say what was measured and what was only computed, and name what cannot be judged without
  ears or eyes (a breath left in, a word clipped by a few milliseconds), so a person checks
  those.
