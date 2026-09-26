# Storyboard as code

Past a few dozen clips, hand-written patches stop scaling. Retiming a scene means editing
every clip after it, and nobody can review a 100 KB patch. Keep the video as a program that
generates the project, and let a diff decide what to send. It pays off most for a video
built of many overlays; a footage edit of a few dozen cuts is often simpler by hand.

```
assets/dsl.py         Chapter, bands, one helper per overlay component, build(), patch()
assets/storyboard.py  the video, scene by scene: texts, positions, times
assets/diff.py        storyboard vs live project → the smallest JSON Patch
assets/patches/       generated: one patch per chapter + the master
```

## The DSL

- `Chapter(cid)` holds `entries` (bin id → entry) and `clips` (kind, band, clip, forced lane).
- `ov(overlay, band, start, end, name, **vars)` makes **one entry and one clip**. Every
  component gets a thin helper (`title`, `lower_third`, `caption`, `arrow`, `sfx`, `music`…)
  that fills in the house defaults and nothing else. Footage gets one too
  (`shot(entry, start, end, src_in)`).
- Times are **absolute chapter seconds** everywhere in the storyboard. A helper converts
  times an overlay reads relative to its clip (a highlight's marks, a character's moods)
  with `t - t0`, in one place, so a scene can be read top to bottom.
- `build()` sorts clips by start, puts each in the first free lane of its band, and names
  tracks `<cid>_<kind><band>_<lane>`. The stacking then follows the band, whatever order the
  storyboard wrote things in.
- `patch()` emits a folder for the chapter's parts, its entries and the sequence entry.
- A `sound(name, t)` helper that also knows each effect's length and default gain keeps the
  effects mix in one table.

### Ids that do not move

- **Clip ids from content**: `<entry>_<int(start*1000)>`, deduplicated with a suffix.
  Adding a clip elsewhere then leaves every other id, and every other track, unchanged.
- **Entry ids** from a per-chapter counter shift when you insert an element. Give anything
  added later an explicit id (`eid="ten_secret"`), or the diff turns one insertion into
  hundreds of add/remove pairs and every overlay re-bakes.
- The diff ignores clip ids and labels when it compares tracks, so a renumbering alone never
  produces an edit.

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

## The diff

- **Normalise** before comparing: drop fields stored at their defaults (`0`, `false`,
  `[]`, `null`, and `1` for gains, speeds and scales), so a hand-written document and the
  one kuma returns compare equal.
- Entries: compare `name` and `vars`; emit `replace …/source/vars` for a change and `add`
  for a new entry. Remove only entries of this chapter's own folder that nothing wants any
  more.
- Tracks: if the list of track ids differs, replace the sequence's `tracks` whole;
  otherwise replace only the tracks whose clips differ, ignoring clip ids and labels.
- Emit adds first (new clips may name them), then replaces, then removes. Print the
  project revision to stderr: it is the `base_revision` to send.

## Applying through MCP

- Write the patch into the project (`patches/diff.json`) and apply it with `apply_edits`
  and `patch_path`: the daemon reads the file, so the whole patch lands in one atomic,
  undoable edit, exactly as the diff wrote it. Pass the `base_revision` the diff printed.
  The answer's `ops` says how many operations were applied.
- For a small change, send targeted ops (`…/clips/<j>/volume`) inline rather than a whole
  track.
- Then **re-run the diff: it must print 0 ops**, and `validate_project` must be clean.
- If the revision moved since you last looked, someone else edited. Read what the diff
  would change and make sure every op is yours before sending it.
- Before a sweeping change (a new time model, a shared helper), copy `assets/` and
  `overlays/` to scratch, so you can compare or roll back.
