# kuma timeline guide

The timeline is a JSON project (get it with `get_project`, its
JSON Schema with `get_schema`). Times are seconds (f64); clip `end` is exclusive; `fps` is rational.
Tracks composite bottom-up, and overlays always draw over video: every overlay track comes after a
sequence's video tracks (a video track after an overlay track is refused). Something an overlay must
draw under, a still or a thumbnail, it draws itself, from a picture in its own folder. Edit with `apply_edits` (RFC 6902 JSON Patch) and pass the `base_revision`
you read, so you never overwrite a human edit made meanwhile. Every edit is validated atomically and
undoable; label your edits with `note`. A patch a script wrote goes in a file in the project, and
`apply_edits` with `patch_path` applies it in one call, whatever its size. A script that builds the
whole project does not diff it itself: `kuma -p <project> diff plan.json` prints the patch between the
stored project and the script's, compared as kuma stores them (it leaves out every field at its
default, a key's `ease: "linear"`), and the revision to send it at; `kuma canonical plan.json`
prints that form.

Everything placeable lives in `bin`, keyed by a short id: media files (`{"source": {"path": …}}`),
configured overlays (`{"source": {"overlay": "overlays/<name>", "vars": {…}}}`), colours, and *sequences*
(`{"source": {"tracks": [...], "markers": [...]}}`). A clip is a *placement* of one entry: it carries
`bin`, `start`, `end`, `in`, `speed` and `effects`, never a source of its own. So changing an
overlay's text edits its bin entry and every placement follows; to vary the text, add a second entry.
Add media with `add_media` rather than by hand: `paths` (or a folder, whose media files it adds and
whose other files it lists as `skipped`) is one edit, with ids minted from the file names. A file
replaced on disk keeps its old `meta` until `reprobe_media` reads it again. `unused_entries` lists what
no clip places, or what only sequences the master never reaches place; removing them is your patch.
A picture with transparency (`meta.alpha`) shows what is below it, and the master's black where
nothing is: a drawing made for a white page needs a white colour entry under it. An SVG is refused:
rasterise it to a PNG, or show it from an overlay.

An effect acts on the picture or the sound (`mlt` on either): `transform`, `crop`, `blur`, `color`,
`blend`, `fade` and `freeze` on what a clip shows, `volume`, `audio_fade`, `limiter` and
`auto_level` on what it sounds. A colour, a
still and an overlay never sound; nothing on an audio track shows. Put only what can act on the clip:
anything else renders as nothing and comes back as a warning. Every effect with a quantity to move
(`transform`, `crop`, `blur`, `color`, `volume`) has static values and optional `keyframes`,
`[{t, <param>: value, ease?}]` with `t` from the clip's start; a parameter a key leaves out is
the static value. `crop`'s `radius` is a fraction of *half* the shorter side of what the crop
leaves (1 on a square is a circle), and the crop is of the whole frame: crop a letterboxed still
to its own edges first. `freeze` `{t}` holds the frame the clip shows `t` s in (anything it
places, sequences too) for the whole clip; its sound runs on (mute the clip to silence it), its
other effects act on the held frame and still move. Split the clip to play, then hold.
`auto_level` `{target (LUFS, -23), window (s, 3), max_gain, min_gain (dB, ±15)}` rides a
drifting source's gain toward `target`, measured over the last `window`: no keyframes needed for
a bed that swells and falls; it lags a sudden change by about a window, and after a seek it
settles within one.

**A sequence's `effects` act on its mix** (`/bin/<seq>/source/effects`, always present):
`volume`, `limiter` and `auto_level` only, keyframes in the sequence's own seconds, on every
placement and when it is rendered itself. The master's are the master stage: e.g.
`[{"type":"volume","gain":3.2},{"type":"limiter","ceiling":-2.5}]`. `limiter`
`{ceiling (dBFS, -1), attack (ms, 5), release (ms, 50)}` holds sample peaks at the ceiling and
never raises anything (the gain before it does); true peak lands up to about a dB over the
ceiling, so check with `measure_audio`. It delays the sound by `attack`. There is no ducking:
key the music's `volume` under the speech by hand.

