# kuma-editor — decision record

Decisions taken during the review and bin refactor on `claude/happy-davinci-tspnqo`
(commits `eede385` and `25849c1`), and during nested sequences on
`claude/fervent-hypatia-cpx7e6`. `docs/plan.md` is the original plan and predates both;
where they disagree, this file is later.

Status key: **adopted** is in the code, unless the entry says otherwise; **rejected** was
considered and turned down; **superseded** was adopted earlier and has since been
replaced, and the entry says by what; **deferred** is agreed direction, not yet built. Where a decision was to
*not* build something, the entry says so rather than implying an implementation.

---

## Nested sequences

**Adopted.** A sequence is a timeline that can be placed on another timeline, so parts of
a video are edited separately and juxtaposed in the final render. It was the priority for
whoever picked the project up, and it is now in the code.

**Shape.** A sequence is a bin entry, which is how Kdenlive does it — its file format
documentation describes `main_bin` as listing clips *and* timeline sequences as entries.
It is a fourth source variant rather than a new concept:

```rust
pub enum BinSource {
    File { path, kind, proxy },
    Overlay { template, vars },
    Color { color },
    Sequence(Sequence),          // { tracks, markers }
}
```

`Project.tracks` and `Project.markers` are gone; the project keeps `master`, the id of the
entry that is the master timeline. Everything a clip already did applies to a sequence
unchanged — naming, trim, `in`, effects, transitions — because a clip is a placement and
does not care what it places. This was the payoff of the bin refactor.

`tracks` is **required** on the sequence variant rather than defaulted. The enum is
untagged, so without that an entry whose source matched nothing would quietly deserialize
as an empty timeline instead of being rejected.

**The format stays version 1, with no migration** — decided rather than inherited. This
supersedes nothing: the earlier "no upgrade path" entry below said changing the format
means bumping `PROJECT_VERSION` and writing one, and the answer here was that there is
nothing to migrate, so neither happened. A `*.kuma.json` written before this change does
not open; `kuma init` writes the new shape.

### What the spike settled

The previous version of this entry said to settle speed on a sequence clip with `melt`
before writing any Rust, because `timewarp` takes a resource path and it was unconfirmed
that it accepts a tractor. It does not, and four other things came out of the same
afternoon. Measurements used a 120-frame source whose frame *i* is a solid grey of level
*i*×2, so a decoded frame names its own source index.

1. **`timewarp` cannot take a tractor.** `resource="0.5:seq"` makes MLT resolve `seq` as a
   file path: `[producer_xml] failed to load producer "0.5:<root>/seq"`, and the output is
   one frozen frame rather than an error. **`timewarp` on a `.mlt` *file* does work** —
   60 output frames spanning source 0..30 at 0.5×. So the file route is available, and
   what shipped is the other option: **speed ≠ 1 on a sequence clip is a validation
   error** naming the workaround (render the part out, place it as media). Doing it
   properly means writing each sped-up sequence to its own `.mlt` in the cache and
   regenerating it per compile and per size, which is a side-file lifecycle the compiler
   does not otherwise have. Left for when someone wants it; it is known to work.
2. **Trimming into a nested tractor is exact**, so `in` on a sequence clip needed no
   special mechanism.
3. **One tractor can back several placements.** Three entries into one tractor with
   different `in`/`out` gave three different ranges. So a sequence is emitted **once**,
   and the alternative — a tractor per placement — was rejected: it duplicates the whole
   subtree, multiplicatively once sequences nest.
4. **A filter on a `<playlist>` is attached to that playlist and honours its frame
   range.** This is what makes one shared tractor workable: each placement is a wrapper
   playlist carrying the cut and *that placement's* effects, which do not reach the other
   placements. Verified twice — a `brightness` filter dimmed one of two placements and
   not the other, and with `in="10" out="19"` on a two-entry wrapper it dimmed only the
   second entry. The MLT DTD does not allow it, and the DTD is what is wrong: Kdenlive
   carries track effects the same way. Our local copy of the DTD was relaxed, with the
   evidence written next to it.
5. **A nested sequence's background track must be transparent** (`0x00000000`), not
   `black`. With an opaque base a sequence painted over everything under it (a gap read 5
   instead of the 128 underneath). Only the master, which is the bottom of the render,
   gets an opaque base. This was a bug that would have shipped: nothing about the master's
   own compile hints at it.

Audio needed nothing: a nested tractor plus wrapper measured identical to the flat path
(−24.1 dB mean, −18.0 dB max both ways). Neither did the C++ preview engine, which loads
the document through a plain `Mlt::Producer(profile, "xml", path)`.

### The three parts that were real work

1. *Cycle detection*, landed with the model as agreed. `Project::compile_order` is one
   depth-first walk that both orders the compile and reports a ring, so the validator and
   the compiler cannot disagree about what is cyclic. It runs over **every** sequence, not
   only what the master reaches, so a ring is refused as it is built rather than when it
   is first placed, and it reports one issue rather than one per entry on the ring.
   `seq_has_audio` carries its own visited set as well, because the daemon keeps calling
   these on documents the validator has already refused.
2. *Topological compile order*, innermost first, so an outer sequence only ever references
   a tractor already written. `compile` emits one block per sequence and the root becomes
   `main_tractor`.
3. *Preview scope*. The daemon holds one open sequence as **session state** — not in the
   document, so it never bumps the revision, never lands in the undo stack and never
   causes a `base_revision` conflict; a project reopened tomorrow starts on its master.
   `GET`/`POST /api/sequence` read and set it, and setting it recompiles the preview. A
   chosen sequence that is deleted (undo, an agent edit) falls back to the master on both
   sides rather than failing every compile until someone notices.

`bake_all` walks every sequence, as the old entry warned. `sample_frames` and `render`
take an optional `sequence` and default to the master, which keeps MCP stateless: the
agent says which timeline it means instead of depending on what the human has open.

### What broke, and how it was found

Four defects shipped in the first cut and were caught by a correctness review afterwards.
Recorded because two of them came from the *cleanup* passes, and because the shape of the
mistake repeats.

1. **A hidden track's overlays stopped being baked, so those projects would not render.**
   A `!t.hidden` filter was added to the bake selection as an optimisation. Hiding is per
   track and applied on the tractor, so the compiler still emits the clips, and under
   `UnbakedPolicy::Error` a render died with "overlay clip has no bake". *The compiler
   decides what needs a bake; the daemon does not get to decide it is unnecessary* — now
   pinned by a test in `tests/golden.rs`.
2. **`muted` did nothing on a sequence clip.** A media clip mutes by not decoding its
   audio (`audio_index=-1`); a wrapper playlist has no decoder to switch off, so the sound
   came through whenever another clip kept the track's mix alive. It now carries a
   `volume` filter at `MUTE_DB`. That constant is -200, not the `db()` floor of -60,
   because -60 dB measures -87 out of the encoder against -91 for a silent file; the floor
   itself stays at -60, since volume *ramps* interpolate through it.
3. **`sample_frames` baked every overlay the sequence reaches**, having lost the
   "intersecting the sampled range" filter in a refactor — so one frame paid for the whole
   timeline, and because that path is strict, one broken template anywhere failed a sample
   that would never have shown it.
4. **A deleted open sequence was remembered.** The fallback to the master did not clear
   the stored id, so re-creating that id — an undo, an agent adding it back — silently
   reopened it on the daemon while every client still showed the master.

The process lesson is worth more than the four fixes. Both quality passes explicitly left
bugs aside ("that is what a correctness review is for"), and one of them introduced a
regression that stopped renders working. *A refactor is not verified by the refactor
passing review; run the correctness pass after it.* None of these four announced itself —
a silent overlay and a stale preview both look exactly like working software, which is why
they are worth tests rather than care.

**Track and clip ids stay unique across the whole project**, not per sequence. They are
MLT element ids in one compiled document and URL segments in one daemon, and keeping them
global means `find_clip(id)`, the overlay preview routes and the bake map all work
unchanged.

**Overlays are live where you are editing and baked where you are not** — adopted, after
the first cut shipped without it and an overlay inside a nested sequence was simply absent
from the preview.

The preview compile skips unbaked overlays, and the editor drives the open sequence's own
overlay clips as live DOM. A nested sequence's overlays fell between the two: nothing
drove them, because their clip times are their own sequence's rather than the playhead's,
and nothing baked them either. The fix is to bake exactly those and leave the rest skipped
— the two sets are disjoint, so no overlay is ever drawn twice.

The split is not a compromise, it is the distinction the editor already draws. An overlay
on the sequence you have open is being edited, so it must be live: that is why live DOM
exists. An overlay inside a sequence you have merely placed is finished content, and a
bake is the honest picture of it — it is what the render will show. Because bake keys are
content hashes, it is a cache hit on every reload after the first (7 ms against 200 ms in
a project with one nested overlay), and it warms the very entry the render then reuses.

Two things this had to not break. Baking is **best effort**: a template that will not bake,
or no hyperframes installed at all, costs that one overlay in the preview and never the
preview itself, which was free before. And the baked set is narrowed to the sequences the
open one actually reaches, so a bin full of unplaced sequences does not buy a bake on every
reload. Narrowing it by *visibility* as well was tried and was a mistake — see "What broke"
above: the compiler emits a hidden track's clips, so skipping their bakes broke renders.

The UI's engine key had to follow: overlay bin entries and overlay tracks used to be
stripped from it wholesale, on the grounds that no overlay could change the engine's
picture. Now the nested ones can, so only the *open* sequence's overlay tracks are
stripped, and an overlay entry is dropped only when nothing outside the open sequence
uses it.

**The daemon decides whether the engine needs a new timeline; the client never guesses** —
adopted, after the client's guess was wrong twice.

Reloading the engine means rebuilding its MLT graph, which is the visible hiccup, so an
edit that cannot change the picture must not cause one. That decision used to live in the
browser: `engineKey` hashed "everything the preview compile depends on" and only sent
`reload` when it changed. Which meant restating, in TypeScript, what the Rust compiler
reads — the root sequence, reachability through `compile_order`, that the open sequence's
overlays are skipped while nested ones are baked, the profile fields, the entries those
tracks place. Every one of those is a fact that lives in the compiler.

It drifted as fast as it was written. Baking nested overlays made the key wrong (nested
overlay edits would not reload); the fix after that excluded an overlay entry placed both
on the open sequence and inside a nested one (its baked copy is in the picture, so it must
reload). Two of three versions shipped wrong, and neither failure was visible — a stale
preview looks like a preview.

So the client stopped predicting and the daemon started comparing. `preview::reload`
hashes the compiled XML — which already names every input, including the bake directories,
whose names are themselves content hashes — and skips the `load` when it matches what the
engine holds. `engineKey` is deleted. The guard is unchanged in effect; it moved to the
side that is holding the answer.

Measured before committing to it: a warm reload (bake-cache probe, compile, write, send)
is ~2 ms against the ~0.5 ms every revision already spends serving `GET /api/project`, so
asking on every edit is not a cost worth optimising. It is also *sharper* than the key
was: `engineKey` serialized whole clip objects, so renaming a clip forced a rebuild; the
document comparison does not, because a label never reaches the XML.

Two things this needs to stay correct, both of them tested. Compiling must be
deterministic, or the hash never matches and the guard silently does nothing — `bakes` and
the transition-extension map are both `HashMap`s, so `compile.rs` has a test that the same
project compiles byte-identically eight times running. And a restarted engine holds
nothing, so the stored hash is cleared whenever one starts; `changed` is reported by
counting loads rather than comparing hashes, because a fresh engine has to be handed the
same document again.

`crates/kuma-daemon/tests/preview.rs` is where those live, and it is the daemon's first
test: it drives a real `AppState` and a real engine process, and skips with a message when
the engine is not built, the way the render tests skip without melt. Its project is one
colour clip, so it needs no media, ffmpeg, melt or hyperframes. The tests were
mutation-checked rather than trusted for being green — making the reload never skip fails
two of the three, which is the only evidence that a passing test is worth anything.

**Reloads are coalesced, not debounced** — adopted.
A caller arriving while a compile is running sets a flag instead of queuing, and the
running pass goes round once more; the extra pass re-reads the project, so the last thing
compiled is always a state observed after the last edit. Thirty-two simultaneous reloads
cost two compiles — the pass that was running, plus one for everything that queued behind
it, which is the whole of what the mechanism can collapse to. (Over HTTP the same burst
measured ten, because the requests arrive spread out rather than together; that is arrival
spread, not the coalescing falling short.)

Debouncing was considered and turned down. The UI already commits rather than streams —
fields patch on `onchange`, drags patch on `pointerup` — so a human editing produces one
patch per deliberate edit and there is no burst to collapse; a delay would only put
latency between an edit and seeing it, which is the one thing a preview must not do.
Bursts come from agents and from held-down undo, and those overlap in time, which is
exactly what coalescing collapses. Cancelling an in-flight compile was also rejected:
there is nothing to save in 2 ms of compiling, and mid-bake it would throw away a headless
browser run that is seconds deep and probably still wanted.

**Rejected: mapping a nested overlay's time up to the playhead and driving it live.** It
is what "live everywhere" would need, and each step costs something. One sequence can be
placed twice, so one overlay clip becomes two iframes and the view map stops being keyed
by clip id. The placement's own trim and effects would have to compose with the inner
clip's. And it would still be an approximation, since every overlay iframe is drawn above
the engine's canvas regardless of what the timeline stacks above it. Baking is less code
and is exactly right by construction.

---

## Subtitles and vertical

**Subtitles are an overlay entry, not a model concept** — adopted.

A transcript is the `cues` var (`[{start, end, text}]`, source seconds) of the `subtitles`
template, so it is a bin entry like any configured overlay: placed on an overlay track,
edited in the inspector (one row per cue, so a misheard word is fixed in place and the live
preview follows), hashed into the bake key, undone as one step. The only model change is a
`cues` var *type* — a list has no CSS custom property, so it reaches the template through
`window.kuma.vars` alone, which the contract already allowed for arrays. The alternative, a
fifth `BinSource` variant, would have touched the compiler, the validator, the bake, the
thumbnail job and the UI for something that "HTML templates subsume title clips" already
covers: a subtitle is a title with timing.

**Listed under the media, aligned from a clip** — adopted, replacing a first cut where
the job took only a bin entry and left placement to the user. Cues stay in source seconds
and the entry carries an `offset` var. From a *clip*, the job places the subtitles over
it in the same patch, copying the clip's start, end and speed and setting `offset` to its
`in`: at every timeline instant the template's own time is then `source − in`, so the
cues line up exactly, speed included, with no per-cue arithmetic. It goes on the first
overlay track with room at that range, else on a new one — alignment is the point, so a
clip is never pushed along a busy track the way an ordinary drop is. From a bin entry the
job makes the entry alone.

**`parent` on a bin entry, not path-shaped ids** — adopted, and it is the ground for
folders. A transcript is listed under its media through `parent: Option<BinId>`,
meaning only "shown under this entry in the browser"; nothing about rendering reads it.
Path-shaped ids (`speech/audio.mp3/subtitles`) were rejected: the id would become the
location, so moving or renaming a folder would rename every descendant and rewrite every
clip that references one, and a `/` breaks the id rule that keeps bin ids usable as XML
ids, URL segments, cache file names and pointer segments. Kdenlive stores a folder id per
clip for the same reason. Not `children` lists on the parent either: two-way lists drift.
A folder is exactly that: `BinSource::Folder { folder: true }`, an entry with a name and
children, refused on a track by the validator and by the compiler, folded in the browser
behind a caret. The tree is drawn from `parent` alone, and the fold state is the panel's
own, never the document's.

**Manual order is an optional `order` per entry** — adopted. The bin is a map keyed by
id, so its own order is alphabetical, and JSON Patch cannot move a key within an object;
an ordered map was rejected for that reason. Entries with an `order` come first, by it,
then the rest by id, so a new entry lands at the bottom and only a drag ever writes the
field: a move renumbers the siblings 0..n in one patch with the `parent` change, one undo
step. The validator refuses a dangling or circular parent, which is also what
stops removing a media entry while its transcript is listed under it.

**Cues are words; lines are the template's** — adopted, replacing a first cut that asked
whisper for 24-character lines. Grouping is presentation, and at transcription time it is
baked in at the one moment that cannot be revisited without re-running the model: from
words any line length follows, from lines the words are gone. The text is identical either
way (`-ml` is post-processing on the decoded tokens), and so is a line's timing, since any
`-ml` above 0 already places its cuts with whisper's token-level timestamps; only a
per-word highlight exposes their jitter, which is inherent to karaoke from any transcriber.
So the job defaults to `-ml 1 -sow`, the template takes `words_per_line` and a `stress`
mode, and `max_chars` stays for whoever wants whisper's own grouping. The cost is a longer
cue list in the inspector, one row per word.

**Where the words go is decided when they exist, not before** — adopted, after both
failures were reproduced against a running daemon. The job used to snapshot the project,
check the id, run the model for minutes, then patch from that snapshot. With a
`base_revision` — what the editor sent — any unrelated edit meanwhile refused the patch
and threw the transcription away (renaming one entry three seconds in was enough). Without
one, the id check was minutes stale and a JSON Patch `add` on a name taken in the meantime
*replaces* that member, so someone else's entry was silently overwritten and the job
reported success.

So the model still runs outside every lock, but nothing about where its words land is
settled until they are in hand: `AppState::apply_built` composes the patch against the
live project under the edit lock, and the id, the entry to replace, the overlay track and
the clip id are all resolved there. An unnamed transcript *mints* its name then
(`<bin>-subs`, then `<bin>-subs-2`…), which is why a second run now adds a second
transcript rather than colliding; naming an `id` is how you mean one entry, and `replace`
is the only way to overwrite it. `base_revision` is gone from this job: over minutes it
only ever meant "nobody touched anything", and a transcript depends on nothing in the
document but the entry it lands on.

**Re-running replaces the words, not the entry** — adopted. A second transcript of the
same media used to be refused outright ("bin id already exists"), which is right by
default — it would overwrite work — but it left correcting a bad language or a bad model
with no move but deleting the entry, and with it the styling, the placement and any
template an agent had pointed it at. So `replace` patches `cues` (and `offset`, which
follows the clip it is aligned to) and nothing else, and places a clip only when the entry
is on no timeline yet. The editor asks before doing it, and offers a second entry beside
the first as the alternative; the language sits in the same dialog, since forcing it is
the other thing only a person can answer, and it is remembered because a person's videos
are usually in one language.

**whisper.cpp as a subprocess** — adopted, the way melt and ffmpeg are. `whisper-cli` on
`PATH` (`KUMA_WHISPER_BIN`) and a ggml model in `~/.local/share/kuma/models`
(`KUMA_WHISPER_MODEL`), discovered at daemon start like the baker; absent is a job error
naming what to install, never a daemon that fails to start. ffmpeg extracts 16 kHz mono
WAV first, `-oj` gives JSON with millisecond offsets, `-ml N -sow` splits segments on
words to the short lines shorts use. Python `openai-whisper` and hosted APIs were passed
over for now: whisper.cpp is the model people run on a laptop, and its JSON is the only
shape the parser knows. The daemon test stands a shell script in for the binary, so the
argument plumbing, the WAV step and the patch are exercised without a model.

