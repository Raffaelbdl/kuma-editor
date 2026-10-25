//! Structural validation. Errors reject an edit; warnings are surfaced to the UI/agent.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::manifest::{check_overlay_path, OverlayManifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    /// JSON pointer into the project document.
    pub path: String,
    pub message: String,
}

impl Issue {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Issue { severity: Severity::Error, path: path.into(), message: message.into() }
    }
    fn warn(path: impl Into<String>, message: impl Into<String>) -> Self {
        Issue { severity: Severity::Warning, path: path.into(), message: message.into() }
    }
}

/// What the validator needs from the outside world.
#[derive(Debug, Default, Clone)]
pub struct ValidationContext {
    /// Project directory; bin paths and overlay folders resolve against it.
    pub root: PathBuf,
    /// If false, nothing on disk is read: media files, overlay folders and overlay vars go
    /// unchecked (the compiler). The daemon passes true.
    pub check_files: bool,
}

/// What a library holds, by folder: a folder with none of them is not one.
pub const LIBRARY_KINDS: [&str; 4] = ["blueprints", "presets", "fonts", "design-systems"];

/// Whether `dir` holds any of [`LIBRARY_KINDS`].
pub fn is_library(dir: &std::path::Path) -> bool {
    LIBRARY_KINDS.iter().any(|k| dir.join(k).is_dir())
}

/// Each library named once and by a path. On disk, one that is not there is a warning, not an
/// error: a drive not mounted must leave the project editable, and it reads as empty.
fn check_libraries(issues: &mut Vec<Issue>, p: &Project, ctx: &ValidationContext) {
    let mut seen = HashSet::new();
    for (i, (name, dir)) in p.libraries.iter().zip(p.library_dirs(&ctx.root)).enumerate() {
        let at = format!("/libraries/{i}");
        if name.trim().is_empty() {
            issues.push(Issue::error(at, "a library is a folder's path, not an empty string"));
        } else if !seen.insert(name) {
            issues.push(Issue::error(at, format!("`{name}` is named twice")));
        } else if ctx.check_files && !dir.is_dir() {
            issues.push(Issue::warn(at, format!("library `{name}` is not there ({}): it reads as empty until it is", dir.display())));
        } else if ctx.check_files && !is_library(&dir) {
            issues.push(Issue::warn(at, format!("`{name}` is not a library: it has none of {}/", LIBRARY_KINDS.join("/, "))));
        }
    }
}

pub fn validate(p: &Project, ctx: &ValidationContext) -> Vec<Issue> {
    let mut issues = Vec::new();

    if p.version != crate::PROJECT_VERSION {
        issues.push(Issue::error("/version", format!("unsupported version {}", p.version)));
    }
    if p.fps.num == 0 || p.fps.den == 0 {
        issues.push(Issue::error("/fps", "fps must be positive"));
    }
    if p.width == 0 || p.height == 0 || p.width % 2 == 1 || p.height % 2 == 1 {
        issues.push(Issue::error("/width", "width/height must be positive even numbers"));
    }
    check_libraries(&mut issues, p, ctx);

    // Bin entries are checked once each, however many clips place them. Overlay vars are
    // checked here rather than per clip, because the entry is what carries them now.
    let mut bin_ids = HashSet::new();
    // Entries may share a folder; read each manifest once.
    let mut manifests: HashMap<&str, Result<OverlayManifest, String>> = HashMap::new();
    for (id, item) in &p.bin {
        let ip = format!("/bin/{id}");
        check_id(&mut issues, &mut bin_ids, id, format!("{ip}/id"), "bin");
        match &item.source {
            BinSource::File { path, .. } => {
                // A warning, not an error: a project that already holds one must stay
                // editable, and the entry renders — as nothing worth having.
                if let Some(why) = unplayable_media(path) {
                    issues.push(Issue::warn(format!("{ip}/source/path"), format!("`{id}`: {why}")));
                }
                if ctx.check_files {
                    let full = if std::path::Path::new(path).is_absolute() { PathBuf::from(path) } else { ctx.root.join(path) };
                    if !full.is_file() {
                        issues.push(Issue::error(format!("{ip}/source/path"), format!("file not found: {path}")));
                    }
                }
            }
            BinSource::Overlay { overlay, vars } => {
                if let Some(problem) = check_overlay_path(overlay) {
                    issues.push(Issue::error(format!("{ip}/source/overlay"), problem));
                } else if ctx.check_files {
                    let loaded = manifests.entry(overlay.as_str()).or_insert_with(|| OverlayManifest::load(&ctx.root.join(overlay)).map_err(|e| e.reason));
                    match loaded {
                        Err(reason) => issues.push(Issue::error(format!("{ip}/source/overlay"), format!("overlay `{overlay}`: {reason}"))),
                        Ok(m) => {
                            for problem in m.check_vars(vars) {
                                issues.push(Issue::error(format!("{ip}/source/vars"), problem));
                            }
                        }
                    }
                }
            }
            BinSource::Color { color } => {
                if !crate::manifest::is_css_color(color) {
                    issues.push(Issue::error(format!("{ip}/source/color"), "colour must be #rgb, #rrggbb or #rrggbbaa"));
                }
            }
            // A sequence's own contents are checked below, with every other timeline.
            BinSource::Sequence(_) | BinSource::Folder { .. } => {}
        }
        if item.volume != 1.0 && !item.is_timed_media() {
            issues.push(Issue::error(format!("{ip}/volume"), "only a sound or video file, or a sequence, has a volume"));
        } else if !(item.volume >= 0.0 && item.volume.is_finite()) {
            issues.push(Issue::error(format!("{ip}/volume"), "volume must be >= 0"));
        }
        // `parent` is organisation, but a dangling or circular one still breaks the
        // browser: the chain up from every entry must end at the top level.
        if let Some(parent) = &item.parent {
            let mut seen = HashSet::from([id.as_str()]);
            let mut at = Some(parent.as_str());
            while let Some(cur) = at {
                if !seen.insert(cur) {
                    issues.push(Issue::error(format!("{ip}/parent"), format!("`{id}` is inside itself through `{cur}`")));
                    break;
                }
                match p.bin.get(cur) {
                    Some(i) => at = i.parent.as_deref(),
                    None => {
                        issues.push(Issue::error(format!("{ip}/parent"), format!("parent `{cur}` is not in the bin")));
                        break;
                    }
                }
            }
        }
    }

    match p.bin.get(&p.master) {
        None => issues.push(Issue::error("/master", format!("master names no bin entry `{}`", p.master))),
        Some(i) if !i.is_sequence() => {
            issues.push(Issue::error("/master", format!("master `{}` is not a sequence: the master timeline must be a sequence entry", p.master)))
        }
        Some(_) => {}
    }

    // A sequence containing itself, however indirectly, would make the compiler recurse
    // forever. Checked over the whole bin rather than only what the master reaches, so a
    // cycle is refused when it is built rather than when it is first placed. One report:
    // every entry on the ring would otherwise raise the same error.
    if let Err(ring) = p.check_cycles() {
        issues.push(Issue::error(
            format!("/bin/{}/source/tracks", ring[0]),
            format!("sequence `{}` contains itself: {}", ring[0], ring.join(" → ")),
        ));
    }

    // Track and clip ids are unique across the *whole project*, not per sequence: they
    // become MLT element ids in one compiled document and URL segments in one daemon.
    let mut track_ids = HashSet::new();
    let mut clip_ids = HashSet::new();
    // Which sequence each group was first seen on: a group is an editing relation on one
    // timeline, and a block cannot move on two at once.
    let mut groups: HashMap<&str, &str> = HashMap::new();
    for (seq_id, seq) in p.sequences() {
        check_sequence(&mut issues, p, ctx, seq_id, seq, &mut track_ids, &mut clip_ids, &mut groups);
    }

    issues
}

