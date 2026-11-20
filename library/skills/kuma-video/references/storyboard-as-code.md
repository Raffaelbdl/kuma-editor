# Storyboard as code

Past a few dozen clips, hand-written patches stop scaling. Retiming a scene means editing
every clip after it, and nobody can review a 100 KB patch. Keep the video as a program that
generates the project, and let kuma work out what to change. It pays off most for a video
built of many overlays; a footage edit of a few dozen cuts is often simpler by hand.

```
assets/dsl.py         Chapter, bands, one helper per overlay component, build()
assets/storyboard.py  the video, scene by scene: texts, positions, times; writes build/project.json
```

## Before the first scene

- **A length budget**, per chapter and whole, checked as the script is written. It is the
  cheapest thing to change and the dearest to change late.
- **Content apart from mechanism**: the script of the video (texts, beats, what appears
  when) in files a person can read and edit, the helpers and the build beside them.
- Generated media goes to **new file names** (a music bed named with its length), never
  over a file a render may be reading.

## The DSL

- `Chapter(cid)` holds `entries` (bin id → entry) and `clips` (kind, band, clip, forced lane).
- `ov(overlay, band, start, end, name, **vars)` makes **one entry and one clip**. Every
  component gets a thin helper (`title`, `lower_third`, `caption`, `arrow`, `sfx`, `music`…)
  that fills in the house defaults and nothing else. Footage gets one too
  (`shot(entry, start, end, src_in)`).
- Times are **absolute chapter seconds** everywhere in the storyboard. A helper converts
  times an overlay reads relative to its clip (a highlight's marks, a character's moods)
  with `t - t0`, in one place, so a scene can be read top to bottom.
- In a narrated video, a scene's times are seconds of its voice-over recording instead,
  and shots are appended inside it: `narration.md`.
- `build()` sorts clips by start, puts each in the first free lane of its band, and names
  tracks `<cid>_<kind><band>_<lane>`. The stacking then follows the band, whatever order the
  storyboard wrote things in.
- The script writes the whole project it means (every entry, every sequence) to a file.
  It does not read the live one and does not work out patches: `apply_plan` does.
- A `sound(name, t)` helper that also knows each effect's length and default gain keeps the
  effects mix in one table.

### Ids that do not move

- **Clip ids from content**: `<entry>_<int(start*1000)>`, deduplicated with a suffix.
  Adding a clip elsewhere then leaves every other id, and every other track, unchanged.
- **Entry ids** from a per-chapter counter shift when you insert an element. Give anything
  added later an explicit id (`eid="ten_secret"`), or the diff turns one insertion into
  hundreds of add/remove pairs and every overlay re-bakes.
- The diff compares ids like anything else: a renumbering is an edit of every clip after
  it, which is one more reason ids come from content.

### Retiming: cuts, not edits

`chapter.cut(a, b)` removes `[a, b)` of chapter time. The DSL applies it at the end
(`finalize()`): every time after `b` moves up by `b - a`, and a clip spanning the gap gets
shorter. Rules that keep this safe:

- Cut only holds. Assert that no clip **starts** strictly inside a cut; anything timed
  inside the clip (marks, moods) must happen before the cut.
- Values derived from lengths (the master's chapter starts, music cue lengths, keyframe
  spans of a push-in, a "part 2 of 5" label) use the **warped** length:
  `chapter.warp(t)`, `chapter.length`.
- Elements computed after every chapter exists (diagrams of the project itself, counts)
  are added in authored time, before `finalize()`.
- Put a transition on the cut's far side: `scene(t)` places a transition clip at
  `t - 0.3 … t + 0.3`, so a cut that ends at `t - 0.3` never swallows its start.

### Facts about the video, in the video

Anything computed from the video itself (a chapter strip, a progress bar's marks, "123
clips on 36 tracks") is computed last, from the built data or from the project on disk.
Re-run the storyboard after applying, so the counts include themselves.

## Applying a plan

`apply_plan` with `path: "build/project.json"` (or `kuma -p project.kuma.json apply
build/project.json`) makes the project the document the script wrote, as one undoable edit,
three-way against the plan last applied from that path, which kuma keeps. Do not write a diff
of your own; the traps are all in this one already.

- **What a person changed in the editor since stays.** Clips and tracks are matched by id,
  markers by time. Where the script and a person changed the same value, the person's stays
  and `kept` lists the path, on every apply until one side gives way: fold it into the script, or ask. The first plan from a path has
  nothing to merge with (`base: "none"`) and makes the project exactly the script's; `exact`
  forces that, reverting every fade, mute, gain and marker set in the editor.
- Both documents are compared **as kuma stores them**, so a field at its default that the
  script left out or spelled out (`"speed": 1`, a key's `ease: "linear"`) is no difference.
  `kuma canonical` prints that form, to read what the script's document comes to.
- **`meta` and `revision` are kuma's**: an entry the script gives no `meta` for keeps the
  probed one. Add media with `add_media` once; the script then only names the entry.
- **Entries the script does not write are removed**, and `removed` names them. `keep`
  leaves them: use it while a person, or another script, also adds entries. With it,
  `unused_entries` lists what no clip places, and what is placed only where the master
  never reaches.
- **Apply again after a build: 0 ops** (`applied: false`) means the script and the project
  agree. A refused plan changes nothing and lists the validator's issues.
- A var written at its manifest default is a difference once (vars are stored as given),
  and none after it is applied.
- Times are compared exactly. Round what the script computes (`round(t, 6)`), keep a clip
  inside its source (`in + (end − start) × speed ≤` the duration, floored to a frame for an
  effect cut to its length), and a music bed inside the sequence, or `validate_project`
  warns on every build.
- **Fold into the script any change it should own**: your own targeted `apply_edits`
  (`…/clips/<j>/volume`), or a person's moved clip or retimed title, so a later rewrite of
  that scene starts from it. A person editing between your turns is the usual case.
- `kuma diff --base <document> build/project.json` prints the same patch without applying
  it, for a base you keep yourself; a conflict it reports is not reported again once you
  copy the document over that base.
- Before a sweeping change (a new time model, a shared helper), copy `assets/` and
  `overlays/` to scratch, so you can compare or roll back.
