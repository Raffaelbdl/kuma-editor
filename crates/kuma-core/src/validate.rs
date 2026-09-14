//! Structural validation. Errors reject an edit; warnings are surfaced to the UI/agent.

use std::collections::{BTreeMap, HashSet};
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
    pub templates: BTreeMap<String, (PathBuf, TemplateManifest)>,
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
            BinSource::Overlay { template, vars } => match ctx.templates.get(template) {
                None if ctx.templates.is_empty() => {
                    issues.push(Issue::warn(format!("{ip}/source/template"), format!("template `{template}` not checked (no templates loaded)")));
                }
                None => issues.push(Issue::error(format!("{ip}/source/template"), format!("unknown template `{template}`"))),
                Some((_, m)) => {
                    for problem in m.check_vars(vars) {
                        issues.push(Issue::error(format!("{ip}/source/vars"), problem));
                    }
                }
            },
            BinSource::Color { color } => {
                if !crate::template::is_css_color(color) {
                    issues.push(Issue::error(format!("{ip}/source/color"), "colour must be #rgb, #rrggbb or #rrggbbaa"));
                }
            }
        }
    }

    let mut track_ids = HashSet::new();
    let mut clip_ids = HashSet::new();
    for (ti, t) in p.tracks.iter().enumerate() {
        let tp = format!("/tracks/{ti}");
        check_id(&mut issues, &mut track_ids, &t.id, format!("{tp}/id"), "track");
        let clips = t.sorted_clips();

        for (ci, c) in &clips {
            let cp = format!("{tp}/clips/{ci}");
            check_id(&mut issues, &mut clip_ids, &c.id, format!("{cp}/id"), "clip");
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
            }
            match item {
                None => issues.push(Issue::error(format!("{cp}/bin"), format!("unknown bin entry `{}`", c.bin))),
                Some(item) => {
                    if item.is_overlay() && t.kind != TrackKind::Overlay {
                        issues.push(Issue::warn(format!("{cp}/bin"), "overlay clip on a non-overlay track"));
                    } else if !item.is_overlay() && t.kind == TrackKind::Overlay {
                        issues.push(Issue::error(format!("{cp}/bin"), "overlay tracks only hold overlay clips"));
                    }
                    if c.source_in < 0.0 {
                        issues.push(Issue::error(format!("{cp}/in"), "in must be >= 0"));
                    } else if c.source_in > 0.0 && !item.is_timed_media() {
                        issues.push(Issue::error(format!("{cp}/in"), "in applies only to video/audio entries; this one is generated to fit the clip"));
                    }
                    if item.is_timed_media() {
                        if let Some(d) = item.meta.as_ref().and_then(|m| m.duration) {
                            let need = c.source_in + c.duration() * c.speed.abs();
                            if need > d + 1e-6 {
                                issues.push(Issue::warn(
                                    format!("{cp}/end"),
                                    format!("clip reads {need:.2}s of a {d:.2}s source; tail will be black/silent"),
                                ));
                            }
                        }
                    }
                }
            }
            // A second effect of these kinds would stack (two volume filters square the
            // gain) or silently shadow the first (blend); only `mlt` may repeat.
            let mut kinds = HashSet::new();
            for (ei, e) in c.effects.iter().enumerate() {
                let ep = format!("{cp}/effects/{ei}");
                let kind = serde_json::to_value(e).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_default();
                if kind != "mlt" && !kinds.insert(kind.clone()) {
                    issues.push(Issue::error(ep.clone(), format!("duplicate `{kind}` effect on this clip")));
                }
                match e {
                    Effect::Crop { left, right, top, bottom, radius } => {
                        if left + right >= 1.0 || top + bottom >= 1.0 {
                            issues.push(Issue::error(ep.clone(), "crop leaves nothing visible"));
                        }
                        if [left, right, top, bottom, radius].iter().any(|v| **v < 0.0 || **v > 1.0) {
                            issues.push(Issue::error(ep.clone(), "crop values must be within 0..1"));
                        }
                    }
                    Effect::Transform { keyframes, opacity, scale, .. } => {
                        if !(0.0..=1.0).contains(opacity) {
                            issues.push(Issue::error(format!("{ep}/opacity"), "opacity must be 0..1"));
                        }
                        let good_scale = |s: f64| s.is_finite() && s > 0.0;
                        if !good_scale(*scale) {
                            issues.push(Issue::error(format!("{ep}/scale"), "scale must be > 0"));
                        }
                        check_keyframes(&mut issues, &ep, c, keyframes.iter().map(|k| k.t));
                        for (ki, k) in keyframes.iter().enumerate() {
                            if k.scale.is_some_and(|s| !good_scale(s)) {
                                issues.push(Issue::error(format!("{ep}/keyframes/{ki}/scale"), "scale must be > 0"));
                            }
                        }
                    }
                    Effect::Fade { fade_in, fade_out } => {
                        if fade_in + fade_out > c.duration() + 1e-9 {
                            issues.push(Issue::warn(ep.clone(), "fade in + out exceed the clip duration"));
                        }
                    }
                    Effect::Blur { radius } => {
                        if *radius < 0.0 {
                            issues.push(Issue::error(ep.clone(), "blur radius must be >= 0"));
                        }
                    }
                    Effect::Volume { keyframes } => {
                        if keyframes.is_empty() {
                            issues.push(Issue::error(ep.clone(), "volume needs at least one keyframe"));
                        }
                        check_keyframes(&mut issues, &ep, c, keyframes.iter().map(|k| k.t));
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
            // The `from` side needs material after its out point; only checkable with probed metadata.
            if let Some(d) = p.item(a).filter(|i| i.is_timed_media()).and_then(|i| i.meta.as_ref()).and_then(|m| m.duration) {
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

    issues
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

fn check_id<'a>(issues: &mut Vec<Issue>, seen: &mut HashSet<&'a str>, id: &'a str, path: String, what: &str) {
    if !is_valid_id(id) {
        issues.push(Issue::error(path, format!("{what} id `{id}` must match [A-Za-z_][A-Za-z0-9_-]*")));
    } else if !seen.insert(id) {
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

    fn project(v: serde_json::Value) -> Project {
        serde_json::from_value(v).unwrap()
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

    fn messages(p: &Project) -> Vec<String> {
        validate(p, &ValidationContext::default()).into_iter().map(|i| i.message).collect()
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
        p2.tracks[0].clips[1].source_in = 0.5;
        assert!(!messages(&p2).iter().any(|m| m.contains("handle")));
        p2.tracks[0].clips[1].speed = 2.0;
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
        let mut ctx = ValidationContext::default();
        ctx.templates.insert("location-tag".into(), (PathBuf::new(), manifest));
        let m: Vec<_> = validate(&p, &ctx).into_iter().map(|i| i.message).collect();
        assert_eq!(m.iter().filter(|m| m.contains("nope")).count(), 1, "{m:?}");
    }
}
