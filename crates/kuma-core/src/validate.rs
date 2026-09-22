//! Structural validation. Errors reject an edit; warnings are surfaced to the UI/agent.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::template::TemplateManifest;

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
    /// Project directory; bin paths resolve against it.
    pub root: PathBuf,
    /// Loaded overlay templates by id.
    ///
    /// `None` means the caller has no template knowledge and overlay ids are left
    /// unchecked — that is the compiler, which knows nothing about the library. `Some`
    /// means checked against exactly this set, so an id that is not in it is an error
    /// **even when the set is empty**. Inferring the difference from emptiness instead
    /// let a daemon with no library installed wave every overlay through as a warning.
    pub templates: Option<BTreeMap<String, (PathBuf, TemplateManifest)>>,
    /// If false, missing media files are not checked at all (useful for tests).
    pub check_files: bool,
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

    // Bin entries are checked once each, however many clips place them. Overlay vars are
    // checked here rather than per clip, because the entry is what carries them now.
    let mut bin_ids = HashSet::new();
    for (id, item) in &p.bin {
        let ip = format!("/bin/{id}");
        check_id(&mut issues, &mut bin_ids, id, format!("{ip}/id"), "bin");
        match &item.source {
            BinSource::File { path, .. } => {
                if ctx.check_files {
                    let full = if std::path::Path::new(path).is_absolute() { PathBuf::from(path) } else { ctx.root.join(path) };
                    if !full.is_file() {
                        issues.push(Issue::error(format!("{ip}/source/path"), format!("file not found: {path}")));
                    }
                }
            }
            BinSource::Overlay { template, vars } => match ctx.templates.as_ref() {
                // The compiler validates structure without knowing the library.
                None => {}
                Some(loaded) => match loaded.get(template) {
                    None if loaded.is_empty() => issues.push(Issue::error(
                        format!("{ip}/source/template"),
                        format!("unknown template `{template}`: no templates are loaded at all — install the library with `kuma library`"),
                    )),
                    None => issues.push(Issue::error(format!("{ip}/source/template"), format!("unknown template `{template}`"))),
                    Some((_, m)) => {
                        for problem in m.check_vars(vars) {
                            issues.push(Issue::error(format!("{ip}/source/vars"), problem));
                        }
                    }
                },
            },
            BinSource::Color { color } => {
                if !crate::template::is_css_color(color) {
                    issues.push(Issue::error(format!("{ip}/source/color"), "colour must be #rgb, #rrggbb or #rrggbbaa"));
                }
            }
            // A sequence's own contents are checked below, with every other timeline.
            BinSource::Sequence(_) | BinSource::Folder { .. } => {}
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
                            let need = c.source_in + c.duration() * c.speed.abs();
                            if need > d + 1e-6 {
                                issues.push(Issue::warn(
                                    format!("{cp}/end"),
                                    format!("clip reads {need:.2}s of a {d:.2}s source; tail will be black/silent"),
                                ));
                            }
                        }
                        None => {}
                    }
                }
            }
            // A second effect of these kinds would stack (two volume filters square the
            // gain) or silently shadow the first (blend); only `mlt` may repeat.
            let mut kinds = HashSet::new();
            // What the clip can show and sound, for the effects that act on neither. Only
            // for an entry that resolves to something placeable (the others are errors
            // already, and a warning per effect on top would bury them), and only for a
            // clip with effects: for a sequence it is a walk of everything nested.
            let placeable = item.is_some_and(|i| !i.is_folder());
            let carries = if placeable && !c.effects.is_empty() { p.clip_streams(t, c) } else { None };
            for (ei, e) in c.effects.iter().enumerate() {
                let ep = format!("{cp}/effects/{ei}");
                let kind = serde_json::to_value(e).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_default();
                if kind != "mlt" && !kinds.insert(kind.clone()) {
                    issues.push(Issue::error(ep.clone(), format!("duplicate `{kind}` effect on this clip")));
                }
                // A warning, not an error: it renders, and does nothing. The editor does not
                // offer it; this is for one that arrived by patch, or whose clip moved to a
                // track that takes away what it acted on.
                if placeable && !carries.is_some_and(|s| e.acts_on(s)) {
                    let lacks = if e.streams().video() { "shows no picture" } else { "has no sound" };
                    issues.push(Issue::warn(ep.clone(), format!("`{kind}` does nothing here: this clip {lacks}")));
                }
                // Keyframes, for every kind that takes them: times in order, and names the
                // kind knows. A key naming a parameter the kind does not have is refused
                // here rather than by serde — the parameters *are* a keyframe's unknown
                // fields — and this error can say what the kind's parameters are.
                let keys = e.keyframes();
                check_keyframes(issues, &ep, c, keys.iter().map(|k| k.t));
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
                // Then the bounds each kind has, checked on every sample the effect will be
                // evaluated at: the statics, and each key as the compiler reads it.
                match e {
                    Effect::Crop { left, right, top, bottom, radius, .. } => {
                        for (path, k) in samples(&ep, e) {
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
                        for (path, k) in samples(&ep, e) {
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
                        for (path, k) in samples(&ep, e) {
                            if at(k, "radius", *radius) < 0.0 {
                                issues.push(Issue::error(path, "blur radius must be >= 0"));
                            }
                        }
                    }
                    Effect::Fade { fade_in, fade_out } | Effect::AudioFade { fade_in, fade_out } => {
                        if fade_in + fade_out > c.duration() + 1e-9 {
                            issues.push(Issue::warn(ep.clone(), "fade in + out exceed the clip duration"));
                        }
                    }
                    Effect::Volume { gain, .. } => {
                        for (path, k) in samples(&ep, e) {
                            let g = at(k, "gain", *gain);
                            if !(g.is_finite() && g >= 0.0) {
                                issues.push(Issue::error(path, "volume gain must be >= 0"));
                            }
                        }
                    }
                    Effect::Mlt { service, .. } => {
                        if service.trim().is_empty() {
                            issues.push(Issue::error(ep.clone(), "mlt effect needs a service name"));
                        }
                    }
                    Effect::Blend { .. } | Effect::Color { .. } => {}
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
                        let lp = std::path::Path::new(l);
                        if lp.is_absolute() || lp.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                            issues.push(Issue::error(format!("{xp}/luma"), "luma path must stay inside the project"));
                        } else if ctx.check_files && !ctx.root.join(lp).is_file() {
                            issues.push(Issue::error(format!("{xp}/luma"), format!("unknown luma `{l}`: use one of {LUMAS:?} or a file under the project")));
                        }
                    }
                }
            }
        }
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

