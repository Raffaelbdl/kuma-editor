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
through the ordinary path — same bake, same compiler, same melt — so the frames are the
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

---

## Still open

Nested sequences are done and the editor is an application now, so this list is the whole
of what is next on the *model and compiler*; the desktop shell's own loose ends are at the
end of its section above. The item this list gained from the sequence work comes first,
because it is the part deliberately left out.

- **Speed on a sequence clip**, refused today. The mechanism is known to work (see the
  spike above): write the sub-sequence to its own `.mlt` in the cache and hand `timewarp`
  that path. What makes it more than an afternoon is the lifecycle — one file per sped-up
  sequence, regenerated on every compile and at every size, since the preview compiles at
  half resolution and would need its own copy.

- **Every numeric effect parameter keyframable, through one shared type** — agreed, not
  built. Today only `transform` and `volume` take keyframes, so a "go blurry" ramp needs
  the `mlt` escape hatch. The shape agreed: a parameter is either a number or a list of
  keyed values, as an untagged enum, so the JSON stays backward compatible and a bare
  number keeps meaning exactly what it means now. `Fps` already uses that trick to accept
  several spellings.

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

- **Defect: the probe reports the wrong frame rate for variable-rate sources.** It records
  ffprobe's `r_frame_rate`, which for a variable-rate file is the smallest rate that can
  express every timestamp rather than the actual one. A test file with real frame deltas
  of 100/67/33 ms, averaging just under 12 fps, reports 30. Informational only, since the
  probed rate never reaches the compiler, but it is what the bin shows and what an agent
  reads from `get_project`. `avg_frame_rate` is the honest number, and storing both would
  let the UI say "variable" when they diverge.

  *Rendering itself is fine, and was checked rather than assumed:* times are seconds and
  become frames once, at the project rate, so nothing assumes the source has a grid. A
  clip trimmed two seconds into that variable-rate file sampled pixel-identical to
  ffmpeg's own extraction at the same source times. Audio drift over a long recording is
  untested.

- **Defect: blur is horizontal only.** The compiler sets `av.sigma` and not `av.sigmaV`,
  and MLT does not mirror one into the other, so a blurred clip keeps perfectly sharp
  horizontal edges. Confirmed by rendering colour bars with and without the vertical
  sigma. One line in `emit_filters`; not yet fixed.

- **The bake cache grows forever and nothing prunes it.** A successful bake only ever
  adds a directory. The only deletions anywhere in the bake path are the scratch frames
  inside one bake and the drop guard that cleans up after a failure, so every superseded
  entry stays complete and unreachable. Keys are content hashes covering the template
  folder, the vendor folder, the resolved vars, and the clip's duration and speed, which
  means trimming a clip creates a new entry just as surely as editing markup does. The
  frames are full-resolution PNG sequences, so a few seconds of 1080p overlay costs tens
  of megabytes per superseded version. Fine for a proof of concept, not fine for a project
  someone keeps for months.

  One accidental mitigation: because keys are content hashes rather than a sequence,
  reverting a template to an earlier state hits that entry again instead of baking a third
  copy, so switching back and forth between two versions costs two entries rather than one
  per switch.

  The fix when it is wanted is a prune that recomputes the live key set from the project
  and deletes everything else. That needs no rendering, because the key function is pure
  and can be evaluated for every overlay entry directly. A size or age cap is the lazier
  version and is worse: it can evict the entry for a clip that is about to be rendered.

- **A configured overlay cannot be shared between projects.** The template travels in the
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
- Overlays bake at full frame size; there is no per-template bake region.
- Export presets are a `const` table in Rust and are not pluggable. Effect presets, being
  JSON on the library search path, already are.
