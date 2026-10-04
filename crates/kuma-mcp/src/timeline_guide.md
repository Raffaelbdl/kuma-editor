# kuma timeline guide

The timeline is a JSON project (get it with `get_project`, its
JSON Schema with `get_schema`). Times are seconds (f64); clip `end` is exclusive; `fps` is rational.
Tracks composite bottom-up. Edit with `apply_edits` (RFC 6902 JSON Patch) and pass the `base_revision`
you read, so you never overwrite a human edit made meanwhile. Every edit is validated atomically and
undoable; label your edits with `note`. A patch a script wrote goes in a file in the project, and
`apply_edits` with `patch_path` applies it in one call, whatever its size.

Everything placeable lives in `bin`, keyed by a short id: media files (`{"source": {"path": …}}`),
configured overlays (`{"source": {"overlay": "overlays/<name>", "vars": {…}}}`), colours, and *sequences*
(`{"source": {"tracks": [...], "markers": [...]}}`). A clip is a *placement* of one entry: it carries
`bin`, `start`, `end`, `in`, `speed` and `effects`, never a source of its own. So changing an
overlay's text edits its bin entry and every placement follows; to vary the text, add a second entry.
Add media with `add_media` rather than by hand.

An effect acts on the picture or the sound (`mlt` on either): `transform`, `crop`, `blur`, `color`,
`blend` and `fade` on what a clip shows, `volume` and `audio_fade` on what it sounds. A colour, a
still and an overlay never sound; nothing on an audio track shows. Put only what can act on the clip:
anything else renders as nothing and comes back as a warning. Every effect with a quantity to move
(`transform`, `crop`, `blur`, `color`, `volume`) has static values and optional `keyframes`,
`[{t, <param>: value, ease?}]` with `t` from the clip's start; a parameter a key leaves out is
the static value.

**There is no project-level `tracks`.** A timeline is a sequence entry in the bin, and `master` names
the one that gets rendered — so a clip lives at `/bin/<sequence>/source/tracks/<i>/clips/<j>`. A
sequence placed on another sequence is a nested sequence: edit a part separately, then place it in
the final cut. `in` offsets into it and trimming works as on any clip; two placements of one sequence
share its content and keep their own effects. A sequence cannot contain itself (refused), cannot be
sped up (MLT cannot time-warp a timeline: render it out and place the result as media), and must not
be empty when placed. Track and clip ids are unique across the *whole project*, not per sequence.
`sample_frames` and `render` take an optional `sequence` and default to the master.

**Markers are the person's comments** (the schema's `Marker`: `t` in its own sequence's seconds,
`label` the note). People add them in the editor (M at the playhead) to point at what they want
changed, so "see the markers" or "I left comments" means: read every sequence's `markers` from
`get_project` (at `/bin/<sequence>/source/markers`), look at each moment with `sample_frames` and
`times` (pass `sequence` for a marker on a nested one; in the master it falls at the placing clip's
`start` + `t` − `in`), and act on each note. Leave a marker in place unless you are asked to clear
it, and say which ones you handled. You can add markers too, to point the person at moments to
review.

If the project directory holds a `design-system/` folder, read `design-system/DESIGN.md` before
authoring or restyling an overlay and take its colours, type, geometry and motion from there;
`tokens.css` beside it is the same values as a `:root` block to copy into an overlay. Nothing is
injected at runtime — a design system is a document, and applying it is your edit. `health` says
whether this project has one.

An overlay entry names a folder in the project (`overlays/<name>/`: `index.html` +
`overlay.json`) plus its `vars`; entries may share a folder. To make one: `new_overlay`, edit,
`check_overlay`, then `import_overlay` (writing a folder does not add it to the bin). Or copy a
**blueprint** from the user's library (`list_blueprints`, `add_blueprint`, with a `folder` name that
says what the copy is for). `make_overlay_unique`
before editing a shared folder; `save_blueprint` only when the user asks. See `overlay_guide`.
Overlays are live DOM in the preview and baked to alpha frames at export. For a plain title, the
`title` and `title-subtitle` blueprints are typography, box, outline, shadow and entrance/exit as
vars — set those rather than writing markup. A `font` var takes a family name from `list_fonts`;
`add_font` copies a new one in (a `.ttc` collection is split into its faces). Each family lists the
`scripts` it can set: text in any other (kanji in a Latin font, say) is drawn by the baking machine's
fonts, not the project's, so pick or add a family that covers the text. `validate_project` warns
about every entry whose `font` var names a family that is not installed, whose text has characters
none of its fonts can set (it lists them), or that shows CJK text on a page whose `lang` is another
language; a render's result carries the same `warnings` for what it drew.

