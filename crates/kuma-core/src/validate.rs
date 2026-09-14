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
    /// Project directory; asset paths resolve against it.
    pub root: PathBuf,
    /// Loaded overlay templates by id.
    pub templates: BTreeMap<String, (PathBuf, TemplateManifest)>,
    /// If false, missing asset files are warnings rather than errors (useful for tests).
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

    for (id, a) in &p.assets {
        if ctx.check_files {
            let path = if std::path::Path::new(&a.path).is_absolute() {
                PathBuf::from(&a.path)
            } else {
                ctx.root.join(&a.path)
            };
            if !path.is_file() {
                issues.push(Issue::error(format!("/assets/{id}/path"), format!("file not found: {}", a.path)));
            }
        }
    }

    let mut track_ids = HashSet::new();
    let mut clip_ids = HashSet::new();
    for (ti, t) in p.tracks.iter().enumerate() {
        let tp = format!("/tracks/{ti}");
        if !track_ids.insert(t.id.as_str()) {
            issues.push(Issue::error(format!("{tp}/id"), format!("duplicate track id `{}`", t.id)));
        }
        let mut clips: Vec<(usize, &Clip)> = t.clips.iter().enumerate().collect();
        clips.sort_by(|a, b| a.1.start.partial_cmp(&b.1.start).unwrap_or(std::cmp::Ordering::Equal));

        for (ci, c) in &clips {
            let cp = format!("{tp}/clips/{ci}");
            if !clip_ids.insert(c.id.as_str()) {
                issues.push(Issue::error(format!("{cp}/id"), format!("duplicate clip id `{}`", c.id)));
            }
            if !(c.start >= 0.0 && c.end > c.start) {
                issues.push(Issue::error(format!("{cp}/end"), "clip must have start >= 0 and end > start"));
            }
            if c.speed == 0.0 || !c.speed.is_finite() || c.speed.abs() > 20.0 || c.speed.abs() < 0.01 {
                issues.push(Issue::error(format!("{cp}/speed"), "speed must be in ±[0.01, 20]"));
            }
            match &c.src {
                ClipSource::Asset { asset, source_in } => {
                    if t.kind == TrackKind::Overlay {
                        issues.push(Issue::error(format!("{cp}/src"), "overlay tracks only hold overlay clips"));
                    }
                    match p.assets.get(asset) {
                        None => issues.push(Issue::error(format!("{cp}/src/asset"), format!("unknown asset `{asset}`"))),
                        Some(a) => {
                            if *source_in < 0.0 {
                                issues.push(Issue::error(format!("{cp}/src/in"), "in must be >= 0"));
                            }
                            if let Some(d) = a.meta.as_ref().and_then(|m| m.duration) {
                                let need = source_in + c.duration() * c.speed.abs();
                                if a.kind != AssetKind::Image && need > d + 1e-6 {
                                    issues.push(Issue::warn(
                                        format!("{cp}/end"),
                                        format!("clip reads {need:.2}s of a {d:.2}s asset; tail will be black/silent"),
                                    ));
                                }
                            }
                        }
                    }
                }
                ClipSource::Overlay { overlay, vars } => {
                    if t.kind != TrackKind::Overlay {
                        issues.push(Issue::warn(format!("{cp}/src"), "overlay clip on a non-overlay track"));
                    }
                    match ctx.templates.get(overlay) {
                        None => {
                            if ctx.templates.is_empty() {
                                issues.push(Issue::warn(format!("{cp}/src/overlay"), format!("template `{overlay}` not checked (no templates loaded)")));
                            } else {
                                issues.push(Issue::error(format!("{cp}/src/overlay"), format!("unknown template `{overlay}`")));
                            }
                        }
                        Some((_, m)) => {
                            for problem in m.check_vars(vars) {
                                issues.push(Issue::error(format!("{cp}/src/vars"), problem));
                            }
                        }
                    }
                }
                ClipSource::Color { color } => {
                    if !color.starts_with('#') {
                        issues.push(Issue::error(format!("{cp}/src/color"), "colour must be #rrggbb or #rrggbbaa"));
                    }
                }
            }
            for (ei, e) in c.effects.iter().enumerate() {
                let ep = format!("{cp}/effects/{ei}");
                match e {
                    Effect::Crop { left, right, top, bottom, radius } => {
                        if left + right >= 1.0 || top + bottom >= 1.0 {
                            issues.push(Issue::error(ep.clone(), "crop leaves nothing visible"));
                        }
                        if [left, right, top, bottom, radius].iter().any(|v| **v < 0.0 || **v > 1.0) {
                            issues.push(Issue::error(ep.clone(), "crop values must be within 0..1"));
                        }
                    }
                    Effect::Transform { keyframes, opacity, .. } => {
                        if !(0.0..=1.0).contains(opacity) {
                            issues.push(Issue::error(format!("{ep}/opacity"), "opacity must be 0..1"));
                        }
                        let mut last = f64::NEG_INFINITY;
                        for (ki, k) in keyframes.iter().enumerate() {
                            if k.t < last {
                                issues.push(Issue::error(format!("{ep}/keyframes/{ki}/t"), "keyframes must be sorted by t"));
                            }
                            last = k.t;
                            if k.t > c.duration() + 1e-9 {
                                issues.push(Issue::warn(format!("{ep}/keyframes/{ki}/t"), "keyframe beyond clip end"));
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

        // Transitions must join adjacent clips and have handles.
        for (xi, x) in t.transitions.iter().enumerate() {
            let xp = format!("{tp}/transitions/{xi}");
            let from = clips.iter().position(|(_, c)| c.id == x.from);
            let to = clips.iter().position(|(_, c)| c.id == x.to);
            match (from, to) {
                (Some(f), Some(tt)) if tt == f + 1 => {
                    let (_, a) = clips[f];
                    let (_, b) = clips[tt];
                    if (b.start - a.end).abs() > 1e-6 {
                        issues.push(Issue::error(xp.clone(), "transition clips must be butt-joined (no gap)"));
                    }
                    if x.duration <= 0.0 {
                        issues.push(Issue::error(format!("{xp}/duration"), "duration must be > 0"));
                    }
                    let half = x.duration / 2.0;
                    if let ClipSource::Asset { source_in, .. } = &b.src {
                        if *source_in < half - 1e-9 {
                            issues.push(Issue::error(
                                format!("{xp}/duration"),
                                format!("`{}` needs {half:.2}s of handle before its in point", b.id),
                            ));
                        }
                    }
                    // The `from` side needs material after its out point; only checkable with probed metadata.
                    if let ClipSource::Asset { asset, source_in } = &a.src {
                        if let Some(d) = p.assets.get(asset).and_then(|as_| as_.meta.as_ref()).and_then(|m| m.duration) {
                            let need = source_in + (a.duration() + half) * a.speed.abs();
                            if need > d + 1e-6 {
                                issues.push(Issue::warn(
                                    format!("{xp}/duration"),
                                    format!("`{}` has no {half:.2}s of handle after its out point; the dissolve tail will be black", a.id),
                                ));
                            }
                        }
                    }
                    if half > a.duration() || half > b.duration() {
                        issues.push(Issue::error(format!("{xp}/duration"), "transition longer than the clips it joins"));
                    }
                }
                (Some(_), Some(_)) => issues.push(Issue::error(xp, "transition clips must be adjacent, from → to")),
                _ => issues.push(Issue::error(xp, "transition references an unknown clip")),
            }
        }
    }

    issues
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
    fn overlap_and_missing_asset_are_errors() {
        let p = project(json!({
            "assets": {},
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "src": { "asset": "nope" } },
                { "id": "b", "start": 4, "end": 8, "src": { "color": "#000" } }
            ]}]
        }));
        let issues = validate(&p, &ValidationContext::default());
        let msgs: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("unknown asset")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("overlaps")), "{msgs:?}");
    }

    #[test]
    fn transition_needs_handle() {
        let p = project(json!({
            "assets": { "x": { "path": "x.mp4" } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "src": { "asset": "x" } },
                { "id": "b", "start": 5, "end": 8, "src": { "asset": "x", "in": 0.1 } }
            ], "transitions": [ { "from": "a", "to": "b", "duration": 1.0 } ]}]
        }));
        let issues = validate(&p, &ValidationContext::default());
        assert!(issues.iter().any(|i| i.message.contains("handle")), "{issues:?}");
    }
}
