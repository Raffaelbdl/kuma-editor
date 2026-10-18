//! The text a video is published with: its subtitles as SRT, and its chapters as the
//! timestamps YouTube reads out of a description. Both are of one sequence — the master
//! unless one is named, as for a render — and both are read off the document alone, so
//! what is open in the editor never changes what is exported.
//!
//! Subtitles are not a model concept (`docs/decisions.md`, *Subtitles and vertical*): a
//! transcript is the `cues` var of an overlay entry, in source seconds, and what puts a cue
//! on the video is a clip placing that entry, possibly inside a sequence that is placed in
//! turn. So the SRT is what the picture would show, when it would show it: every line the
//! template draws, mapped through every placement that reaches the sequence and cut to
//! what each placement shows of it.
//!
//! Chapters are the sequence's clips that place another sequence, the same ones
//! `measure_audio` measures one by one ([`crate::jobs::chapters`]). Markers are not
//! chapters: they are a person's comments on the cut, and would publish review notes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use kuma_core::{BinSource, Clip, OverlayManifest, Project, VarType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::jobs::Section;
use crate::state::{read, AppState};

/// The longest pause, in seconds, a line of the `subtitles` blueprint spans: a longer one
/// starts a new line, and a shorter one keeps the line up until the next. The template's
/// own `GAP`, restated because the SRT must time lines the way the picture does.
pub const LINE_GAP: f64 = 0.8;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SubtitlesRequest {
    /// The sequence whose time the SRT is in. The master when absent.
    #[serde(default)]
    pub sequence: Option<String>,
    /// Only these subtitles entries (bin ids). Every one the sequence reaches when absent.
    #[serde(default)]
    pub entries: Option<Vec<String>>,
    /// Group this many cues into a line instead of each entry's own `words_per_line`, for
    /// captions longer than the lines drawn on the picture.
    #[serde(default)]
    pub words_per_line: Option<u32>,
    #[serde(flatten)]
    pub to: Output,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ChaptersRequest {
    /// The sequence whose chapters these are. The master when absent.
    #[serde(default)]
    pub sequence: Option<String>,
    #[serde(flatten)]
    pub to: Output,
}

/// Where an export goes: back in the answer, or to a file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Output {
    /// Write the file here (relative to the project, or absolute). Implies `write`.
    #[serde(default)]
    pub output: Option<PathBuf>,
    /// Write the file, at `output` or beside the renders (`renders/<name>.srt`,
    /// `renders/<name>-chapters.txt`). Without it the text comes back in the answer and
    /// nothing is written.
    #[serde(default)]
    pub write: bool,
}

/// How one subtitles entry draws its cues: what the export needs of its manifest and vars.
#[derive(Debug, Clone, PartialEq)]
pub struct Captions {
    pub cues: Vec<kuma_overlay::Cue>,
    /// Source seconds at the clip's first frame: the template's own time is `source − offset`.
    pub offset: f64,
    /// Cues per line, and lines held across short pauses, as the `subtitles` blueprint does.
    /// `None` for a template that does not group (no `words_per_line` var): each cue is
    /// shown for its own window.
    pub words_per_line: Option<usize>,
}

impl Captions {
    /// The captions an entry's template draws, if its manifest takes a `cues` var.
    pub fn of(manifest: &OverlayManifest, vars: &Map<String, Value>) -> Option<Captions> {
        let (cues, _) = manifest.vars.iter().find(|(_, v)| v.ty == VarType::Cues)?;
        let vars = manifest.resolve_vars(vars);
        let number = |name: &str| manifest.vars.contains_key(name).then(|| vars.get(name).and_then(Value::as_f64)).flatten();
        Some(Captions {
            cues: vars.get(cues).cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            offset: number("offset").unwrap_or(0.0),
            words_per_line: number("words_per_line").map(|n| n.round().max(1.0) as usize),
        })
    }