To look at the footage, call `sample_frames` with a time range (and an optional pixel `rect`); it
returns the composited frames as images plus their PNG paths. To look at an *overlay you are
authoring*, call it with `overlay` set to the bin entry's id instead: that renders the entry on its
own over a backdrop, with no placement anywhere, and `t0`/`t1` count from the start of the overlay —
so an animation can be checked frame by frame before anything is placed in the cut — in another
frame with `frame: [1080, 1920]`, and over `backdrop` white and black to see whether it stays
legible over footage you have not seen. It is the same view the editor's overlay workbench gives a
person, and `overlay_guide` ends with the checklist to run through. To look at chosen moments,
pass `times` (`[3.5, 41.7, 42.3]`) instead of a range: each bakes only what it shows, and with
`tile: true` they come back as one captioned contact sheet, so every cut and a moment either side
of it is one image.

A sample is exact, so every overlay it shows is baked first, for its whole clip: a changed
overlay costs about 0.1 s per frame at 1080p before the first frame comes back, while an
unchanged one is cached. Changed overlays bake side by side, so the wait is the longest one's,
not their sum; but a clip bakes in one process for its whole length, and any change to it rebakes
all of it. So **compose many short overlays rather than one long one**: a title per scene, a callout
per point, subtitles in stretches. While you are drawing, sample the overlay entry on its own (seconds),
and sample the sequence once it looks right. A sample still baking after `wait_seconds` returns
its job id and what it is baking; call `sample_frames` with `job` set to that id to keep waiting.
Asking again with the same arguments costs nothing extra either: an overlay already baking is
waited for, not baked twice.

Sound: `measure_audio` renders a sequence's sound through the real graph (no overlay is baked) and
meters it: integrated loudness (LUFS), range and true peak (dBTP), overall and per chapter (each
clip placing a sequence, or the `sections` you name), the loudest moments and every silent stretch
of a second or more. kuma has no limiter, so loudness and peak are whatever the gains make them:
measure before a final render, and read the silences against what you meant. To measure one part
alone, pass track ids as `solo` (the voice, the music bed) or `mute`: they apply to that
measurement only, never to the project. A soloed track inside a nested sequence is heard through
the clips placing it, and everything else on the way is silenced.

`measure_media` measures one source instead (a media `bin` entry or a `path` in the project, whole or
a `range`, in its own seconds), with no ffmpeg by hand: loudness, range and true peak; a loudness
`curve` to key a music bed's volume by (short-term every `step` seconds, about 120 values by
default); and its quiet stretches as `[start, end]` pairs. Quiet is every sample under `silence_db`
(-35) for `silence_min` (0.15 s): speech pauses to cut a voice-over at. Raise `silence_min` to 1 s
to find the gaps between takes; lower `silence_db` to -50 to find where a sound effect starts (the
end of the silence before it).

Subtitles: `transcribe` runs a local speech-to-text model (whisper.cpp) and adds a `subtitles`
overlay entry whose `cues` var is `[{start, end, text}, …]` in source seconds, one cue per word (the
overlay groups them by its `words_per_line` var and can stress the word being said), listed under the media
(`parent`). Given a `clip`, it also places the subtitles over that clip, aligned; given a `bin`, the
entry is yours to place (set its `offset` var to a trimmed clip's `in`). Fix a misheard word by
patching the cue's `text`; re-transcribe with `replace` to overwrite the words in place and keep the
entry's styling and placement, and with `language` (an ISO 639-1 code) to force the language rather
than detect it. `parent` on a bin entry is organisation only: the entry it is listed under
in the browser, which can be any entry or a folder (`{"source": {"folder": true}}`, never placeable);
`order` sorts siblings there (ordered ones first, the rest by id).

**Never leave a transcript longer than about a minute in one entry.** `transcribe` puts a whole
recording in one entry, so split it straight after: one `subtitles` entry per stretch of up to about
60 s, cut at a pause between cues, each holding only its own cues. Give each the same styling vars,
`offset` set to the stretch's first source second, and place it over that stretch of the clip. One
transcript over a 20-minute recording bakes as a single 20-minute clip, again after every correction.

Vertical (9:16) is only
`width`/`height`: patch them to 1080×1920 (or swap them) and every preset renders portrait.
