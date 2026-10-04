# kuma-editor — the manual

The full reference: every editor feature, the CLI, the project file, the MCP tools and
troubleshooting. The short version is the [README](../README.md).

A hybrid video editor for people who animate in HTML and edit in a real NLE.

- **Video half:** [MLT](https://www.mltframework.org/) — the engine under Kdenlive and
  Shotcut. Cuts, transforms, crops, speed, dissolves/wipes, colour, blur, audio mixing,
  hundreds of filters, and an export that Kdenlive can open (`.mlt` *is* the Kdenlive
  format).
- **Overlay half:** HTML/CSS/TS overlays (labels, tags, callouts, animated explanations)
  rendered as **live DOM** while you scrub, and **baked to alpha video** by
  [hyperframes](https://github.com/heygen-com/hyperframes) at export — any length, any
  resolution, supersampled, deterministic.
- **Agent half:** a JSON project file with a schema, and an MCP server so Claude Code can
  read the timeline, edit it with revision-checked patches, author overlays, and
  **look at the actual composited frames** (`sample_frames`).

Status: proof of concept. Linux only. See "What works" below before relying on anything.

## Why

Authoring animations as HTML is the right medium, but capturing them in a browser tab caps
you at ~20 s, samples at screen resolution and can't emit alpha; and every iteration means
a manual export/import round trip into Kdenlive. kuma removes the seam: one timeline, one
project file, one preview, and the overlay is never "exported" by hand.

## Architecture

```
  desktop app (Electron) ── starts and stops one daemon per open project
        │  launcher → window
        ▼
                      ┌──────────────── kuma daemon (Rust, axum) ────────────────┐
  editor UI   ◄─WS/HTTP─►  project state · revisions · undo · jobs · blueprints   │
  (TS, Vite)          │       │                │                    │             │
                      │  kuma-mlt          kuma-overlay        preview engine     │
  Claude Code ◄─stdio─►  JSON → MLT XML    hyperframes bake     (C++ libmlt++)    │
  (MCP, rmcp)         │       │            PNG seq w/ alpha      JPEG frames       │
                      └───────┼──────────────────┼────────────────────┼───────────┘
                           melt (render)     Chrome headless      SDL2 audio clock
```

The same UI runs in the desktop app and in a plain browser against `kuma serve`; the app
adds a launcher, native menus and a file picker, and owns the daemon's lifetime.

Same HTML, two consumers:

```
overlay.html ──seek(t)──┬── preview: live iframe in the editor (GPU, zero latency)
                        └── export:  hyperframes → RGBA PNG sequence → MLT producer
```

Details and the reasoning behind each choice are in `docs/plan.md`.

## Quick start

System packages (Debian/Ubuntu):

```
sudo apt install ffmpeg melt libmlt-dev libmlt++-dev libsdl2-dev libturbojpeg0-dev cmake xvfb
```

Subtitles need a local speech-to-text model, [whisper.cpp](https://github.com/ggml-org/whisper.cpp).
`npm run rebuild` clones and builds it under `whisper.cpp/` and downloads a ggml model
into `~/.local/share/kuma/models/` (`WHISPER_MODEL=small npm run rebuild` for another size;
`base` is the default, `.en` variants are English-only). To use your own, put `whisper-cli`
on `PATH` (or `KUMA_WHISPER_BIN`) and a model in that folder (or `KUMA_WHISPER_MODEL`).
`kuma doctor` reports both.

Build:

```
npm run rebuild                              # everything, in order, then `kuma doctor`
npm test                                     # every suite: Rust, the desktop shell, the UI
```

Three of the artifacts kuma runs from are not in git — the `kuma` binary, `ui/dist` (which
the daemon serves) and `app/dist` (the desktop shell) — so **run this after every pull**.
A stale one fails far from its cause: an old binary writes projects the editor cannot
open, an old `ui/dist` dies on the shape of a new one. Step by step, if you prefer:

```
npm install                                  # hyperframes + gsap
cargo build --release
cmake -S engine -B engine/build && cmake --build engine/build -j
(cd ui && npm install && npm run build)
```

The preview engine and whisper.cpp are the optional pieces; `npm run rebuild` reports
them and carries on if a system library or a download is missing.

`cargo build --release` produces the `kuma` binary at `target/release/kuma` (it is the
`crates/kuma-cli` crate). Either run it from there or install it on your `PATH`:

```
cargo install --path crates/kuma-cli
```

`kuma` needs the checkout for `node_modules/hyperframes`, `ui/dist` and
`engine/build/kuma-preview`. It finds it by walking up from the current directory or from
the executable's location, so from `target/release/` nothing is needed; if you installed
it into `~/.cargo/bin`, tell it where the repository is once:

```
export KUMA_REPO=/path/to/kuma-editor        # or pass --repo <path> on each call
```

Failing those, it reads the `KUMA_REPO` that `kuma mcp-config` (or `kuma init`) wrote into
the `.mcp.json` beside the project file, so the CLI finds what the project's MCP server is
told; and last, the source tree the binary was built from, while that still has its
`node_modules`. `kuma doctor` says where it found it.

Blueprints (reusable overlays), effect presets, design systems and fonts are **assets shared
across projects**, not part of the codebase: they live in a user library at
`~/.local/share/kuma/{blueprints,presets,design-systems,fonts}`
(`$XDG_DATA_HOME/kuma`), plus any directories in `KUMA_LIBRARY` (a `:`-separated path
list); presets are also read from a project's own `presets/`. Later entries win on an id
clash. The repository ships a starter set under `library/`. Until a user library has
blueprints of its own, and when `KUMA_LIBRARY` is unset, every daemon reads that starter set
from the checkout (the desktop app says so in its launcher); install it once to make it yours:

```
kuma library                                 # copies library/{blueprints,presets,design-systems,fonts} → ~/.local/share/kuma; adds what is missing (--force to overwrite)
kuma thumbs                                  # a still and a moving preview (animated WebP, alpha) per blueprint
```

A project is a directory with `<name>.kuma.json` and your media:

```
mkdir myvideo && cd myvideo
kuma init --project myvideo.kuma.json --fps 30          # or 29.97, 30000/1001; --width 1080 --height 1920 for a Short
kuma add-media media/screen.mp4
kuma serve                                               # open http://127.0.0.1:7455 (--bind / KUMA_BIND to change)
```

Drag bin entries onto the timeline, scrub, edit vars in the inspector, press **Render**.
Tracks are drawn overlays on top, then video, then audio at the bottom — how they
composite, rather than the order the document happens to store them in. `←`/`→` step a
frame, `Shift` makes it a second, and `Alt` jumps to the next **cut** — the start or end of
any clip on the sequence, whichever track it is on.
Output lands in `renders/`. Render means the **master** — the sequence `master` names, which
is the whole video; while you are editing another sequence a picker appears beside the
export preset to render that one instead, and it lands beside the other as
`<name>-<sequence>-<preset>.<ext>`. Overlays are iterated on in their own tab — see *The overlay
workbench* below — rather than by scrubbing the timeline to where they happen to be.

**Copy, cut and paste.** `Ctrl/Cmd+C`, `Ctrl/Cmd+X` and `Ctrl/Cmd+V` — also the Project
menu in the app — move clips around a timeline, or onto another one. A paste of **one** clip needs a *where* as well as a *when*: the playhead
answers the second half and **clicking a track** answers the first, so a clip copied from
*Video* lands on *Video 2* by clicking *Video 2* and pasting. Any track of the kind it was
copied from will take it, in this sequence or in another one you switch to first; with no
track clicked it goes back on the track it came from, or on the first of that kind. Where the playhead sits inside a clip the
paste lands at the first free slot after it that is long enough, so pasting twice lays two
clips end to end.

**Ctrl/Cmd+click** (or Shift+click) a clip to select more than one. Several clips are a
*shape* rather than a position: they keep their spacing and they keep their own tracks, so
the clicked track does not come into it, and the group moves together to the first place
all of it fits. Each track they came from is matched by id in the sequence being pasted
into, else by kind, and a track is added when there is nothing to match — two of them
never collapse onto one. Delete and `S` (split at the playhead) apply to the whole
selection too, each in a single undoable patch; the transport strip says how many are
selected, and dragging and the inspector's fields are still one clip at a time, the last
one you clicked.

What travels is the placement — `in`, speed, volume, label, effects — never the bin entry:
both clips play the same entry, which is what the model's split already means.

**Right-click is where the timeline's actions are.** On a clip: split it at the pointer
(`S` splits at the playhead), add a transition into the clip it touches, copy, cut, group,
delete. On the ruler: a marker at that moment (`M` at the playhead). A **transition** joins
two clips that touch on one track and is centred on the cut, so it needs *handles* —
material after the first clip's out point and before the second's in point; "Transition
into the next clip" makes a one-second dissolve, or as long as the handles allow, and says
which clip to trim when there are none. The clips stay end to end in the project; the transition is drawn hatched across the cut, over the handle frames it plays (red, black at the end, when the first clip runs out of source). Click it to edit it in the inspector — kind, length, a wipe's pattern and softness, and how much handle each side needs and has; double-click
it for its length, right-click to make it a wipe or remove it.

**What a clip is shows on it.** A media clip carries frames of its file along the top and
its waveform along the bottom — one clip, picture and sound, drawn in two bands rather than
split into a video and an audio track, so it stays one thing to move for you and for an
agent. Both are made by the daemon on first sight (`/api/bin/{id}/strip.json`,
`peaks.json`) and cached in `.kuma/thumbs/`. A track's height is dragged at the bottom edge
of its head, and its caret folds it to one line of kind and name; double-click a track's name (or right-click its head) to rename it; `Ctrl`+wheel zooms around
the pointer. The bin, the inspector and the timeline are sized by dragging the seams
between them (double-click one to reset). Heights, folds and sizes are this browser's view
state, never the document. Tabs close with a middle click.

**The inspector is a game engine's.** Every number in it is a field you **drag**: press and
pull sideways to change it, `Shift` for a tenth of a step, `Alt` for ten, or click once and
type. A drag is one edit — the field moves while you hold it and the document changes when
you let go — so scrubbing a value costs one revision and one undo step, not one per pixel.
Values that go together are one row wearing their axis colours (a transform's **X** red and
**Y** green, a crop's four edges), and a value with a useful range gets a slider beside its
field. The shapes live in `ui/src/props.ts`, so an effect, an overlay var and a clip's own
fields are edited the same way; an effect declares which shape each of its fields is, in
one table in `ui/src/panels/inspector.ts`, and the bounds there are the model's own, so the
panel cannot offer a value the daemon would refuse.

**Effects are offered where they can act.** Each effect acts on the picture (transform,
crop, blur, colour, blend, fade) or the sound (volume, audio fade), so `+ add effect` lists
only what the selected clip can use: a colour or a still is not offered a volume, a song is not
offered a picture fade. An effect that got there anyway — by an agent's patch, or on a clip
moved to an audio track — stays, dimmed, and says *no effect here*; the validator warns too.

**Keyframes are pointed at, not typed.** Every effect with numbers to animate — transform,
crop, blur, colour, volume — starts as plain numbers, with an outlined **◇** in its head;
press it and it gains one key at the playhead holding
what it already does — nothing moves until you move something. From then on the fields are
the value **at the playhead**: scrub, change a number, and the key under the playhead takes
it, or a new one is made there carrying every other value as it already was. A strip under
the effect shows where the keys are (click to go to one, drag to retime, double-click to
add), `‹ ›` step between them, `+ key` / `− key` add and remove one where you are, and
the diamond, filled **◆** now, stops animating while keeping the value you are looking at. The same keys are
drawn along the bottom of the clip in the timeline, so an animated clip says so at a glance
and you watch them go past while scrubbing — and can be dragged there too, or clicked to
jump to. **Auto-key** (`R`, a red edge on the inspector while it is on) is record mode:
while it is on, changing any animatable value writes a key at the playhead, starting the
animation if there was none. **curve** in the keys row opens a graph of one parameter over the clip, the shape
between keys drawn as the render will produce it, with the keys as points you drag in both
axes at once. With the playhead outside the clip there is no
"now" to write to, so the fields read the nearest end and say so rather than pretending.

**The overlay workbench.** The centre of the window is tabbed, Kdenlive style. A
**sequence** tab is a timeline you have open — the engine's picture with that sequence's
overlays live on top, driven by the timeline below; the topbar picker opens one and gives
it a tab, and clicking between them switches which sequence is being edited (a round trip:
the daemon holds that choice, so switching recompiles, and the last tab stays put because
something is always open). Every other tab is one **overlay entry**, open on its own. `+ overlay` (or the
Blueprints tab's *Workbench*, or **New Overlay…** in the Project menu) copies a
blueprint into the project, adds it to the bin and opens it there **without placing it**
— double-clicking an overlay in the bin opens it the same way: creating an overlay and
deciding where it plays are two decisions, and the workbench is for the first one.

Inside it the overlay runs on its own clock — play, scrub and frame-step with `Space` and
the arrows, looping, with no relation to the timeline's playhead — over a backdrop you
pick (alpha as a checkerboard, dark, light, or the sequence's frame at the playhead), at a
length you set, and in another frame if you want one: `9:16` renders it at 1080×1920
without touching the project, which is how you check the responsiveness the overlay
contract asks for. Its vars are edited where every bin entry is edited, in the inspector
on the right. **Reload** re-reads the overlay's folder and rebuilds the document, so an
overlay being written in an editor is one click from being on screen; **Duplicate** adds a
second entry on the same folder so a variant wording costs nothing; **Place at playhead** puts a clip of it on the open
sequence at that length, when it is ready.

An agent gets the same view: `sample_frames` with `overlay` set to the entry's id (or
`kuma sample 0 2 --count 5 --overlay paris`) renders that entry on its own, over a
backdrop, with `t0`/`t1` counted from the start of the overlay — so an animation can be
checked frame by frame before anything is placed. Nothing is added to the document to make
that possible: the frames come from a throwaway one-clip timeline through the real MLT
graph, so they are the composite the render would produce.

**Vertical.** Landscape or portrait is nothing but the project's `width`/`height`: the
size button in the top bar (`1920×1080 ⇄`) swaps them, undoably, and the preview, the
overlays and every export preset follow. Overlays size in `vh`, so they need no edits;
pixel-valued `transform` offsets are the one thing that stays put.

**Subtitles.** Select a clip and press **Transcribe → subtitles** (or `kuma transcribe
--clip <id>`, or the `transcribe` tool with `clip`). A dialog asks the **language** — detect
it, or force one, and your choice is remembered for next time — and, when that media has
been transcribed before, offers to **replace** that transcript: the same entry keeps its
styling and where it is placed, and only the words change. Leave it unticked for a second
transcript beside the first, which is also what a second `kuma transcribe` does: an
unnamed transcript is named when its words are in (`<bin>-subs`, then `<bin>-subs-2`…), so
runs never collide and an edit made while the model works cannot cost you the
transcription. whisper.cpp runs locally, the
transcript lands in the bin as a `subtitles` overlay entry listed under its media, and a
clip of it is placed over the one you selected, aligned: same start, end and speed, and the
entry's `offset` set to the clip's `in`, so the cues land on the words. It goes on the
first overlay track with room there, else on a new one. The same button on a media entry
in the bin makes the entry alone, for you to place. Its `cues` var is the transcript,
`[{start, end, text}]` in source seconds, **one cue per word**: the finest timing whisper
gives, and the form any overlay can group as it likes. The shipped `subtitles` blueprint groups them
by `words_per_line` and can stress the word being said (`stress`: colour or a pop, in the
`accent` colour), so the karaoke style is one var away and line length is editable live.
The inspector shows one row per cue, so a misheard word is fixed in place and the preview
follows as you type. `max_chars` on the job asks whisper for lines of that many characters
instead, or 0 for its own sentences; those cues are already lines, so set `words_per_line`
to 1.

**Themes.** The palette button in the top bar (or *View → Theme…* in the app) swaps the
editor's colours: **Kuma**, the default, and **Tokyo Night**, retro neon on deep indigo.
The choice lives in the browser, not the project, and stops at the editor's chrome — no
overlay, bake or render sees it, so the video looks the same whichever one is on. Styling
the *video* is what a design system is for, below. A new theme is a block of tokens in
`ui/src/themes.css` plus a line in `ui/src/theme.ts`; a test fails if either half is missing.

### Desktop app

`kuma serve` plus a browser tab is still the whole editor. The desktop app is the same UI
in a window, with the things a tab cannot give you: a launcher, an application menu, a
native file picker, file drops, and a daemon that starts and stops with the window.

```
npm run app                                  # installs its own deps on first run, builds, starts
```

It opens a **launcher**: recent projects on the right, *New project* and *Open project…*
on the left. You can also drop a `.kuma.json` on the window, or pass one on the command
line. *New project* asks for a name, a location, a resolution and a frame rate, then runs
`kuma init` — so a project created here is identical to one created from the CLI, down to
the `.mcp.json` that lets Claude Code join it.

Opening a project starts a daemon for it and shows the editor. Closing the window stops
that daemon; quitting stops all of them. Each project gets its own daemon and its own
window, so several can be open at once. The first one takes `127.0.0.1:7455`, which is
where `kuma mcp` looks — so Claude Code attaches to the project you have open without any
configuration.

The one daemon the app does *not* stop is one that was already serving that project file
when you opened it. Two daemons writing one `.kuma.json` would each think they own it, and
the one already there is usually a `kuma mcp` with a Claude Code session behind it, so the
app shares it instead and leaves it running.

The shell needs to find the `kuma` binary and the checkout. It looks for `KUMA_BIN`, then
`target/release/kuma`, then `target/debug/kuma`, then `PATH`; and for the repository the
same way `kuma` itself does (`KUMA_REPO`, else the nearest ancestor with
`node_modules/hyperframes`). The launcher's footer says what it found and what is missing.

Everything the app adds to the editor page sits behind one check in `ui/src/desktop.ts`,
so the UI keeps working unchanged in an ordinary browser.

### Claude Code

`kuma init` writes a **project-scoped** `.mcp.json` next to the project file, so running
`claude` in that directory offers the kuma server (Claude Code asks once to approve it).
Nothing is installed globally. To (re)generate it for an existing project, or to see what
it contains:

```
kuma mcp-config            # writes .mcp.json next to the project file (merges into an existing one)
kuma mcp-config --print
```

Both also install the **`kuma-video` skill** into `.claude/skills/kuma-video/`: how to make
a video through the MCP, from a screen recording to a motion-graphics explainer (footage,
overlays, loudness, pacing, working as code, CJK type, review), with scripts that check
what kuma does not (`reading_audit.py`, `contact_sheet.py` of a rendered file, …). Its source is `library/skills/kuma-video/`; the
binary embeds it, so a project gets the version that documents that `kuma`, and
`kuma mcp-config` refreshes it (other files in that folder are left alone). `kuma doctor`
says whether a project's copy is current.

The equivalent by hand, for any of Claude Code's scopes:

```
claude mcp add --scope project kuma -- kuma mcp --project myvideo.kuma.json   # .mcp.json, shareable
claude mcp add --scope local   kuma -- kuma mcp --project myvideo.kuma.json   # this directory, you only
claude mcp add --scope user    kuma -- kuma mcp --project /abs/path.kuma.json  # everywhere
```

`kuma mcp` starts the daemon in-process if none is running on the port, so the browser UI
and the agent always share one state. Tools: `timeline_guide`, `get_project`, `get_schema`, `apply_edits`,
`undo`, `redo`, `validate_project`, `list_presets`, `add_media`, `probe_media`,
`transcribe`, **`sample_frames`**, `measure_audio`, `bake_overlay`, `render`, `job_status`, `cancel_job`,
`prune_cache`, `health`, and for overlays `overlay_guide`, `new_overlay`, `check_overlay`,
`import_overlay`, `make_overlay_unique`, `list_blueprints`, `add_blueprint`,
`save_blueprint`. An agent scaffolds an overlay with `new_overlay`, checks it with
`check_overlay` and adds it with `import_overlay`; `sample_frames` with `overlay` (a bin id)
shows one entry on its own, and with `times` and `tile` a contact sheet of chosen moments.
`apply_edits` takes a patch inline or, with `patch_path`, from a file in the project, so a
script's patch of any size is one exact edit. A script that diffs its own document against
the stored one puts its own in kuma's form first — `kuma canonical plan.json`, or
`POST /api/project/canonical` to the daemon, which answers the text the file would hold —
since kuma leaves out every field at its default (a keyframe's `ease: "linear"`). `measure_audio` meters a sequence's sound (EBU
R128, per chapter, loudest moments, silences), rendered through the real graph without
baking anything. The tools that start a job (`render`, `measure_audio`, `transcribe`,
`bake_overlay`, `sample_frames`) wait `wait_seconds` and then return it still running, with
its id; `job_status` with that id and its own `wait_seconds` waits again, in calls as short
as the client needs (Claude Code moves a call that runs past 120 s to the background), and
without an id lists the jobs. While a tool waits it sends MCP progress notifications when the
call carries a progress token: the job's percent and message, and a heartbeat every 20 s, so
a client that gives up on a silent call does not. A server that outlives a rebuild of `kuma` says so on every call, since its
tools are the old build's. `overlay_guide` returns `library/blueprints/CLAUDE.md`, and
`timeline_guide` returns `crates/kuma-mcp/src/timeline_guide.md`: the server's own
instructions are kept under the 2048 characters a client shows and point at both.

### CLI

```
kuma render --preset youtube-1080p         # bakes overlays, compiles, renders with melt
kuma sample 12.0 14.0 --count 4 --rect 400,300,800,400
kuma sample 0 3 --count 6 --overlay paris          # one overlay entry on its own, no placement (--backdrop, --duration, --frame 1080x1920)
kuma sample --times 3.2,3.8,41.7,42.3 --tile      # chosen moments, and one captioned contact sheet of them
kuma sample 0 60 --count 12 --no-overlays          # footage only: no overlay is baked first
kuma measure-audio                         # loudness (LUFS), true peak, per chapter, loudest moments, silences
kuma bake                                  # bake all overlay clips (cached by content)
kuma cache                                 # how big .kuma/ is: bakes, sampled frames, thumbnails
kuma cache prune                           # delete what the project no longer uses (--all: every bake and frame)
kuma compile > timeline.mlt                # inspect the MLT graph (or open it in Kdenlive); baked overlays included, --bake bakes the rest
kuma render --sequence intro               # render one nested sequence instead of the master
kuma transcribe --clip c1 --language fr    # whisper.cpp → `subtitles` entry under the media, placed over clip c1
kuma transcribe screen --replace           # re-transcribe in place: same entry, styling and placement, new words
kuma new-overlay my-tag                    # a contract-abiding skeleton in the project's overlays/ (--blueprint for the user library)
kuma import-overlay overlays/my-tag        # add that folder to the bin (--id)
kuma lint                                  # every blueprint and every folder the bin uses (exit 1 on errors); `kuma lint overlays/my-tag` or `kuma lint location-tag` for one
kuma validate | thumbs | library | doctor  # validate also names text the entries' fonts will not draw
kuma canonical plan.json                   # a project document (any version; `-` for stdin) in the form kuma stores it, to diff against
```

## The project file

```json
{
  "fps": { "num": 30000, "den": 1001 }, "width": 1920, "height": 1080,
  "master": "main",
  "bin": {
    "screen": { "name": "Screen recording", "source": { "path": "media/screen.mp4", "kind": "video" } },
    "paris":  { "name": "Paris tag", "source": { "overlay": "overlays/location-tag", "vars": { "label": "Paris" } } },
    "navy":   { "name": "Navy card", "source": { "color": "#102030" } },

    "intro":  { "name": "Intro", "source": { "tracks": [
      { "id": "iv1", "kind": "video", "clips": [
        { "id": "i1", "start": 0, "end": 6, "bin": "screen", "in": 20 } ] } ] } },

    "main":   { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "clips": [
        { "id": "s1", "start": 0, "end": 6, "bin": "intro" },
        { "id": "c1", "start": 6, "end": 18.5, "bin": "screen", "in": 3.2,
          "effects": [
            { "type": "crop", "left": 0.05, "right": 0.05 },
            { "type": "transform", "keyframes": [ { "t": 0, "scale": 1 }, { "t": 3, "scale": 1.4, "x": -120 } ] } ] } ] },
      { "id": "o1", "kind": "overlay", "clips": [
        { "id": "o1c1", "start": 2, "end": 8, "bin": "paris",
          "speed": 0.5, "effects": [ { "type": "blend", "mode": "screen" } ] } ] }
    ] } }
  }
}
```

**The bin holds everything placeable** — media files, configured overlays, colours and
*sequences* — each with an id, a name, optional probed metadata and an optional `parent`:
the entry it is listed under in the browser (a transcript under its media, anything in a
folder). A **folder** is an entry with nothing but a name, `{ "source": { "folder": true } }`,
and can never be placed. That is organisation only; ids stay the identity, so moving an
entry is one field. An optional `order` sorts siblings (ordered ones first, the rest by
id, so new entries land at the bottom). In the editor, `+ folder` adds one; drag a row onto
the middle of a folder to move it there, onto the upper or lower part of any row to put it
before or after that row, or onto the list's empty space to send it last at the top level;
the caret folds a folder. In the desktop app, files dropped anywhere in the window join
the bin, or the folder they were dropped on; an `overlay.json` is imported as an overlay, its folder copied into `overlays/` if it is not there already. A clip is a
*placement*: it says which entry, when, how fast and with which effects, and never carries
a source of its own. So an overlay's text lives on its bin entry and every placement of it
follows; a second wording is a second entry. Overlay clips take the same effects as video
clips.

**A timeline is a bin entry too.** There is no project-level `tracks`: a sequence entry
holds the tracks, and `master` names the one that gets rendered. A sequence placed on
another sequence is a nested sequence — edit a part separately, place it in the final cut,
trim into it and put effects on this placement without touching the other. The editor
opens one sequence at a time (the picker in the topbar); the preview compiles whichever is
open, since the engine loads one timeline. Overlays on that sequence stay live DOM so edits
show as you type; overlays inside the sequences it nests are baked into the preview, which
is content you are not editing and is what the render will show. A sequence cannot contain itself, and cannot be
sped up — MLT's `timewarp` takes a resource path, not a tractor, so render the part out
and place the result as media.

Seconds everywhere, `end` exclusive, fps rational, tracks bottom-up, `in` meaningful for
video, audio and sequence entries. Track and clip ids are unique across the whole project,
not per sequence. The full schema: `GET /api/schema` or the `get_schema` tool.

## Overlays and blueprints

An **overlay** is a project folder, `overlays/<name>/` with `index.html` + `overlay.json`;
a bin entry names it (`"overlay": "overlays/<name>"`) with its own vars, which the manifest
types for the inspector. An agent writes one and imports it; entries may share a folder, and
**Make unique** gives one its own copy.

A **blueprint** is the same shape in `~/.local/share/kuma/blueprints/<id>/`. Using one from
the **Blueprints** tab **copies** it into the project, so a finished video never changes
because a blueprint did; **Save as blueprint** in the inspector copies an entry back, its
vars as defaults. **Preview** in the inspector (or `kuma thumbs`) renders a still and a looping animated
WebP of each blueprint and overlay entry. Starter set (`library/blueprints/`, installed by
`kuma library`, which only adds): `title` and `title-subtitle` (classic titles, every look a
var), `location-tag`, `lower-third`, `callout-arrow`, `paint-wipe` (a hand-drawn transition,
drawn on a canvas with `_vendor/kuma-canvas.js` and Rough.js),
`label-pin`, `section-title`, `countdown`, `watermark` (static), `subtitles` (a `cues` var;
transcripts share one copy of it). Contract: `library/blueprints/CLAUDE.md`.

**Titles** are made from the bin, the way Kdenlive's *Add Title Clip* is: **+ New** (or a
right-click in the bin, which creates inside the folder under the pointer) lists every
blueprint whose manifest has a `create` label — *Title*, *Title + subtitle* — and makes a new
entry of one, opened on the workbench with its vars in the inspector. Each is its own copy.
The inspector shows a manifest's vars in the order written, in foldable sections (`group`),
hiding a control while its `when` does not hold.

**Fonts** are what a `font` var names: a family, found in the project's `fonts/` (which wins)
or a library's `fonts/`. kuma reads the family, weights and italic from the files and writes
the `@font-face` rules, and the bake stages exactly the files it draws with. The starter set is
twelve open-licence families (`library/fonts/`, OFL/Apache/UFL licences beside each). The **+** by a
font picker adds `.ttf`/`.otf` files by path (the app's file picker, or a typed path in a
browser tab), to this project or to every project; agents use `add_font`. A `.ttc`/`.otc`
collection, which is how most CJK fonts ship, is split into one file per face as it is added,
and a `.woff`/`.woff2` web font is unwrapped into the `.ttf`/`.otf` it holds (`wuff`, a pure-Rust
port of Google's decoder), so a font folder only ever holds those; one copied in by hand is not
read, and `check_overlay` says so where the family is missing.
Each family reports the writing systems it sets (`scripts`: latin, han, kana, hangul, …): text
in any other is drawn by whatever fonts the baking machine has. Which forms kanji take, and
which font a missing character falls back to, follow the page's `lang`: an overlay that
declares a `lang` var (the `title`, `title-subtitle` and `subtitles` blueprints do) gets the
entry's value written onto `<html lang>` in the bake, the check and the preview alike, so one
folder serves entries in several languages.

Effect presets are `<library>/presets/*.json` (Ken Burns, zoom to region, vignette,
picture-in-picture, dim) and expand to ordinary effects; `$expr` values may use the
project's `width`/`height`.

## Design systems

A design system is **a document, copied into the project**, that says what this video's
overlays look like: colour roles, type sizes in `u`, geometry, motion, and the legibility
floors that footage imposes. It is how six overlays made at different times end up looking
like one video.

```
kuma design-system                          # what is installed
kuma design-system <id>                     # copy it to <project>/design-system/ (--force to replace)
```

`design-system/DESIGN.md` is what an agent reads before authoring or restyling an overlay;
`design-system/tokens.css` is the same values as a `:root { --ds-… }` block to copy into a
overlay's first `<style>`. **Nothing is injected at runtime** — applying a design system
is an edit to the overlay or to an entry's vars, so an overlay stays readable on its own
and the values live with the rest of the project's decisions.

**None ship yet** — the mechanism is here and the starter set is not, so
`kuma design-system` lists whatever you have written or installed yourself.
`library/design-systems/CLAUDE.md` is the guide: two files, `DESIGN.md` for the prose and
`tokens.css` for the same values as a `:root` block.

## What works (verified in this repo's tests and smoke runs)

- 60 s 1080p30 overlay bake to ProRes 4444 / PNG sequence with exact straight alpha;
  4K supersampling; byte-identical repeat bakes with one worker.
- Compiler: dissolves (A/B lanes + luma), wipes (generated PGM lumas), timewarp speed,
  crop, transform, blur, colour correction and volume — each static or keyframed, linear,
  held or smooth per key — picture and sound fades, blend modes (discrete keyframes on `qtblend`),
  solid-colour clips, PiP stills, overlay PNG sequences; golden-file + DTD + real `melt` render tests.
- Daemon + MCP end to end: patch/undo/409 on stale revisions, `sample_frames` returning
  image blocks, background render jobs.
- Preview engine: seek/play at ~23 fps for a 960×540 composite on 4 CPU cores, audio as
  the clock; browser UI showing engine frames under live overlay iframes at the same
  playhead.

## Troubleshooting

- **The preview looks stale or an overlay is missing from it?** Press the top bar's reload
  button (the circular arrow). It hands the engine the timeline again even if nothing in
  the project changed (a media file replaced on disk, say), then says how many overlays are
  drawn live and how many were baked. Overlays of nested sequences that have not been baked
  yet are left out at first and baked by a `preview-bake` job in the job queue; the preview
  picks them up when it ends, and an overlay that would not bake is named with the reason.
  If the preview engine is not built, the button says that instead.
- **Just pulled? Run `npm run rebuild`.** The binary, `ui/dist` and `app/dist` are not in
  git, so a pull leaves them behind, and each fails in a way that does not name the build
  step: `kuma init` writing a project the editor refuses to open, or the editor window
  failing to start with something like `t.tracks is not iterable`. The desktop app warns
  about the first two and refuses to open a window against a stale `ui/dist`.
- **Start with `kuma doctor --project <file>`** — it reports the binary, repo root, tools,
  engine, display, whether a daemon answers on the port (and for which project), and the
  MCP config.
- **Claude Code says `kuma … Failed to connect ("Connection closed")`** — the `kuma mcp`
  process exited at startup. Run the exact command from `.mcp.json` by hand
  (`kuma mcp --project <file>`); the reason is printed on stderr. `kuma mcp` reuses a
  running `kuma serve` for the same project, starts a private daemon on a free port if the
  running one serves another project, and never routes loopback requests through
  `http_proxy`. Then `/mcp` → reconnect in Claude Code.
- **"Preview engine unavailable: …"** — the toast carries the daemon's reason; `GET /api/health`
  shows `preview_engine` and `preview_error`. The engine is a separate build step:
  `cmake -S engine -B engine/build && cmake --build engine/build -j` (needs `libmlt++-dev
  libsdl2-dev libturbojpeg0-dev`). Without it the preview falls back to single-frame
  snapshots (scrubbing works, playback does not).
- **Preview looks soft** — the engine defaults to half resolution for speed; pick
  `preview ¾` or `preview full` in the transport bar. Snapshots are requested at the
  stage's device-pixel width.
- **No display** (SSH, CI) — MLT's Qt module needs X11; install `xvfb` and kuma wraps
  `melt`/the engine in `xvfb-run` automatically.
- **"Transcribe" fails with "is not runnable" or "no whisper model"** — `kuma doctor`
  shows what it looked for: `whisper-cli` on `PATH` (`KUMA_WHISPER_BIN`) and a ggml model in
  `~/.local/share/kuma/models` (`KUMA_WHISPER_MODEL`). `GET /api/health` has the same under
  `whisper`.
- **`.kuma/` keeps growing** — superseded bakes and sampled frames are never deleted on
  their own. `kuma cache prune` (or the job queue's button, or `prune_cache`) removes what
  the project no longer uses; `--all` empties bakes and frames.
- **Bakes or renders load the machine, or you want them faster** — overlay clips bake side
  by side, each in its own hyperframes process with its own Chrome (about 250 MB).
  `KUMA_BAKE_JOBS` sets how many run at once, across everything the daemon bakes; the
  default is half the hardware threads, at most 8, and `KUMA_BAKE_JOBS=1` bakes one at a
  time. One long overlay still bakes in one process, so a transcript split into several
  entries bakes faster than one that spans the video. A render builds several frames at once
  (melt's `real_time`), `KUMA_RENDER_THREADS` of them with the same default; `1` renders one
  at a time. The output is the same either way.
- **`youtube-nvenc` is greyed out** — it renders on an NVIDIA GPU (NVENC), and kuma
  offers it only where a short test encode works; the preset's tooltip in the render dialog
  (and `unavailable` in `GET /api/presets`) says why it did not, usually no CUDA device or
  driver. The other presets encode on the CPU.
- **A blueprint is missing from the Blueprints tab** — `GET /api/health` lists
  `blueprint_dirs` (where kuma looked) and `blueprint_errors` (manifests that failed to load
  and why). No library at all: run `kuma library` (until then the checkout's starter set
  stands in, unless `KUMA_LIBRARY` is set).
- **A title renders in the wrong font** — `validate_project` (`kuma validate`) warns about
  every entry whose `font` names a family that is not installed, and about text none of an
  entry's fonts can set; a render's result lists the same `warnings`.
- **A project from before blueprints will not open** ("names the template …") — the
  message says how to convert the entry.
- **Bakes fail with "timelines did not become ready"** — an overlay without a GSAP timeline
  needs `data-no-timeline` on its composition root (see `library/blueprints/CLAUDE.md`).
  `kuma lint overlays/<name>` says so in a second instead of after a 45-second wait, and reports the
  rest of the contract with it.

## Known limitations

- Headless MLT needs Xvfb (Qt module). The daemon and CLI wrap `melt`/the engine
  automatically when `DISPLAY` is unset.
- The live-DOM preview approximates MLT-side effects on overlays with CSS; bake or
  `sample_frames` to see the exact result. Blend modes on overlays are exact only in export.
- Audio in dissolves is summed (not crossfaded) on the main tractor; the A/B lane
  crossfade is applied within the transition range only.
- No proxies/preview chunk cache yet — heavy timelines will scrub below real time.
- `.mlt` output opens in Kdenlive as a clip/timeline import; Kdenlive's own project
  scaffolding (`main_bin`, `kdenlive:` properties) is not emitted.
- Overlays bake at full frame size; there is no per-overlay bake region yet.
- Transitions are refused on overlay clips: bakes carry no handle frames yet.
- Clip and track ids must match `[A-Za-z_][A-Za-z0-9_-]*` (they double as XML ids and URL segments).

## Licence

MIT. Runtime dependencies: MLT (LGPL, dynamically linked in `engine/`), ffmpeg/melt as
subprocesses, hyperframes (Apache-2.0), GSAP (its standard licence).