**Vertical is `width`/`height` and nothing else** — adopted. The compiler already read the
profile off the project, presets carry no size, templates size in `vh`, and the stage
letterboxes from the project size — so "switch orientation" is one button that swaps the
two fields in a patch. A portrait *flag* was rejected: a second source of truth for the
frame size that every reader would have to reconcile. The one thing a swap does not move
is a pixel-valued `transform` offset, and that is documented rather than guessed at.

---

## Desktop shell

The editor became an application: a launcher that opens or creates a project, and a window
per project, with the daemon's lifetime tied to the window. `app/` is an Electron main
process; the editor page inside the window is the existing UI, served by the daemon,
unchanged.

**Electron rather than Tauri** — adopted, against the grain of a Rust workspace.

Tauri was the obvious choice and lost on one point. Its Linux webview is WebKitGTK, and
the overlay baker drives **Chromium** (hyperframes on Playwright's
`chromium_headless_shell`). The premise in the README is "same HTML, two consumers": live
DOM while you scrub, baked alpha at export. Previewing in one engine and baking in another
breaks that quietly — a blend mode, a font metric or a filter that differs shows up only
after a bake, which is the slowest possible feedback loop. Electron is Chromium, so the
scrub and the export agree by construction.

What that cost: Tauri would have been ~10 MB against Electron's ~200 MB, and — the part
that actually stings — `kuma-app` could have been a Rust binary calling
`kuma_daemon::serve()` in-process, with no subprocess to supervise at all. The shell
supervises `kuma serve` instead. Judged worth it, because overlay fidelity is the product
and the binary size is not.

Also weighed and rejected: **Chromium in `--app=` mode** driven by a `kuma app`
subcommand. Right engine, almost no new machinery, but no native menus or dialogs, and a
launcher window that spawns editor windows is awkward without a process that owns them.
**Wry, Neutralino, Photino** are all system-webview, so they lose for Tauri's reason
without Tauri's compensations.

**One daemon per project, owned by the shell** — adopted.

The alternative was one daemon taught to switch projects, which is what a "launcher plus
editor" shape suggests. It was rejected: `AppState` is loaded from a project file and
handed to the router as an `Arc`, and the preview engine, job list and undo stack all hang
off it, so switching means making every handler read through an indirection and tearing
down the engine on each swap. More importantly `kuma mcp` resolves a daemon *by the
project it serves* — a daemon that changes projects underneath that check is a race with
no good answer. A daemon per project needs no daemon change at all, and multiple open
projects fall out of it.

So the shell starts one, waits for `/api/health`, and stops it when the window closes;
quitting stops all of them. A daemon that dies on its own raises a dialog with the tail of
its output and an offer to restart, rather than leaving a blank window.

**Attaching to a daemon already serving that project** — adopted, and it is the one
exception to the rule above.

Two daemons on one `.kuma.json` would both believe they are its single owner, which is the
invariant everything else rests on. And a daemon already on that file is most likely a
`kuma mcp` with a Claude Code session behind it. So the shell joins it, shares its state,
and does **not** stop it when the window closes — it did not start it. Every daemon the
shell started, it owns completely.

**The first project takes 7455** — adopted.
That is where `kuma mcp`, and so the `.mcp.json` that `kuma init` writes, looks. Opening a
project there means an agent attaches to the window you have open with no configuration.
Later projects take an ephemeral port; their agents get a private daemon, which `kuma mcp`
already explains when it happens.

**The launcher is a page in the shell, not a route in the editor UI** — adopted.
The editor UI is served *by a daemon*, and a daemon always has exactly one project. There
is no point in that page where "no project is open" is a reachable state, so a launcher
route there would have to be served by something else anyway. It lives on a `kuma-app://`
scheme rather than `file://`, so ES modules load and the CSP has a real origin to name.

**Menu accelerators for the editor's own shortcuts** — rejected.
Space, `S` and Ctrl+Z are bound in the page, where the handler can tell whether a text
field has focus. An Electron menu accelerator fires regardless, so `S` would be stolen
from every input in the inspector and Ctrl+Z would run a project undo while you are typing
a label. The Project menu therefore carries those items with **no** accelerator, and the
Edit menu keeps the native `undo`/`redo` roles, which are text editing and should stay
that way.

**Desktop features degrade rather than gate** — adopted.
`ui/src/desktop.ts` exports `desktop()`, which returns the preload's bridge or `null`. The
native file picker and the menu commands are behind it; in a browser tab the UI is exactly
what it was. `kuma serve` plus a browser has to keep working — it is what CI, a remote
machine and `kuma mcp` all use.

**The shell distrusts the binary it runs** — adopted, after it bit someone.

`kuma init` is an external program, and the shell can be pointed at a `kuma` older than
the checkout — a stale `target/release/kuma`, or one `cargo install`ed months ago. A
binary from before the bin refactor writes a project with `tracks` on it and no `bin`,
which nothing here can open. The first report of this was a new project that created its
folder, failed to open, and then refused to be created again because the folder was in the
way: three confusing symptoms for one cause.

So `create` reads back what `kuma init` wrote and, if it cannot be opened, **removes the
directory it made** — a folder that blocks a retry while being useless is worse than no
folder — and says the binary is the problem. `checkOpenable` names the old format when it
sees one rather than reporting "not a kuma project", which sends people to inspect a file
that is fine for the version that wrote it. And the launcher's footer shows which binary
was resolved and whether its commit is an ancestor of HEAD, which is what `build.rs`
stamps the commit into `--version` for in the first place. Only a strict ancestor is
reported, so a binary built from another branch or a non-git install stays quiet.

**Nothing the daemon serves may be reused without revalidating** — adopted, after this
turned out to be the real bug behind two rounds of misdiagnosis.

`ServeDir` sent no `Cache-Control` and no `ETag`, only `Last-Modified`. A browser then
falls back to *heuristic* freshness: it invents a lifetime from the file's age and serves
the response without asking again. So an editor window that had cached `ui/dist` kept
running that bundle after a correct rebuild — the files on disk were new, the daemon was
new, and the window was executing month-old JavaScript. It reported `t.tracks is not
iterable`, which reads exactly like a stale build, so the first two fixes chased staleness
on disk and found nothing wrong with it.

The router now adds `Cache-Control: no-cache` to every response, `if_not_present` so the
handlers that chose `no-store` keep it. `no-cache` permits storing and requires
revalidating, which on loopback is a 304. It covers more than the editor: a template being
authored is reloaded constantly, a regenerated thumbnail keeps its URL, and API JSON is
live state that was never safe to cache.

A cache entry stored *before* this keeps its invented freshness, so the shell also clears
its session cache at startup. Everything it loads is on loopback and worth nothing between
runs.

The lesson worth keeping: "the artifact on disk is current" and "the running program is
that artifact" are different claims, and only the second one matters. Both earlier fixes
checked the first.

The same reasoning covers `ui/dist`, which is not in git either and which the *daemon*
serves. A build from before sequences became bin entries reads `project.tracks`, spreads
it, and dies with `t.tracks is not iterable` before the editor draws anything — a message
that names nothing a user could act on. That one is not a footer warning: the window is
certain to be broken, so opening a project checks whether `ui/dist` is older than `ui/src`
and asks first, with "Open anyway" kept because the check is an mtime heuristic and must
never be a hard no.

Three stale-artifact traps in a row (`app/dist`, the `kuma` binary, `ui/dist`) is a
pattern, not bad luck: the shell is the one component that runs all three and is therefore
the only place that can see the skew. `app/dist` it simply builds, since it owns it. The
other two it refuses to paper over, because rebuilding a Rust binary or the daemon's own
assets behind the user's back is not the shell's call.

Still open here: packaging (there is no `electron-builder` config, so the app runs from the
checkout), remembering window size and position, and opening a second project from an
editor window rather than going back to the launcher.

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

**A two-tier model: reusable templates inline, one-offs in the bin** — rejected. (A
different split was made later; see *Overlays and blueprints*.)
Considered making the split by intent, so a location tag stayed a template plus vars on
the clip while a bespoke animation became a bin entry. Dropped because the distinction is
about expected reuse rather than about the content, and because the argument for it
rested on an agent cost that does not exist: adding an entry and a clip is two operations
in one patch, one revision, one undo entry. One mechanism is also less code than two.

**HTML overlays subsume title clips** — adopted.
Kdenlive needs a separate title-clip producer. kuma does not, because an overlay with a
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
byte-identical, which is the proof that restructuring the model changed no pixels. It did
so again through nested sequences — the emitted document for the pre-existing tracks did
not move by a byte while the compiler was restructured around them, and stayed still
through two later refactors of the same code.

**The DTD is a local copy with two fixes** — adopted.
`tests/fixtures/mlt-xml.dtd` is not what MLT 7.22 ships. `properties` was declared as
exactly one `property`, which no real document obeys, and `playlist` did not allow
`filter` — although MLT attaches one to a playlist and honours its frame range, which is
how Kdenlive carries track effects and how a nested sequence's placements carry their own.
Both were measured before the DTD was relaxed, and the evidence is written beside the
declarations. Prefer fixing the copy over emitting worse XML to satisfy it.

**A baked overlay is read at the project's rate, `force_fps`** — adopted, after subtitles
rendered later and later behind the preview. A bake is one PNG per timeline frame, but an
image sequence carries no rate, and ffmpeg's image2 demuxer reads one at 25 fps: in a 30 fps
project every animated overlay played at five sixths speed, a fifth of a second late per
second into the clip. The preview never showed it because the open sequence's overlays are
live DOM, and the render test's bake was a box that does not move. The compiler now sets
`force_fps` to the project rate on every animated bake, and
`a_baked_overlay_plays_at_the_project_rate` reads a bake whose frame k is grey level 4k.
MLT's own sequence readers (`pixbuf`/`qimage` with a `%06d` resource) were measured frame
exact too, by seek and in sequence; `force_fps` was kept as the one-line change.

The frame cache of `sample_frames` is keyed on the compiled document, like the preview,
rather than on the project revision and the bake paths: under the old key, a kuma that
compiles differently kept serving frames an older one had sampled, which is how this bug
first looked unfixed after it was fixed.

## Overlays and blueprints

**An overlay belongs to its project; a blueprint belongs to the user** — adopted, format
version 7. "Template" meant two things: the library's reusable overlays, and every one-off
an agent wrote into a project's `overlays/`, which the search path loaded as a template too.
An entry named a template id, so a library edit re-styled finished videos and a project
rendered differently on another machine.

- An **overlay** is a project folder, `overlays/<name>/` (`overlay.json` + `index.html`);
  an entry names it, `{ "overlay": "overlays/<name>", "vars" }`, relative and inside the
  project. The folder is the identity: the manifest has no `id`.
- A **blueprint** is the same shape in `<library>/blueprints/`, read only from libraries,
  never through an entry.
- Every move is a copy (`crates/kuma-daemon/src/overlays.rs`): using a blueprint copies it
  in; a folder reaches the bin only by `import_overlay`; entries may share a folder and
  **Make unique** copies it for one; **Save as blueprint** copies an entry back to the
  library with its vars as defaults. Undo leaves copied folders on disk.
- `kumaTemplate()` in `_vendor/kuma.js` keeps its name: every blueprint calls it.
- **No migration**: a v6 project with template entries is refused on open, saying how to
  convert it; one without is stamped.

**Import copies from anywhere** — adopted, reversing a first cut that refused folders
outside the project. Dragging an `overlay.json` in from any folder is the natural gesture;
it is copied into `overlays/` first, so the project still owns what it plays.

**Named vaults** — deferred. A shared set of blueprints for a series of videos, e.g.
`<library>/vaults/<name>/`, chosen per project. `KUMA_LIBRARY` is a crude version today.

**Blueprints and presets live outside the codebase** — adopted. kuma reads
`$XDG_DATA_HOME/kuma`, then every `KUMA_LIBRARY` directory, later winning; presets also
come from the project's `presets/`, blueprints never do.

**`library/` in the repo is a starter set, not a runtime path** — adopted.
`kuma library` installs it into the user library. Tests point `KUMA_LIBRARY` or the baker
at the repo copy.

**A matching search path for media** — deferred.
Templates got a library; media did not, so a shared animation or sting folder has no
first-class home and each project relinks it by hand. Absolute paths already work, so
this is about browsing and organisation rather than capability.

## Classic titles and fonts

**A title is a blueprint whose every look is a var** — adopted. Kdenlive has a title
producer and a title editor; kuma already decided that HTML overlays subsume title clips,
so a "classic" title is `library/blueprints/title` (and `title-subtitle`): typography
(`font`, weight, italic, size, colour, case, spacing, line height, alignment), layout
(position, anchor, max width), outline, shadow, a box, and an entrance and exit by name.
Size is a percent of the frame's short side, so a title reads the same in portrait.

**The bin's New menu is manifests saying `create`** — adopted. *+ New* in the bin (and a
right-click, which creates inside the folder under the pointer) lists every blueprint with
a `create` label, then *Overlay from a blueprint…*, *Sequence…* and *Folder…*; the desktop
menu has *New Title*. Rejected: a list of classic kinds in the UI, which a user's own
blueprint could never join, and a separate "title" source variant, which the overlay
already is.

**Created without a dialog, one folder each** — adopted. The entry is named "Title",
"Title 2"…, opened on the workbench with its vars in the inspector; renaming is the bin's.
Considered: every title sharing one `overlays/title` folder, since the vars carry
everything. Rejected by the user: nobody should have to look in `overlays/`, and a shared
folder makes an edit to one title's markup an edit to all of them. The bin's folders are
how titles are organised.

**The workbench opens an entry at the middle of its length** — adopted. It opened at 0,
where every overlay with an entrance is invisible, so a title made from the bin appeared
as a blank stage. Where it was left still wins after the first look.