    /// The lines drawn by a clip whose template runs `duration` seconds, in the template's
    /// own time: the blueprint's grouping, step for step (`library/blueprints/subtitles`).
    /// `per` overrides the entry's own `words_per_line`.
    pub fn lines(&self, duration: f64, per: Option<usize>) -> Vec<Line> {
        let words = self
            .cues
            .iter()
            .map(|c| (c.start - self.offset, c.end - self.offset, c.text.trim()))
            .filter(|&(start, end, _)| end > 0.0 && start < duration && end > start);
        let (per, hold) = match per.or(self.words_per_line) {
            Some(n) => (n.max(1), true),
            None => (1, false),
        };
        let mut lines: Vec<Line> = Vec::new();
        for (start, end, text) in words {
            match lines.last_mut() {
                Some(l) if l.words < per && start - l.end <= LINE_GAP => {
                    l.end = end;
                    l.words += 1;
                    if !text.is_empty() {
                        if !l.text.is_empty() {
                            l.text.push(' ');
                        }
                        l.text.push_str(text);
                    }
                }
                _ => lines.push(Line { start, end, text: text.to_string(), words: 1 }),
            }
        }
        // A line stays up until the next one starts when the pause between them is short,
        // so the text never blinks between words; and nothing is drawn outside the clip.
        for i in 0..lines.len() {
            let next = lines.get(i + 1).map(|n| n.start);
            let l = &mut lines[i];
            let end = match next {
                Some(n) if hold && n - l.end < LINE_GAP => n,
                _ => l.end,
            };
            l.start = l.start.max(0.0);
            l.end = end.min(duration);
        }
        lines.retain(|l| l.end > l.start && !l.text.is_empty());
        lines
    }
}

/// One line of subtitles, in whatever time it is given in.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
    words: usize,
}

/// A clip that shows an overlay entry somewhere in a sequence's time, nesting included:
/// how the overlay's own time maps onto the root's, and the stretch of the root it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement<'p> {
    pub clip: &'p Clip,
    /// Root time = `scale` × overlay time + `shift`. Negative for a reversed clip.
    pub scale: f64,
    pub shift: f64,
    /// `[from, to)` of the root's time this placement shows: its own clip, cut to every
    /// clip on the way that places the sequence holding it.
    pub from: f64,
    pub to: f64,
    /// How long the overlay's own time runs: the clip's length × |speed|.
    pub duration: f64,
    /// On a hidden track, here or on the way.
    pub hidden: bool,
}

impl Placement<'_> {
    pub fn entry(&self) -> &str {
        &self.clip.bin
    }

    /// `[a, b)` of overlay time in root time, cut to what this placement shows.
    pub fn map(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        let (x, y) = (self.scale * a + self.shift, self.scale * b + self.shift);
        let (lo, hi) = (x.min(y).max(self.from), x.max(y).min(self.to));
        (hi > lo).then_some((lo, hi))
    }
}

/// Every clip placing an overlay entry that `root` shows, through every sequence it nests
/// and however often each is placed, in the order the tracks hold them. Hidden tracks are
/// included and say so: hiding keeps an overlay out of the picture, which is exactly how a
/// person keeps captions for an SRT alone.
pub fn overlay_placements<'p>(project: &'p Project, root: &str) -> Vec<Placement<'p>> {
    let mut out = Vec::new();
    if let Some((root, _)) = project.bin.get_key_value(root) {
        walk(project, root, (1.0, 0.0), (f64::NEG_INFINITY, f64::INFINITY), false, &mut Vec::new(), &mut out);
    }
    out
}

