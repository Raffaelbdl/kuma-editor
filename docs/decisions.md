# kuma-editor — decision record

Decisions taken during the review and bin refactor on `claude/happy-davinci-tspnqo`
(commits `eede385` and `25849c1`). `docs/plan.md` is the original plan and predates
these; where the two disagree, this file is later.

Status key: **adopted** is in the code, unless the entry says otherwise; **rejected** was
considered and turned down; **superseded** was adopted earlier in the same session and
then replaced; **deferred** is agreed direction, not yet built. Where a decision was to
*not* build something, the entry says so rather than implying an implementation.

---

## Project model

**One bin holds everything placeable** — adopted.
Media files, configured overlays and colours all live in `Project.bin`, keyed by a short
id, each with a name, a source and optional probed metadata. Previously media sat in
`assets` while overlays and colours existed only inside a clip's `src`, which meant an
HTML animation had nowhere to live that a video file did. Real editors do not make that
split: Kdenlive's Project Bin holds title clips and colour clips next to footage.
*Lands in `crates/kuma-core/src/model.rs`: `BinItem`, `BinSource`.*

**A clip is a placement, not a thing** — adopted.
A clip carries `bin`, `start`, `end`, `in`, `speed` and `effects`, and never a source.
What is shown lives on the bin entry; when it is shown lives on the clip. The
`ClipSource` enum is deleted, so the compiler, validator and UI each lost a match arm
rather than gaining one.
*Lands in `crates/kuma-core/src/model.rs`: `Clip`.*

**Overlay vars live on the bin entry** — adopted.
Editing an overlay's text edits its entry, and every placement follows. A second wording
is a second entry. This is the behaviour of a title clip in any NLE, and it is what makes
a named, thumbnailed row in the browser mean something. The cost is that varying one
placement takes two operations instead of one, which JSON Patch absorbs into a single
atomic call.

**A two-tier model: reusable templates inline, one-offs in the bin** — rejected.
Considered making the split by intent, so a location tag stayed a template plus vars on
the clip while a bespoke animation became a bin entry. Dropped because the distinction is
about expected reuse rather than about the content, and because the argument for it
rested on an agent cost that does not exist: adding an entry and a clip is two operations
in one patch, one revision, one undo entry. One mechanism is also less code than two.

**HTML templates subsume title clips** — adopted.
Kdenlive needs a separate title-clip producer. kuma does not, because a template with a
text var is a title. `BinSource` therefore has three variants, not four.

**Transitions stay out of the bin** — adopted.
A transition is a relationship between two adjacent clips on one track, not something
that occupies time on its own, so it has no entry to own. Kdenlive agrees by placing
Compositions and Effects in tabs beside the Project Bin rather than inside it.

**`in` applies only to video and audio entries** — adopted.
A still, a colour and an overlay are generated to fit the clip and have no timeline to
offset into, so a non-zero `in` on one is a validation error rather than a silent no-op.
*Lands in `crates/kuma-core/src/validate.rs`, via `BinItem::is_timed_media`.*

**Probed metadata tolerates unknown keys** — adopted.
`MediaMeta` drops `deny_unknown_fields`. It is written by the probe, and probe output
evolves, so a document written by another build must still open. This was found the hard
way: a stale `codec` key made the daemon refuse to start.

**No upgrade path; the format stays version 1** — adopted, superseding a v2 migration.
A `migrate` function with v1 fixtures and tests was written and then removed once it was
established that nothing is using kuma yet. About ninety lines that could only ever have
run against documents that do not exist. Changing the format later means bumping
`PROJECT_VERSION` and writing one.

## Naming

**One meaning per word** — adopted.
"Asset" had come to mean both a media file in the project and a reusable template in the
user's library. It now means neither: the project has a **bin**, the user has a
**library**. Model types renamed `MediaKind` and `MediaMeta`; tools renamed `add_media`
and `probe_media`; routes `POST /api/bin` and `GET /api/media/probe`; the command is
`kuma add-media`.

**Call it `bin`, not `media` or `assets`** — adopted.
`bin` is the film-editing term of art, inherited from the cloth-lined bin that trims hung
into between the rack and the splice, and kept deliberately by Avid so film editors would
recognise what they were looking at. `media` was the plainer alternative and was passed
over because the map holds overlays and colours too, which are not media.

## MLT compiler

**Producers start at `in="0"`; the playlist entry does the cut** — adopted.
Putting the clip's source in-point on the producer element clamps its playtime, so every
trimmed clip played its first frame and froze for the rest of its length. This was the
showstopper of the review. The cut belongs to `<entry in out>` alone.
*Lands in `crates/kuma-mlt/src/compile.rs`: `emit_clip_producer`.*

**The golden MLT file is the regression oracle** — adopted.
`tests/fixtures/reference.mlt` is regenerated with `UPDATE_GOLDEN=1` and the diff is read
by hand. It earned its keep during the bin refactor: the compiled XML came out
byte-identical, which is the proof that restructuring the model changed no pixels.

## Templates and presets as user assets

