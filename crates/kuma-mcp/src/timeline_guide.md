# kuma timeline guide

The timeline is a JSON project (get it with `get_project`, its
JSON Schema with `get_schema`). Times are seconds (f64); clip `end` is exclusive; `fps` is rational.
Tracks composite bottom-up. Edit with `apply_edits` (RFC 6902 JSON Patch) and pass the `base_revision`
you read, so you never overwrite a human edit made meanwhile. Every edit is validated atomically and
undoable; label your edits with `note`.

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

If the project directory holds a `design-system/` folder, read `design-system/DESIGN.md` before
authoring or restyling an overlay and take its colours, type, geometry and motion from there;
`tokens.css` beside it is the same values as a `:root` block to copy into an overlay. Nothing is
injected at runtime — a design system is a document, and applying it is your edit. `health` says
whether this project has one.

An overlay entry names a folder in the project (`overlays/<name>/`: `index.html` +
`overlay.json`) plus its `vars`; entries may share a folder. To make one: `new_overlay`, edit,
`check_overlay`, then `import_overlay` (writing a folder does not add it to the bin). Or copy a
**blueprint** from the user's library (`list_blueprints`, `add_blueprint`). `make_overlay_unique`
before editing a shared folder; `save_blueprint` only when the user asks. See `overlay_guide`.
Overlays are live DOM in the preview and baked to alpha frames at export. For a plain title, the
`title` and `title-subtitle` blueprints are typography, box, outline, shadow and entrance/exit as
vars — set those rather than writing markup. A `font` var takes a family name from `list_fonts`;
`add_font` copies a new one in.

To look at the footage, call `sample_frames` with a time range (and an optional pixel `rect`); it
returns the composited frames as images plus their PNG paths. To look at an *overlay you are
authoring*, call it with `overlay` set to the bin entry's id instead: that renders the entry on its
own over a backdrop, with no placement anywhere, and `t0`/`t1` count from the start of the overlay —
so an animation can be checked frame by frame before anything is placed in the cut — in another
frame with `frame: [1080, 1920]`, and over `backdrop` white and black to see whether it stays
legible over footage you have not seen. It is the same view the editor's overlay workbench gives a
person, and `overlay_guide` ends with the checklist to run through.

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
Vertical (9:16) is only
`width`/`height`: patch them to 1080×1920 (or swap them) and every preset renders portrait.