/// `seq`'s clips, its time mapping onto the root's as `root = map.0 × t + map.1` (always
/// increasing: a sequence clip plays forwards) and showing only `window` of the root.
fn walk<'p>(p: &'p Project, seq: &'p str, map: (f64, f64), window: (f64, f64), hidden: bool, path: &mut Vec<&'p str>, out: &mut Vec<Placement<'p>>) {
    // A ring is refused by the validator; the guard keeps a refused document finite.
    if path.contains(&seq) {
        return;
    }
    let Some(s) = p.seq(seq) else { return };
    path.push(seq);
    let (a, b) = map;
    for t in &s.tracks {
        for c in &t.clips {
            let (from, to) = ((a * c.start + b).max(window.0), (a * c.end + b).min(window.1));
            if to <= from || !c.speed.is_finite() || c.speed == 0.0 {
                continue;
            }
            let hidden = hidden || t.hidden;
            match p.bin.get_key_value(&c.bin) {
                // Sequence time `v` plays at `c.start + (v − in) / speed` of this one.
                Some((id, item)) if item.is_sequence() && c.speed > 0.0 => {
                    let s = c.speed;
                    walk(p, id, (a / s, a * (c.start - c.source_in / s) + b), (from, to), hidden, path, out);
                }
                // Overlay time `t` is at `start + t / speed`, or for a reversed clip
                // `end − t / |speed|`; an overlay has no `in`.
                Some((_, item)) if item.is_overlay() => {
                    let s = c.speed;
                    let shift = a * if s > 0.0 { c.start } else { c.end } + b;
                    out.push(Placement { clip: c, scale: a / s, shift, from, to, duration: c.duration() * s.abs(), hidden });
                }
                _ => {}
            }
        }
    }
    path.pop();
}

/// One subtitle in the root's seconds, and the entry and clip it comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Subtitle {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub entry: String,
    pub clip: String,
}