**Vars are shown in the order the manifest writes them** — adopted. `vars` is an
`IndexMap` (with `serde_json`'s `preserve_order`, already on); it was a `BTreeMap`, which
showed every blueprint's controls alphabetically. The resolved vars are in the bake key, so
every cached bake misses once.

**`group` and `when` are presentation** — adopted. `group` folds vars into sections of the
inspector; `when` hides a control while other vars do not hold the given values (an outline
colour with the outline off). A hidden var keeps its value and still reaches the overlay,
and the manifest check refuses a `when` naming a var that is not there or a value it
cannot take. Rejected: a `groups` array at the manifest's top level, which says the same
thing twice once vars keep their order.

**Entrances and exits are named, in `_vendor/kuma-motion.js`** — adopted. The same nine
entrances and seven exits for every classic blueprint, so an enum means the same thing in
each. Shared like `kuma.js`, not copied into projects, which means changing what a name
does re-animates finished videos: the names are a contract, keep them stable and add
rather than change.

**A `font` var is a family name, resolved against folders of font files** — adopted.
The search path is every library's `fonts/`, then the project's `fonts/`, which wins
whole: a project carrying a family renders that one. The family, weight range (a variable
font's `wght` axis) and italic are read from the files' own tables (`ttf-parser`), so a
file dropped in under any name is found under the name a person types. kuma writes the
`@font-face` rules into the contract; each face has one name in its family,
`<family>/<i>.<ext>`, which the bake stages under `_kuma/fonts/` and the daemon serves
under `/fonts/` (to the preview and to the editor's font picker alike), and the bake key
hashes the font bytes — a digest taken when the file is read, not on every bake. The
starter set is twelve open-licence families (`library/fonts/`, licence beside each:
OFL, Apache, and the Ubuntu Font Licence), 9.4 MB. Rejected:
- **System fonts.** A project would render differently on another machine — the problem
  copying blueprints exists to avoid.
- **Fonts inside each overlay folder.** Choosing a family from a list would have to copy
  files into the folder, so a var edit would have a filesystem side effect.
- **Google Fonts.** Network in a bake; the lint already refuses it.

`.ttf`/`.otf` only: WOFF and WOFF2 are compressed and would need decoding to read a name.
Adding a font is a file, not an edit, like an overlay folder: it is copied into the
project's `fonts/` or, for every project, the user library's.

**Fonts are added by path** — adopted, as media and overlays are. `POST /api/fonts
{ path, shared }` (the **+** by a font picker, MCP `add_font`): the desktop app's picker
gives the path, a browser tab types it, and the daemon reads and copies the file. A first
cut uploaded the bytes instead, which works from a browser that cannot say where a file is
and from an editor on another machine than the daemon; it was the one upload-shaped route
in an API that otherwise takes paths, and a CLI or an agent had no way in. **Deferred: the
byte upload**, for the day the daemon runs apart from the editor. It is a fallback beside
the path, not a replacement, and it is not only for fonts — adding media and importing an
overlay from anywhere have the same limit then.

**A library font is not copied into the project** — adopted, for now. A project names a
family that lives in the library the way it references `_vendor/`, so opened on a machine
without the family it renders in the overlay's fallback stack, and `check_overlay` warns
`font_missing`. *Collect fonts into the project* is the obvious follow-up, **deferred**.
Save as blueprint has the mirror gap: a blueprint saved from an entry that uses a project
font names a family the library may not have.

**`kuma library` always refreshes `_vendor/`, and adds new faces to installed fonts** —
adopted. It skipped any folder that existed, so a new or changed file in `_vendor/`
(`kuma-motion.js`) never reached an existing install without `--force`, which also
overwrites blueprints someone edited. `_vendor/` is kuma's runtime rather than a user
asset — the blueprints installed beside it need the one shipped with them — so it is always
replaced; a font family already installed takes the faces it lacks; everything else there
is left alone. Considered: merging missing files into every folder, which would also bring
back a file someone deleted from their own copy of a blueprint.

**What `_vendor/` reads is not the overlay's** — adopted. The lint counts the stylesheets
an overlay links as part of what it reads, so `kuma.css`'s `var(--var-size, 1)` made every
overlay linking it read `size`, and the titles, which have none, were told they read an
undeclared var. A read in a `_vendor/` stylesheet now counts toward a var being used (so
`size` is not reported unused where `--u` is how it is read), never toward one being
unknown. Rejected: excusing every read that has a fallback, which would also have hidden a
typo like `var(--var-colr, red)` in the overlay's own CSS.

**More classic blueprints** — deferred. Shapes, rolling credits and crawls, a quote card,
a progress bar, a spotlight, a timer, a checklist, a typed code block: the same pattern,
each with `create` once it exists.

## Preview

**Engine reload is keyed on non-overlay state** — superseded, by "the daemon decides when
the engine needs a new timeline" under *Nested sequences*.
The rule itself still holds — an overlay-only edit on the open sequence must not reload
the engine — but the client no longer works it out. The daemon compares the compiled
document instead, which covers this case and the ones a client-side key kept getting
wrong.

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

**Picture and sound as two graphs** — adopted, and built end to end. Today one graph carries both, so an audio edit changes the compiled document and
the engine rebuilds every producer and re-sends a picture that did not change, pausing
playback to do it. That will not survive audio effects, where dragging a knob is an edit
per step. The split: `CompileOptions::streams` (`Both`, `Video`, `Audio`) compiles a half
graph from the same document. The video half drops tracks that only sound, decodes no audio
(`audio_index=-1`) and keeps only the video half of each transition; the audio half drops
tracks that make no sound, overlays included (so it needs no bake), decodes no video
(`video_index=-1`) and keeps only `mix`. A render is always `Both`, so the golden file and
melt are untouched. `each_half_graph_matches_the_full_graph` in `kuma-mlt/tests/render.rs`
is what makes this an optimisation and not a second renderer: per-frame MD5s of each half
must equal the full graph's, and it was checked to fail when either half drops a filter.

The engine holds the two graphs on two threads. The sound's thread is the clock: samples
actually handed to the device, counted from the frame playback started on. The picture's
thread shows the frame that count has reached, dropping frames when late, and a frame it
finishes after a pause or seek is dropped rather than shown over the seek's. Both threads
seek by frame index into graphs of the same document, which is why speed and nested
sequences need nothing of their own here: the compiler already put them in both.
`load-video` and `load-audio` swap a graph between frames without pausing.
`kuma-daemon/tests/engine.rs` pins the pacing, frame by frame, and was checked to fail
with a picture that runs free.

`preview::reload` compiles both and stamps each, and sends the engine only the graph whose
document changed: a colour reloads the picture, a volume the sound, a cut both. Clients
still ask on every edit and never say which half; `/api/preview/reload` reports it, and
`an_edit_reloads_only_the_graph_it_changes` pins it. Audio-reactive visuals do not
couple the halves again: they are meant to be prebaked from an analysis of the sound into
an overlay, the way subtitles are, not drawn by MLT's audio filters in the video graph.

Not built, recorded for when the video half is too slow:

- **Frames in parallel.** MLT's own consumers render several frames at once
  (`real_time` above 1; Shotcut's and Kdenlive's "parallel processing"). The engine pulls
  frames itself, so it would be a worker pool rendering and encoding ahead of the playhead,
  reordered before sending. It raises playback throughput, not single-seek latency, and not
  every MLT service is thread-safe: compare parallel against serial output first. Only
  sensible once sound is its own graph, since audio has to be pulled in order and on time.
  Renders now do this through melt, and `parallel_frames_render_what_serial_frames_do` is
  the comparison (see *Bake and render speed*).
- **Tracks in parallel within a frame.** Not possible with MLT as it is: a tractor pulls its
  tracks in turn. It would mean an upstream change or the engine blending tracks itself,
  which is a second compositor and refused as such. It is the one thing that would help
  scrubbing.
- **Cheaper, available now:** decoder threads (the avformat producer's `threads`
  property, which changes speed and not output), JPEG encoding off the pull thread, and
  caching heavy nested sequences the way bakes cache overlays.

**An overlay off the playhead is hidden by `visibility`, never `display`** — adopted, after
a template rendered one size in the editor and another in the render. `display: none` gives
an iframe a **0×0 viewport**, and a template is allowed to measure itself — fitting a line
of text to its column is the ordinary way. The stage created every overlay wrapper hidden,
so those scripts ran against nothing, read `0` and kept whatever they had; the bake loads
the same page full-size and got it right. It only bites when the clip is not under the
playhead as the iframe loads, which is the ordinary case and is why it survived so long:
a clip you are looking at gets shown by a stray update before its scripts run.
`visibility: hidden` keeps the box, so the page always sees the project frame; it costs the
layout and skips the paint, which is the half that matters. The wrappers are absolutely
positioned, so laying them all out moves nothing.

## Editor tabs and the overlay workbench

**The centre of the window is tabbed** — adopted. One tab is the sequence being edited
(the engine's picture with that sequence's overlays live on top); every other tab is one
overlay entry open in a workbench. Kdenlive tabs its monitors for the same reason: a
timeline is not the only thing worth looking at, and the thing you are making is not
always in it yet.

The occasion was overlays. An overlay entry could only be seen by placing a clip of it and
scrubbing the timeline to where that clip happened to be — so *creating* an overlay was
stuck behind *deciding where it plays*, two decisions that have nothing to do with each
other. Worse, iterating on one meant leaving a placement lying around in the middle of a
cut you were not editing.

**A tab is an editor, not a document** — adopted. `state.tab` is the empty string for the
sequence monitor and a bin id otherwise; ids must match `[A-Za-z_][A-Za-z0-9_-]*`, so the
sentinel cannot collide with an entry. `state.overlays` is which workbench tabs are open,
pruned on every refresh exactly as the open sequence is, because an agent edit or an undo
can remove the entry a tab is showing.

**One tab per open sequence, Kdenlive's timeline tabs** — adopted, superseding this
entry's own earlier reading that it should not be built. That reading conflated two
things: several sequences *running* at once, which would indeed be a second engine and is
still not built, and several sequences *one click away*, which is all a tab strip has to
be. Kdenlive's timeline tabs show one timeline at a time too.

So `state.sequences` is the list of tabs and `state.sequence` — the daemon's — is which of
them is being edited. Clicking one is the round trip the topbar picker already made, and
the strip is honest about the cost: switching recompiles. The picker stays, because it is
how a sequence that has no tab is found; opening from it adds one.

The last sequence tab cannot be closed. The daemon always has a sequence open, so a strip
with none would misrepresent what the preview is showing; closing the one being edited
falls back to a neighbour, which is another round trip.

**Render means the master, and says so** — adopted. `render` (and `sample_frames`) take a
sequence and default to the master on every surface; the editor's button sent no sequence
at all, so it rendered the master whatever you had open. That was right and unsaid: the
button's tooltip was empty, and with sequence tabs you can now spend an hour inside a
sub-sequence before pressing it.

So the button's tooltip names what it will render, and a second picker appears beside the
preset **only when the open sequence is not the master** — until then there is nothing to
choose and a dropdown saying so is noise. It offers "master" (the default, as in the
daemon) or "open"; the choice is one of those two words rather than a sequence id, so it
survives switching tabs and relabels itself. The daemon is unchanged: leaving `sequence`
out still means the master, so the client never has to know which entry that is.

A dedicated render view — queue, range, per-sequence outputs — is the obvious next step
and is not built.

**The workbench edits the entry, never a placement** — adopted. It creates no clip, moves
no playhead and reads none: the length under the stage and the frame beside it are view
state, not document values, and the entry model has no field for either (a clip is what
gives an overlay a length). `Place at playhead` is the one thing that writes, and it uses
that length so what you were looking at is what lands.

**Vars stay in the inspector** — adopted; a properties panel inside the workbench was
rejected. A bin entry has one editor for what it is, and duplicating the var controls
would mean keeping two generated forms in step for no new ability. Opening an entry
selects it, which is what puts its vars on the right.

**Previewing an unplaced entry is a route, not a synthetic clip** — adopted.
`/overlay-entry/{id}` renders the same contract as `/overlay-preview/{clip}` with the
length from the query and no clip id. The alternative — minting a hidden clip so the
existing route would work — would have put a placement in the document that the user never
asked for, and undo would have had to know it was not real.

`?width=`/`?height=` render it into another frame. The template contract *requires* a
template to look right at 1920×1080 and 1080×1920 alike, and before this the only way to
check was to flip the whole project. Clamped rather than validated, because nothing is
stored: a silly number costs one render.

**Playing an overlay here is `requestAnimationFrame`, not MLT** — adopted. The document is
already loaded and an overlay is a pure function of time, so playback is a seek per
animation frame and needs no engine — which also means it works on a machine where the
preview engine does not build. It loops, because deciding whether an animation is right
never happens on one viewing.

**The monitor you are not looking at stops playing** — adopted. Leaving the sequence tab
while the engine is streaming pauses it rather than decoding JPEGs into a hidden canvas.

**The timeline stays visible under the workbench** — adopted. The tabs are over the centre
only, as Kdenlive's monitor tabs are, so `Place at playhead` has somewhere visible to land
and the tracks do not disappear and reappear as you flip.

**An agent can look at an unplaced entry too** — adopted, and the reason it is not a
footnote: the editor gained a way to iterate on an overlay that the agent half of the
product could not see. `sample_frames` took a sequence and a time, so checking an animation
meant placing a clip, sampling, and removing it — an edit, an undo entry and a revision
bump for a *look*.

It now takes `overlay` (a bin id), with `duration` and `backdrop`. What that builds is a
throwaway project whose master is one overlay clip over a colour, compiled and sampled
through the ordinary path — a mechanism that wants generalising to every bin entry, which
is its own item under *Still open* — same bake, same compiler, same melt — so the frames are the
composite the render would produce rather than a second rendering path that could disagree
with the first. It is never stored and never validated: it is well formed by construction
and lives for one compile. The alternative, patching a hidden clip in and out around the
sample, would have put a placement in the document nobody asked for and made undo answer
for it.

The synthetic sequence's id carries a hash of the length and backdrop because it is part
of the frame cache's key: the same entry at the same frame over black and over white are
two pictures, and one must not be served for the other. `overlay` and `sequence` together
are refused rather than ranked — the times would mean different things — and so is an
entry that is not an overlay.

Still open here: the workbench shows the overlay as the template renders it, not as the
compiler will composite it — there is no clip, so there are no clip effects to approximate.
The exact answer is a sample, which is now available for an unplaced entry as well.
Template *markup* is still edited outside the editor; `Reload` is the loop, not an editor.

## The timeline clipboard

Copy, cut and paste for clips, the first of the quality-of-life gestures the editor was
missing. The interesting part is not the copy; it is where a paste *lands* — which has two
answers, one for a single clip and one for a group, and they are not the same answer.

**The clipboard is UI state, not document state** — adopted.
Everything else in the editor is a revision-checked patch against the daemon's document,
so a clipboard living in `store.ts` looks like the exception it is not: copying is not an
edit. "This clip is on my clipboard" is nothing another process needs to see, and putting
it in the document would spend a revision, an undo step and a line in every agent's view
of the project on an act that changed nothing. The paste is the edit, and it is one patch
like any other. The price is that the clipboard is per window and dies with it — already
true of the selection, the playhead and which tab is open.

**One clip needs a *where*, and clicking a track is it** — adopted.
The playhead answers *when*, and nothing answered *where*: the first sketch pasted back
onto the track the clip came from, which is no use for the thing actually wanted ("this
clip, but on Video 2"). So the timeline remembers the track last clicked — the head, an
empty lane, or a clip, all of which are ways of pointing at a track — and marks it in the
head. It is remembered by **id** rather than by index, because indices shift the moment a
track is added. A track of the wrong kind cannot take the clip, so a paste aimed at one
falls back to the first track of the right kind, which is the rule a drop from the bin
already follows; refusing would be correct and useless.

**An occupied slot pushes the clip forward; it does not refuse** — adopted.
A paste names a position, so the answer to "that one is taken" that keeps the gesture
usable is *the next position that is free*: the paste walks the target tracks' clips and
pushes its offset past whatever it lands in, looking again each time, because clearing one
clip can walk into the next. Two consequences worth
having: pasting twice in a row lays two clips end to end, and pasting onto the track a clip
was copied from never lands on top of what is already there. Kdenlive refuses instead, and
`place` (what a drop from the bin uses) sends the clip to the end of the track; neither
answers the question the gesture asked.

**The track travels with the clip — its kind, and its id** — adopted.
`trackFor` would answer "video" for a video file, but a video file can be sitting on an
audio track, and a paste of it belongs where it was copied from. So the clipboard holds the
*kind* of the track each copy came off, and compatibility is that kind rather than the bin
entry's. It holds the track's *id* as well, which is what lets a group of clips be put back
on their own tracks (below).

**What travels is the placement, and only the placement** — adopted, and it is the model's
own split. The pasted clip carries `in`, speed, volume, label and effects; it does not copy
the bin entry, because both clips play the same entry and that is what an entry is for.
A transition stays behind: it names two clips on one track, and a copy of one of them has
nothing to attach to. This is also what makes pasting into *another sequence* free — the
entry is already project-wide — so the only check a cross-sequence paste needs is the
recursion one `placeBinItem` already makes.

**A selection is a list of clips; a bin selection is one entry** — adopted.
`Selection` became `{ kind: "clip"; ids: string[] } | { kind: "bin"; id: string }`, which is
not symmetry for its own sake: copying, deleting and splitting are all things you want to
do to several clips at once, while every bin-entry action — rename, vars, delete — is
about *that* entry. The list is in click order, so its last member is "the clip" a
single-clip action means, and `store.selectedClip` still answers that; the panels that
edit one thing did not have to change. Ctrl/Cmd- or Shift-click builds the list, and a
plain click collapses it, which is also what makes a drag unambiguous.

**Several clips are a shape, not a position** — adopted.
A group paste keeps the spacing between the clips and the tracks they came off, so the
clicked track does not come into it: there is no one track a group belongs on, and the
answer for a single clip ("the one you clicked") does not generalise. Each source track is
matched by id in the sequence being pasted into, else by kind, and a track is added when
there is nothing left to match — two source tracks never collapse onto one, because that
is the one way the shape could be lost without anyone noticing. The whole group then takes
**one** offset, pushed forward until every clip in it fits; per-clip pushes would preserve
the position and destroy the shape, which is the wrong half to keep.

**What a multiple selection does *not* do** — the deliberate edges.
The inspector still edits the last clip clicked and says so, rather than showing fields
that look like they apply to all of them. And there is no marquee: a drag on an empty lane
scrubs, which is older and more useful than a rubber band. (A drag *did* move one clip
whatever was selected, until [Clip groups](#clip-groups) made the selection a block.)

**Deleting a clip takes its transitions with it** — adopted, and it was a latent bug.
A transition names two clips and the validator refuses one that names a clip that has
gone, so deleting an end of a dissolve was already rejected — a 422 toast and nothing
deleted. Multiple selection made it likely enough to fix: `deleteSelectedClips` drops
every transition naming a clip it removes, in the same patch, the way a clip dragged to
another track already did. Removals go highest index first, because removing a clip
renumbers the ones after it on its track.

**Buttons for copy, cut and paste in the transport strip** — tried, removed.
They were there to say the editor had a clipboard at all, which is a real problem for a
gesture with no menu bar of its own. But three buttons for the three key combinations
every editor already uses is a lot of strip to spend on being told, and the strip is a
*transport*. What survives is the part a person cannot work out by looking: how many clips
are selected, which the outlines make you count. The keys, the inspector's shortcut list
and the app's Project menu carry the actions.

**Ctrl+C/X/V are bound off the workbench only** — adopted.
The workbench edits a bin entry and has no track to paste onto, so a paste there would
edit a timeline nobody is looking at. The bindings sit in `shortcuts.ts` with the rest,
which is what keeps a text field's own copy and paste working (the handler ignores events
from fields), and the desktop app carries the same three actions in the Project menu with
**no** accelerator — the Edit menu's `cut`/`copy`/`paste` roles are text editing and stay
that way, for the reason recorded under *Desktop shell*.

## Track lanes: drawn by kind, stored by composite

**Overlays on top, then video, then audio at the bottom** — adopted.
The timeline used to draw the track array reversed, which put audio at the top of the
window for no better reason than that a new project appends it last. Tracks are now grouped
by kind for drawing — overlays, video, audio — and within a kind the later track is drawn
higher, because that is the one compositing over the other.

**The drawing is grouped; the document is not** — adopted, and it is the point.
Array order *is* the composite: a video track after an overlay track composites over it.
Reordering the document to match the screen would silently change every project it was
applied to, so nothing is reordered — `render` sorts a copy, each row still carries its
document index in `data-ti`, and every track edit (mute, hide, remove, a clip dragged onto
another track) is addressed by that index as before. `ui/test/timeline.test.mjs` pins both
halves at once: the screen says one order, the document still says the other.

## Moving the playhead

**The arrows are frames, Alt is cuts** — adopted.
A frame is the fine move and was already what an arrow did; the coarse move people
actually reach for is not "ten frames" but "the next place something changes", so Alt+←/→
walks the **cuts**: every clip start and end on the sequence on screen, whichever track it
is on, plus 0. Shift keeps the second it always was. Nothing that way means staying put,
rather than sliding to an end that is not there, and the search ignores anything within
half a frame of where the playhead already is — `seek` rounds to the frame grid, so an
edge closer than that would round straight back and look like a key that did nothing.

**Every seek takes the key away from the browser** — adopted, after it looked like a bug in
the timeline. The arrows had no `preventDefault`, so the browser also scrolled — and what
it scrolls is the nearest scrollable box around whatever has focus, which after clicking a
track's mute button is the timeline. Pressing an arrow appeared to make the whole timeline
run sideways. Two halves to the fix: the seeks are all `preventDefault` now, and the
"a field owns its keys" guard stopped counting **buttons** as fields for every key — a
button owns Space and Enter, which press it, and nothing else. Before that, the last thing
you clicked could swallow the arrows entirely.

## Clip groups

A subtitle belongs over the audio it transcribes, and two clips that overlap do so on
purpose. Both are relations the timeline had no way to say, so moving one half of a pair
was a silent way to break it.

**A group is a field on the clip, not a list on the sequence** — adopted.
`clip.group` is an id; the members are whichever clips carry it. The alternative — a
`groups: [[id, id], …]` beside `tracks` — makes the set explicit and then needs every
member validated to exist and rewritten on every delete, which is exactly the upkeep
`transitions` already costs (see `deleteSelectedClips`). The field goes with the clip, so
deleting one leaves nothing dangling and splitting one gives both halves the group. It is
the same shape as a clip naming its bin entry rather than the entry listing its clips.

**A group is an editing relation; the compiler never reads it** — adopted, and it is what
keeps the feature cheap. A grouped timeline and the same timeline ungrouped produce
byte-identical MLT, so `compile.rs` needed no change and the golden file did not move. The
model's whole side of this is one optional string and two validation rules.

**A group lives on one timeline** — adopted, and the validator says so.
A block that spans two sequences could never move as one, so the same id on two of them is
refused rather than silently half-ignored by a UI that looks in the sequence it has open.

**Selecting a member selects the group, in the store** — adopted.
`selectClips` expands, so no gesture can forget: a click, a Ctrl-click, a paste and an
agent edit all get the same answer. The group joins *in front of* the ids asked for, which
keeps "the last one clicked" last — the inspector still edits the clip you pointed at, and
says so when several are selected.

**A drag moves a block, and two things make one** — the selection when the dragged clip is
part of a multi-selection, else the dragged clip's group, else just it. Groups came first
and a selection borrowed the same machinery a day later, once it was obvious that
Ctrl-clicking three clips and dragging one of them moving only that one was not a rule
anyone would defend. Multi-select had been wired for copy, cut, delete and split since it
landed, and never for the drag.

The two differ in exactly one way, which is the reason to have both: a group is in the
document and outlives everything, a selection lasts until you click elsewhere. So
**pressing a clip that is already one of several selected keeps the selection** rather than
collapsing to the clip pressed — otherwise the press that begins the drag would destroy the
thing being dragged. A press that turns out to be a click, not a drag, collapses on release
instead, or there would be no way back to a single clip except by clicking empty space
first.

Three rules follow from the block being real. It travels **one delta**, so the shape
survives; it stops when its **earliest** member reaches 0, not when the dragged one does,
or the daemon would refuse the whole patch for a negative start; and it never snaps to its
own members, or it would stick to itself. It is **one patch** — half a group having moved
is not a state anyone asked for.

**Tracks are an offset, not a destination** — adopted, after the first cut of this said a
block simply could not change track. The reasoning there was that a block spans the tracks
its clips are on, so there is no one track to drop it onto; that much is true, and it was
the wrong conclusion. "Which track does this land on" has no answer for two clips, but
"how many tracks has it come" has exactly one. So the row under the pointer says how far
the **pressed** clip has travelled, and every member travels that far — which is the same
rule time already followed, one row up. A block of one is a clip, so picking a track for a
single clip is that rule too, and there is no longer a special case for it.

The offset is taken only when **every member lands on a track of its own kind**, so a
block cannot come apart on the way, and an overlay clip cannot arrive on a video track for
the daemon to refuse. A block that straddles kinds (a subtitle over its audio) therefore
stays put unless the tracks happen to line up — which is the honest answer rather than a
half-move. Off the tracks entirely, over the ruler or past the edge, the drag keeps the
offset it had rather than springing back.

A move across tracks rewrites each affected track's `clips` **whole** instead of removing
and adding one clip at a time: a remove renumbers the clips after it, and two members of a
block can trade tracks, so a sequence of removes and adds would depend on its own order to
come out right. It is built from where the drag found the clips, so a document that moved
under it is a drag to abandon rather than one to guess at.

**Trimming stays a clip's own** — adopted. "Move by block" is what was asked for and what
is coherent; a group is not a claim that its clips are the same length, and resizing four
clips from one handle is a different gesture with a different name.

**A pasted copy gets a new group** — adopted, and it is the one place the id has to be
re-minted. Keeping the old one would put the copy in the original's group, and the two
would move together forever. A group only one of whose clips was copied arrives loose,
because a group of one is not a group.

**Ctrl+G and Ctrl+Shift+G**, the pair every editor and drawing program uses, off the
workbench like the clipboard. Grouped clips carry a chain before the name and a brighter
edge — not the selection's outline, which is what you have hold of rather than what these
clips *are*.

## The shortcut list

**The list is the bindings** — adopted, and it is the whole point.
`shortcuts.ts` already held every key in one table; a binding now carries how it is written
and what it does, and the list (`?`, or the button in the top bar) is built from that table.
A hand-written list beside it would be a second copy of the same facts and would disagree
within a week — which is exactly what the inspector's help paragraphs had become, so they
are gone and point at this instead. A binding that is the mirror of the one above it — `→`
of `←` — carries no text and is not listed twice.

**Gestures are in the list too, and written as sentences** — adopted.
Half of what this editor can do is done with the pointer, so a list of keys alone would be
a lie by omission; they are the one part that cannot be derived, since a pointer handler is
not a table. They render as the phrases they are rather than as key caps: "Drag a clip" is
three words, not three keys to press, and the first draft drew it as `Drag` `a` `clip`.

## The inspector's property fields

The effects panel was a column of `<input type="number">`: correct, and nothing you would
want to *use*. Godot's inspector is the reference — not for looks, but because a parameter
you can drag is a parameter you explore rather than guess at.

**A number is a slider you can type in** — adopted.
Press and drag sideways to change it, click without moving to put a caret in it. Shift is
fine (a tenth of a step), Alt is coarse (ten), and the arrow keys step it while it has
focus. The dead zone before a press counts as a drag is taken *off* the travel rather than
added to it, or the value jumps three steps the moment the drag is recognised — thirty
with Alt. A coarse drag snaps to the grid it is dragging on, which is what makes Alt the
way back to round numbers.

**A drag commits once, on release** — adopted, and it is the only honest option here.
Every value in this panel is a revision-checked patch to the daemon: committing per
pointermove would be a round trip per pixel and an undo history made of them. So the field
shows the number moving and sends one edit when the pointer comes up — the same bargain
`ui/src/panels/timeline.ts` already makes for dragging a clip, and the reason both of them
keep a local "what it reads now" separate from "what the document says".

**A vector is one row wearing its axis colours** — adopted.
X red, Y green, Z blue, W amber, as a chip at the left of each field; a pair that is not
spatial takes its own letters instead (a fade's In/Out). Reading `x` off a row of identical
boxes is exactly the thing the colours remove. Past two fields the row takes the width
under its label: four boxes beside a label in a 300px panel leave about twenty pixels of
number each, which is how `0.05` came to read as `0` — found by screenshotting the panel
rather than by reasoning about it.

**The shapes live in `props.ts`; which shape a thing is lives with the thing** — adopted.
`numberField`, `vectorField` and `sliderField` know nothing about effects. The effects
table in the inspector says that a transform is a position vector, a scale, a rotation, an
opacity slider and its keyframes; a template var's `spec.type` says the same for a var; a
clip's own start, end, speed and volume say it inline. One control per shape, so `x` is
dragged the same way wherever you meet it, and a new effect kind is still one row in one
table.

**The bounds in that table are the model's** — adopted, after the first draft shipped a
field that could compose a refused patch. `scale` may not be 0 (the validator says `> 0`),
so the field stops one step above it; `opacity` and a crop's edges are 0..1 there and
0..1 here. Where the useful range and the legal one differ — a contrast, a gamma, a clip's
gain — the slider covers the useful one and the field is `soft`: it will take a typed value
past either end, because clamping to a range the model does not have would be the panel
lying about the document. The rules a field cannot express (a crop's left + right < 1, a
speed's ±[0.01, 20] with a hole in the middle) stay the daemon's to refuse.

**An effect folds away, and open is the default** — adopted.
A clip carrying a transform, a crop, a colour and a blur is taller than the panel, and the
two controls that are not an effect — `+ add effect…` and the preset picker — are the ones
that fall off the bottom. So the head is a disclosure: it reads as the heading it replaced
rather than as a button, and the chevron turns to point at what it opens. *Closed* is what
is stored (`closedEffects`, by `clip/effect`, the key the curve view already uses), so a
new effect and a newly selected clip both open — a panel that says what it has beats one
that makes you open three boxes to find out. Like the curve, the fold is view state: it
survives the rebuild every edit causes, it costs no revision, and nothing saves it to the
document, because what you are looking at is not part of the video.

Two things follow from folding rather than from the fold. A closed effect **follows
nothing** — its keyed fields are not on screen, and the playhead ticks 25 times a second,
so the panel's per-field followers are collected per effect behind one check and the
fields catch up on the way open. And a closed effect **says that it is keyframed**, with a
◆ in its head: the strip that would otherwise tell you is the first thing the fold hides.

## Keyframes you can point at

Keyframes were a `<textarea>` of JSON, which is not an editor, it is a hole in one. The
model needed nothing: times in clip-local seconds, linear between them, already shared by
the compiler and the live preview. What was missing was the pointing.

**The playhead is the clock, and the fields are the editor** — adopted, from Kdenlive.
A keyed field shows what its value *is at the playhead*, and writing it writes the key
under the playhead — making one there when there is none. There is no "which keyframe am I
editing" to hold in your head, because the answer is always "the one you are looking at",
and the drag-to-scrub fields from the previous change work on animation unmodified.

**Animation is opt-in, per effect** — adopted, from Godot's key button, and it is what the
model's own agreed direction already says ("a parameter stays a plain number field until
asked to animate"). An effect with no keys shows an outlined diamond in its head (once a
**◆ Animate** button on a row of its own; see *Effects: offered where they act, static until
keyed*); pressing it writes one key holding what the effect already does, so turning
animation on changes nothing. The filled diamond turns it off by writing the values at the
playhead back into the static fields — anything
else snaps the picture back to whatever the statics were before the first key, which is
never what was meant.

**A key is a full sample** — forced by the model, and worth writing down because it looks
like a limitation. A keyframe that omits a field does not mean "leave that field alone", it
means "use the effect's static value" (`k.x.unwrap_or(*x)` in the compiler, the same
fallback in `transformAt`). So inserting a key writes *every* animated field at that
moment, which is exactly what makes inserting one a no-op until you move something. Real
per-property tracks would be a different model, and MLT packs x, y, scale and opacity into
one `rect` property anyway, so they would have to be resampled into full keys to be
emitted. Not worth it.

**The panel gets a strip; the clip gets dots** — adopted, and this is the answer to "does
this belong in a separate panel". Godot keeps properties in the inspector and puts keys in
a separate bottom **Animation** panel, with its own timeline and its own playhead; Kdenlive
keeps them together in the effect stack, on a ruler the width of the clip. kuma has no room
for Godot's split — the bottom of the window is already a timeline, with the same clock and
the same clip — so the keys live in two places by *role*: a compact strip in the panel,
where the values are and where editing happens, and read-only diamonds along the bottom of
the clip in the timeline, where time is and where you scrub past them. One place to edit,
one glance to see. A curve editor, keys draggable in the timeline itself, and per-key
easing are all deliberately not here yet.

**Per-key easing, and the upgrade path that came with it** — adopted, format version 2.
A key now carries `ease`: `linear`, `hold` or `smooth`, mapping onto MLT's three animation
operators (`=`, `|=`, `~=`). The rule is MLT's, and it is the part worth remembering: the
key that **opens** a segment decides that segment's shape, so the last key's ease never
means anything. The UI reads it the same way, Catmull-Rom and all, so the live overlay
preview curves the way the render will; `shiftEffect`/`trimEffect` carry the segment's ease
onto the key a split inserts, and a key added inside a hold inherits the hold — otherwise
"adding a key changes nothing" would be a lie exactly where it matters most.

`linear` is the default and is not serialized, so a project that does not ease is
byte-identical to the version 1 file it was. The version still went to 2, because an older
kuma reading `"ease": "hold"` would say "unknown field" rather than "unsupported version",
and that is what the field is for. Which brought back the thing **No upgrade path; the
format stays version 1** (above) deleted: `kuma_core::parse_project`, one step per bump,
applied to the JSON before it is deserialized. That entry's reasoning was that nothing was
using kuma yet; projects exist now, and breaking them to add a defaulted field would be
absurd. The step itself is four lines — 1 → 2 is a stamp, since a version 1 keyframe always
did what `linear` means — and the golden test now reads its version 1 fixture through it,
so "the upgrade changes nothing about what is rendered" is checked rather than asserted.

*Not verified by rendering here*: this machine has no melt, so the `~=` on `qtblend`'s
`rect` is pinned by a compiler test on the string and by Kdenlive driving the same filter
with the same operator — not by frames. The check to run where melt exists is the one the
blur ramp got: render a smooth scale ramp over sixty frames and look at them.

**Every keyframed effect uses one `Keyframe` type; crop, blur and colour take it too** —
adopted, format version 3, superseding the shape agreed under *Still open* below.
That entry agreed a parameter should be "a number or a list of keyed values"
(`radius: [ { "t": 0, "v": 0 } ]`), one track per parameter. Building the transform's
editor first (A and B above) turned up the two things that decide against it:

- **MLT packs parameters.** A transform's x, y, scale and opacity are one `rect`
  property, and so are a crop's four edges. Independent per-parameter tracks would have to
  be resampled into full keys at the union of their times before either could be emitted —
  machinery that exists only to undo a shape the model chose.
- **The two keyframe shapes that already existed were the same shape.** A transform's
  `{ t, x?, scale?, … }` and a volume's `{ t, gain }` are both "a time, plus whichever of
  the effect's parameters this key sets". Name that and there is nothing left to design.

So `Keyframe { t, ease, values }`, with the values `#[serde(flatten)]`ed under the
parameter's own names, replaces both, reads every existing document unchanged, and is what
crop, blur and colour now carry as an optional, unwritten-when-empty `keyframes`. The
goals the earlier entry set are all met by it — one type, one validator path, a bare
number keeps meaning what it means, seconds and numbers only, not exposed until asked — and
one is met better: a key that leaves a parameter out means the effect's static value, so a
hand-written key can set one thing, while a key the editor writes carries every animatable
parameter of its kind (`Effect::animatable`, mirrored by `ANIMATABLE` in the UI), which is
what keeps "adding a key changes nothing" true for every kind at once.

The parameters are a keyframe's unknown fields, so the type cannot `deny_unknown_fields`;
the validator checks the names instead and, unlike serde, says which ones the kind has.
It also holds a key to the same bounds as the static value — on what the key *means*, with
the statics filling in what it leaves out — so a keyframe cannot smuggle in a crop that
leaves nothing visible. The compiler writes every animated property through one `anim`
helper; the golden file is unchanged; a compiler test pins the crop, blur and colour
strings. The UI's `paramAt` is the one place a parameter is read at a time, and the live
preview, the inspector and a split all ask it. Volume keeps its `min: 1` and gains a
meaning for a key without `gain`: 1, the clip as recorded.

Not rendered here, as with B: `avfilter.eq` and `qtcrop` taking animation strings is
pinned by the compiler test and by the record's own earlier note (blur's `sigma` ramp
was rendered and looked at; colour "should be checked rather than assumed"). Still should.

**The rest of the keyframe editor: keys dragged on the clip, record mode, a curve view** —
adopted, and each stopped where the model stops.

*Keys on the clip take the pointer now.* The dots the timeline draws along a clip were
read-only first, to keep one place to edit; that lasted until the first time someone
reached for one. They drag in time and click to seek, through the same `Animation` the
panel uses (`animationOf` builds it for both), so a key moved on the clip and a key moved
in the strip go through one set of rules. Values stay the panel's — the clip has no room
to say what a key is worth, and the strip and the graph do.

*Auto-key is Godot's record button, and it lives in the transport strip.* While it is on,
a change to any animatable value writes a key at the playhead, starting the animation if
the effect had none; the statics are left alone. It is in the strip rather than the
inspector because it is about every edit that follows, not about one panel, and it is read
at the moment of the write rather than when the row is built, so switching it needs no
re-render. The button goes red and so does the top edge of the inspector: the whole page
should say it is recording, not one control.

*The curve view is a graph you can drag points on, not a curve editor.* It draws one
parameter over the clip with the same sampler the preview and the compiler agree on, so
what it shows is the shape the render will have — a hold's step, a Catmull-Rom's overshoot
— and its points move in two axes at once, one edit. What it does *not* offer is bezier
handles, because the model has three eases and MLT's animation strings have no bezier;
drawing one would be drawing a promise the render cannot keep. It is off until asked, per
effect, and remembers being open across the re-render every edit causes, so the panel
grows a graph only where someone put one.

It earned its keep twice on screenshots. Once for a pixel: the playhead in the strip sat
a pixel to the right of the key it was on, and the curve and the timeline were out too, by
different amounts and each for its own accidental reason — a 2px line drawn *from* a
position rather than across it, a 9px diamond given an 5px offset, a key lane whose origin
was inside the clip's 1px border. One rule now, stated in `style.css` where the first
marker is defined: **a marker at a moment is centred on it**. A test asserts the three
surfaces agree to the pixel for one instant, because this is the kind of wrong that is
seen long before it can be named.

And once for the sampler: a key sat a sample to the left of the curve's
corner, and the reason was the sampler, not the drawing. At exactly a key's time
`sampleTrack` stayed in the segment that *ends* there rather than the one the key opens —
invisible with linear keys, since both segments meet at the same value, and wrong after a
hold, where the field, the preview and the curve all read the held value at the instant
the key should have taken over. A `>` that should have been `>=`; now pinned by a test at
that instant, and the curve samples at every key time as well as on its grid.

**The panel does not re-render as the playhead moves** — adopted, and it took a bug to get
right. A keyed field follows the playhead through one `store.select` per effect that sets
values in place; rebuilding the inspector 25 times a second is exactly what that helper
exists to prevent. But *whether the playhead is inside the clip* is a different question —
outside it there is no "now" to write to, so the fields are read-only — and that is in the
panel's subscription as a flag. Without it, selecting a clip while the playhead was
elsewhere left the fields frozen after scrubbing into it.

## Icons

**Shapes, not characters** — adopted, after `⏮` gave the game away.
The transport's buttons were glyphs, and one of them never matched the others: U+23EE has
*emoji* presentation, so the browser reached past the UI font for the colour emoji one and
drew a bigger, differently weighted picture beside a monochrome triangle — and drew a
different one on each machine. CSS cannot reach that, because which font rendered a
character is not the page's to choose. `ui/src/icons.ts` holds the three the editor uses
as SVG shapes in `currentColor`, built as nodes rather than markup (`dom.ts` uses no
`innerHTML`, and static icon data is not a reason to start).

**An icon pack, not yet** — deferred, deliberately.
Lucide, Feather, Phosphor and Tabler are all plain SVG and all usable without a framework,
which is the question worth recording: nothing about this editor's no-framework UI rules
one out. Three icons do not pay for a dependency and a build step that has none, and every
caller goes through `icon(name)`, so the day this wants a dozen the pack goes in behind
that function and no panel changes.

## Themes

**A theme is the editor's chrome, in this browser's storage** — built, with one alternative
theme to prove the mechanism is a mechanism.

The palette was 20 tokens in one `:root` block and every other colour was a literal spread
through `style.css`, so "another theme" meant a second stylesheet or a sweep. It is now one
block of tokens per theme in `ui/src/themes.css`, selected by a `data-theme` on the root;
`style.css` names tokens and nothing else. Switching is one attribute write — no reload, no
second sheet, no rebuild — and `ui/src/theme.ts` holds the list the picker shows, the top
bar's button and the shell's *View → Theme…*, which are the same command.

**Which theme is not a project value.** It goes in `localStorage` beside the transcribe
language, never into the document: two people opening one file may want different answers,
and an editor's appearance is not an edit. Putting it in the project would also make
choosing a theme a revision-checked, undoable patch that saves the file and hands an agent
a `409` — the same argument that keeps the open sequence out of the document.

The cost is that storage is per origin and a daemon's origin is its port: a project opened
on 7456 starts at the default while the one on 7455 is themed. The transcribe language has
always had this, and the fix — if it becomes annoying — is the desktop shell carrying the
choice across its windows through the bridge, not a value in the project file.

**A theme is not a design system, and the distinction is the whole point.** A design system
is a document copied into the project and applied by editing a template or an entry's vars;
it is about the *video*. A theme is about the *window*, and it stops at the editor's chrome
— it reaches no template, no bake and no render, so the video looks identical whichever one
is on. The section above records why a project-level theme injected into overlays was turned
down; this is not that returning by another door.

**Tokyo Night, and what a second theme is for.** The default is unchanged, and proven so: a
screenshot of the editor before and after the token sweep differs in zero pixels below the
top bar, where the only difference is the new button. The second is retro neon on deep
indigo — hot pink playhead, violet overlay clips, teal audio — and it earns its place by
being far enough from the default to catch anything the sweep missed. It also carries the
one token that is not a colour, `--glow`, which is `none` in the default and a soft neon on
the playhead and the Render button in Tokyo Night: the two things you look *for* rather than
at. A theme that could only change hues would be a palette, not a theme; one that can change
anything is a second stylesheet by instalments.

**What holds the two halves together** is `ui/test/theme.test.mjs`, which reads the CSS and
the TypeScript as text and needs no browser, daemon or build — so unlike its neighbours in
`ui/test/`, it never skips. It fails if a palette has no entry in `THEMES` (a theme nobody
can choose) or an entry has no palette (a picker that appears to do nothing), if a theme
leaves a token out — the silent one, where that theme quietly keeps one of the default's
colours — if a colour literal comes back into `style.css`, or if any palette sets
`color-scheme`, which is the one property that would take the overlay-compositing bug out
of retirement.

## Overlay authoring: checked, scaffolded, and shown moving

Three changes made after reading through [OpenDesign](https://github.com/nexu-io/open-design),
whose four-axis split — functional skills, rendering templates, brand `DESIGN.md`
packages, and universal `craft/` rules injected into the prompt — is the same problem kuma
has with one asset kind instead of four.

**The contract is enforced, not just written** — adopted. `library/overlays/CLAUDE.md`
described rules nothing checked, and every one of them fails far from its cause: no
`data-no-timeline` waits 45 s for a timeline that never registers, `Date.now()` renders a
wrong frame with no error, an undeclared var arrives as `undefined`. `Baker::lint` reads
the staged document — the same staging the bake uses, extracted so the two cannot drift —
and adds hyperframes' own linter, which was installed all along and never called.

Two of its findings are suppressed rather than shown: it looks for a literal
`window.__timelines` assignment, which kuma templates reach through `timeline()` in
`_vendor/kuma.js` (every shipped template tripped it), and it reports `Math.random()` a
second time in its own words. A report full of false positives is a report nobody reads.
Comments are stripped before the behaviour rules run, because the scaffold *warns* about
`Date.now()` in one and failed its own check.

**A scaffold, not a blank file** — adopted, straight from OpenDesign's "use the
deterministic scaffold and edit only what the prompt changes". `kuma new-template` writes
a skeleton that passes the check; a test pins that, so the example an agent copies cannot
drift from the contract it illustrates.

**A preview that moves** — adopted. Every overlay is an animation and both places that
showed one picked a single frame about a second in, which is the settled pose: the least
informative moment. The `thumbs` job now also writes an animated WebP with alpha, and
`preview.webp` joins `thumb.png` as a file that is neither staged nor hashed — without
that, generating a preview would change the template's content hash and invalidate every
bake of it.

**Motion discipline in the contract** — adopted, as documentation. OpenDesign's
`craft/animation-discipline.md` and its `review-animations` skill are written for
interfaces: interruptibility, hover frequency, GPU-only properties, reduced-motion. Almost
none of that survives the move to baked video, where nobody hovers, nothing can be
interrupted and every frame is rendered offline. What does survive — animate arrival,
departure and change; out-curves for entrances; never scale from zero — is now in
`library/overlays/CLAUDE.md` with numbers, beside the thing that has no UI equivalent at
all: how long a line must stay on screen to be read.

## Design systems

**A design system is a document copied into the project** — adopted, and deliberately not
a project field or a runtime cascade.

OpenDesign ships 154 brand packages, each a `DESIGN.md` plus a compiled `tokens.css`, and
its agents paste the `:root` block into the artifact they generate; there is no global
stylesheet at runtime. kuma takes the same shape at 1/77th the scale: `kuma design-system
<id>` copies one into `<project>/design-system/`, an agent reads it there, and applying it
is an ordinary edit to a template or to an entry's vars.

The alternative — a `theme` on the project, injected into every overlay document beside
the per-entry `--var-…` — was considered and turned down. It would re-style finished
videos the moment the theme changed, which is the same objection already recorded against
shared overlay configurations under *Still open*: vars are project-owned editing
decisions, and they must stay a revision-checked, undoable patch rather than a value that
arrives from elsewhere. Injection also hides where a colour came from, and a template that
only looks right inside kuma has stopped being a plain HTML file.

What this costs: changing the system does not propagate, so a video already made keeps
what it had. That is the intended trade, not an oversight.

**No starter set ships** — decided after writing one. Two were written (`studio`, dark and
accent-led; `broadcast`, light and editorial) and removed before merge: the mechanism is
what the change is for, and an opinionated pair of design systems is *content*, which
wants its own review rather than riding in on a mechanism change. What stays is the way in
and the guide — `library/design-systems/CLAUDE.md` says what belongs in one, and it is
video-specific where a web design system would not be: lengths in kuma's `--u` so they
survive both orientations, safe margins, when a scrim is required, and how long a line
must stay on screen to be read.

**A starter set is owed before any public release.** A feature whose first run lists
nothing teaches nobody what it is for: the guide describes a shape, and a shape is not an
example. At least two, far enough apart to show that the system is a choice rather than a
default — the removed pair is a reasonable starting point, in a change that is about them
and nothing else.

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


## Files dropped from the desktop

**Anywhere in the window, into the bin** — or into the folder row it lands on, in one edit
(`add_media` takes a `parent`). A drop on the bin panel alone was hard to find, so the bin
is framed as soon as a file is over the window. Placing it where it was dropped is to do.
Only the shell can: the daemon adds by path, and only `webUtils.getPathForFile` knows it.

**GNOME on Wayland loses about two drags in three from Nautilus.** They carry no types
and no files, the drop included; native Wayland (`ELECTRON_OZONE_PLATFORM_HINT=wayland`)
and waiting longer do not help. It is a known Chromium-on-Wayland bug. So a drag with no
types is accepted, and the frame stays grey until one says it carries files — a drag that
will fail shows before the drop. Pasting files copied in the file manager would avoid drag
and drop; it was left for now, since Ctrl+V pastes clips.

## Regression tests for the things that are not Rust

**Built.** The suite covered the model, the compiler and the daemon well, and covered
nothing that only goes wrong in a browser or in the shell. Three bugs reached the user in
one sitting, all of them in that gap, none of them the kind a Rust test would have seen —
and each is now pinned by one of the tiers below:

- **Overlay transparency.** A `color-scheme: dark` on the editor's root stopped Chromium
  compositing the overlay iframes: a template declares no colour scheme, so it is a
  light-scheme document, and a light document is not allowed to show through a dark-scheme
  embedder. The preview painted the template's white canvas where the video should be,
  while the render stayed correct — the preview/render gap the live-DOM preview exists to
  close. Nothing failed; it just looked wrong.
- **Stale assets served from cache.** `ServeDir` sent no `Cache-Control` and no `ETag`, so
  a browser invented a freshness lifetime and kept running a bundle from weeks earlier
  after everything on disk had been rebuilt.
- **The shell's own logic** — daemon lifecycle, attach-versus-spawn, stale-binary and
  stale-`ui/dist` detection, the library fallback. All verified during development with
  throwaway Node scripts that were never committed, which is the same as not being tested.

Three tiers, cheapest first. **All three are built.**

1. **Response headers, in Rust — done** (`crates/kuma-daemon/tests/http.rs`). That
   everything the daemon serves carries `Cache-Control`, and that a handler asking for
   `no-store` keeps it, against a daemon on a loopback port with a stand-in `ui/dist` and
   a one-template library, so it needs nothing built and nothing installed. The request is
   hand-rolled rather than adding a client crate for a test. Confirmed to fail with the
   header removed. The cache bug was the most expensive of the three to find and is the
   cheapest to catch.
2. **The shell, in `app/` — done** (`app/test/`, `node --test`, no new dependency and no
   Electron: those modules avoid importing it precisely so they can be driven directly).
   18 cases over the library fallback's precedence, the staleness checks, and what happens
   when `kuma init` writes something unreadable — including that the folder is rolled back
   so the name is free to retry, which is the bug as it was reported. A shell script
   stands in for a stale `kuma`, so no Rust build is involved.
3. **The browser — done** (`ui/test/`, over a shared `harness.mjs`: finding the browser
   and the binary, a daemon on a loopback port, and a CDP driver over Node's own
   WebSocket, which needs no automation library). Two suites.

   **The tab strip** (`ui/test/tabs.test.mjs`) drives the tabs and asserts against
   `/api/sequence`, because which sequence the daemon has open is the state a sequence tab
   switches, and a strip that disagrees with it shows a timeline nobody is editing. It
   exists because a × inside a tab, whose click also reached the tab, closed and
   immediately reopened it — which the screenshot-only harness could never have caught,
   since it cannot click.

   **The composite** (`ui/test/composite.test.mjs`). A fixture
   project with a half-red, half-transparent overlay, the editor screenshotted by the
   browser the baker already uses, and two assertions: the red reaches the page, and
   nothing on it is white. Reintroducing the dark root turns the second into 106,323
   near-white pixels against a threshold of 2,000, so it catches the bug with fifty times
   the margin it needs.

   Both assertions are **position-independent on purpose**. Where the stage sits depends
   on the window, the letterboxing and the panel widths, and a test that had to know would
   break on every layout change without ever finding a compositing fault. Counting colours
   over the whole page does not care. The pair matters too: "nothing is white" alone would
   also pass on a page where the overlay never loaded.

   Pixels are read by a ~60-line PNG decoder (`ui/test/png.mjs`) over Node's `zlib`, which
   is cheaper than depending on an image library or on ImageMagick being installed. It
   skips when the browser or `ui/dist` is missing, the way the render tests skip without
   melt.

The pixel assertions stay coarse on purpose — "this region is not white", "this region is
the colour of the clip under it" — because the point is compositing, not appearance. A
screenshot-diff suite over the whole editor would fail on every font and theme change and
be turned off within a month.

Worth saying plainly: tier 3 is the one that matters and the one most likely to be
skipped, because the bug it catches is invisible rather than loud. The preview looking
right *is* the product.

## Effects: offered where they act, static until keyed

Three entries from *Still open* (offering a clip only the effects that can act on it, what
`fade` does to sound, and every effect animatable the same way) were done together, with
the blur defect, because they are one question: what an effect is to the clip it sits on.

**What a clip carries is a capability, decided in kuma-core** — adopted.
`Project::clip_streams(track, clip)` says whether a clip can show and whether it can sound;
`Effect::acts_on` is `streams().meets(…)` against that. Picture is the track's to allow (an
audio track draws none) and the source's to have (an audio file has none); sound is the
source's alone — a file by the probe's `has_audio`, "yes" for a file not yet probed (the
daemon's rule when it transcribes), a sequence when anything in it sounds, and never a
colour, a still or an overlay. It deliberately ignores mutes: muting a clip must not make its
volume effect invalid, nor take the Muted box away. The editor mirrors it in `types.ts`
(`clipStreams`, `actsOn`, a `Record` of every kind's streams, so a new kind is a compile
error) the way `ANIMATABLE` mirrors `animatable()`; `+ add effect` lists only what acts, and
the clip's own Volume and Muted appear only on a clip that sounds.

An effect already placed that cannot act is **kept, dimmed, and says so** ("no effect here"),
and the validator **warns** rather than errors. An error would refuse every unrelated edit to
a document an agent wrote, or one whose clip was moved to an audio track; it renders, as
nothing, so a warning is the honest weight. The MCP instructions state the split so an agent
does not have to discover it from warnings.

**The sound gets its own fade: `audio_fade`** — adopted, the second of the three shapes on
the table. Making `fade` ramp the gain whenever the clip sounds would change the sound of
every finished video that carries a picture fade. One `fade` with picture and sound switches
would be one kind that is offered to every clip and means something different on each, and
the one-effect-per-kind rule would force the picture and the sound to fade for the same
length. Two kinds keep the old meaning without a migration, let each clip be offered exactly
its own, and give Kdenlive's shape, which is what a user coming from it expects. It compiles
to MLT's `volume` with `gain`/`end` over the clip's own first and last frames: a linear
amplitude ramp to true silence, where a `level` ramp in dB would stop at the -60 dB floor and
still be heard. Checked on a render: a sine averages -42 dB over the first quarter second,
reaches its full -24 dB at one second, and falls symmetrically at the end.

**`volume` is static until keyed, like the rest** — adopted. It has a `gain` (default 1)
and optional `keyframes`, where it used to *be* its keyframes and refuse none. The clip's
own `volume` stays: it is the placement's fader, and the effect is a gain stage on top that
can move, multiplying it as before — Kdenlive has the same two. Folding the clip's value into
the effect instead would be the same number in two places or a field removed by a version
step; neither buys anything. A key without `gain` used to mean 1 and now means the static,
which defaults to 1, so every existing volume reads exactly as it did: version 5 is a stamp.

**Not animatable, on purpose: `fade`, `audio_fade`, `blend`, `mlt`.** A fade's numbers are
lengths of time, so a fade *is* an animation; keying its length has no meaning. A blend mode
is a choice, not a quantity: the only key it could take is a hold, and changing mode
mid-clip is what a cut is for. `mlt` properties are strings MLT reads, where an animation
string can already be written directly — the reason an animation string never enters the
model. `Effect::animatable()` says so, and stays the one table.

**The animate control is a diamond in the effect's head** — adopted, replacing the
**◆ Animate** row. Every animatable effect used to spend a labelled row and a full-width
button on something most effects never use. Now it is an icon beside ×: outlined and faint
until the effect is keyed, filled in the selection colour once it is, which is also how a
folded effect says it moves (the separate `◆` marker is gone). Pressing the filled one
clears, which is where the keys row's **Clear** went. The keys row itself appears only on
an effect that has keys. Clicking the filled diamond throws the keys away — the stopwatch in
After Effects does the same — and it is one undo.

**Blur is blurred both ways** — adopted. The compiler writes `av.sigmaV` beside
`av.sigma`, the same animation. Measured with melt 7.22 on a 640×360 test pattern (mean
absolute difference between neighbouring rows, lower is blurrier): a static blur was already
blurred vertically here without it (0.34 either way), but a keyframed one was not fully — 0.49
without `sigmaV`, 0.34 with it. So what the wrapper does with an unset `sigmaV` depends on the
path, and possibly on the version; writing it takes the question away. The overlay preview
had its own blur bug: CSS `blur()` takes a standard deviation and the render uses half the
radius, so the live DOM drew every overlay blur twice as strong as the export. It now halves.

---

## Probed frame rate

The probe used to record ffprobe's `r_frame_rate`, which for a variable-rate file is the
smallest rate that can express every timestamp rather than the actual one: frames 100/67/33
ms apart, averaging 15 fps, read as 30. It now reads `avg_frame_rate` as well. When the two
agree to within 0.5% the file is constant-rate and `r` is kept, because it is the clean
rational where an average can come back as `2997/100`; when they diverge, `fps` is the
average and `variable_fps` is set, which the inspector shows as "(variable)". Either value
can be `0/0`, and then the other is used.

**Storing both rates** — rejected. `r_frame_rate` on a variable file is a timestamp grid,
not a rate anyone wants to read, and a flag says the one thing the pair would have said.

No format bump: `MediaMeta` tolerates unknown keys and the flag is omitted when false, so
files load either way. Entries probed before this keep their old rate until re-probed,
the stale-metadata problem above. Rendering was never affected — times are seconds
sampled at the project rate, checked pixel-identical against ffmpeg's own extraction on
a variable-rate file. Audio drift over a long variable-rate recording is still untested.

## Render dialog and queue

Render used to fire straight from the top bar, with the preset and a master/open target as
loose selects beside it, and every render ran at once and to the end. It now opens a
dialog modelled on Kdenlive's: a Render tab (sequence, master first and preselected;
preset; optional output) and a Job queue tab listing every render with its progress and a
Cancel button. The top bar's status line opens the queue.

Renders run **one at a time**, in the order they were asked for. Two melts encoding side by
side each run at half speed, and neither finishes first. A render takes a ticket in
`in_render_lane` and waits on a condvar that a freed lane and a cancel both notify. One
cancelled while queued gives up its place and never starts. A first cut polled a mutex
with `try_lock`, which does not keep waiters in order. Each job has one `state`
(`queued`, `running`, `done`, `failed`, `cancelled`) that the UI reads directly. An early
version inferred "queued" from the message text instead. Progress reaches clients at most
every 250 ms unless the percentage moves, because melt reports every frame. Its sequence and output path are
settled when it is asked for (`render_target`), so the queue shows what will be written
and opening another sequence meanwhile changes nothing. A job is published the moment it
exists; before, a queued one said nothing until it moved, and the queue could not show it.

Cancelling is a flag the job watches. The bake loop checks it between overlays, since
bakes are content-cached and abandoning one costs nothing. During the encode, a watcher
beside the stderr reader kills melt and its children, then the partial file is removed.
**A process group of its own for melt** was rejected. It kills the tree in one signal, but
it also stops a terminal's Ctrl-C from reaching melt, which then outlives a stopped
`kuma render` or daemon. So the children are signalled (`pkill -P`) and then the child
itself, which reaches melt under `xvfb-run`.

Found on the way: melt 7.22 takes `-silent` to suppress `-progress` too. A render had
never reported a frame, so it sat at 50% until done. Renders no longer pass `-silent`, and
the melt render test asserts that progress arrives.

**Advanced render options** — adopted. The Render tab has an **Advanced** section, folded
by default like an effect in the inspector, whose head lists what it changes while folded
("CRF 20 · medium · 2 threads"). It sets three things for one render. Left empty, each is
the preset's own, or the machine's for threads.

- **Threads**: frames melt builds at once (`real_time`, see *Bake and render speed*). The
  placeholder is this machine's default, which `GET /api/render/target` reports, since
  it is the daemon's machine and not the browser's. The file is identical however many.
- **Quality** and **Speed**: the codec's rate factor and speed. A preset declares whether
  it has them (`ExportPreset::quality` and `speed` in `kuma-mlt/src/presets.rs`), with its
  own value as the default and, for speed, the options fastest first. x264 has CRF 0–51 and
  its presets. VP9 has CRF 0–63 and `cpu-used` 0–5: 45 s at its default of 1 for two
  seconds of 1080p, 24 s at 2, 8 s at 4. The VP9 preset now writes `cpu-used=1` out, which
  was checked to encode the same bytes as leaving it to libvpx, so the default the dialog
  shows is the one passed. ProRes has neither: its profile is its quality. A test holds
  each declared default to the preset's own property.
- They are a typed `kuma_mlt::Tuning` on `RenderJob`, so melt's vocabulary stays in
  kuma-mlt. `ExportPreset::check` refuses what the codec does not have, rather than passing
  melt a key it would ignore, and `render_target` calls it, so a bad one is refused when
  the render is asked for. `ExportPreset::tuned` writes each in place of the preset's own
  value, not as a second key. `a_render_encodes_with_the_quality_and_speed_asked_for`
  reads x264's own settings back out of the stream (`crf=`, and `subme=` by preset).
- They are job params, so the queue shows them beside the preset. The dialog keeps what
  was last rendered with for the session, quality and speed only with their own preset.
- On the HTTP API (`threads`, `quality`, `speed` on `POST /api/render`) and the MCP
  `render` tool, whose `list_presets` says what each preset offers. Not on `kuma render`.

## Pruning the cache

**Adopted** (`crates/kuma-daemon/src/cache.rs`). Bakes, sampled frames and bin thumbnails
only ever accumulated. One prune on every surface: `kuma cache prune`, `POST
/api/cache/prune` (a job), MCP `prune_cache`, and a button in the job queue beside the
cache's size. It is asked for; a size or age cap could evict a bake a render is about to read.

It deletes bakes whose key is not live, sampled frames older than a minute (a caller may
not have read a younger one) and thumbnails of ids gone from the bin. Live keys are
recomputed, not recorded: every overlay clip in every sequence (the preview bakes nested
ones, `kuma bake` warms unplaced ones) plus each entry's thumbnail bake, through
`jobs::bake_request`, the one definition the bake loop also uses. `Baker::key` splits into
`folder_digest` and `key_with` so each folder is hashed once. `--all` deletes every bake
and frame but keeps the bin's thumbnails, which nothing regenerates until asked.

Work in flight is safe by name and by lock. Every writer names its scratch `….tmp-…`
(`is_in_flight`) until it renames it. Renders (for the whole encode), samples, bakes and
thumbnails hold `AppState::cache_users` shared; the prune takes it exclusively by polling
`try_write`, since std's `RwLock` prefers writers and a blocked prune would stall every
sample behind an hour-long render. Rejected: pruning in the render lane (protects renders
only, and queues later renders behind it). The preview reload is not a holder: its bakes
are live by construction. The lock is per process, so `kuma cache prune` goes through a
running daemon when there is one.

## Drawn overlays: Canvas 2D and Rough.js

**Adopted.** An overlay can paint a `<canvas>` as a pure function of time through
`_vendor/kuma-canvas.js`, with Rough.js vendored for a hand-drawn look; `paint-wipe` is the
first blueprint to use it. Prompted by [ClaudeAnimationBase](https://github.com/JohnHeibel/ClaudeAnimationBase)
(p5.js + p5.brush), whose motion helpers `kuma-canvas.js` adapts (MIT).

- **Not p5.js**: mostly sugar over Canvas 2D, for 1 MB, LGPL, a draw loop to disable and a
  `redraw()` 2.x appears to make asynchronous. **Not p5.brush**: its watercolour needs
  WebGL, and a bake has no GPU.
- **Per-frame JS runs from `onFrame` in `kuma.js`, not `onUpdate`.** It tweens a
  getter/setter, which GSAP writes on every render however the timeline is seeked;
  `tl.seek(t)` suppresses callbacks, and the first cut drew in the bake and stayed blank in
  the preview (`ui/test/drawn-overlay.test.mjs`). It lives in `kuma.js` because any
  non-GSAP drawing has the same trap.
- **`Math.random` is seeded while a frame draws.** Rough.js calls it even when given a seed,
  so seeds alone were not deterministic; they still keep keys independent, through
  Rough.js's own default options. Two bakes give identical pixels in every frame
  (`a_drawn_overlay_bakes_the_same_pixels_twice`; the PNG bytes differ, the image does not).
  Rough.js used directly is a lint error.

## Field report: a seamless loop

An agent built a 10 s seamless loop (a drawn 1986 Tokyo Bay dusk: ten `kuma-canvas.js`
scene layers in a nested sequence placed twice, five titles from one blueprint copy, CRT
and grain textures, a synthesised city-pop track) through MCP alone, and wrote up what
cost it time. Each claim below was checked against the code before it was recorded, and
the blend-mode bug against melt 7.22. Items 1, 2, 4, 5 and 7 are fixed, 3 is half fixed and
6 is built except its model-side part. What the session praised is worth keeping as well:
`base_revision` plus atomic patches with a `note` never lost an edit, nested sequences made
a bloom pass trivial, one overlay folder behind five entries worked as intended, and
`sample_frames` with `overlay` was fast enough to draw with.

**Order of work**, cheapest correctness first:

1. **Blend modes were a silent no-op** — **fixed**. `blend_anim` wrote qtblend's
   `compositing` as discrete keyframes (`0|=0;60|=14;240|=0`), and MLT 7.22 reads
   `compositing` as a plain integer, so the string parsed as `0`, SourceOver, for every mode
   on every track. Reproduced with melt alone: two opaque colour tracks, `compositing=14`
   gives the screen result, `compositing=0|=14` the top colour unchanged. The test
   (`blend_keyframes_are_discrete`) passed because it asserted the string — the *Not
   verified by rendering here* caveat under *Keyframes you can point at*, coming true for a
   different property. Now `blend_spans` cuts the timeline into spans of one mode: a track
   with one mode throughout keeps its single `always_active` transition with a plain
   integer, and a track whose mode changes gets one qtblend per span, each with its own
   `in`/`out` and no `always_active` (which would make every one of them act on every
   frame). The spans tile the timeline, so each frame is composited exactly once, and
   adjacent spans of the same mode merge. `a_blend_mode_changes_the_pixels` renders a
   screen blend for one second of three, then for a whole track, and reads the pixel back
   on both sides of each cut; it fails against the old compiler, and is the test that catches an MLT upgrade
   changing this again.
2. **The MCP instructions were truncated** — **fixed**. They were ~5.4 K characters and
   Claude Code keeps the first 2048, which cut them off after the nested-sequence
   paragraph, losing exactly the rules a user asks agents to respect. Worse, the daemon's
   `note` ("tell the user at the start: …") was appended *after* them, so it had never
   reached an agent at all. Now the instructions are the invariants and a pointer (~1.3 K;
   `instructions_fit_the_client_limit` holds them plus the longest note the CLI writes
   under 2048), the note goes first, and the full text is `timeline_guide` (`kuma-mcp/src/timeline_guide.md`),
   a tool like `overlay_guide`, which a client never truncates.
3. **Looking at a sequence cost full bakes, synchronously** — **half fixed**: the waiting
   is gone, the bake is not smaller. It was the largest wall-clock cost of the session:
   `sample_frames` on a sequence bakes every changed overlay clip for its whole length
   (300 frames, ~0.1 s each at 1080p, measured) before returning one frame, and after most
   edits it passed the MCP call limit with no progress shown. *Melt always* (under *Still
   open*) stands, so the fix could only be in what gets baked or in how the wait is
   handled.

   *Baking only the requested frames was investigated and not built.* hyperframes 0.8.37's
   `render` has no frame-range option. Its `snapshot --at t` does capture chosen instants
   as RGBA at full size, but not the pixels `render` writes: on `location-tag` at 1 s,
   ~12 000 pixels differed from the bake's frame, by up to 57 in alpha, around the text. A
   sample built from it would not be what renders. Rendering a *window* of the clip
   through `render` (the contract telling hyperframes a short composition while the
   overlay keeps its real `duration`) was considered and set aside: hyperframes mounts
   children by its own clock from `data-start`/`data-duration`, so shifting the GSAP
   timeline alone would desynchronise them, and shifting both means depending on
   hyperframes internals. Revisit if hyperframes gains a frame range.

   What was built is the other half. `POST /api/sample/job` is the same sample as a job,
   its progress naming the overlay baking; MCP `sample_frames` runs on it, waits
   `wait_seconds` (90 by default) and then returns the job id and progress instead of
   blocking, and `job: <id>` picks it up again. `/api/sample` stays synchronous for the
   CLI and the snapshot route. And `Baker::bake` claims its key while it renders: a second
   bake of the same key in the same process waits and reads the first's result, so a
   retry, or a preview reload overlapping a sample, never renders the same frames twice
   (`concurrent_bakes_of_one_key_render_once`). The timeline guide now says what a sample
   costs and to sample an entry on its own while drawing it.

   Still open here: baking different overlays of one sample in parallel — since measured,
   see *Bake and render speed* below — and the bake size in *Recorded, not scheduled*.
4. **`kumaCanvas.hash` had no final mix** — **fixed, by addition**. It is bare FNV-1a
   (`_vendor/kuma-canvas.js`), so keys differing in their last character (`s1`, `s2`, …)
   land close together in the high bits that `/ 2^32` keeps. Measured: 400 stars placed at
   `(hash(sx_i), hash(sy_i))` fill 32 cells of a 10×10 grid, and neighbouring keys correlate
   at 0.84; the agent's first star field came out in diagonal lines and every overlay then
   carried its own murmur finaliser. `g.rand`, the per-drawing value, was the same FNV
   unmixed.

   Built: `kumaCanvas.rand(key)` and `g.jitter(key)`, the same two with murmur3's
   finaliser on top (all 100 cells, correlation 0.03). `hash` and `g.rand` are **kept
   unchanged** and documented as the first versions: `_vendor/` is read from the user's
   library and hashed into every bake key, so changing them would re-bake and re-draw
   every drawn overlay that uses them the next time anything re-bakes — exactly what
   copying blueprints into projects exists to prevent. A lint warning
   (`correlated_random`, not an error: they still render deterministically) points at the
   replacements, `paint-wipe` moved to them (projects keep their own copy, so no finished
   video changes), and `ui/test/kuma-canvas.test.mjs` pins the values of all four. That
   rule is general and now in `CLAUDE.md`: a `_vendor/` helper may be added, never changed
   in what it returns.
5. **Lint false positives** — **fixed**.
   - `duration_not_read` fired on overlays that have a timeline but no length to anchor
     to. It now skips a `static` overlay (one frame), and an overlay whose times are its
     own on purpose says so with **`data-fixed-timing`** on its root: a texture that runs
     for as long as its clip, a countdown that takes as long as it counts. That is a
     declaration rather than a guess because a forgotten exit and a deliberate absence of
     one look the same in the markup; it is the same shape as `data-no-timeline`, and
     the warning's fix now names it. `countdown` carries it, and it was the one shipped
     blueprint that warned.
   - hyperframes' `studio_missing_editable_id` (and every `studio_*` rule) is about edit
     targets in hyperframes' Studio, which kuma never opens, and `gsap_infinite_repeat`
     asks for a finite root `data-duration` that the contract always stamps. Both are
     dropped in `not_kumas`, beside the existing filter for its timeline-registry miss.
     `paint-wipe` and `callout-arrow` carried them.
   - The test let all three blueprints through because it asserted no *errors*. It now
     asserts no findings at all, as the scaffold's test does: a blueprint is read as an
     example of the contract, so a warning in one teaches the wrong thing, or is a rule
     that is wrong. Without hyperframes installed it still checks kuma's own rules.
6. **Loops as an idea** — **the cheap pieces built**; the model-side part is not.
   - `kumaCanvas.cycles(duration, period)` (whole cycles, at least one), `phase(t,
     duration, n)` and `loopNoise(x, y, t, duration, n)`, which walks a circle through
     the noise field instead of a line so it returns to its start. `_vendor/` is
     append-only in behaviour, so these are additions and a test pins what they return.
   - `g.still(key, paint)`: a frame-sized canvas painted once and kept. It is seeded by
     its key alone (Math.random and its `rough()`), because which frame asks first differs
     between the bake (frame 0) and the preview (anywhere), and the picture must not.
     Checked in the real renderer: an overlay built on it lints clean and bakes the same
     pixels twice, 24 frames of 24.
   - `kumaCanvas.rgba(hex, a)` and `mix(a, b, k)`, keeping a colour var's own alpha.
   - *Scenes and ambient layers* in `library/blueprints/CLAUDE.md`: no exit
     (`data-fixed-timing` or `static`), whole cycles over `duration`, small slow
     amplitudes, depth as speed, `g.still` for what never moves, grain carrying its own
     alpha, and looking at the seam. The motion table's "Loops" row now says it is about
     overlays over footage.
   - **Not built:** a `repeat` on keyframe lists and a defined tangent at the ends of a
     `smooth` list, so a closed loop is smooth at the seam. Both are model changes and a
     `PROJECT_VERSION` bump, and neither has a shape yet; agreed in direction only.
7. **Smaller, each an afternoon** — **built**.
   - `sample_frames` over a range with no `count` returns `RANGE_COUNT` (5) frames rather
     than one; one time is still one frame. On every surface, since the default moved
     into the daemon's `SampleRequest`.
   - `add_font` takes a `.ttc`/`.otc` and splits it into one file per face
     (`split_collection`: each face's table directory rewritten with its tables copied
     after it, nothing inside a table changed), each under its own family. A collection is
     split on the way in rather than read in place because `@font-face` takes one face per
     file, and "a font is a file" stays true for everything downstream. Checked with
     `wqy-zenhei.ttc` in a bake with the system's CJK fonts hidden from Chrome: the split
     face drew 夕焼け, and Inter drew NO GLYPH boxes.
   - That check found a second fault: a family was read under whichever name record
     decoded first, which for that font was "文泉驛正黑". The US-English record now wins
     when there is one, since that is the name a person types. The other names stay as
     aliases: `resolve` finds the family by any of them and `Family::css` declares its
     faces under each, so a `font` var written when the localized name won still draws
     the same font rather than falling back — a renamed family would re-style a finished
     video at its next bake.
   - Splitting copies the shared tables into every face: `wqy-zenhei.ttc` (16.8 MB, three
     faces) becomes 39 MB. Accepted, because `@font-face` takes one face per file; reading
     collections in place would mean a face index on every `Face` and an extraction on
     every stage and every served font. A `.ttc` dropped straight into a `fonts/` folder
     is still not read, and the lint's `font_missing` fix now names it and says to add it
     with `add_font`. Not built: splitting inside `FontLibrary` into a derived cache, so
     that dropping one in works too. The lint that names the characters of a text var no
     resolved family can set was built later (`glyphs_missing`, the explainer's report).
   - Every face and family reports `scripts` (latin, greek, cyrillic, arabic, hebrew,
     devanagari, thai, han, kana, hangul), each by one character it cannot set without;
     `list_fonts` says what it means and the editor's font picker shows it on hover.
     Scripts rather than a CJK flag, because han without kana is a Chinese font that
     cannot set Japanese.
   - `add_blueprint` takes a `folder` name; the daemon's copy already accepted one and the
     MCP tool never passed it.
   - `kuma compile` includes every overlay the cache has baked (`Baker::cached`, keyed by
     the same `bake_request` as the render) and names the clips it left out; `--bake`
     bakes those first. Its help said "skipped unless baked" but it never looked.

**Recorded, not scheduled** — each is a feature with its own design question:

- **Bake size.** Full-frame RGBA PNGs for a four-letter label (~40 MB per 10 s) and ~250 MB
  for grain; the cache reached ~2.9 GB in an hour of tweaks. Already on the list below as
  *no per-overlay bake region*; this adds a second shape, a short `bake_period` for
  textures that tile in time. Automatic pruning stays rejected (*Pruning the cache*): a
  cap can evict a bake a render is about to read.
- **Adjustment layers or effects on a track.** The bloom was a second placement of the
  scene, so any camera move would have had to be keyed twice, identically.
- **A `glow` effect.** The bloom needed the `mlt` escape hatch and ffmpeg's `lumakey`.
- **`transform` offsets in pixels** do not carry between 1080p, 4K and 9:16, where the
  overlay contract is resolution-independent everywhere. Fractions of frame would be a
  format bump.
- **Audio has no counterpart to `sample_frames`.** Loudness and peak from `probe_media`,
  and a waveform or spectrogram image, would let an agent check what it cannot hear.
  Loudness and peak are built, for a sequence rather than a file: `measure_audio` (the
  explainer's report). A picture of the sound is not.
- **A `history` or diff-since-revision tool**, so an agent that sees the revision move can
  learn what moved instead of re-reading the project.
- **Checking a loop's seam**: comparing the last frame with the first was done by hand in
  ffmpeg. Only worth a tool if loops become a declared property of a project.

## Bake and render speed

A study, prompted by "baking and rendering take a very long time for long videos". Every
number below was measured on one machine: an i7-9750H laptop (6 cores, 12 threads, 16 GB)
with a GTX 1660 Ti, hyperframes 0.8.37 and melt 7.22. The project was a 60 s 1080p30
timeline: one H.264 source clip, a `subtitles` overlay over all of it (141 cues), a 10 s
`lower-third` and a 5 s `title`. Where it says *identical*, frames were compared one by
one: PNG bytes or decoded RGBA for bakes, and per-frame MD5s from melt's `framemd5` output
for renders.

**Where the time goes.**

- **Baking** is the long pole for long videos. The 60 s subtitles (1800 frames) bake in
  77 s, about 23 fps, and one hyperframes process uses about one core of the twelve.
  `bake_clips` baked one clip after another, so the rest of the machine sat idle (the
  first change below). Each captured frame costs 65–80 ms, which is Chrome's PNG
  screenshot over CDP (software raster, then zlib). Each bake also has about 2.3 s of fixed start-up (a 5-frame bake).
  hyperframes skips frames it can prove static (its *static dedup*, armed from the GSAP
  timelines): it reused 885 of 1800 frames (49%), while 1457 of the 1800 (81%) were in
  fact byte-identical to another frame. At this rate, a 20-minute transcript is about 26
  minutes of baking on its own.
- **Rendering** the 60 s timeline takes 69.5 s. Building the frames without encoding takes
  56 s, of which the video track alone is 7.2 s. The subtitles track adds about 25 ms per
  frame: about 12 ms of it is PNG decode of a full 1080p RGBA frame, and the rest is
  `qtblend` converting the frame to RGBA, blending and converting it back. A track's blank
  stretches cost next to nothing. That graph runs on about one core. The encoder is the
  largest CPU cost: x264 `preset=slow` takes 228 of the render's 321 CPU-seconds, on
  threads of its own.

**Bake measurements**, the 60 s subtitles:

| Change | Time | Output |
|---|---|---|
| Today, one process | 77 s | — |
| 4 separate one-worker processes at once | 82 s for 4× the frames (3.75×) | byte-identical to serial, ×4 |
| 8 at once | 105 s for 8× the frames (5.9×) | byte-identical to serial, ×8 |
| hyperframes' distributed API, one clip in 4 chunks | 29 s (2.65×) | 298 of 1800 frames differ: fonts are substituted |
| `--browser-gpu` | 83 s | identical |
| hyperframes 0.8.78 | 79 s | identical |

**Render measurements**, the 60 s timeline with its bakes cached, preset `youtube-1080p`
unless stated:

| Change | Time | Output |
|---|---|---|
| Today, serial | 69.5 s | — |
| `real_time=-6`, `preset=medium` | 44.3 s | file 2.4% larger |
| `real_time=-6`, `h264_nvenc` (`p6`, `cq 19`) | 33.9 s, at 380% CPU rather than 840% | file 118 MB rather than 74 MB; quality not compared |
| The timeline split into 4 ranges, one melt each (frames only, no encode) | 28 s rather than 56 s | identical, the seams included |
| Overlays as `qtrle` `.mov` rather than PNG sequences | frames only: 51.3 s rather than 51.8 s | identical; 20 MB rather than 42 MB |
| Overlay cropped to its alpha box, placed with `qtblend`'s `rect` | frames only: 40.5 s rather than 51.3 s | identical |
| `composite` in place of `qtblend` | frames only: 29.6 s | every frame differs: it blends in YUV |

**Bake different clips in parallel** — adopted. Separate one-worker hyperframes processes
are deterministic. The rule in `CLAUDE.md` is about hyperframes' own multi-worker capture
inside one render, and 12 runs of 1800 frames, 4 and 8 at a time, came out byte-identical
to the serial bake.

- `bake_clips` runs a pool, **longest clip first**, and fetches clips that come to **one
  key once**. Otherwise a clip placed five times would park up to four workers on
  `Baker::claim`. It hands the key it computed to `Baker::bake_keyed`, so each folder is
  hashed once per call.
- **The limit is the baker's, not the loop's.** `Baker::concurrency` caps the hyperframes
  processes across every caller (render, preview, sample, thumbnails), since a pool per call
  would multiply when they overlap. The slots are first come, first served, so the preview's
  one bake waits for at most the renders already running. A cache hit never waits. The
  default is half the hardware threads, at most 8 (about 250 MB each), and `KUMA_BAKE_JOBS`
  overrides it (`1` is serial).
- **Measured in kuma** (`kuma bake`, the same laptop): the 60 s transcript split into four
  15 s entries bakes in 21.8 s rather than 87.2 s (4.0×). The three-clip project goes from
  91 s to 85 s, because its 60 s subtitles are the long pole. Every bake was byte-identical
  to its serial counterpart.
- Tests: `renders_run_side_by_side_up_to_the_limit`,
  `clips_bake_side_by_side_and_a_shared_key_bakes_once` and
  `a_cancelled_bake_starts_nothing`. The first two were checked to fail with the limit
  ignored or the key-sharing removed.

It does nothing for one long clip: see *One long overlay clip* below.

**Melt rendering several frames at once (`real_time=-N`)** — adopted. Every render passes
`real_time=-N` (`command` in `kuma-mlt/src/melt.rs`), N from the job's `Tuning::threads`
or else the machine's default. Below zero, melt builds N frames at once and drops none.

- **It changes no frame.** On the reference timeline (`reference.mlt`: timewarp, luma
  dissolves, blur, `eq`, fades, `qtcrop`, eleven `qtblend`s with a blend mode, `mix`,
  volume, stills, an animated and a still bake, nested sequences), every one of 450 video
  frames and 451 audio packets was identical to a serial render, at 2, 3, 6 and 12
  threads, three runs each. The comparison is not vacuous: 289 of the frames are distinct,
  and a one-frame shift would change 288 of them. *Preview* above records that not every
  MLT service is thread-safe, so this is now a test,
  `parallel_frames_render_what_serial_frames_do` (`kuma-mlt/tests/render.rs`): the
  reference project at preview size with a moving overlay, picture and sound compared
  serial against 4 threads. A new effect belongs in the reference project, so it is checked
  here too. Filters passed through the `mlt` escape hatch are not covered.
- **How many.** The bench render, 60 s with its bakes present, on the laptop:

  | `real_time` | `youtube-1080p` (x264 `slow`) | `draft` (x264 `ultrafast`) |
  |---|---|---|
  | `-1` (before) | 69.9 s | 61.2 s |
  | `-2` | 55.1 s | — |
  | `-3` | 52.5 s | 36.1 s |
  | `-4` | 51.8 s | 35.4 s |
  | `-6` | 51.1 s | 35.1 s |
  | `-12` | — | 37.0 s |

  The default is half the hardware threads, at most 8, the same rule as bakes. Above 6
  it is unmeasured, and 12 was slower on 12 threads. `KUMA_RENDER_THREADS` overrides it. With
  `slow` the gain stops at about 1.37×, because x264 then has the CPU to itself (830% of
  1200%). With `ultrafast` it is 1.74×.
- Samples are one-frame melt runs and gain nothing. The preview engine pulls frames itself
  and is untouched: *Frames in parallel* under *Preview* still applies to it.

Splitting the timeline into ranges and running one melt per range gains about the same,
also exactly, but it has to stitch the encodes and render the sound separately, so
`real_time` was preferred.

**Encoder presets** — built: a render can choose its preset's speed and quality (*Advanced
render options* under *Render dialog and queue*), and there is an NVENC preset. The render
is CPU-bound once its frames are parallel, and x264 `slow` is the largest single cost.
`medium` gains 13% with parallel frames and next to nothing without.

**`youtube-nvenc`** — adopted. H.264 on the NVIDIA GPU: `h264_nvenc`, `p6`, `tune=hq`,
`rc=vbr`, `cq 21`, `vb=0`, 3 B-frames, with CQ 1–51 and p1–p7 as its quality and speed.

- **Calibrated on real footage**, a 1080p screen recording, against `youtube-1080p` (x264
  `slow`, CRF 18), with ffmpeg's SSIM and PSNR against the source (this ffmpeg has no
  VMAF). On its busiest 30 s, x264 made 11.5 MB (SSIM 0.99756, PSNR 51.1 dB), and NVENC
  at CQ 21 made 15.1 MB (0.99791, 52.9 dB). At CQ 22 it made 13.5 MB (0.99748, 52.0 dB),
  just under x264 on SSIM. On a near-still 30 s, CQ 22 was 1.5 MB against x264's 1.4 MB,
  and a hair under it on SSIM. So CQ 21 is the default: as good or better on both
  metrics, for about a third more bytes, which an upload YouTube re-encodes can afford.
  `p7` measured no better than `p6`, and spatial and temporal adaptive quantisation spent
  bytes on what these metrics do not score, so neither is on.
- **Speed.** The 60 s bench with its bakes cached took 33.9 s on NVENC (measured at CQ 19,
  before the calibration) against 51.1 s on x264 (1.5×), at 380% CPU rather than 835%, which leaves the CPU for bakes. Through
  `kuma render` on a machine busy with another render it was 67 s against 100 s, the
  same ratio. Once the encoder is on the GPU, building the frames is the bottleneck (29.9 s
  of the 33.9).
- **Offered only where it works.** `ffmpeg -encoders` lists `h264_nvenc` whether or not
  there is a GPU, and **melt crashes** (a segfault, exit 139) on a render with it where
  there is none. So a preset says what it `needs`, and `presets::available` tries it once
  per process (`melt::try_preset`): five black frames through melt with all the preset's
  properties. It reports the encoder's own last line from stderr as the reason
  (`CUDA_ERROR_NO_DEVICE: no CUDA-capable device is detected`). The trial has to be
  progressive (`atsc_720p_25`). melt's default profile is interlaced PAL, which NVENC
  refuses, so it failed with the GPU present too until that was found. Every kuma profile
  is progressive.
- `GET /api/presets` (and MCP `list_presets`) is `presets::offered()`: each preset with
  `unavailable`, the reason or null. A daemon from before it leaves the field out, which
  reads as usable. The dialog lists an unavailable one greyed, with the reason on hover, and
  never preselects it. `render_target` refuses one with its reason, so a render never reaches
  melt to crash. The first ask costs about a second, off the async threads.
- Tests: `the_nvenc_preset_renders_where_there_is_a_gpu` (skipped, with the reason, where
  there is none), `a_preset_whose_encoder_is_missing_cannot_be_tried` and
  `a_preset_this_machine_cannot_run_is_refused`. Both branches were run here, the second
  with `CUDA_VISIBLE_DEVICES=-1`.
- Not built: VAAPI or QSV on Intel and AMD, HEVC, and a 4K-specific NVENC preset (presets
  do not set a size, so this one renders any size the GPU encodes). Intel's encoders are
  listed on this laptop and unmeasured.

**Baking in the background** — deferred. A render bakes every overlay on the sequence it
renders, and on the open sequence none of them are baked, because the preview draws them
as live DOM. So a render always starts with the whole bake. Baking the open sequence's
overlays while the editor is idle (lowest priority, one or two workers) would take that
wait out of the edit-then-render loop. The costs are CPU while editing and more superseded
bakes to prune. **Not planned unless it proves needed.** Bakes are cached by content. With
overlays kept short (below), an edit rebakes only the clip it touched, so the bake left
before a render is small, and the render itself is the wait.

**One long overlay clip** — avoided by composition, adopted as guidance. Parallel clips do
not help a transcript that spans the whole video, and nothing splits one clip:

- An overlay clip starts at its own time zero (`p_in` is 0 for everything that is not timed
  media), and vars live on the entry, so a razor cut cannot give the second half a
  different start. What works is one `subtitles` entry per minute or so, each with its own
  cues and `offset`, cut at pauses. The pieces bake in parallel, and fixing a typo or
  trimming a piece rebakes only that piece. The cost is a bin full of pieces, and a style
  change that has to reach all of them.
- **Decided:** the agent guidance (`timeline_guide`) says to compose many short overlays
  rather than one long one, and to split a transcript into entries of at most about a
  minute straight after `transcribe`. A long overlay is then a mistake the guide warns
  against, rather than a case the baker must make fast. `transcribe` still writes one
  entry; splitting there instead is possible if agents keep missing the rule.
- *hyperframes' distributed API* (`@hyperframes/producer/distributed`: `plan`,
  `renderChunk`, `assemble`) runs locally and supports PNG sequences. It is rejected as it
  stands: it substitutes its own faces for system fonts, which were 298 frames of the
  subtitles, so the render would not match the preview. It also only takes 24, 30 or
  60 fps, which rules out 29.97 and the effective rate of a sped-up bake.
- hyperframes' in-process capture stage already accepts a frame range and does not expose
  it (`render` has no such option in 0.8.37 or 0.8.78). A `render --frame-range` upstream
  would split one clip across processes with nothing substituted, and is worth asking for.

**`qtrle` intermediates** — deferred. `qtrle` is lossless and stores an unchanged line in
almost nothing, which suits overlays. It is half the size of the PNG sequence and decodes
27× faster (0.8 s rather than 21.9 s for 1800 frames, single-threaded). But melt already
decodes on its own thread, so a render gains only the CPU (about 5% once frames are
parallel), and it would break the rule that alpha intermediates are PNG sequences.
Reconsider if bake size (*Recorded, not scheduled*) becomes the problem.

**Cropping bakes to their alpha box** — deferred, with *no per-overlay bake region* under
*Still open*. The overlays cover 1.5–1.9% of the frame, and blending the cropped box is
exact and 21% faster. `qtblend`'s fixed cost of converting the whole frame stays. The
blend is a per-track transition while the box is per clip, so the compiler change is
larger than the gain.

**Rejected:** `--browser-gpu` (no faster, since PNG encoding is the cost rather than
raster), upgrading hyperframes for speed (0.8.78 is the same speed, same pixels), and
`composite` in place of `qtblend` (faster, but it changes every overlaid frame).

**Found on the way:** a prune never deletes `.tmp-` scratch, however old. A killed bake's
leftover (38 MB in one project, from a week before) stays forever. The name carries the
baking process's id (`<key>.tmp-<pid>-<nanos>`), so a prune could delete the scratch of
processes that are gone.

## The agent skill

The MCP's `timeline_guide` and `overlay_guide` state the contract. What an agent needs to
make a *whole* video well is craft around it: how to organise hundreds of clips, and the
traps that each cost a render to find. That lives in `library/skills/kuma-video/`, a Claude
Code skill (`SKILL.md`, references, scripts that check a project).

- **Copied into the project, not installed globally.** `kuma init` (and so the launcher)
  and `kuma mcp-config` write it to `<project>/.claude/skills/kuma-video/`, beside the
  `.mcp.json` they already write. Claude Code picks up a project's skills there, so nothing
  outside the project is touched, as with `.mcp.json`.
- **Embedded in the binary** (`crates/kuma-cli/src/skill.rs`), so a project gets the
  version that documents the `kuma` it runs with. `kuma mcp-config` refreshes kuma's own
  files, and leaves anything else in the folder alone. A stale copy misleads more than a
  missing one. `kuma doctor` reports current, stale or missing. A test fails if a file in
  the folder is not listed in `FILES`.
- **Written from one video, kept general.** Its first version described the explainer it
  came from (manga page turns, kanji glosses, a pixel-art seascape). It now starts from what
  kind of video is being made (footage-first or overlay-first), and keeps that project's
  lessons as examples: a skill every project receives must not steer a vlog towards
  screentone wipes.
- **Scripts over prose where a check is possible**: reading time per scene
  (`reading_audit.py`), contact sheets of a rendered file, a Japanese font subsetter, and a
  procedural footage template. It shipped with a model of the audio mix (`mixsim.py`) and a
  CJK glyph check (`fontcheck.py`) too; `measure_audio` and the font lint replaced them (the
  explainer's field report below).

## Field report: a ten-minute explainer

An agent made a 9½-minute explainer about kuma in kuma, through MCP alone: 26 overlay
components, ~400 entries, ~590 clips, seven nested sequences, a storyboard kept as code and
diffed into patches. What cost it time is below, each checked against the code before it
was recorded. All seven items are fixed or built, and so are the four things the report
wanted moved into kuma, except where it says otherwise.

1. **A quote in a string var broke the vars after it** — **fixed**. `inject.rs` wrote string
   vars unquoted into one `:root { … }` line, neutralising only `; { } < > \n`, so the `'` in
   "Let's" opened a CSS string that ran to the end of the stylesheet, or to the next var's
   apostrophe. Reproduced both ways: `getComputedStyle` in a headless shell read the title as
   `Let's go; --var-b_color: …` and no colour at all, and a bake drew the colour var's
   fallback.
   - `css_value` keeps a value's tokens as written, since a string var may hold a CSS value
     (a shadow, a quoted string for `content:`). It escapes only what cannot close inside the
     value: an unclosed quote (`\'`), a comment with no end (`/\*`), a backslash at the very
     end. `\r` and form feeds become spaces, as `\n` always did. Quoting every string var
     would have broken the CSS values. The report's other suggestion, a declaration per line,
     still loses one: the bad string stops at the newline, and its declaration runs on to the
     next `;`.
   - **The bake key hashes the vars, not the document they are injected into**, so the fix
     alone would have left the broken bakes cached. The key now also hashes `repaired_css`:
     the declarations the repair writes differently, which is nothing for almost every
     overlay. Every other bake is the same document it was and stays cached; bumping
     `kuma-bake-v5` would have re-baked every finished video to no effect. Hashing the text
     rather than a flag means a later change to `css_value` moves exactly the keys it
     changes, with no salt to remember.
   - Tests: `a_quote_that_never_closes_cannot_swallow_the_vars_after_it`,
     `only_a_repaired_var_needs_a_new_bake`, and a real bake,
     `an_apostrophe_in_a_var_leaves_the_vars_after_it_alone`: blue without the repair, red
     with it.
2. **`sample_frames` with `overlay` refused an entry that has a `parent`** — **fixed**. The
   throwaway project keeps the entry and drops its `parent`: where the bin lists an entry is
   nothing it shows. `an_entry_filed_in_a_folder_can_be_sampled`.
3. **A fade that rounds to one frame covered the whole clip** — **fixed**. On a clip whose
   range starts at frame 0 it was a filter at `in="0" out="0"`, which MLT reads as no range.
   A fade is now at least two frames, clamped by the clip, so only a one-frame clip keeps
   one, and there the whole clip is that frame. Two frames draw the same picture a one-frame
   fade did (frame 0 at 0), and a sound fade gets a ramp. `a_fade_under_two_frames_is_two_frames`
   checks the ranges. The measuring test below renders a 0.02 s fade-in and measures its
   chapter: without the fix the chapter is silent.
4. **Library fonts vanished** — **fixed**, both halves.
   - **Why they vanished.** The desktop shell hands the daemons it starts the checkout's
     `library/` when the user library has no blueprints (`libraryFallback`). `kuma mcp` and
     `kuma serve` did not. On this machine `~/.local/share/kuma` has no `blueprints/` or
     `fonts/`, so a daemon the app started had the starter fonts, and one that `kuma mcp`
     started after the app closed had none. `library_dirs` (`state.rs`) now applies the rule
     to every daemon, whoever starts it: the app no longer sets `KUMA_LIBRARY`, which would
     have made its copy win, and keeps `libraryFallback` only to say so in the launcher.
     `serve` logs it, and `health` lists the folders either way.
     `the_starter_set_stands_in_for_a_missing_library`.
   - **Why nobody noticed.** `Baker::font_findings` is now what `check_overlay`,
     `validate_project`, `kuma validate` and a render's result report. `validate_project`
     checks every overlay entry with its own vars; a render's `warnings` cover the entries it
     drew. `validation_names_text_the_fonts_will_not_draw`.
5. **Kanji fell back to Chinese forms** — **built** as the glyph-coverage lint (3 below).
6. **No audio view** — **built** as `measure_audio` (2 below).
7. **The MCP process outlived a rebuild** — **fixed**. The server stats the executable it
   started from on every call. Once that file has changed (another size or time, or gone
   mid-build), every tool result starts with a note: the tools are the old build's, so tell
   the user and restart the server. An error carries the note too.
   - Rejected: comparing versions with the daemon. The git hash is the same across rebuilds
     of uncommitted work, and what bit was this process being old, whatever the daemon is.
   - Rejected: re-executing itself. The client has initialised the old process, and a new
     one would wait for a handshake that never comes.
   - Tests: `a_rebuilt_binary_is_noticed`, `a_stale_server_says_so_before_the_answer`.

**What moved into kuma**, so the skill's scripts and workarounds shrink, in the report's
order of payoff:

1. **A patch from a file** — **built**. `apply_edits` (and `POST /api/project/patch`) take
   `patch_path`: a JSON Patch array in a file inside the project, which the daemon reads and
   applies as one atomic, revision-checked edit. The answer counts its `ops`. The path is
   relative, or absolute under the project; anything else is refused before it is read.
   `a_patch_is_applied_from_a_file_in_the_project` applies 200 operations as one revision.
   - Not built: a server-side "make these equal". What a storyboard may remove is its own
     to know (its diff removes only entries of its chapter's folder), so the diff stays in
     the storyboard.
   - Not built: `history` / diff-since-revision, still recorded under the loop report.
2. **Measuring audio** — **built**: `measure_audio`, `POST /api/audio/measure`, `kuma
   measure-audio`; a job `cancel_job` can stop, since it is a melt run of the whole length.
   - **How.** The sequence compiles as the preview's audio half graph (`Streams::Audio`,
     checked sample by sample against the full graph), so nothing is baked. melt writes it as
     32-bit float WAV (`presets::AUDIO_ONLY`): float, so a mix over full scale is metered
     rather than clipped. ffmpeg's `ebur128` with `framelog=verbose` then gives every 100 ms
     the momentary and short-term loudness and the true peak. The whole comes from the
     meter's summary. A section's integrated loudness comes from its 400 ms blocks, gated as
     BS.1770 gates them (`kuma-overlay/src/loudness.rs`).
   - **What it returns.** Sections default to the clips that place a sequence, the chapters;
     `sections` names others. `hottest` is the five loudest steps by true peak, at least 2 s
     apart. `silences` are stretches of at least 1 s under the absolute gate (-70 LUFS).
   - **Measured on the explainer** (9½ min, seven chapters): 43 s with the release build,
     430 MB at most. It read -17.3 LUFS, 5.1 LU of range and -1.9 dBTP; the final render's
     file meters -17.3 LUFS, 5.2 LU and -1.9 dBTP. The script it replaces modelled the mix
     within 0.5 LU and could not have seen item 3.
   - Tests: `a_sequence_is_measured_whole_by_chapter_and_where_it_is_silent` (two chapters
     6.0 dB apart, the gap between them found to within 0.15 s), and the parse and gating in
     `loudness.rs`.
   - Not built: a loudness target on render (normalise or limit on the master), still a
     master-bus concept the model does not have.
   - **Found on the way:** a progress callback that panicked hung its job for ever. This
     job's first version overflowed a `u8`; `run_cancellable`'s watcher then waited for a
     `finished` flag the unwinding reader never set, and the scope waited for the watcher. A
     drop guard sets the flag now. `a_panicking_callback_does_not_hang_the_run`.
3. **Glyph coverage in the lint** — **built**. Each face keeps its cmap as code-point ranges
   (`Coverage`, shared between clones). Two warnings, both from `font_findings`:
   - `glyphs_missing` names the characters of each text var (`string`, `text`, the lines of
     `cues`) that none of the overlay's `font` vars' families can set. It checks against the
     union of those families, since which var draws which text is the markup's business, and
     not at all when no family resolved.
   - `cjk_lang` flags kana, hangul or han on a page whose `lang` is not `ja`, `zh` or `ko`,
     and names the language the text suggests.
   - On the explainer's own project there is nothing to report (it had fixed everything by
     hand), and the check takes 3 s for ~400 entries. With one label's fonts switched to
     Inter, `ketsu_label4` is reported showing 五第話.
   - **Decided: no project-level fallback family, for now.** The lint names every
     character, and a `jp` font var in an overlay is two lines. If it is built, the shape is
     `unicode-range`: the fallback's faces declared again under each font var's family,
     restricted to what that family lacks, so `font-family: var(--var-font)` draws kanji
     from it and no overlay changes. It is a project field, so a `PROJECT_VERSION` bump.
     Worth it when a second CJK project asks for it.
4. **`sample_frames` with explicit times and a sheet** — **built**.
   - `times` (up to 32) replaces `t0`/`t1`/`count`. A sample bakes the overlays drawn at
     the frames it renders and nothing else, in frames rather than seconds (`overlay_clips`
     takes the times): frames at 3 s and 400 s bake what those two show and nothing between,
     a range sample stopped baking what falls between its frames, and 41.99 s, which rounds
     to the first frame of a clip starting at 42 s, bakes that clip where a test in seconds
     left it out.
   - `tile` returns the frames as one sheet, each captioned with its time (ffmpeg
     `drawtext`, left off if ffmpeg finds no font). Frames are 480 px wide unless `width`
     says, and `columns` defaults to about square. The MCP tool returns the sheet in place of
     the frames.
   - A sample's frames now render up to four at a time, since each is a melt run that loads
     the whole document, and a frame two times round to renders once.
   - `frames_at_chosen_times_tile_into_one_sheet`. The skill keeps `contact_sheet.py` for a
     rendered file.

**The skill** lost `mixsim.py`, `fontcheck.py`, the three trap rows the fixes removed
(quotes, the one-frame fade, sampling a filed entry) and chunked applying. It points at
`measure_audio`, `validate_project`, `patch_path` and `times`/`tile` instead.

**Better left in the skill:** `reading_audit.py`, which relies on its components' var
names, unless manifests someday declare which vars hold text to be read;
`pixel_footage_template.py`, which is content, not tooling; and `subset_jp_fonts.py`, since
`add_font` of the full collection already gives a complete JP family.

**Also found:** the real-bake tests skip on a machine whose only headless Chrome is
hyperframes' own download, or a Playwright build that lays itself out as
`chrome-headless-shell-linux64/chrome-headless-shell` (1208 and later here), which
`find_playwright_headless_shell` does not know. `KUMA_ALLOW_BROWSER_DOWNLOAD=1` runs them
against hyperframes' own Chrome, the one the daemon's bakes use. Not changed: the browser
path is hashed into every bake key, so finding one where none was found would re-bake every
finished video, and change which Chrome draws it.

## Field report: a pixel-art music video

Six agents made a 90 s pixel-art music video: 21 tracks, 117 clips, 13 overlays drawn as
480×270 canvases scaled up ×4. They worked with a binary from 2026-09-14, eight days older
than the checkout, so the list was checked against the code before anything was recorded.
Its worst item, bakes read at 25 fps whatever the project's rate, had already been fixed
(`force_fps`, 59985a4).

1. **An overlay that opens on an opaque frame lost its alpha for the whole clip** —
   **fixed**. Chrome writes a screenshot with no transparent pixel as an RGB PNG, and
   ffmpeg's image2 demuxer takes the whole sequence's pixel format from its first file, so
   a flash or a wipe that opens full-frame composited as an opaque rectangle to its last
   frame. `mlt_image_format=rgba` on the producer does not help, since the alpha is gone
   before MLT sees the frame. Reproduced with melt: an RGB first frame over purple showed
   the transparent pixels' colour, an RGBA one showed purple. Only the first file decides:
   RGB frames later in an RGBA sequence composite as the opaque frames they are.
   - A bake's first frame is rewritten as RGBA when its header says it has no alpha
     (`first_frame_rgba`), with ffmpeg, pixels unchanged. One frame per bake, so nothing
     is paid for the overlays that already begin transparent.
   - **A cached bake is repaired when it is read**, not re-keyed: bumping `kuma-bake-v5`
     would re-bake every finished video for the few that open opaque. The rewrite is
     scratch (`.tmp-…`) until renamed, since readers may hold the bake.
   - `an_opaque_first_frame_keeps_the_sequence_alpha`: ffprobe reads the sequence as
     `rgba`, for a new bake and for one cached with the frame Chrome wrote.

2. **A clip whose ends fall between frames baked a frame short** — **fixed**. A bake had
   `fps.length(end - start)` frames and the compiler places `frame(end) - frame(start)`:
   0.9 s from 426.0 at 25 fps is 22 frames baked and 23 placed. About a quarter of clips
   timed to the hundredth disagree. The report saw that frame white, which was the missing
   `force_fps`; with it, melt holds the last frame, seeking or playing, so what was left
   was a one-frame freeze.
   - A `BakeRequest` carries the frames its clip is placed on (`clip_frames`, in
     `bake_request`) beside the duration, and bakes that many. The overlay is still told
     its clip's duration, so a document an older kuma baked is the same document.
   - **The key adds the count only when it differs from what the duration rounds to**, so
     only clips between frames re-bake. Snapping the duration to whole frames instead
     would have re-keyed nearly every bake at 29.97: 3.0 s is 90 frames either way, but
     90 frames last 3.003 s.
   - `a_clip_between_frames_bakes_the_frames_it_is_placed_on`, which pins two keys an
     older kuma gave.

## Still open

Nested sequences are done and the editor is an application now, so this list is the whole
of what is next on the *model and compiler*; the desktop shell's own loose ends are at the
end of its section above. The item this list gained from the sequence work comes first,
because it is the part deliberately left out.

- **One pipeline for every bin entry, not just sequences** — agreed direction,
  **postponed until a feature needs it**, and the largest thing on this list.

  Today `sample_frames` takes a *sequence* or an *overlay entry*, and the overlay case
  builds a throwaway one-clip project and runs the ordinary path — same bake, same
  compiler, same melt. That mechanism generalises; the interface does not. It grew two
  ways in because the API had two fields, and then a rule refusing both at once.

  **A sequence is a bin entry.** So there are not two kinds of thing to look at, only one,
  and the rule is a sentence: *a subject is a bin entry; if it is already a timeline you
  compile it, and if it is not you put it on one.* Written that way the `sequence`/
  `overlay` split folds into a single `id` and the either/or rule is deleted rather than
  generalised.

  ```rust
  pub struct Subject { id: Option<BinId>, length: Option<f64>, frame: Option<(u32, u32)>, backdrop: Option<String> }
  pub fn view(state, doc, subject) -> Result<View /* (Project, root BinId) */, SubjectError>
  ```

  `None` is the master, which is a bin entry like any other. `length` and `backdrop`
  describe the timeline *built for* an entry that has not got one, so they are refused for
  a sequence rather than ignored — two readings, so ask for one, as the old `overlay` plus
  `sequence` combination was refused. `frame` applies to either: it is the profile size,
  which the compiler already varies for the preview scale.

  Time on an entry subject is time **into the thing itself** — for media that means `t` is
  time into the file, which is what a colour-grading editor wants to scrub, and it needs no
  extra parameter because the built timeline's axis *is* the source's.

  Which track an entry goes on is the one thing that varies per kind, and that table is
  already written twice: open-coded in `validate.rs` (an overlay track takes overlay
  entries and nothing else) and as `TRACK_FOR` in the UI. It belongs in `kuma-core`, shared
  by all three — otherwise the builder can construct a document the validator would refuse.

  **Melt always**, decided rather than assumed. Sampling a transparent overlay could skip
  melt entirely, since the baked frames are already RGBA at the right size, and it was
  turned down: one rendering path is what makes "what you sample is what renders" true, and
  a second compositor would have to be kept in agreement with the first forever. No subject
  kind gets its own path, however cheap its frames look.

  **The obvious alternative — put the temporary sequence in the bin** — was considered and
  turned down, and it is worth writing down because it is the first thing anyone proposes.
  Adding a real scratch entry when a tab opens would need *no* new machinery at all:
  sampling, rendering, compiling and baking are untouched, every tab becomes a sequence
  tab, and the UI's live-DOM overlay layer works unchanged because it reads the tracks out
  of the document. That last point is the one real deficit of doing it in session.

  It breaks a rule the project is built on: looking at something is not an edit. A bin
  entry arrives by patch, and a patch writes the project file before it swaps it in memory
  (`state::commit`), pushes an undo step (`state::edit`), and bumps the revision — so
  opening a tab would save scratch to `myvideo.kuma.json`, put "opened a tab" in the undo
  stack, and hand an agent a `409` on its next `base_revision` because a human clicked
  something. `bake_all` walks every clip in every sequence, so `kuma bake` would bake
  scratch; the validator walks every sequence, so broken scratch would block unrelated
  edits. Keeping those out means a hidden-entry flag, and then the *model* knows about tab
  state — exactly what the open sequence is kept out of the document to avoid. A crash
  would leave orphans in someone's bin.

  What the session version owes in exchange is one endpoint: the resolved view (subject,
  root and its tracks) so the UI can draw a timeline that is not in the document — a
  generalisation it needs anyway. And it gains something the bin version cannot have:
  keeping a scratch arrangement becomes one deliberate patch ("promote this to a
  sequence") instead of everything you glanced at living in the file until something
  cleans it up.

  What it unlocks is the reason to do it. The daemon's session state is an open *sequence*;
  make it a subject and the engine, the snapshot fallback, sampling and rendering all point
  at the same thing — so a bin-entry editor for video (colour correction, say) gets the
  preview engine for free, and the overlay workbench's live-DOM iframe reads as what it is:
  an optimisation of one subject kind, the way live-DOM overlays are an optimisation for
  the open sequence. `render` on an entry falls out with the output naming already in
  place, which means an overlay rendered to ProRes 4444 with alpha for somebody else's
  editor.

  Three phases, only the first of which is free: `Subject` plus the resolver, with the old
  fields as sugar and no behaviour change (the existing tests are the proof, ~120 lines net
  and half of it moved); then entry subjects for media and colour entries, with a test per
  kind that the synthetic project compiles; then the session state and the UI's tab strip,
  where `state.sequences` and `state.overlays` collapse into one list of subjects.

  Invariants to keep whichever way it is built: nothing synthetic is ever stored or
  validated (it is well formed by construction and lives for one compile — the test that
  sampling an entry leaves the document byte-identical should gain a case per kind); the
  cache key covers the whole subject, because two views of one entry are two pictures; and
  a sequence subject must not be wrapped in a synthetic root, since a nested sequence's
  background is transparent and wrapping would give it one that compiling it as root does
  not.

  **Postponed until needed** — decided when bin-level volume was built without it. What
  it fixes today is an API wart: `sample_frames` has two fields and a rule refusing both,
  and that works and is tested. Its first phase changes no behaviour, so it pays nothing
  on its own; the payoff is in the second and third, and the third (session state and the
  tab strip) is the churn, in code that works. Doing it ahead of a feature means
  reshaping those without a use to check the shape against.

  The use that would justify it is a view of an entry on its own clock: a colour-grading
  or other video editor on a bin entry, rendering one entry alone (an overlay to ProRes
  4444 for somebody else's editor), sampling media by time into the file — and above all
  **keyed** bin-level effects, whose clock is exactly the "time into the thing itself"
  an entry subject defines. Static entry effects do not need it: bin volume went in as a
  field and a multiply in the compiler (see *Bin-level effects* below). When one of those
  is next, the phases above are its first step rather than a project of their own.

- **Ship a starter set of design systems** before any public release. The mechanism is in
  and nothing is in it, which makes the feature invisible to anyone who has not read the
  guide; see *Design systems* above for what such a set has to demonstrate.

- **Templates and overlays are the same word today and will not be** — decided for HTML
  overlays (see *Overlays and blueprints*); the paragraphs below describe the old names.
  What other reusable kinds are called is still open.

  Today "template" means exactly one thing: a folder with `template.json` and `index.html`
  that bakes to an overlay. That meaning is wired in everywhere — `TemplateManifest`,
  `/api/templates`, `list_templates`, `template_guide`, `kuma new-template`, `kuma lint`,
  the Templates tab — and, most consequentially, in the document: an overlay bin entry is
  `{ "template": "<id>", "vars": {…} }`, where `template` names an HTML template and
  nothing else could go there.

  The word is about to widen. Effect templates and sequence templates (a skeleton you
  stamp out — tracks, placeholder clips, an intro pattern) are both plausible next, and
  neither is HTML. When they arrive, either "template" becomes the genus and today's
  meaning becomes one kind of it, or it stays the HTML thing and something else has to be
  the genus. The first matches how people will speak; it also means `source.template` is
  misnamed the moment a second kind exists, and the format has no upgrade path, so it is a
  `PROJECT_VERSION` bump — which is the real reason to decide early.

  Two things are already tangled and worth naming before the discussion starts. **"Preset"
  is overloaded now**: `/api/presets` is export presets (a `const` table in Rust) and
  `/api/presets/effects` is effect presets (JSON on the library search path), and they
  have nothing in common but the word. And **the genus partly exists in code already**:
  `VarSpec` is shared by overlay templates and effect presets, the UI renders both with
  one `varInput`, so "an asset with typed parameters that is instantiated into a project"
  is a concept the codebase has without having named it.

  What such a discussion has to settle, beyond the word:

  - **What varies per kind.** Discovery and precedence are already per-kind directories and
    work well (`overlays/`, `presets/`, `design-systems/`) — OpenDesign's four-axis split
    is evidence for keeping them separate rather than one bag with a `kind` field.
    Instantiation is where they genuinely differ: an overlay template mints a bin entry, an
    effect preset appends effects to a clip, a sequence template mints a sequence, a design
    system is copied into the project and never referenced at all.
  - **What "check this" means per kind.** `Baker::lint` stages HTML and runs hyperframes;
    an effect template would validate against the effect schema and a sequence template
    against the model. One dispatch point, designed once, or four unrelated commands.
  - **Whether a configured overlay is a template.** *A configured overlay cannot be shared
    between projects* (below) wants to put `{ name, source: { template, vars } }` on the
    library path. That is a fourth shape — an instance, not a template — and whether it
    lives under the same roof is the same question in miniature.

- **Speed on a sequence clip**, refused today. The mechanism is known to work (see the
  spike above): write the sub-sequence to its own `.mlt` in the cache and hand `timewarp`
  that path. What makes it more than an afternoon is the lifecycle — one file per sped-up
  sequence, regenerated on every compile and at every size, since the preview compiles at
  half resolution and would need its own copy.

- **Every numeric effect parameter keyframable, through one shared type** — **built**,
  with a different shape from the one agreed here: see *Every keyframed effect uses one
  `Keyframe` type* under *Keyframes you can point at*, which says why. The rest of this
  entry is kept as the reasoning that led there. Originally: only `transform` and `volume`
  took keyframes, so a "go blurry" ramp needed the `mlt` escape hatch. The shape agreed
  then: a parameter is either a number or a list of keyed values, as an untagged enum, so
  the JSON stays backward compatible and a bare number keeps meaning exactly what it means
  now. `Fps` already uses that trick to accept several spellings.

  ```json
  { "type": "blur", "radius": 4 }
  { "type": "blur", "radius": [ { "t": 0, "v": 0 }, { "t": 2.0, "v": 20 } ] }
  ```

  Keyframes are **not exposed by default** in the inspector: a parameter stays a plain
  number field until asked to animate, so the panel does not grow an editor per field.
  One shared type rather than a `keyframes` vector per effect variant, because the latter
  is seven shapes, seven validator paths and seven inspector rows for one idea.

  Times stay in seconds and values stay numbers. An MLT animation string never enters the
  model; the `mlt` effect already passes raw properties through for anyone who wants one,
  and that is the right place for it.

  *Verified before agreeing:* the animation string form does work through MLT's avfilter
  wrapper, tested by rendering a sigma ramp over sixty frames and looking at the frames.
  Most of the machinery is also already there — the compiler writes animation strings in
  three places, the seconds-to-filter-frame helper already accounts for the dissolve
  handle offset, `check_keyframes` is shared rather than transform-specific, and the UI
  has a generic interpolation helper the live preview can reuse.

  Per effect: blur is confirmed; `color` is the same avfilter wrapper with four parameters
  and should be checked rather than assumed; `crop` is an MLT-native filter and rect
  animation is already proven on `qtblend`; `fade` is internally an animation already and
  overlaps the transform's opacity, so leave it; `blend` is a discrete mode, not
  interpolatable, and is already emitted as discrete switches with a test to that effect.
  *That test pinned a string MLT does not read*, and every blend mode rendered as Normal;
  blends are now one transition per span — see *Field report: a seamless loop* below.

- **Bin-level effects** — agreed, lower priority than the keyframe work above. Effects
  that live on the entry and apply to every placement, the way Kdenlive's clip effects do.
  The motivating case is one music file used at several points that should share a gain and
  be changed once.

  It fits the split the bin already draws. An entry effect says "this source always sounds
  or looks like this": a gain, a camera correction, a LUT. A clip effect says "this
  placement does this": a fade at this cut, a zoom here. Same shape as the decision on
  overlay vars, where editing the entry moves every placement.

  Shape: `effects` on `BinItem`, emitted before the clip's own on the same producer, since
  entry effects are source correction and should sit underneath. Composition is already
  established on the audio side, where a clip's volume and a keyframed volume effect
  already multiply.

  Two things to settle first. The duplicate-kind check runs per clip today, so an entry
  volume plus a clip volume would stack rather than shadow, and stacking volume filters
  multiplies the gain — decide whether that is intended, as it is in Kdenlive, or rejected.
  More importantly, **a keyframed entry effect has no unambiguous clock**: clip effects
  keyframe against clip-relative time, but an entry placed twice at different lengths has
  no single answer. Either restrict entry effects to static parameters, or define their
  keyframes as source-relative, which only means anything for video and audio entries.

  If only the audio case matters, the cheap version is a gain field on file entries and no
  general mechanism.

  **The audio case is built that way** (format version 6): `volume` on `BinItem`, linear,
  absent at 1, multiplied into each placement's own level in the volume filter the clip
  already had. Timed media only (`is_timed_media`): on a picture it is an error. It stacks
  with the clip's volume, as Kdenlive's does. General and keyed entry effects stay open.

- **Nothing in `.kuma/` is ever cleaned up** — **built**: see *Pruning the cache* above.
- **A configured overlay cannot be shared between projects** — addressed by **Save as
  blueprint**; the reasoning as it stood follows. The template travels in the
  library and its manifest defaults with it, but an entry's var overrides live in the
  project file, so a house lower third styled once has to be rebuilt in the next project.
  The fix is small because a saved configuration *is* a bin entry with the project
  removed, `{ name, source: { template, vars } }`, so putting it on the library search
  path next to effect presets makes adding one a copy rather than new machinery. Copying
  is the point, not a shortcut: vars are project-owned editing decisions, so they must
  stay a revision-checked, undoable patch rather than a path to a file the daemon does
  not own and other projects also read. Templates are live-referenced by id instead
  because the author owns markup while the editor owns values; a shared *theme* whose
  changes do propagate is a separate feature, and merging it into saved configurations
  would re-render finished projects. Forking
  the template and baking the values into its defaults works today but forks markup for
  no reason and stops improvements reaching the copy. Same shape as the media search path
  below: the library wants to hold more kinds of thing than templates, and the two should
  be one mechanism.
- A marquee selection does not exist: a drag on an empty lane scrubs. Several clips are
  built up one Ctrl-click at a time.
- A group paste keeps the transitions *inside* it no better than a single paste does: it
  keeps none. A transition is a relation between two clips on one track, and copying it
  means copying it only when both ends came along — worth doing, and the same bookkeeping
  "make a sequence from the selected clips" (below) needs.
- Folders and search in the browser, now that the bin is the thing they organise. The bin
  now holds sequences too, which makes this more pressing than it was.
- A thumbnail for a sequence entry. The `thumbs` job renders overlay entries with their
  own vars; a sequence would want a frame of itself, which is `sample_frames` rather than
  a bake, so it is a different path and was left alone.
- "Make a sequence from the selected clips", the Kdenlive move that collapses part of a
  timeline into a new sequence in place. Everything under it exists — a sequence is one
  patch — but it is a UI affordance nobody has written.
- A re-probe or reload action on a bin entry.
- `kuma add-media` resolves a relative path against the shell's directory rather than the
  project's, which surprises when run from elsewhere.
- Overlays bake at full frame size; there is no per-overlay bake region.
- Export presets are a `const` table in Rust and are not pluggable. Effect presets, being
  JSON on the library search path, already are.