A clip may run past its source's end: the missing frames show the tracks below and are silent,
and the validator warns only when a whole frame is missing.

**There is no project-level `tracks`.** A timeline is a sequence entry in the bin, and `master` names
the one that gets rendered — so a clip lives at `/bin/<sequence>/source/tracks/<i>/clips/<j>`. A
sequence placed on another sequence is a nested sequence: edit a part separately, then place it in
the final cut. `in` offsets into it and trimming works as on any clip; two placements of one sequence
share its content and keep their own effects. A sequence cannot contain itself (refused), cannot be
sped up (MLT cannot time-warp a timeline: render it out and place the result as media), and must not
be empty when placed. Track and clip ids are unique across the *whole project*, not per sequence.
`sample_frames` and `render` take an optional `sequence` and default to the master.

**Markers are notes on moments** (the schema's `Marker`: `t` in its own sequence's seconds, `label`
the note, `origin` who left it — the daemon stamps it, never write it). A person adds one in the
editor (P at the playhead) to point at what they want changed, so "see the markers" or "I left
comments" means: `list_markers`, look at each moment with `sample_frames` and `times` (pass
`sequence` for a marker on a nested one; in the master it falls at the placing clip's `start` + `t`
− `in`), and act on each note. `list_markers` lists every sequence's markers, each with an id
(`<sequence>@<t>`, e.g. `main@8.600`), its number in the editor, `author` (`user`, `agent`, `other`)
and `waiting`: a person's comment no edit has answered yet — your requests. When an edit answers
one, pass its id in `apply_edits`' `answers`, and the editor ticks it off against that revision.
Leave a marker in place unless you are asked to clear it, and say which ones you handled. You can
add markers too — a moment for the person to review, a TODO, a zone (two markers, its edges): they
are listed as `author: "agent"`, yours, never waiting, so never answer your own.

If the project directory holds a `design-system/` folder, read `design-system/DESIGN.md` before
authoring or restyling an overlay and take its colours, type, geometry and motion from there;
`tokens.css` beside it is the same values as a `:root` block to copy into an overlay. Nothing is
injected at runtime — a design system is a document, and applying it is your edit.
`list_design_systems` says what the project's libraries offer and which one it carries;
`apply_design_system` copies one in.

When the user names a library to use ("use `/path/to/house-style`"), set it up before the first
edit: `add_library`, read every file in the `docs` it returns, `apply_design_system` with its
design system, then build from its blueprints and effect presets (`list_blueprints` and
`list_effect_presets` name each one's library; `remove_library` undoes it).

An overlay entry names a folder in the project (`overlays/<name>/`: `index.html` +
`overlay.json`) plus its `vars`; entries may share a folder. To make one: `new_overlay`, edit,
`check_overlay`, then `import_overlay` (writing a folder does not add it to the bin). Or copy a
**blueprint** from the user's library or one the project uses (`list_blueprints`, `add_blueprint`,
with a `folder` name that says what the copy is for). `make_overlay_unique` before editing a shared
folder; `save_blueprint` only when the user asks. See `overlay_guide`.
Overlays are live DOM in the preview and baked to alpha frames at export. For a plain title, the
`title` and `title-subtitle` blueprints are typography, box, outline, shadow and entrance/exit as
vars — set those rather than writing markup. A `font` var takes a family name from `list_fonts`;
`add_font` copies a new one in (a `.ttc` collection is split into its faces, a `.woff2` unwrapped). Each family lists the
`scripts` it can set: text in any other (kanji in a Latin font, say) is drawn by the baking machine's
fonts, not the project's, so pick or add a family that covers the text. `validate_project` warns
about every entry whose `font` var names a family that is not installed, whose text has characters
none of its fonts can set (it lists them), or that shows CJK text on a page whose `lang` is another
language; a render's result carries the same `warnings` for what it drew. The page's language is an
entry's `lang` var where the overlay declares one (`list_blueprints` shows which do): set it to `ja`
on a Japanese title rather than copying the folder.

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
of it is one image. One frame comes back 960 px wide, several 480 px each; `width` overrides.

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