/// The subtitles `root` shows, in its own seconds and in time order: each entry's lines
/// drawn as each placement draws them (its own length bounds what it groups), then mapped
/// onto the root and cut to what the placement shows. `per` overrides `words_per_line`.
pub fn subtitles_on(placements: &[Placement], captions: &BTreeMap<String, Captions>, per: Option<usize>) -> Vec<Subtitle> {
    let mut out: Vec<Subtitle> = placements
        .iter()
        .filter_map(|pl| Some((pl, captions.get(pl.entry())?)))
        .flat_map(|(pl, c)| {
            c.lines(pl.duration, per).into_iter().filter_map(move |l| {
                let (start, end) = pl.map(l.start, l.end)?;
                Some(Subtitle { start, end, text: l.text, entry: pl.entry().to_string(), clip: pl.clip.id.clone() })
            })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    out
}

/// Whole milliseconds, as SRT counts them: rounded to the nearest, never below zero.
fn millis(t: f64) -> i64 {
    (t.max(0.0) * 1000.0).round() as i64
}

/// `HH:MM:SS,mmm`: SRT's time, with a comma before the milliseconds.
pub fn srt_time(t: f64) -> String {
    let ms = millis(t);
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// SubRip text: cues numbered from 1, each `start --> end` and its text, a blank line
/// after each. A cue that rounds to no time at all, or has no text, is left out rather than
/// numbered; a blank line inside a text would end its cue, so blank lines are dropped.
/// The text, and how many cues it holds.
pub fn srt(subs: &[Subtitle]) -> (String, usize) {
    let mut out = String::new();
    let mut n = 0;
    for s in subs {
        let text: Vec<&str> = s.text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if millis(s.end) <= millis(s.start) || text.is_empty() {
            continue;
        }
        n += 1;
        out.push_str(&format!("{n}\n{} --> {}\n{}\n\n", srt_time(s.start), srt_time(s.end), text.join("\n")));
    }
    (out, n)
}

/// A chapter as the description lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chapter {
    /// Seconds into the sequence, as the clip starts.
    pub start: f64,
    /// The timestamp YouTube reads: whole seconds, `M:SS` or `H:MM:SS`.
    pub at: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
}

/// YouTube's rules for reading timestamps in a description as chapters.
pub const MIN_CHAPTERS: usize = 3;
pub const MIN_CHAPTER_SECONDS: u64 = 10;

/// Whole seconds a timestamp names: rounded down, so a chapter link lands on its cut and
/// never after its first word. A hair under a second counts as that second.
fn whole_seconds(t: f64) -> u64 {
    (t.max(0.0) + 1e-6).floor() as u64
}

/// `M:SS`, or `H:MM:SS` from an hour, as YouTube writes timestamps: the first is `0:00`.
pub fn timestamp(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A description's chapter lines, `M:SS Title`, from a sequence's sections and its length,
/// and what keeps YouTube from reading them as chapters: the first not at 0:00, fewer than
/// three, or one shorter than ten seconds. The text is written whatever they say; fixing
/// the cut is the person's call.
pub fn youtube_chapters(sections: &[Section], duration: f64) -> (String, Vec<Chapter>, Vec<String>) {
    let chapters: Vec<Chapter> = sections
        .iter()
        .map(|s| {
            let title = s.label.as_deref().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
            Chapter { start: s.start, at: timestamp(whole_seconds(s.start)), title, clip: s.clip.clone() }
        })
        .collect();
    let text: String = chapters.iter().map(|c| format!("{} {}\n", c.at, c.title)).collect();
    let mut warnings = Vec::new();
    if let Some(first) = chapters.first().filter(|c| whole_seconds(c.start) > 0) {
        warnings.push(format!("the first chapter, “{}”, starts at {}; YouTube needs the first at 0:00", first.title, first.at));
    }
    if !chapters.is_empty() && chapters.len() < MIN_CHAPTERS {
        warnings.push(format!("YouTube needs at least {MIN_CHAPTERS} chapters; this has {}", chapters.len()));
    }
    let ends = chapters.iter().skip(1).map(|c| whole_seconds(c.start)).chain([whole_seconds(duration)]);
    for (c, end) in chapters.iter().zip(ends) {
        let lasts = end.saturating_sub(whole_seconds(c.start));
        if lasts < MIN_CHAPTER_SECONDS {
            warnings.push(format!("“{}” ({}) lasts {lasts} s; YouTube needs every chapter to last at least {MIN_CHAPTER_SECONDS} s", c.title, c.at));
        }
    }
    (text, chapters, warnings)
}

/// The answer for an export: the `text` when it is not to be written, else written — at
/// `output`, or beside the renders and named as they are (`suffix`) — and its `path`
/// (relative to the project when inside it). Replaced whole, so a reader never sees half a file.
fn answer(state: &AppState, mut out: Value, to: &Output, project: &Project, sequence: &str, suffix: &str, text: String) -> anyhow::Result<Value> {
    if !to.write && to.output.is_none() {
        out["text"] = json!(text);
        return Ok(out);
    }
    let default = PathBuf::from(format!("{}{suffix}", crate::jobs::output_stem(project, sequence)));
    let path = crate::jobs::resolve_path(state, to.output.as_deref().unwrap_or(&default));
    crate::jobs::write_atomic(&path, text.as_bytes())?;
    out["path"] = json!(crate::jobs::project_relative(state, &path));
    Ok(out)
}

/// Export `req.sequence`'s subtitles (the master's by default) as SRT: returned, or
/// written. Warns about what will not be in it — a subtitles entry nothing the sequence
/// reaches places, an overlay folder that cannot be read — and about what may surprise:
/// cues from hidden tracks, two entries speaking at once.
pub fn subtitles(state: &AppState, req: &SubtitlesRequest) -> anyhow::Result<Value> {
    let project = read(&state.project).clone();
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    let mut warnings = Vec::new();

    // Each overlay folder's manifest, read once: entries may share one. A folder that
    // cannot be read is only worth a word when the sequence shows its entry.
    let shown = overlay_placements(&project, &sequence);
    let placed: BTreeSet<&str> = shown.iter().map(Placement::entry).collect();
    let mut manifests: HashMap<&str, Result<OverlayManifest, String>> = HashMap::new();
    let mut captions = BTreeMap::new();
    for (id, item) in &project.bin {
        let BinSource::Overlay { overlay, vars } = &item.source else { continue };
        let manifest = manifests.entry(overlay.as_str()).or_insert_with(|| crate::overlays::resolve(state, overlay).map(|r| r.manifest).map_err(|e| format!("{e:#}")));
        match manifest {
            Ok(m) => {
                if let Some(c) = Captions::of(m, vars) {
                    captions.insert(id.clone(), c);
                }
            }
            Err(e) if placed.contains(id.as_str()) => warnings.push(format!("`{id}`: {e}; any cues it has are not in the SRT")),
            Err(_) => {}
        }
    }
    if let Some(wanted) = &req.entries {
        for id in wanted {
            anyhow::ensure!(captions.contains_key(id), "`{id}` is not a subtitles entry: its overlay takes no `cues`");
        }
        captions.retain(|id, _| wanted.contains(id));
    }

    let placements: Vec<Placement> = shown.iter().filter(|p| captions.contains_key(p.entry())).cloned().collect();
    let reached: BTreeSet<&str> = placements.iter().map(Placement::entry).collect();
    for id in captions.keys().filter(|id| !reached.contains(id.as_str())) {
        warnings.push(format!("`{id}` is not placed in anything `{sequence}` shows, so its cues are not in the SRT; place it over its clip (transcribing from a clip does)"));
    }
    let hidden: BTreeSet<&str> = placements.iter().filter(|p| p.hidden).map(|p| p.clip.id.as_str()).collect();
    if !hidden.is_empty() {
        let list = hidden.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
        warnings.push(format!("cues from {list}, on a hidden track, are in the SRT: hiding keeps them off the picture, not out of the captions"));
    }

    let per = req.words_per_line.map(|n| n.max(1) as usize);
    let subs = subtitles_on(&placements, &captions, per);
    warnings.extend(overlaps(&subs));
    let (text, cues) = srt(&subs);
    if cues == 0 && warnings.is_empty() {
        warnings.push(format!("`{sequence}` shows no subtitles: transcribe a clip, or place a subtitles entry on it"));
    }
    let mut out = json!({ "sequence": sequence, "cues": cues, "entries": reached, "warnings": warnings });
    // An empty SRT is never worth a file: the answer says why there is none instead.
    if cues == 0 {
        out["text"] = json!(text);
        return Ok(out);
    }
    answer(state, out, &req.to, &project, &sequence, ".srt", text)
}

/// Two entries' subtitles on screen at once — two languages, or a transcript placed twice —
/// each pair once, where it first happens. YouTube shows both, stacked.
fn overlaps(subs: &[Subtitle]) -> Vec<String> {
    let mut said = BTreeSet::new();
    let mut out = Vec::new();
    let mut showing: Option<&Subtitle> = None;
    for s in subs {
        if let Some(prev) = showing.filter(|p| s.start < p.end - 1e-6 && p.entry != s.entry) {
            let pair = if prev.entry < s.entry { (&prev.entry, &s.entry) } else { (&s.entry, &prev.entry) };
            if said.insert(pair) {
                out.push(format!("`{}` and `{}` both show subtitles at {}: the SRT has both, overlapping", pair.0, pair.1, srt_time(s.start)));
            }
        }
        if showing.is_none_or(|p| s.end > p.end) {
            showing = Some(s);
        }
    }
    out
}

/// Export `req.sequence`'s chapters (the master's by default) as YouTube description
/// timestamps: returned, or written. Warns, never refuses, when YouTube would not read
/// them as chapters.
pub fn chapters(state: &AppState, req: &ChaptersRequest) -> anyhow::Result<Value> {
    let project = read(&state.project).clone();
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    let sections = crate::jobs::chapters(&project, &sequence);
    let (text, chapters, mut warnings) = youtube_chapters(&sections, project.seq_duration(&sequence));
    if chapters.is_empty() {
        warnings.push(format!("`{sequence}` places no sequence, so it has no chapters: put each part of the video in a sequence of its own and place those on it"));
    }
    let out = json!({ "sequence": sequence, "chapters": chapters, "warnings": warnings });
    answer(state, out, &req.to, &project, &sequence, "-chapters.txt", text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuma_overlay::Cue;
    use serde_json::json;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue { start, end, text: text.into() }
    }

    /// One cue per line, each for its own window: what the mapping tests read most easily.
    fn plain(cues: Vec<Cue>) -> Captions {
        Captions { cues, offset: 0.0, words_per_line: None }
    }

    fn project(bin: Value) -> Project {
        serde_json::from_value(json!({ "master": "main", "bin": bin })).unwrap()
    }

    fn seq(tracks: Value) -> Value {
        json!({ "source": { "tracks": tracks } })
    }

    fn track(id: &str, clips: Value) -> Value {
        json!({ "id": id, "kind": "overlay", "clips": clips })
    }

    fn subs_of(p: &Project, root: &str, captions: &[(&str, Captions)]) -> Vec<(f64, f64, String)> {
        let captions: BTreeMap<String, Captions> = captions.iter().map(|(id, c)| (id.to_string(), c.clone())).collect();
        let round = |t: f64| (t * 1e6).round() / 1e6;
        subtitles_on(&overlay_placements(p, root), &captions, None).into_iter().map(|s| (round(s.start), round(s.end), s.text)).collect()
    }

    /// A transcript placed in a nested sequence comes out in the master's seconds: shifted
    /// by where the sequence is placed, and by where its clip starts in that sequence.
    #[test]
    fn a_nested_transcript_is_timed_in_the_master() {
        let p = project(json!({
            "subs": { "source": { "overlay": "overlays/subtitles" } },
            "part": seq(json!([track("o1", json!([{ "id": "s1", "start": 2, "end": 12, "bin": "subs" }]))])),
            "main": seq(json!([{ "id": "v1", "clips": [{ "id": "p1", "start": 30, "end": 50, "bin": "part" }] }])),
        }));
        let got = subs_of(&p, "main", &[("subs", plain(vec![cue(1.0, 2.5, "Hello"), cue(3.0, 4.0, "there")]))]);
        assert_eq!(got, vec![(33.0, 34.5, "Hello".into()), (35.0, 36.0, "there".into())]);
        // The sequence itself, exported on its own, is in its own seconds.
        assert_eq!(subs_of(&p, "part", &[("subs", plain(vec![cue(1.0, 2.5, "Hello")]))]), vec![(3.0, 4.5, "Hello".into())]);
    }

    /// A placement that starts into its sequence (`in`) and stops before its end shows only
    /// that stretch: a cue before it is gone, one after it too, and one across either edge
    /// is cut to it.
    #[test]
    fn a_cut_placement_shows_only_what_it_plays() {
        let p = project(json!({
            "subs": { "source": { "overlay": "overlays/subtitles" } },
            "part": seq(json!([track("o1", json!([{ "id": "s1", "start": 0, "end": 20, "bin": "subs" }]))])),
            // Plays 5..9 of `part` at 100..104 of the master.
            "main": seq(json!([{ "id": "v1", "clips": [{ "id": "p1", "start": 100, "end": 104, "in": 5, "bin": "part" }] }])),
        }));
        let cues = vec![cue(1.0, 2.0, "before"), cue(4.5, 5.5, "straddles-in"), cue(6.0, 7.0, "inside"), cue(8.5, 9.5, "straddles-out"), cue(12.0, 13.0, "after")];
        assert_eq!(
            subs_of(&p, "main", &[("subs", plain(cues))]),
            vec![(100.0, 100.5, "straddles-in".into()), (101.0, 102.0, "inside".into()), (103.5, 104.0, "straddles-out".into())]
        );
    }

    /// A cue straddling a cut between two placements of one sequence is split: each side
    /// shows the part it plays, at its own place in the master.
    #[test]
    fn a_cue_across_a_cut_is_shown_by_both_sides() {
        let p = project(json!({
            "subs": { "source": { "overlay": "overlays/subtitles" } },
            "talk": seq(json!([track("o1", json!([{ "id": "s1", "start": 0, "end": 60, "bin": "subs" }]))])),
            // 0..10 of the talk, then 20..30 of it straight after: the cut drops 10..20.
            "main": seq(json!([{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 10, "bin": "talk" },
                { "id": "b", "start": 10, "end": 20, "in": 20, "bin": "talk" }
            ] }])),
        }));
        let cues = vec![cue(9.0, 11.0, "across-a-out"), cue(15.0, 16.0, "cut"), cue(19.5, 21.0, "across-b-in")];
        assert_eq!(
            subs_of(&p, "main", &[("subs", plain(cues))]),
            vec![(9.0, 10.0, "across-a-out".into()), (10.0, 11.0, "across-b-in".into())]
        );
    }

    /// One sequence placed twice shows its subtitles twice; and an overlay clip's own speed
    /// and offset, as a transcript aligned to a sped-up clip has them, are honoured.
    #[test]
    fn two_placements_show_it_twice_and_speed_and_offset_apply() {
        let p = project(json!({
            "subs": { "source": { "overlay": "overlays/subtitles" } },
            "intro": seq(json!([track("o1", json!([{ "id": "s1", "start": 0, "end": 5, "bin": "subs" }]))])),
            "main": seq(json!([
                { "id": "v1", "clips": [{ "id": "p1", "start": 0, "end": 5, "bin": "intro" }, { "id": "p2", "start": 60, "end": 65, "bin": "intro" }] },
                track("o2", json!([{ "id": "fast", "start": 10, "end": 15, "bin": "aligned", "speed": 2 }]))
            ])),
            "aligned": { "source": { "overlay": "overlays/subtitles" } },
        }));
        let twice = subs_of(&p, "main", &[("subs", plain(vec![cue(1.0, 2.0, "hi")]))]);
        assert_eq!(twice, vec![(1.0, 2.0, "hi".into()), (61.0, 62.0, "hi".into())]);
        // At 2×, source 30 + 4 s of template time is 2 s of the master after the clip's start.
        let fast = Captions { cues: vec![cue(34.0, 36.0, "quick"), cue(41.0, 42.0, "past the end")], offset: 30.0, words_per_line: None };
        assert_eq!(subs_of(&p, "main", &[("aligned", fast)]), vec![(12.0, 13.0, "quick".into())]);
    }

    /// A reversed overlay clip draws its own time backwards, so its first cue is last.
    #[test]
    fn a_reversed_clip_runs_its_cues_backwards() {
        let p = project(json!({
            "subs": { "source": { "overlay": "overlays/subtitles" } },
            "main": seq(json!([track("o1", json!([{ "id": "r", "start": 10, "end": 14, "bin": "subs", "speed": -1 }]))])),
        }));
        assert_eq!(subs_of(&p, "main", &[("subs", plain(vec![cue(0.0, 1.0, "first")]))]), vec![(13.0, 14.0, "first".into())]);
    }

    /// Lines are grouped as the blueprint groups them: `words_per_line` cues, a new line
    /// after a long pause, a line held until the next across a short one, nothing outside
    /// the clip. Cues before the clip's own zero are dropped before grouping, as there.
    #[test]
    fn lines_group_as_the_blueprint_draws_them() {
        let c = Captions {
            cues: vec![cue(0.2, 0.6, "Subtitles"), cue(0.6, 0.9, "go"), cue(0.9, 1.4, "here,"), cue(1.6, 1.9, "a"), cue(1.9, 2.3, "few"), cue(4.0, 4.5, "later"), cue(9.0, 11.0, "cut")],
            offset: 0.0,
            words_per_line: Some(3),
        };
        let got: Vec<(f64, f64, String)> = c.lines(10.0, None).into_iter().map(|l| (l.start, l.end, l.text)).collect();
        assert_eq!(
            got,
            vec![
                (0.2, 1.6, "Subtitles go here,".into()), // held to the next: 0.2 s of pause
                (1.6, 2.3, "a few".into()),               // 1.7 s to the next: not held
                (4.0, 4.5, "later".into()),
                (9.0, 10.0, "cut".into()),               // cut at the clip's end
            ]
        );
        // Overridden for longer captions: one line until the long pause.
        assert_eq!(c.lines(10.0, Some(8))[0].text, "Subtitles go here, a few");
        // An offset moves the clip's zero into the source.
        let off = Captions { offset: 1.0, ..c.clone() };
        assert_eq!(off.lines(10.0, None)[0].text, "here, a few");
    }

    #[test]
    fn captions_read_the_manifest_and_the_entry_vars() {
        let manifest: OverlayManifest = serde_json::from_value(json!({
            "name": "Subs", "duration_default": 4,
            "vars": {
                "cues": { "type": "cues", "default": [{ "start": 0, "end": 1, "text": "default" }] },
                "words_per_line": { "type": "number", "default": 3 },
                "offset": { "type": "number", "default": 0 }
            }
        }))
        .unwrap();
        let vars = json!({ "cues": [{ "start": 5, "end": 6, "text": "mine" }], "offset": 4.5, "words_per_line": 2.4 }).as_object().unwrap().clone();
        assert_eq!(Captions::of(&manifest, &vars), Some(Captions { cues: vec![cue(5.0, 6.0, "mine")], offset: 4.5, words_per_line: Some(2) }));
        assert_eq!(Captions::of(&manifest, &Map::new()).unwrap().cues, vec![cue(0.0, 1.0, "default")], "the default cues are what it draws");
        let title: OverlayManifest = serde_json::from_value(json!({ "name": "T", "duration_default": 4, "vars": { "label": { "type": "string", "default": "x" } } })).unwrap();
        assert_eq!(Captions::of(&title, &Map::new()), None, "no cues, no subtitles");
    }

    #[test]
    fn srt_numbers_cues_with_comma_millis_and_rounds_to_the_millisecond() {
        let sub = |start: f64, end: f64, text: &str| Subtitle { start, end, text: text.into(), entry: "e".into(), clip: "c".into() };
        let subs = vec![
            sub(0.0, 1.2346, "One"),
            sub(1.0004, 1.0004, "rounds to nothing"),
            sub(61.9996, 3725.5, "Two\n\nlines"),
            sub(4000.0, 4001.0, "   "),
            sub(0.1 + 0.2, 0.7, "float"),
        ];
        assert_eq!(
            srt(&subs).0,
            "1\n00:00:00,000 --> 00:00:01,235\nOne\n\n2\n00:01:02,000 --> 01:02:05,500\nTwo\nlines\n\n3\n00:00:00,300 --> 00:00:00,700\nfloat\n\n"
        );
        assert_eq!(srt_time(-0.001), "00:00:00,000");
        assert_eq!(srt_time(36000.0), "10:00:00,000");
    }

    fn section(start: f64, label: &str) -> Section {
        Section { start, end: start, label: Some(label.into()), clip: None }
    }

    #[test]
    fn chapters_are_youtube_timestamps() {
        let (text, chapters, warnings) = youtube_chapters(&[section(0.0, "Intro"), section(65.4, "The  long\nway"), section(3725.0, "Outro")], 3800.0);
        assert_eq!(text, "0:00 Intro\n1:05 The long way\n1:02:05 Outro\n");
        assert_eq!(chapters[1].at, "1:05");
        assert!(warnings.is_empty(), "{warnings:?}");
        // A hair under a whole second is that second, not the one before.
        assert_eq!(timestamp(whole_seconds(9.9999999)), "0:10");
        assert_eq!(timestamp(whole_seconds(599.0)), "9:59");
    }

    #[test]
    fn chapters_warn_where_youtube_would_not_read_them() {
        let (text, _, warnings) = youtube_chapters(&[section(5.0, "Late"), section(12.0, "Short")], 30.0);
        assert_eq!(text, "0:05 Late\n0:12 Short\n", "written anyway");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("needs the first at 0:00"));
        assert!(warnings[1].contains("at least 3 chapters; this has 2"));
        assert!(warnings[2].contains("“Late” (0:05) lasts 7 s"));
        // The last one is measured to the end of the sequence.
        let (_, _, warnings) = youtube_chapters(&[section(0.0, "A"), section(20.0, "B"), section(40.0, "C")], 45.0);
        assert_eq!(warnings, vec!["“C” (0:40) lasts 5 s; YouTube needs every chapter to last at least 10 s".to_string()]);
        let (text, chapters, warnings) = youtube_chapters(&[], 45.0);
        assert!(text.is_empty() && chapters.is_empty() && warnings.is_empty(), "none is the caller's to explain");
    }

    #[test]
    fn two_entries_at_once_are_said_once() {
        let sub = |start: f64, end: f64, entry: &str| Subtitle { start, end, text: "x".into(), entry: entry.into(), clip: "c".into() };
        let w = overlaps(&[sub(0.0, 5.0, "en"), sub(1.0, 2.0, "fr"), sub(3.0, 4.0, "fr"), sub(6.0, 7.0, "en")]);
        assert_eq!(w, vec!["`en` and `fr` both show subtitles at 00:00:01,000: the SRT has both, overlapping".to_string()]);
        assert!(overlaps(&[sub(0.0, 1.0, "en"), sub(1.0, 2.0, "fr")]).is_empty(), "end is exclusive");
    }
}