fn check_sequence<'a>(
    issues: &mut Vec<Issue>,
    p: &'a Project,
    ctx: &ValidationContext,
    seq_id: &'a str,
    seq: &'a Sequence,
    track_ids: &mut HashSet<&'a str>,
    clip_ids: &mut HashSet<&'a str>,
    groups: &mut HashMap<&'a str, &'a str>,
) {
    let base = format!("/bin/{seq_id}/source");
    // Overlays draw over video, never under it: the live preview draws the open sequence's
    // overlays above its whole picture, so a video track after one would render otherwise.
    if let Some(first) = seq.tracks.iter().position(|t| t.kind == TrackKind::Overlay) {
        for (ti, t) in seq.tracks.iter().enumerate().skip(first + 1).filter(|(_, t)| t.kind == TrackKind::Video) {
            issues.push(Issue::error(
                format!("{base}/tracks/{ti}"),
                format!(
                    "video track `{}` comes after overlay track `{}`: overlays draw over video, so every overlay track goes after the video tracks (move `{}` before `{}` in `tracks`). Something an overlay must draw under, it draws itself, as a picture in its own folder",
                    t.id, seq.tracks[first].id, t.id, seq.tracks[first].id
                ),
            ));
        }
    }
    check_sequence_effects(issues, p, seq_id, seq);
    for (ti, t) in seq.tracks.iter().enumerate() {
        let tp = format!("{base}/tracks/{ti}");
        check_id(issues, track_ids, &t.id, format!("{tp}/id"), "track");
        let clips = t.sorted_clips();

        for (ci, c) in &clips {
            let cp = format!("{tp}/clips/{ci}");
            check_id(issues, clip_ids, &c.id, format!("{cp}/id"), "clip");
            // A group id is shared on purpose, so it gets the format check and not the
            // uniqueness one.
            if !c.group.is_empty() && check_id_format(issues, &c.group, format!("{cp}/group"), "group") {
                if *groups.entry(&c.group).or_insert(seq_id) != seq_id {
                    // Whichever sequence it was met on first is the group's; a member
                    // anywhere else would be a clip the block can never take with it.
                    issues.push(Issue::error(
                        format!("{cp}/group"),
                        format!("group `{}` is on sequence `{}`; a group's clips must be on one timeline", c.group, groups[c.group.as_str()]),
                    ));
                }
            }
            if !(c.start >= 0.0 && c.end > c.start) {
                issues.push(Issue::error(format!("{cp}/end"), "clip must have start >= 0 and end > start"));
            } else if c.duration() * p.fps.as_f64() < 1.0 - 1e-9 {
                issues.push(Issue::error(format!("{cp}/end"), "clip is shorter than one frame"));
            }
            let item = p.item(c);
            if c.speed == 0.0 || !c.speed.is_finite() || c.speed.abs() > 20.0 || c.speed.abs() < 0.01 {
                issues.push(Issue::error(format!("{cp}/speed"), "speed must be in ±[0.01, 20]"));
            } else if item.is_some_and(BinItem::is_overlay) && !(1.0..=240.0).contains(&(p.fps.as_f64() / c.speed.abs())) {
                // The bake renders at fps/|speed|, which hyperframes bounds to 1..240.
                issues.push(Issue::error(format!("{cp}/speed"), "overlay speed must keep fps/|speed| within 1..240"));
            } else if item.is_some_and(BinItem::is_sequence) && (c.speed - 1.0).abs() > 1e-9 {
                // MLT's `timewarp` producer takes a resource *path*: handed the id of a
                // tractor it resolves it as a file, fails to load, and renders one frozen
                // frame. Measured, not assumed — see docs/decisions.md.
                issues.push(Issue::error(
                    format!("{cp}/speed"),
                    "a sequence clip cannot be sped up or slowed down; render the sequence out and place the result as media",
                ));
            }
            match item {
                None => issues.push(Issue::error(format!("{cp}/bin"), format!("unknown bin entry `{}`", c.bin))),
                Some(item) => {
                    // A track's kind changes what the compiler does, so only the case that
                    // actually breaks is an error. An overlay track is skipped by the
                    // preview compile, so anything else on one is invisible while
                    // scrubbing and then reappears in the render. The rest render
                    // correctly and are warnings about intent: a baked overlay is an
                    // ordinary producer anywhere, and an audio-only entry on a video track
                    // simply shows the tracks beneath it while its sound still plays.
                    // Video on an audio track stays silent here: the compiler hides its
                    // picture deliberately, which is the normal "use only the audio" move.
                    if item.is_folder() {
                        issues.push(Issue::error(format!("{cp}/bin"), format!("`{}` is a folder and cannot be placed", c.bin)));
                    } else if t.kind == TrackKind::Overlay && !item.is_overlay() {
                        issues.push(Issue::error(format!("{cp}/bin"), "overlay tracks only hold overlay clips"));
                    } else if t.kind != TrackKind::Overlay && item.is_overlay() {
                        issues.push(Issue::warn(format!("{cp}/bin"), "overlay clip on a non-overlay track"));
                    } else if t.kind == TrackKind::Video && item.is_audio_only() {
                        issues.push(Issue::warn(
                            format!("{cp}/bin"),
                            format!("`{}` has no video: on a video track it takes up a lane and shows nothing. Put it on an audio track.", c.bin),
                        ));
                    }
                    if c.source_in < 0.0 {
                        issues.push(Issue::error(format!("{cp}/in"), "in must be >= 0"));
                    } else if c.source_in > 0.0 && !item.is_timed_media() {
                        issues.push(Issue::error(format!("{cp}/in"), "in applies only to video, audio and sequence entries; this one is generated to fit the clip"));
                    }
                    // An empty sequence compiles to nothing, so a clip placing one would
                    // silently be a gap. Say so where it is placed.
                    match source_duration(p, c) {
                        Some(d) if d <= 0.0 && item.is_sequence() => {
                            issues.push(Issue::error(format!("{cp}/bin"), format!("sequence `{}` is empty; put a clip on it before placing it", c.bin)))
                        }
                        Some(d) => {
                            // Counted in frames, as the compiler cuts: a source a fraction
                            // of a frame short of the clip is not short at all — MLT plays
                            // a frame that starts inside the source — so only a whole frame
                            // the source does not have is worth a word.
                            let (_, end) = c.source_span(p.fps);
                            let short = p.source_frames(c).map_or(0, |have| end - have);
                            if short > 0 {
                                let frames = if short == 1 { "frame".to_string() } else { format!("{short} frames") };
                                issues.push(Issue::warn(
                                    format!("{cp}/end"),
                                    format!("clip reads past the end of its {d:.2}s source: its last {frames} will show the tracks below and be silent"),
                                ));
                            }
                        }
                        None => {}
                    }
                }
            }
            // A second effect of these kinds would stack (two volume filters square the
            // gain) or silently shadow the first (blend); only `mlt` may repeat, and the
            // masked ones — each a window of its own, stacking being what they are for.
            let mut kinds = HashSet::new();
            // What the clip can show and sound, for the effects that act on neither. Only
            // for an entry that resolves to something placeable (the others are errors
            // already, and a warning per effect on top would bury them), and only for a
            // clip with effects: for a sequence it is a walk of everything nested.
            let placeable = item.is_some_and(|i| !i.is_folder());
            let carries = if placeable && !c.effects.is_empty() { p.clip_streams(t, c) } else { None };
            for (ei, e) in c.effects.iter().enumerate() {
                let ep = format!("{cp}/effects/{ei}");
                let kind = e.kind();
                if kind != "mlt" && e.mask().is_none() && !kinds.insert(kind) {
                    issues.push(Issue::error(ep.clone(), format!("duplicate `{kind}` effect on this clip")));
                }
                // A warning, not an error: it renders, and does nothing. The editor does not
                // offer it; this is for one that arrived by patch, or whose clip moved to a
                // track that takes away what it acted on.
                if placeable && !carries.is_some_and(|s| e.acts_on(s)) {
                    let lacks = if e.streams().video() { "shows no picture" } else { "has no sound" };
                    issues.push(Issue::warn(ep.clone(), format!("`{kind}` does nothing here: this clip {lacks}")));
                }
                check_effect(issues, &ep, e, kind, ("clip", c.duration()));
                match e {
                    Effect::Fade { fade_in, fade_out } | Effect::AudioFade { fade_in, fade_out } => {
                        if fade_in + fade_out > c.duration() + 1e-9 {
                            issues.push(Issue::warn(ep.clone(), "fade in + out exceed the clip duration"));
                        }
                    }
                    Effect::Grade(g) => check_grade_luts(issues, &ep, g, ctx),
                    Effect::Freeze { t } => {
                        // Outside the source the compiler holds the nearest frame: a warning,
                        // not a refusal of whatever edit moved the clip's `in`.
                        if !t.is_finite() {
                            issues.push(Issue::error(format!("{ep}/t"), "freeze `t` must be a number of seconds"));
                        } else if p.held_frame(c, *t, (0, 0)).1 {
                            issues.push(Issue::warn(format!("{ep}/t"), "freeze `t` lands outside the clip's source; it holds the source's nearest frame"));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Overlaps on the same track.
        for w in clips.windows(2) {
            let (ai, a) = w[0];
            let (_, b) = w[1];
            if b.start < a.end - 1e-9 {
                issues.push(Issue::error(
                    format!("{tp}/clips/{ai}/end"),
                    format!("clip `{}` overlaps `{}` on track `{}`", a.id, b.id, t.id),
                ));
            }
        }

        // Transitions must join adjacent media clips and have handles. A clip may be the
        // `to` of one transition and the `from` of the next; the two halves must fit.
        let mut handles: BTreeMap<&str, f64> = BTreeMap::new();
        let mut pairs = HashSet::new();
        for (xi, x) in t.transitions.iter().enumerate() {
            let xp = format!("{tp}/transitions/{xi}");
            if !pairs.insert((x.from.as_str(), x.to.as_str())) {
                issues.push(Issue::error(xp.clone(), "duplicate transition"));
            }
            let from = clips.iter().position(|(_, c)| c.id == x.from);
            let to = clips.iter().position(|(_, c)| c.id == x.to);
            let (a, b) = match (from, to) {
                (Some(f), Some(tt)) if tt == f + 1 => (clips[f].1, clips[tt].1),
                (Some(_), Some(_)) => {
                    issues.push(Issue::error(xp, "transition clips must be adjacent, from → to"));
                    continue;
                }
                _ => {
                    issues.push(Issue::error(xp, "transition references an unknown clip"));
                    continue;
                }
            };
            if (b.start - a.end).abs() > 1e-6 {
                issues.push(Issue::error(xp.clone(), "transition clips must be butt-joined (no gap)"));
            }
            if x.duration <= 0.0 {
                issues.push(Issue::error(format!("{xp}/duration"), "duration must be > 0"));
            }
            if [a, b].iter().any(|c| p.item(c).is_some_and(BinItem::is_overlay)) {
                issues.push(Issue::error(xp.clone(), "transitions are not supported on overlay clips (bakes carry no handles)"));
            }
            let half = x.duration / 2.0;
            for c in [a, b] {
                let h = handles.entry(c.id.as_str()).or_default();
                *h += half;
                if *h > c.duration() + 1e-9 {
                    issues.push(Issue::error(format!("{xp}/duration"), format!("transitions on `{}` are longer than the clip", c.id)));
                }
            }
            // Handles are in source time: a 2× clip eats twice the material. Stills and
            // colours are generated to any length and need none.
            if p.item(b).is_some_and(BinItem::is_timed_media) {
                let need = half * b.speed.abs();
                if b.source_in < need - 1e-9 {
                    issues.push(Issue::error(format!("{xp}/duration"), format!("`{}` needs {need:.2}s of handle before its in point", b.id)));
                }
            }
            // The `from` side needs material after its out point; only checkable when the
            // source length is known (probed for a file, exact for a sequence).
            if let Some(d) = source_duration(p, a) {
                let need = a.source_in + (a.duration() + half) * a.speed.abs();
                if need > d + 1e-6 {
                    issues.push(Issue::warn(
                        format!("{xp}/duration"),
                        format!("`{}` has no {:.2}s of handle after its out point; the dissolve tail will be black", a.id, half * a.speed.abs()),
                    ));
                }
            }
            if x.kind == TransitionKind::Wipe {
                if !(0.0..=1.0).contains(&x.softness) {
                    issues.push(Issue::error(format!("{xp}/softness"), "softness must be 0..1"));
                }
                if let Some(l) = &x.luma {
                    if !LUMAS.contains(&l.as_str()) {
                        if !stays_inside(l) {
                            issues.push(Issue::error(format!("{xp}/luma"), "luma path must stay inside the project"));
                        } else if ctx.check_files && !ctx.root.join(l).is_file() {
                            issues.push(Issue::error(format!("{xp}/luma"), format!("unknown luma `{l}`: use one of {LUMAS:?} or a file under the project")));
                        }
                    }
                }
            }
        }
    }
}

/// What an effect is wherever it sits, on a clip or a sequence: its keyframes (times in
/// order, within `duration`, naming parameters the kind has) and the bounds of its kind,
/// checked on every sample the effect will be evaluated at.
fn check_effect(issues: &mut Vec<Issue>, ep: &str, e: &Effect, kind: &str, owner: (&str, f64)) {
    if let Some(m) = e.mask() {
        let mp = format!("{ep}/mask");
        let shapes = std::iter::once((mp.clone(), m.clone())).chain(m.keyframes.iter().enumerate().map(|(ki, k)| (format!("{mp}/keyframes/{ki}"), m.at_key(k))));
        for (sp, s) in shapes {
            if ![s.x, s.y, s.w, s.h, s.rotation, s.feather].iter().all(|v| v.is_finite()) {
                issues.push(Issue::error(sp, "a mask's numbers must be finite"));
            } else if !(s.w > 0.0 && s.h > 0.0) {
                issues.push(Issue::error(sp, "a mask's `w` and `h` must be above 0: it is the rectangle `region` is, in composition pixels"));
            }
        }
        if m.feather < 0.0 {
            issues.push(Issue::error(format!("{mp}/feather"), "a mask's feather is pixels of soft edge, 0 or more"));
        }
        check_keyframes(issues, &mp, owner, m.keyframes.iter().map(|k| k.t));
        for (ki, k) in m.keyframes.iter().enumerate() {
            if let Some(name) = k.values.keys().find(|n| !Mask::KEYED.contains(&n.as_str())) {
                issues.push(Issue::error(format!("{mp}/keyframes/{ki}/{name}"), format!("a mask's keys may set {}; `{name}` holds still", Mask::KEYED.join(", "))));
            }
            if k.ease != Ease::Linear {
                issues.push(Issue::error(format!("{mp}/keyframes/{ki}/ease"), "a mask moves in straight lines between its keys: leave `ease` out"));
            }
        }
    }
    // A key naming a parameter the kind does not have is refused here rather than by serde
    // — the parameters *are* a keyframe's unknown fields — and this error can say what the
    // kind's parameters are.
    let keys = e.keyframes();
    check_keyframes(issues, ep, owner, keys.iter().map(|k| k.t));
    for (ki, k) in keys.iter().enumerate() {
        for name in k.values.keys() {
            if !e.animatable().contains(&name.as_str()) {
                issues.push(Issue::error(
                    format!("{ep}/keyframes/{ki}/{name}"),
                    format!("`{kind}` has no parameter `{name}`; its keyframes may set {}", e.animatable().join(", ")),
                ));
            }
        }
    }
    // Then the bounds each kind has: the statics, and each key as the compiler reads it.
    match e {
        Effect::Crop { left, right, top, bottom, radius, .. } => {
            for (path, k) in samples(ep, e) {
                let (l, r, t, b, rad) = (at(k, "left", *left), at(k, "right", *right), at(k, "top", *top), at(k, "bottom", *bottom), at(k, "radius", *radius));
                if l + r >= 1.0 || t + b >= 1.0 {
                    issues.push(Issue::error(path.clone(), "crop leaves nothing visible"));
                }
                if [l, r, t, b, rad].iter().any(|x| *x < 0.0 || *x > 1.0) {
                    issues.push(Issue::error(path, "crop values must be within 0..1"));
                }
            }
        }
        Effect::Transform { opacity, scale, .. } => {
            for (path, k) in samples(ep, e) {
                if !(0.0..=1.0).contains(&at(k, "opacity", *opacity)) {
                    issues.push(Issue::error(format!("{path}/opacity"), "opacity must be 0..1"));
                }
                let s = at(k, "scale", *scale);
                if !(s.is_finite() && s > 0.0) {
                    issues.push(Issue::error(format!("{path}/scale"), "scale must be > 0"));
                }
            }
        }
        Effect::Blur { radius, .. } => {
            for (path, k) in samples(ep, e) {
                if at(k, "radius", *radius) < 0.0 {
                    issues.push(Issue::error(path, "blur radius must be >= 0"));
                }
            }
        }
        Effect::Volume { gain, .. } => {
            for (path, k) in samples(ep, e) {
                let g = at(k, "gain", *gain);
                if !(g.is_finite() && g >= 0.0) {
                    issues.push(Issue::error(path, "volume gain must be >= 0"));
                }
            }
        }
        // The ranges ffmpeg's `alimiter` and MLT's `dynamic_loudness` take: outside them
        // the filter clamps or refuses, so the number would not mean what it says.
        Effect::Limiter { .. } | Effect::AutoLevel { .. } => {
            let ranges = match *e {
                Effect::Limiter { ceiling, attack, release } => vec![("ceiling", ceiling, -24.0, 0.0), ("attack", attack, 0.1, 80.0), ("release", release, 1.0, 8000.0)],
                Effect::AutoLevel { target, window, max_gain, min_gain } => {
                    vec![("target", target, -50.0, -10.0), ("window", window, 1.0, 60.0), ("max_gain", max_gain, 0.0, 30.0), ("min_gain", min_gain, -30.0, 0.0)]
                }
                _ => vec![],
            };
            for (name, v, lo, hi) in ranges {
                if !(lo..=hi).contains(&v) {
                    issues.push(Issue::error(format!("{ep}/{name}"), format!("{kind} {name} must be within {lo}..{hi}")));
                }
            }
        }
        Effect::Mlt { service, .. } => {
            if service.trim().is_empty() {
                issues.push(Issue::error(ep, "mlt effect needs a service name"));
            }
        }
        Effect::Grade(g) => check_grade(issues, ep, g),
        Effect::Blend { .. } | Effect::Color { .. } | Effect::Fade { .. } | Effect::AudioFade { .. } | Effect::Freeze { .. } => {}
    }
}

/// A grade's numbers: within the ranges its fields document, and a curve the compiler can
/// draw. Its LUT files are checked where the project's root is known, with the clip.
fn check_grade(issues: &mut Vec<Issue>, ep: &str, g: &Grade) {
    if g.log.is_some() && g.input_lut.is_some() {
        issues.push(Issue::error(format!("{ep}/log"), "a grade takes one input: a camera `log` or an `input_lut`, not both"));
    }
    let scalars = [
        ("temperature", g.temperature, Grade::TEMPERATURE),
        ("tint", g.tint, Grade::TINT),
        ("exposure", g.exposure, Grade::EXPOSURE),
        ("contrast", g.contrast, Grade::CONTRAST),
        ("saturation", g.saturation, Grade::SATURATION),
        ("look_amount", g.look_amount, Grade::LOOK_AMOUNT),
        ("blacks", g.blacks, Grade::TONE),
        ("shadows", g.shadows, Grade::TONE),
        ("highlights", g.highlights, Grade::TONE),
        ("whites", g.whites, Grade::TONE),
        ("vibrance", g.vibrance, Grade::TONE),
    ];
    for (name, v, range) in scalars {
        if !range.contains(&v) {
            issues.push(Issue::error(format!("{ep}/{name}"), format!("grade {name} must be within {}..{}", range.start(), range.end())));
        }
    }
    for (ki, k) in g.keyframes.iter().enumerate() {
        for (name, range) in [("temperature", Grade::TEMPERATURE), ("tint", Grade::TINT), ("exposure", Grade::EXPOSURE)] {
            if k.get(name).is_some_and(|v| !range.contains(&v)) {
                issues.push(Issue::error(format!("{ep}/keyframes/{ki}/{name}"), format!("grade {name} must be within {}..{}", range.start(), range.end())));
            }
        }
    }
    let wheel = Grade::WHEEL;
    for (name, rgb) in [("lift", g.lift), ("gamma", g.gamma), ("gain", g.gain)] {
        for (ch, v) in ["r", "g", "b"].into_iter().zip(rgb.channels()) {
            if !wheel.contains(&v) {
                issues.push(Issue::error(format!("{ep}/{name}/{ch}"), format!("grade {name} must be within {}..{} on each channel", wheel.start(), wheel.end())));
            }
        }
    }
    let tone = [("curve", &g.curve)].into_iter().chain(g.channel_curves());
    for (name, points) in tone {
        check_curve(issues, &format!("{ep}/{name}"), points, 0.0..=1.0, true, "[in, out], each within 0..1");
    }
    for (name, points, wraps) in g.hue_curves() {
        let what = if wraps { "[hue, amount]: hue within 0..1 and below 1, amount within -1..1" } else { "[at, amount]: at within 0..1, amount within -1..1" };
        check_curve(issues, &format!("{ep}/{name}"), points, Grade::TONE, !wraps, what);
    }
}

/// A grade curve's points: at most 32, `x` rising within 0..1 (below 1 for a curve that
/// wraps, where 1 is 0 again), `y` within `ys`.
fn check_curve(issues: &mut Vec<Issue>, fp: &str, points: &[[f64; 2]], ys: std::ops::RangeInclusive<f64>, one: bool, what: &str) {
    if points.len() > 32 {
        issues.push(Issue::error(fp.to_string(), "a grade's curve takes at most 32 points"));
    }
    let mut last = f64::NEG_INFINITY;
    for (i, [x, y]) in points.iter().enumerate() {
        if !(0.0..=1.0).contains(x) || (!one && *x == 1.0) || !ys.contains(y) {
            issues.push(Issue::error(format!("{fp}/{i}"), format!("curve points are {what}")));
        } else if *x <= last {
            issues.push(Issue::error(format!("{fp}/{i}"), "curve points must be in order of their first number, no two at the same one"));
        }
        last = *x;
    }
}

/// A grade's LUT files: `.cube`s under the project that parse. Read only when files are.
fn check_grade_luts(issues: &mut Vec<Issue>, ep: &str, g: &Grade, ctx: &ValidationContext) {
    for (field, path) in g.luts() {
        let fp = format!("{ep}/{field}");
        if !stays_inside(path) {
            issues.push(Issue::error(fp, "a LUT path must be relative and stay inside the project"));
        } else if !path.to_ascii_lowercase().ends_with(".cube") {
            issues.push(Issue::error(fp, "a LUT must be a `.cube` file"));
        } else if ctx.check_files {
            if let Err(e) = lut_reads(&ctx.root.join(path)) {
                issues.push(Issue::error(fp, format!("LUT `{path}`: {e}")));
            }
        }
    }
}

/// Whether the LUT at `path` reads, remembered per file as long as its size and time do
/// not change: every edit validates the whole project, and a 65³ `.cube` takes tens of
/// milliseconds to parse — per clip that names it, on every edit, unremembered.
fn lut_reads(path: &std::path::Path) -> Result<(), String> {
    type Seen = HashMap<PathBuf, (u64, Option<std::time::SystemTime>, Result<(), String>)>;
    static SEEN: std::sync::OnceLock<std::sync::Mutex<Seen>> = std::sync::OnceLock::new();
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = (meta.len(), meta.modified().ok());
    let seen = SEEN.get_or_init(Default::default);
    if let Some((len, time, read)) = seen.lock().unwrap_or_else(|p| p.into_inner()).get(path) {
        if (*len, *time) == stamp {
            return read.clone();
        }
    }
    let read = crate::grade::Lut::load(path).map(drop);
    seen.lock().unwrap_or_else(|p| p.into_inner()).insert(path.to_path_buf(), (stamp.0, stamp.1, read.clone()));
    read
}

/// Whether `path` is relative and never climbs out of the directory it is read from — the
/// test for a file the project names that must be the project's own (a LUT, a luma).
pub fn stays_inside(path: &str) -> bool {
    let p = std::path::Path::new(path);
    !p.is_absolute() && p.components().all(|c| matches!(c, std::path::Component::Normal(_))) && !path.is_empty()
}

/// A sequence's own effects, which act on its mix: only the kinds that can, once each, and
/// keyed in the sequence's own seconds.
fn check_sequence_effects(issues: &mut Vec<Issue>, p: &Project, seq_id: &str, seq: &Sequence) {
    let mut kinds = HashSet::new();
    // A walk of everything nested, so only for a sequence that has effects.
    let sounds = !seq.effects.is_empty() && p.seq_has_audio(seq_id);
    for (ei, e) in seq.effects.iter().enumerate() {
        let ep = format!("/bin/{seq_id}/source/effects/{ei}");
        let kind = e.kind();
        if !e.fits_a_sequence() {
            issues.push(Issue::error(
                ep,
                format!("a sequence carries only effects on its mix (`volume`, `limiter`, `auto_level`); put `{kind}` on the clips that place it"),
            ));
            continue;
        }
        if !kinds.insert(kind) {
            issues.push(Issue::error(ep.clone(), format!("duplicate `{kind}` effect on this sequence")));
        }
        if !sounds {
            issues.push(Issue::warn(ep.clone(), format!("`{kind}` does nothing here: this sequence has no sound")));
        }
        check_effect(issues, &ep, e, kind, ("sequence", seq.duration()));
    }
}

/// How much material a clip's entry has, when that is knowable: the probed duration of a
/// media file, or a sequence's own length. `None` for a still, a colour, an overlay or an
/// unprobed file, all of which are generated to whatever length is asked of them.
fn source_duration(p: &Project, c: &Clip) -> Option<f64> {
    let item = p.item(c)?;
    if item.is_sequence() {
        Some(p.seq_duration(&c.bin))
    } else if item.is_timed_media() {
        item.meta.as_ref().and_then(|m| m.duration)
    } else {
        None
    }
}

/// Every point an effect is evaluated at — its static values (`None`), then each keyframe
/// — with the path an issue about that sample should name. Read a parameter there with
/// [`at`]: bounds are checked on every sample, so a key cannot smuggle in a value the
/// static field would have been refused for.
fn samples<'a>(ep: &'a str, e: &'a Effect) -> impl Iterator<Item = (String, Option<&'a Keyframe>)> + 'a {
    std::iter::once((ep.to_string(), None)).chain(e.keyframes().iter().enumerate().map(move |(ki, k)| (format!("{ep}/keyframes/{ki}"), Some(k))))
}

/// A parameter at a sample: the static value, or what the key says with the static
/// filling in what it leaves out — exactly as the compiler reads it.
fn at(k: Option<&Keyframe>, name: &str, stat: f64) -> f64 {
    k.map_or(stat, |k| k.or(name, stat))
}

/// Keyframe times must be non-negative and sorted; beyond the end of the clip (or the
/// sequence) that carries them they are pointless. `owner` is which, and how long.
fn check_keyframes(issues: &mut Vec<Issue>, ep: &str, (owner, duration): (&str, f64), times: impl Iterator<Item = f64>) {
    let mut last = f64::NEG_INFINITY;
    for (ki, t) in times.enumerate() {
        if t < 0.0 || t < last {
            issues.push(Issue::error(format!("{ep}/keyframes/{ki}/t"), "keyframes must be >= 0 and sorted by t"));
        }
        last = t;
        if t > duration + 1e-9 {
            issues.push(Issue::warn(format!("{ep}/keyframes/{ki}/t"), format!("keyframe beyond {owner} end")));
        }
    }
}

/// Well-formed, or an error at `path`. True when it is.
fn check_id_format(issues: &mut Vec<Issue>, id: &str, path: String, what: &str) -> bool {
    let ok = is_valid_id(id);
    if !ok {
        issues.push(Issue::error(path, format!("{what} id `{id}` must match [A-Za-z_][A-Za-z0-9_-]*")));
    }
    ok
}

/// Well-formed and not seen before.
fn check_id<'a>(issues: &mut Vec<Issue>, seen: &mut HashSet<&'a str>, id: &'a str, path: String, what: &str) {
    if check_id_format(issues, id, path.clone(), what) && !seen.insert(id) {
        issues.push(Issue::error(path, format!("duplicate {what} id `{id}`")));
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a project from `{bin, tracks, …}`, folding a bare `tracks` into the master
    /// sequence entry. Most of these fixtures are about one timeline, and saying so twice
    /// would bury what each test is actually checking.
    fn project(mut v: serde_json::Value) -> Project {
        if let Some(tracks) = v.as_object_mut().and_then(|o| o.remove("tracks")) {
            v["bin"]["main"] = json!({ "name": "Main", "source": { "tracks": tracks } });
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_folder_parents_and_is_never_placed() {
        let p = project(json!({
            "bin": { "f": { "name": "Speech", "source": { "folder": true } }, "c": { "source": { "color": "#000" }, "parent": "f" } },
            "tracks": [{ "id": "v1", "clips": [ { "id": "x", "start": 0, "end": 1, "bin": "f" } ] }]
        }));
        assert!(p.bin["f"].is_folder() && !p.bin["f"].is_timed_media());
        let issues = validate(&p, &Default::default());
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].path == clip_path("main", 0, 0) + "/bin" && issues[0].message.contains("folder"));
    }

    #[test]
    fn a_parent_must_exist_and_not_ring() {
        let entry = |parent: Option<&str>| json!({ "source": { "color": "#000" }, "parent": parent });
        let ok = project(json!({ "bin": { "a": entry(None), "b": entry(Some("a")), "c": entry(Some("b")) }, "tracks": [] }));
        assert!(validate(&ok, &Default::default()).is_empty());
        assert_eq!(ok.children(Some("a")).into_iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["b"]);
        let dangling = project(json!({ "bin": { "b": entry(Some("gone")) }, "tracks": [] }));
        assert!(validate(&dangling, &Default::default()).iter().any(|i| i.path == "/bin/b/parent" && i.message.contains("not in the bin")));
        let ring = project(json!({ "bin": { "a": entry(Some("b")), "b": entry(Some("a")), "s": entry(Some("s")) }, "tracks": [] }));
        let bad: Vec<_> = validate(&ring, &Default::default()).into_iter().filter(|i| i.path.ends_with("/parent")).collect();
        assert_eq!(bad.len(), 3, "{bad:?}");
    }

    /// A video track may sit under an overlay track, never over one; audio tracks may go
    /// anywhere, since they composite nothing.
    #[test]
    fn a_video_track_never_goes_over_an_overlay_track() {
        let tracks = |kinds: &[&str]| json!(kinds.iter().enumerate().map(|(i, k)| json!({ "id": format!("t{i}"), "kind": k })).collect::<Vec<_>>());
        let errors = |kinds: &[&str]| -> Vec<String> {
            validate(&project(json!({ "bin": {}, "tracks": tracks(kinds) })), &Default::default()).into_iter().filter(|i| i.severity == Severity::Error).map(|i| i.path).collect()
        };
        assert!(errors(&["video", "video", "overlay", "overlay", "audio"]).is_empty());
        assert!(errors(&["audio", "video", "overlay", "audio", "overlay"]).is_empty());
        assert_eq!(errors(&["video", "overlay", "video", "audio", "video"]), ["/bin/main/source/tracks/2", "/bin/main/source/tracks/4"]);
    }

    /// Where a clip lives in the document now that timelines hang off the bin.
    fn clip_path(seq: &str, ti: usize, ci: usize) -> String {
        format!("/bin/{seq}/source/tracks/{ti}/clips/{ci}")
    }

    #[test]
    fn overlap_and_missing_bin_entry_are_errors() {
        let p = project(json!({
            "bin": { "black": { "source": { "color": "#000" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "nope" },
                { "id": "b", "start": 4, "end": 8, "bin": "black" }
            ]}]
        }));
        let issues = validate(&p, &ValidationContext::default());
        let msgs: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("unknown bin entry")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("overlaps")), "{msgs:?}");
    }

    /// The master sequence's tracks, mutably: fixtures poke at them between assertions.
    fn main_tracks(p: &mut Project) -> &mut Vec<Track> {
        match &mut p.bin.get_mut("main").unwrap().source {
            BinSource::Sequence(s) => &mut s.tracks,
            _ => unreachable!(),
        }
    }

    fn messages(p: &Project) -> Vec<String> {
        validate(p, &ValidationContext::default()).into_iter().map(|i| i.message).collect()
    }

    /// A group holds clips together for editing, on one timeline: the same id on two
    /// sequences names a block that could never move as one, so it is refused.
    #[test]
    fn a_group_must_be_on_one_timeline() {
        let p = project(json!({
            "master": "main",
            "bin": {
                "x": { "source": { "path": "x.mp4" } },
                "other": { "source": { "markers": [], "tracks": [
                    { "id": "v2", "clips": [{ "id": "b", "start": 0, "end": 5, "bin": "x", "group": "g1" }] }
                ] } }
            },
            "tracks": [{ "id": "v1", "clips": [{ "id": "a", "start": 0, "end": 5, "bin": "x", "group": "g1" }] }]
        }));
        let m = messages(&p);
        assert!(m.iter().any(|m| m.contains("group `g1`") && m.contains("one timeline")), "{m:?}");

        // …and a group on one timeline, across its tracks, is the whole point.
        let ok = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [
                { "id": "v1", "clips": [{ "id": "a", "start": 0, "end": 5, "bin": "x", "group": "g1" }] },
                { "id": "a1", "kind": "audio", "clips": [{ "id": "b", "start": 0, "end": 5, "bin": "x", "group": "g1" }] },
            ]
        }));
        assert!(!has_errors(&validate(&ok, &ValidationContext::default())), "{:?}", messages(&ok));
    }

    /// No group is the absent field, so grouping nothing writes nothing.
    #[test]
    fn a_group_is_not_written_when_there_is_none() {
        let c: Clip = serde_json::from_value(json!({ "id": "a", "start": 0, "end": 5, "bin": "x" })).unwrap();
        assert_eq!(c.group, "");
        assert_eq!(serde_json::to_value(&c).unwrap().get("group"), None);
    }

    #[test]
    fn only_timed_media_has_a_volume() {
        let p = project(json!({
            "bin": {
                "song": { "source": { "path": "song.wav", "kind": "audio" }, "volume": 0.5 },
                "loud": { "source": { "path": "loud.wav", "kind": "audio" }, "volume": -1 },
                "logo": { "source": { "path": "logo.png", "kind": "image" }, "volume": 0.5 }
            },
            "tracks": []
        }));
        let issues = validate(&p, &ValidationContext::default());
        let at = |id: &str| issues.iter().filter(|i| i.path == format!("/bin/{id}/volume")).count();
        assert_eq!((at("song"), at("loud"), at("logo")), (0, 1, 1), "{issues:?}");
    }

    /// An SVG already in the bin is said, not refused: the project stays editable, and the
    /// warning says how to get a picture that renders.
    #[test]
    fn an_svg_entry_is_a_warning_that_says_how_to_rasterise_it() {
        let p = project(json!({ "bin": { "logo": { "source": { "path": "art/logo.svg", "kind": "image" } } }, "tracks": [] }));
        let issues = validate(&p, &ValidationContext::default());
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0].severity, issues[0].path.as_str()), (Severity::Warning, "/bin/logo/source/path"));
        assert!(issues[0].message.contains("rasterise"), "{issues:?}");
    }

    /// A sequence's effects act on its mix, so it takes only the kinds that do — once each,
    /// keyed in its own seconds, bounded like anywhere else — and refuses the rest saying
    /// where they go instead.
    #[test]
    fn a_sequence_carries_only_effects_on_its_mix() {
        let p = project(json!({
            "bin": {
                "song": { "source": { "path": "song.wav", "kind": "audio" } },
                "red": { "source": { "color": "#f00" } },
                "quiet": { "source": { "effects": [ { "type": "limiter" } ], "tracks": [
                    { "id": "qv", "clips": [ { "id": "q1", "start": 0, "end": 2, "bin": "red" } ] }
                ] } }
            },
            "tracks": [{ "id": "a1", "kind": "audio", "clips": [ { "id": "a", "start": 0, "end": 4, "bin": "song" } ] }]
        }));
        let mut p = p;
        let BinSource::Sequence(main) = &mut p.bin.get_mut("main").unwrap().source else { unreachable!() };
        main.effects = serde_json::from_value(json!([
            { "type": "volume", "gain": 2, "keyframes": [ { "t": 0, "gain": 2 }, { "t": 5, "gain": 1 } ] },
            { "type": "limiter", "ceiling": -1.5 },
            { "type": "blur", "radius": 2 },
            { "type": "limiter", "ceiling": 3 }
        ]))
        .unwrap();
        let issues = validate(&p, &ValidationContext::default());
        let at = |path: &str| issues.iter().filter(|i| i.path.starts_with(&format!("/bin/main/source/effects/{path}"))).map(|i| (i.severity, i.message.as_str())).collect::<Vec<_>>();
        assert_eq!(at("0"), [(Severity::Warning, "keyframe beyond sequence end")], "the master is 4 s long");
        assert!(at("1").is_empty(), "{issues:?}");
        assert!(matches!(at("2")[..], [(Severity::Error, m)] if m.contains("only effects on its mix") && m.contains("`blur`")), "{issues:?}");
        let fourth = at("3");
        assert!(fourth.iter().any(|(_, m)| m.contains("duplicate `limiter`")) && fourth.iter().any(|(_, m)| m.contains("ceiling must be within -24..0")), "{fourth:?}");
        // A sequence with nothing to sound has nothing for its limiter to act on.
        assert!(issues.iter().any(|i| i.path == "/bin/quiet/source/effects/0" && i.severity == Severity::Warning && i.message.contains("no sound")), "{issues:?}");
    }

    /// A clip is short of its source only by a whole frame: MLT plays a frame that starts
    /// inside the source. A 0.366 s sound placed for 11 frames at 30 fps reads all of it,
    /// and a sequence placed for its exact length is not read past by a rounding error.
    #[test]
    fn a_clip_is_short_of_its_source_only_by_a_whole_frame() {
        let p = project(json!({
            "fps": 30,
            "bin": {
                "sfx": { "source": { "path": "sfx.wav", "kind": "audio" }, "meta": { "duration": 0.366, "has_audio": true } },
                "clip": { "source": { "path": "clip.mp4" }, "meta": { "duration": 1.0, "has_audio": true } },
                "red": { "source": { "color": "#f00" } },
                "part": { "source": { "tracks": [ { "id": "pv", "clips": [ { "id": "p1", "start": 0, "end": 25.3, "bin": "red" } ] } ] } }
            },
            "tracks": [
                { "id": "a1", "kind": "audio", "clips": [
                    { "id": "s1", "start": 0, "end": 0.37, "bin": "sfx" },
                    { "id": "c1", "start": 1, "end": 2.07, "bin": "clip" }
                ] },
                { "id": "v1", "clips": [ { "id": "n1", "start": 0, "end": 15.2004, "bin": "part", "in": 10.1 } ] }
            ]
        }));
        let short: Vec<_> = validate(&p, &ValidationContext::default()).into_iter().filter(|i| i.path.ends_with("/end")).collect();
        assert_eq!(short.len(), 1, "{short:?}");
        assert!(short[0].path.ends_with("tracks/0/clips/1/end") && short[0].message.contains("last 2 frames"), "{short:?}");
    }

    /// A freeze holds a frame the clip's source has; one outside it is held at the nearest,
    /// which is worth a warning rather than a refusal of the edit that moved the clip.
    #[test]
    fn a_freeze_outside_its_source_is_a_warning() {
        let p = project(json!({
            "bin": {
                "clip": { "source": { "path": "clip.mp4" }, "meta": { "duration": 3.0, "has_audio": false } },
                "red": { "source": { "color": "#f00" } }
            },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 1, "bin": "clip", "in": 1, "effects": [ { "type": "freeze", "t": -0.5 } ] },
                { "id": "b", "start": 1, "end": 2, "bin": "clip", "in": 1, "effects": [ { "type": "freeze", "t": 2.5 } ] },
                { "id": "c", "start": 2, "end": 3, "bin": "red", "effects": [ { "type": "freeze", "t": 1.5 } ] },
                { "id": "d", "start": 3, "end": 4, "bin": "clip", "effects": [ { "type": "freeze", "t": -0.1 } ] }
            ] }]
        }));
        let issues = validate(&p, &ValidationContext::default());
        let warned = |ci: usize| issues.iter().any(|i| i.path == format!("{}/effects/0/t", clip_path("main", 0, ci)) && i.severity == Severity::Warning);
        assert_eq!((warned(0), warned(1), warned(2), warned(3)), (false, true, true, true), "{issues:?}");
    }

    /// A grade's numbers stay in their ranges, its curve rises, and its LUTs are `.cube`s
    /// inside the project that parse — read only when files are.
    #[test]
    fn a_grade_is_held_to_its_ranges_and_its_luts_to_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        std::fs::write(dir.path().join("short.cube"), "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
        let grades = json!([
            { "type": "grade", "temperature": 0.5, "gain": { "r": 0.2 }, "curve": [[0.2, 0.1], [0.8, 0.9]], "input_lut": "good.cube" },
            { "type": "grade", "exposure": 6, "saturation": -1, "lift": { "g": 1.5 } },
            { "type": "grade", "curve": [[0.5, 0.5], [0.4, 0.6], [0.9, 1.2]] },
            { "type": "grade", "input_lut": "../elsewhere.cube", "look": "look.png" },
            { "type": "grade", "input_lut": "missing.cube", "look": "short.cube" },
            { "type": "grade", "log": "s-log3-cine", "input_lut": "good.cube", "shadows": 0.5, "keyframes": [{ "t": 0, "exposure": 9 }, { "t": 0.5, "saturation": 1 }], "vibrance": -2, "curve_g": [[0.5, 1.5]],
              "hue_sat": [[0.2, 0.5], [1.0, 0.2]], "hue_hue": [[0.0, 1.0], [0.6, -0.4]], "sat_sat": [[0.0, 0.5], [1.0, -1.5]] }
        ]);
        let tracks: Vec<_> = (0..6).map(|i| json!({ "id": format!("v{i}"), "clips": [
            { "id": format!("c{i}"), "start": 0, "end": 1, "bin": "x", "effects": [grades[i].clone()] }
        ] })).collect();
        let p = project(json!({ "bin": { "x": { "source": { "path": "x.mp4" } } }, "tracks": tracks }));
        let ctx = ValidationContext { root: dir.path().to_path_buf(), check_files: true };
        let issues = validate(&p, &ctx);
        let on = |ti: usize| -> Vec<String> {
            let prefix = format!("{}/effects/0/", clip_path("main", ti, 0));
            issues.iter().filter(|i| i.path.starts_with(&prefix) && i.severity == Severity::Error).map(|i| format!("{} {}", &i.path[prefix.len()..], i.message)).collect()
        };
        assert!(on(0).is_empty(), "{:?}", on(0));
        let paths = |ti: usize| on(ti).into_iter().map(|m| m.split(' ').next().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(paths(1), ["exposure", "saturation", "lift/g"], "{:?}", on(1));
        assert_eq!(paths(2), ["curve/1", "curve/2"], "{:?}", on(2));
        assert!(on(3).iter().any(|m| m.starts_with("input_lut") && m.contains("inside the project")) && on(3).iter().any(|m| m.starts_with("look") && m.contains(".cube")), "{:?}", on(3));
        assert!(on(4).iter().any(|m| m.starts_with("input_lut") && m.contains("missing.cube")) && on(4).iter().any(|m| m.starts_with("look") && m.contains("needs 8 rows")), "{:?}", on(4));
        // The later steps: a hue curve wraps, so 1 is 0 again; the others reach 1.
        assert_eq!(paths(5), ["keyframes/1/saturation", "log", "vibrance", "keyframes/0/exposure", "curve_g/0", "hue_sat/1", "sat_sat/1"], "{:?}", on(5));
        // The compiler's check reads no files, so what is on disk cannot fail it.
        let unread = validate(&p, &ValidationContext { check_files: false, ..ctx });
        assert!(!unread.iter().any(|i| i.path.contains("/tracks/4/")), "{unread:?}");
    }

    /// A clip has one of each kind over its whole picture and any number masked; a mask is
    /// a rectangle with a size.
    #[test]
    fn masked_effects_may_repeat_and_a_mask_has_a_size() {
        let window = json!({ "x": 10, "y": 10, "w": 100, "h": 80 });
        let p = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 1, "bin": "x", "effects": [
                    { "type": "grade", "exposure": 0.5 },
                    { "type": "grade", "exposure": -0.5, "mask": window },
                    { "type": "grade", "saturation": 0.5, "mask": { "x": 0, "y": 0, "w": 50, "h": 50, "shape": "rectangle", "invert": true } },
                    { "type": "blur", "radius": 12, "mask": window },
                    { "type": "blur", "radius": 2 }
                ] },
                { "id": "b", "start": 1, "end": 2, "bin": "x", "effects": [
                    { "type": "color", "brightness": 0.1, "mask": { "x": 0, "y": 0, "w": 0, "h": 50 } },
                    { "type": "blur", "radius": 1, "mask": { "x": 0, "y": 0, "w": 5, "h": 5, "feather": -1 } },
                    { "type": "grade" }, { "type": "grade" }
                ] },
                { "id": "c", "start": 2, "end": 3, "bin": "x", "effects": [
                    { "type": "blur", "radius": 4, "mask": { "x": 0, "y": 0, "w": 50, "h": 50, "keyframes": [
                        { "t": 0, "x": 10 }, { "t": 0.5, "w": 0, "ease": "smooth" }, { "t": 0.8, "feather": 9 }
                    ] } }
                ] }
            ] }]
        }));
        let m = messages(&p);
        assert_eq!(m.len(), 6, "{m:?}");
        assert!(m.iter().any(|m| m.contains("straight lines")) && m.iter().any(|m| m.contains("`feather` holds still")), "{m:?}");
        assert!(m.iter().any(|m| m.contains("`w` and `h` must be above 0")), "{m:?}");
        assert!(m.iter().any(|m| m.contains("feather")), "{m:?}");
        assert!(m.iter().any(|m| m.contains("duplicate `grade`")), "two over the whole picture still clash: {m:?}");
    }

    /// A keyframe's parameters are its unknown fields, so the validator is what says a
    /// name is wrong — and it says which names are right.
    #[test]
    fn a_keyframe_may_only_set_parameters_its_effect_has() {
        let p = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [{ "id": "a", "start": 0, "end": 5, "bin": "x", "effects": [
                { "type": "blur", "radius": 2, "keyframes": [ { "t": 0, "radius": 0 }, { "t": 1, "sigma": 9 } ] }
            ] }] }]
        }));
        let m = messages(&p);
        assert!(m.iter().any(|m| m.contains("`blur` has no parameter `sigma`") && m.contains("radius")), "{m:?}");
    }

    /// An effect with nothing to act on renders and does nothing, so it is a warning that
    /// says what is missing — never an error, which would refuse unrelated edits.
    #[test]
    fn an_effect_that_cannot_act_on_its_clip_is_a_warning() {
        let p = project(json!({
            "bin": {
                "song": { "source": { "path": "song.mp3", "kind": "audio" } },
                "red": { "source": { "color": "#f00" } }
            },
            "tracks": [
                { "id": "v1", "clips": [{ "id": "a", "start": 0, "end": 5, "bin": "red", "effects": [
                    { "type": "volume", "gain": 0.5 }, { "type": "fade", "in": 1 }
                ] }] },
                { "id": "a1", "kind": "audio", "clips": [{ "id": "b", "start": 0, "end": 5, "bin": "song", "effects": [
                    { "type": "fade", "in": 1 }, { "type": "audio_fade", "in": 1, "out": 1 },
                    { "type": "volume", "gain": -1, "keyframes": [ { "t": 0, "gain": 2 } ] }
                ] }] }
            ]
        }));
        let issues = validate(&p, &ValidationContext::default());
        let at = |path: &str| issues.iter().filter(|i| i.path.ends_with(path)).map(|i| (i.severity, i.message.as_str())).collect::<Vec<_>>();
        assert_eq!(at("tracks/0/clips/0/effects/0"), [(Severity::Warning, "`volume` does nothing here: this clip has no sound")]);
        assert!(at("tracks/0/clips/0/effects/1").is_empty(), "a colour fades");
        assert_eq!(at("1/clips/0/effects/0"), [(Severity::Warning, "`fade` does nothing here: this clip shows no picture")]);
        assert!(at("1/clips/0/effects/1").is_empty(), "a song fades its sound");
        // A static volume is bounded like any other static, and each key on what it means.
        assert_eq!(at("1/clips/0/effects/2"), [(Severity::Error, "volume gain must be >= 0")]);
        assert!(at("effects/2/keyframes/0").is_empty(), "{issues:?}");
    }

    /// The bounds a static value gets, a key gets too — on what the key *means*, with the
    /// statics filling in what it leaves out.
    #[test]
    fn a_keyframe_is_held_to_the_same_bounds_as_the_static_value() {
        let p = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [{ "id": "a", "start": 0, "end": 5, "bin": "x", "effects": [
                // Static crop is fine; the second key, with `right` filled in from the
                // static 0.5, leaves nothing visible.
                { "type": "crop", "left": 0.2, "right": 0.5, "keyframes": [ { "t": 0, "left": 0.2 }, { "t": 2, "left": 0.6 } ] },
                { "type": "blur", "radius": 2, "keyframes": [ { "t": 0, "radius": -1 } ] }
            ] }] }]
        }));
        let issues = validate(&p, &ValidationContext::default());
        assert!(issues.iter().any(|i| i.path.ends_with("/effects/0/keyframes/1") && i.message.contains("nothing visible")), "{issues:?}");
        assert!(issues.iter().any(|i| i.path.ends_with("/effects/1/keyframes/0") && i.message.contains(">= 0")), "{issues:?}");
        assert!(!issues.iter().any(|i| i.path.ends_with("/effects/0/keyframes/0")), "the first key is within bounds: {issues:?}");
    }

    #[test]
    fn transition_needs_handle_in_source_time() {
        let p = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "x" },
                { "id": "b", "start": 5, "end": 8, "bin": "x", "in": 0.1 }
            ], "transitions": [ { "from": "a", "to": "b", "duration": 1.0 } ]}]
        }));
        assert!(messages(&p).iter().any(|m| m.contains("handle")));
        // 0.5 s of handle is enough at 1×, not at 2×.
        let mut p2 = p.clone();
        main_tracks(&mut p2)[0].clips[1].source_in = 0.5;
        assert!(!messages(&p2).iter().any(|m| m.contains("handle")));
        main_tracks(&mut p2)[0].clips[1].speed = 2.0;
        assert!(messages(&p2).iter().any(|m| m.contains("handle")));
    }

    #[test]
    fn chained_transitions_must_fit_the_middle_clip() {
        let p = project(json!({
            "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "x" },
                { "id": "b", "start": 5, "end": 6, "bin": "x", "in": 2 },
                { "id": "c", "start": 6, "end": 10, "bin": "x", "in": 2 }
            ], "transitions": [ { "from": "a", "to": "b", "duration": 1.5 }, { "from": "b", "to": "c", "duration": 1.5 } ]}]
        }));
        assert!(messages(&p).iter().any(|m| m.contains("longer than the clip")));
    }

    #[test]
    fn ids_sub_frame_clips_and_colours_are_checked() {
        let p = project(json!({
            "fps": 30,
            "bin": { "red": { "source": { "color": "#red" } } },
            "tracks": [{ "id": "v 1", "clips": [
                { "id": "1a", "start": 0, "end": 0.01, "bin": "red" }
            ]}]
        }));
        let m = messages(&p);
        assert!(m.iter().any(|m| m.contains("track id")) && m.iter().any(|m| m.contains("clip id")), "{m:?}");
        assert!(m.iter().any(|m| m.contains("shorter than one frame")), "{m:?}");
        assert!(m.iter().any(|m| m.contains("colour")), "{m:?}");
    }

    #[test]
    fn in_is_rejected_on_a_generated_entry() {
        let p = project(json!({
            "bin": { "black": { "source": { "color": "#000000" } }, "v": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "black", "in": 2 },
                { "id": "b", "start": 5, "end": 9, "bin": "v", "in": 2 }
            ]}]
        }));
        let m = messages(&p);
        assert_eq!(m.iter().filter(|m| m.contains("in applies only")).count(), 1, "{m:?}");
    }

    #[test]
    fn overlay_vars_are_checked_once_per_entry_not_per_clip() {
        // Two clips of one entry must not report the same bad var twice.
        let tmp = tempfile::tempdir().unwrap();
        overlay_folder(tmp.path(), "overlays/location-tag");
        let p = project(json!({
            "bin": { "tag": { "source": { "overlay": "overlays/location-tag", "vars": { "nope": 1 } } } },
            "tracks": [{ "id": "o1", "kind": "overlay", "clips": [
                { "id": "a", "start": 0, "end": 4, "bin": "tag" },
                { "id": "b", "start": 4, "end": 8, "bin": "tag" }
            ]}]
        }));
        let ctx = ValidationContext { root: tmp.path().to_path_buf(), check_files: true };
        let m: Vec<_> = validate(&p, &ctx).into_iter().map(|i| i.message).collect();
        assert_eq!(m.iter().filter(|m| m.contains("nope")).count(), 1, "{m:?}");
    }

    /// Track kinds change what the compiler emits, so the rules differ by direction:
    /// only the combination that renders wrongly is refused.
    #[test]
    fn a_clip_is_checked_against_its_track_kind() {
        let p = project(json!({
            "bin": {
                "song": { "source": { "path": "song.mp3", "kind": "audio" } },
                "cam":  { "source": { "path": "cam.mp4", "kind": "video" } }
            },
            "tracks": [
                { "id": "v1", "kind": "video", "clips": [ { "id": "a", "start": 0, "end": 4, "bin": "song" } ] },
                { "id": "a1", "kind": "audio", "clips": [ { "id": "b", "start": 0, "end": 4, "bin": "cam" } ] },
                { "id": "o1", "kind": "overlay", "clips": [ { "id": "c", "start": 0, "end": 4, "bin": "cam" } ] }
            ]
        }));
        let issues = validate(&p, &ValidationContext::default());
        let at = |ti: usize| issues.iter().find(|i| i.path == format!("{}/bin", clip_path("main", ti, 0)));

        // Audio on a video track renders fine but hides nothing and shows nothing: warn.
        let w = at(0).expect("audio on a video track is flagged");
        assert_eq!(w.severity, Severity::Warning);
        assert!(w.message.contains("no video"), "{w:?}");
        // Video on an audio track is the deliberate "use only the audio" case: silent.
        assert!(at(1).is_none(), "{issues:?}");
        // Anything but an overlay on an overlay track is invisible in preview: error.
        assert_eq!(at(2).expect("checked").severity, Severity::Error);
    }

    fn overlay_folder(root: &std::path::Path, rel: &str) {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::manifest::MANIFEST), "{}").unwrap();
        std::fs::write(dir.join("index.html"), "<div data-composition-id=\"main\"></div>").unwrap();
    }

    /// An agent must not be able to add an overlay whose folder is not there: it used to
    /// pass validation and fail much later, at bake time.
    #[test]
    fn an_overlay_needs_its_folder_in_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project(json!({
            "bin": { "ghost": { "source": { "overlay": "overlays/ghost" } } },
            "tracks": [{ "id": "o1", "kind": "overlay", "clips": [
                { "id": "g1", "start": 0, "end": 4, "bin": "ghost" }
            ]}]
        }));
        let daemon = ValidationContext { root: tmp.path().to_path_buf(), check_files: true };
        let bad = |ctx: &ValidationContext| validate(&p, ctx).into_iter().filter(|i| i.severity == Severity::Error && i.path.ends_with("/source/overlay")).count();
        assert_eq!(bad(&daemon), 1);
        // The compiler reads nothing from disk and must stay silent about folders.
        assert_eq!(bad(&ValidationContext::default()), 0, "compiler path must not read overlay folders");

        overlay_folder(tmp.path(), "overlays/ghost");
        assert_eq!(bad(&daemon), 0);
    }

    /// A folder outside the project is refused even without disk access.
    #[test]
    fn an_overlay_folder_outside_the_project_is_refused() {
        let p = project(json!({
            "bin": { "far": { "source": { "overlay": "../elsewhere/title" } } },
            "tracks": []
        }));
        let issues = validate(&p, &ValidationContext::default());
        assert!(issues.iter().any(|i| i.severity == Severity::Error && i.path == "/bin/far/source/overlay"), "{issues:?}");
    }

    /// A library is named once, by a path; one not there, or not a library, is a warning —
    /// a drive not mounted leaves the project editable.
    #[test]
    fn libraries_are_named_once_and_missing_ones_warn() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("house/presets")).unwrap();
        std::fs::create_dir_all(tmp.path().join("plain")).unwrap();
        let p = Project { libraries: ["house", "plain", "gone", "house", " "].map(String::from).to_vec(), ..Default::default() };
        let ctx = ValidationContext { root: tmp.path().to_path_buf(), check_files: true };
        let found: Vec<(Severity, String)> = validate(&p, &ctx).into_iter().filter(|i| i.path.starts_with("/libraries")).map(|i| (i.severity, i.path)).collect();
        let at = |i: usize| format!("/libraries/{i}");
        assert_eq!(found, [(Severity::Warning, at(1)), (Severity::Warning, at(2)), (Severity::Error, at(3)), (Severity::Error, at(4))]);
        assert!(validate(&p, &ValidationContext::default()).iter().filter(|i| i.path.starts_with("/libraries")).all(|i| i.severity == Severity::Error), "nothing read off the disk unless asked");
    }
}