Long jobs (`render`, `measure_audio`, `transcribe`, `bake_overlay`) likewise wait `wait_seconds`, then
return the job still running with its id. Keep waiting with `job_status` (`id`, `wait_seconds` about
100): a client may move a longer call to the background (Claude Code does at 120 s), where it still
finishes. `job_status` without an id lists the jobs, to find one again.

Sound: `measure_audio` renders a sequence's sound through the real graph (no overlay is baked) and
meters it: integrated loudness (LUFS), range and true peak (dBTP), overall and per chapter (each
clip placing a sequence, or the `sections` you name), the loudest moments and every silent stretch
of a second or more, after the sequence's own effects. Loudness and peak are whatever the gains
and the master's limiter make them: measure before a final render, set the master's `volume`
from the integrated figure (gain = 10^((target − measured)/20)), and read the silences against
what you meant. To measure one part
alone, pass track ids as `solo` (the voice, the music bed) or `mute`: they apply to that
measurement only, never to the project. A soloed track inside a nested sequence is heard through
the clips placing it, and everything else on the way is silenced.

`measure_media` measures one source instead (a media `bin` entry or a `path` in the project, whole or
a `range`, in its own seconds), with no ffmpeg by hand: loudness, range and true peak; a loudness
`curve` to key a music bed's volume by (short-term every `step` seconds, about 120 values by
default); and its quiet stretches as `{start, end}`. Quiet is every sample under `silence_db`
(-35) for `silence_min` (0.15 s): speech pauses to cut a voice-over at. Raise `silence_min` to 1 s
to find the gaps between takes; lower `silence_db` to -50 to find where a sound effect starts (the
end of the silence before it).

Subtitles: `transcribe` runs a local speech-to-text model (whisper.cpp) and adds `subtitles`
overlay entries whose `cues` var is `[{start, end, text}, …]` in source seconds, one cue per word (the
overlay groups them by its `words_per_line` var and can stress the word being said), listed under the media
(`parent`). Cue edges are moved onto the pauses in the sound (`snap`, on by default; `silence_db`,
`silence_min` say what a pause is), so a gap between cues is a real pause and a sentence boundary taken
from them can be cut at. Given a `clip`, it also places the subtitles over that clip, aligned; given a `bin`, the
entries are yours to place (each `offset` var is the source second its clip must start at). Fix a misheard word by
patching the cue's `text`; re-transcribe with `replace` to overwrite one entry's words in place and keep the
entry's styling and placement, and with `language` (an ISO 639-1 code) to force the language rather
than detect it; a new entry then also gets it as its `lang` var, the page's language. `model` picks a larger model than the default (`large-v3`, `medium`: much better
outside English; `health` lists `whisper_models`). `parent` on a bin entry is organisation only: the entry it is listed under
in the browser, which can be any entry or a folder (`{"source": {"folder": true}}`, never placeable);
`order` sorts siblings there (ordered ones first, the rest by id).

**A transcript is cut into entries of about a minute**, since one entry bakes as one clip, again
after every correction. `transcribe` cuts at the best pause near every `split` seconds (default 60;
0 for one entry): `<id>-1`, `<id>-2`… each with its own cues and `offset`, returned as `pieces`. From
a clip, it makes the pieces the clip plays and places each over its stretch. `replace` overwrites one
entry and refuses `split`: to redo a split transcript, remove its pieces and transcribe again.

**Publishing text beside the render.** `export_subtitles` gives the SRT for a YouTube upload: the
lines the master's subtitles overlays draw, in master seconds. A hidden subtitles track still counts
(captions off the picture, in the SRT); `words_per_line` makes captions longer than the burned-in
lines. `export_chapters` gives the master's chapters (clips placing a sequence, titled by `label`) as
`M:SS Title` lines, and warns where YouTube would not read them. Both take `sequence` and return
the text unless `output`/`write` asks for a file.

Vertical (9:16) is only
`width`/`height`: patch them to 1080×1920 (or swap them) and every preset renders portrait.