**Templates and presets live outside the codebase** — adopted.
They are assets shared across projects, not source. kuma reads `$XDG_DATA_HOME/kuma`
(default `~/.local/share/kuma`), then every directory in `KUMA_LIBRARY`, then the
project's own `overlays/` and `presets/`; later wins on an id clash. A template authored
for one video belongs in that project; a reusable one belongs in the library.

**`library/` in the repo is a starter set, not a runtime path** — adopted.
`kuma library` installs it into the user library. Tests point `KUMA_LIBRARY` or the baker
at the repo copy.

**A matching search path for media** — deferred.
Templates got a library; media did not, so a shared animation or sting folder has no
first-class home and each project relinks it by hand. Absolute paths already work, so
this is about browsing and organisation rather than capability.

## Preview

**Engine reload is keyed on non-overlay state** — adopted.
Overlays are live DOM in the editor and are skipped by the preview compile, so an
overlay-only edit must not make the engine reload the whole timeline.

**Overlay iframes are driven through the hyperframes player** — adopted.
When the runtime's player is up, seeking goes through it, which is the same scrub path
the bake uses. Driving GSAP timelines and Web Animations directly is the fallback for
before it loads.

**A compile error does not mark the engine dead** — adopted.
The preview socket distinguishes "no engine" from "this timeline failed to compile"; only
the first disables playback.

**No wall clock in the engine** — adopted.
Without an audio device, SDL's dummy driver still paces the callback in real time, so
playback is always audio-paced and the alternate clock path was dead code.

## Thumbnails

**A thumbnail belongs to the configured entry** — adopted.
Previously the thumbnail belonged to the template, so every location tag in the browser
showed the same picture of the manifest defaults. Entries now render with their own vars
and are served from `/api/bin/{id}/thumb.png`; the `thumbs` job does templates and
entries in one pass.

## Validation

**Overlay vars are checked once per entry, not once per clip** — adopted.
Less work and a clearer error path: a bad var is reported at `/bin/<id>/source/vars`
rather than repeated for every placement.

**Bin ids are validated like clip and track ids** — adopted.
They appear in URLs and are used to name cached thumbnails, so they take the same
`[A-Za-z_][A-Za-z0-9_-]*` rule.

## Plugins and generated content

**Plugins call generic endpoints; kuma gains no plugin API** — adopted as direction; no
code was added for it. Two seams already exist. Anything that terminates in a file goes
through the bin: write it into the project directory, call `add_media`, place it with
`apply_edits`. Anything that terminates in a filter goes through the `mlt` effect, which
takes any MLT service by name with raw properties. The MCP server is already a client of
exactly this shape, which is the evidence the seams work.

**An upload endpoint** — rejected.
Proposed so a browser plugin could hand kuma bytes. Turned down because a desktop plugin
has filesystem access and the filesystem is already kuma's interchange layer: media,
templates, libraries and presets are all directories. A blob path would add a second
answer to "where is my image" and break the round trip through other tools, where editing
a generated PNG in an image editor is picked up by the next render with no export step.

**A free-form attribute map on bin entries** — rejected.
Proposed so a generation plugin could record the prompt behind an image, since entries
deny unknown fields and unknown keys inside probed metadata are dropped on the next save.
Turned down because kuma neither produces nor consumes that data, and a field nothing
validates becomes a dumping ground. A plugin that wants provenance can put it in the file
it writes, as a PNG text chunk, the way ComfyUI and AUTOMATIC1111's web UI do; that
survives copying and keeps kuma out of it, at the cost of image editors sometimes
stripping unknown chunks. **Nothing here is implemented**: kuma does not read or write
image metadata, and this entry exists only so the attribute map is not proposed again.

## Stale probed metadata

**Periodic re-probe** — rejected.
Probed metadata lives inside the project document, so refreshing it is a patch: it bumps
the revision, rewrites the file and broadcasts to clients. A timer doing that produces
revisions nobody asked for, lands them in the undo stack, and causes spurious conflicts
when an agent's `base_revision` goes stale while it is thinking. That is the exact failure
the revision counter exists to prevent, fired by noise.

**Cheap drift detection, explicit correction** — deferred.
The direction taken from Kdenlive, which stores `kdenlive:file_size` and
`kdenlive:file_hash` per clip, raises a Clip Problems dialog for missing files on open,
and otherwise makes you pick Reload Clip by hand. Detection can be frequent because a
stat costs no subprocess; correction stays a user action that writes one revision.

**Moving metadata out of the document into a cache** — deferred.
The structural fix, since probed metadata is derived data sitting in the source of truth.
Keyed by path, size and mtime the way the bake cache already works, staleness becomes
impossible and the revision is never touched. Held back until flag-and-reload proves
annoying, because the validator and the agent both read metadata today.

## Still open

- Folders and search in the browser, now that the bin is the thing they organise.
- A re-probe or reload action on a bin entry.
- `kuma add-media` resolves a relative path against the shell's directory rather than the
  project's, which surprises when run from elsewhere.
- Overlays bake at full frame size; there is no per-template bake region.
- Export presets are a `const` table in Rust and are not pluggable. Effect presets, being
  JSON on the library search path, already are.