/// Keyframe times must be non-negative and sorted; beyond the clip end they are pointless.
fn check_keyframes(issues: &mut Vec<Issue>, ep: &str, c: &Clip, times: impl Iterator<Item = f64>) {
    let mut last = f64::NEG_INFINITY;
    for (ki, t) in times.enumerate() {
        if t < 0.0 || t < last {
            issues.push(Issue::error(format!("{ep}/keyframes/{ki}/t"), "keyframes must be >= 0 and sorted by t"));
        }
        last = t;
        if t > c.duration() + 1e-9 {
            issues.push(Issue::warn(format!("{ep}/keyframes/{ki}/t"), "keyframe beyond clip end"));
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
        let p = project(json!({
            "bin": { "tag": { "source": { "template": "location-tag", "vars": { "nope": 1 } } } },
            "tracks": [{ "id": "o1", "kind": "overlay", "clips": [
                { "id": "a", "start": 0, "end": 4, "bin": "tag" },
                { "id": "b", "start": 4, "end": 8, "bin": "tag" }
            ]}]
        }));
        let manifest: TemplateManifest = serde_json::from_value(json!({ "id": "location-tag" })).unwrap();
        let ctx = ValidationContext { templates: Some([("location-tag".to_string(), (PathBuf::new(), manifest))].into()), ..Default::default() };
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

    /// An agent must not be able to add an overlay that does not exist. The daemon always
    /// passes `Some`, so an unresolvable id is an error even when nothing is installed —
    /// it used to downgrade to a warning and fail much later, at bake time.
    #[test]
    fn an_unknown_template_is_an_error_even_with_an_empty_library() {
        let p = project(json!({
            "bin": { "ghost": { "source": { "template": "totally-made-up" } } },
            "tracks": [{ "id": "o1", "kind": "overlay", "clips": [
                { "id": "g1", "start": 0, "end": 4, "bin": "ghost" }
            ]}]
        }));
        // Daemon with nothing installed: refused, and the message says what to do.
        let known_empty = ValidationContext { templates: Some(BTreeMap::new()), ..Default::default() };
        let issues = validate(&p, &known_empty);
        let bad: Vec<_> = issues.iter().filter(|i| i.severity == Severity::Error && i.path.contains("template")).collect();
        assert_eq!(bad.len(), 1, "{issues:?}");
        assert!(bad[0].message.contains("kuma library"), "{:?}", bad[0]);

        // The compiler passes `None` and must stay silent about ids it cannot know.
        let no_knowledge = ValidationContext::default();
        assert!(validate(&p, &no_knowledge).iter().all(|i| !i.path.contains("template")), "compiler path must not judge template ids");
    }
}
