//! `measure_color`: what a region of the picture is, in the grade's own terms
//! (`kuma_core::color`), and the white balance and exposure that make a clip's region look
//! like a reference — a region elsewhere in the video, a colour, or neutral.
//!
//! Frames come through the ordinary sample path — the compiled document, melt — so what is
//! measured is what renders. Nothing is stored. A match measures the clip *without* its
//! grade, by sampling a copy of the project that lacks it, and answers with the grade and
//! the patch that writes it; applying it is the caller's edit.

use std::sync::atomic::{AtomicBool, Ordering};

use kuma_core::color::{match_light, ColorStats, Target};
use kuma_core::grade::Lut;
use kuma_core::manifest::Region;
use kuma_core::{BinSource, Effect, Grade, Project};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::jobs::{sample_frames_of, Before, OverlayScope, SampleRequest};
use crate::state::{read, AppState};

/// The most pixels a match pushes through the grade on each try: an even spread over the
/// region, plenty for a mean.
const MATCH_PIXELS: usize = 20_000;

/// What to measure: a region (the whole frame by default, in composition pixels) of a
/// sequence (the master by default) at some moments, as `sample_frames` takes them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ColorRequest {
    #[serde(default)]
    pub sequence: Option<String>,
    #[serde(default)]
    pub t0: f64,
    #[serde(default)]
    pub t1: Option<f64>,
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub times: Option<Vec<f64>>,
    #[serde(default)]
    pub region: Option<Region>,
    /// Whether overlays are in the picture measured. Off by default: a title over a face
    /// is not the face's colour.
    #[serde(default)]
    pub overlays: bool,
    /// Solve a grade for a clip: its region, at these moments, made to look like the
    /// reference.
    #[serde(default, rename = "match")]
    pub matching: Option<Match>,
    /// Also measure the same frames of another version of the document — before an edit,
    /// or without some effects (`sample_frames` takes the same) — to say what the edit did.
    #[serde(default)]
    pub before: Option<Before>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Match {
    /// The clip whose grade is solved. The measurement is of its sequence, at moments it
    /// is on screen.
    pub clip: String,
    pub reference: Reference,
}

/// What the clip's region should look like.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Reference {
    /// `{"neutral": true}`: grey, at the brightness it already has — white balance alone,
    /// for a region that is white or grey in life.
    Neutral { neutral: bool },
    /// `{"color": "#rrggbb"}`: the region's mean as displayed.
    Colour { color: String },
    /// `{"t", "region"?, "sequence"?}`: another region as the video shows it now — the
    /// same face in the shot before, a wall in the master's reference shot. The region
    /// defaults to the measured one, and the sequence to the clip's.
    At {
        t: f64,
        #[serde(default)]
        region: Option<Region>,
        #[serde(default)]
        sequence: Option<String>,
    },
}

/// Where a clip is in the document, and its grade (a neutral one when it has none).
struct Located {
    sequence: String,
    track: usize,
    clip: usize,
    start: f64,
    end: f64,
    grade: Grade,
    /// Which of its effects the grade is, when it has one.
    graded_at: Option<usize>,
}

fn locate(doc: &Project, id: &str) -> anyhow::Result<Located> {
    for (seq_id, seq) in doc.sequences() {
        for (ti, track) in seq.tracks.iter().enumerate() {
            if let Some((ci, c)) = track.clips.iter().enumerate().find(|(_, c)| c.id == id) {
                let graded_at = c.effects.iter().position(|e| e.as_grade().is_some());
                let grade = graded_at.and_then(|ei| c.effects[ei].as_grade()).cloned().unwrap_or_default();
                return Ok(Located { sequence: seq_id.clone(), track: ti, clip: ci, start: c.start, end: c.end, grade, graded_at });
            }
        }
    }
    anyhow::bail!("no clip `{id}`")
}

/// `region`, or the whole frame, checked against it.
fn region_in(doc: &Project, region: Option<Region>) -> anyhow::Result<Region> {
    let r = region.unwrap_or(Region { x: 0, y: 0, w: doc.width, h: doc.height });
    anyhow::ensure!(r.w > 0 && r.h > 0, "a region needs a width and a height");
    anyhow::ensure!(
        r.x as u64 + r.w as u64 <= doc.width as u64 && r.y as u64 + r.h as u64 <= doc.height as u64,
        "region {}×{} at {},{} is outside the {}×{} frame",
        r.w,
        r.h,
        r.x,
        r.y,
        doc.width,
        doc.height
    );
    Ok(r)
}

/// One measured moment: its time, its frame, and the region's pixels as the frame holds
/// them, a byte a channel — a full 1080p region is 6 MB this way, and 50 MB as floats.
struct Measured {
    t: f64,
    frame: i64,
    pixels: Vec<[u8; 3]>,
}

/// A pixel as the colour maths takes one.
fn unit(p: &[u8; 3]) -> [f64; 3] {
    p.map(|v| v as f64 / 255.0)
}

/// Every pixel of every moment, as one region, read in place.
fn pooled(frames: &[Measured]) -> impl Iterator<Item = [f64; 3]> + '_ {
    frames.iter().flat_map(|f| f.pixels.iter().map(unit))
}

/// At most `most` of `frames`' pixels, evenly spread: what a match pushes through the
/// grade on each try.
fn spread(frames: &[Measured], most: usize) -> Vec<[f64; 3]> {
    let total: usize = frames.iter().map(|f| f.pixels.len()).sum();
    pooled(frames).step_by(total.div_ceil(most).max(1)).collect()
}

/// Sample `doc` as `req` asks and read the region's pixels of each frame.
fn measure_frames(state: &AppState, doc: Project, req: &SampleRequest, region: Region, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<Vec<Measured>> {
    let (samples, _) = sample_frames_of(state, doc, req, progress, Some(cancel))?;
    samples
        .into_iter()
        .map(|s| {
            let bytes = kuma_overlay::media::rgb24(&s.path).map_err(|e| anyhow::anyhow!("reading frame {}: {e}", s.frame))?;
            anyhow::ensure!(bytes.len() == region.w as usize * region.h as usize * 3, "frame {} is not the region's size", s.frame);
            Ok(Measured { t: s.t, frame: s.frame, pixels: bytes.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect() })
        })
        .collect()
}

/// A grade's LUT, read from the project.
fn lut(state: &AppState, path: Option<&str>) -> anyhow::Result<Option<Lut>> {
    let Some(path) = path else { return Ok(None) };
    let (_, lut) = Lut::load(&state.config.project_root().join(path)).map_err(|e| anyhow::anyhow!("LUT `{path}`: {e}"))?;
    Ok(Some(lut))
}

/// Measure, and with `match` solve; see the module. The result is JSON for the job.
pub fn measure(state: &AppState, req: &ColorRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<Value> {
    let doc = read(&state.project).clone();
    let target = req.matching.as_ref().map(|m| locate(&doc, &m.clip).map(|at| (m, at))).transpose()?;
    let sequence = match (&target, &req.sequence) {
        (Some((m, at)), Some(s)) if *s != at.sequence => anyhow::bail!("clip `{}` is in sequence `{}`, not `{s}`", m.clip, at.sequence),
        (Some((_, at)), _) => at.sequence.clone(),
        (None, s) => doc.sequence_or_master(s.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone(),
    };
    let region = region_in(&doc, req.region)?;
    let sample = SampleRequest {
        t0: req.t0,
        t1: req.t1,
        count: req.count,
        times: req.times.clone(),
        rect: Some(region),
        sequence: Some(sequence.clone()),
        overlays: if req.overlays { OverlayScope::All } else { OverlayScope::None },
        ..Default::default()
    };
    // The measurements in turn: these frames, the `before` ones, then a match's two.
    let b = u8::from(req.before.is_some());
    let share = 1 + b + if target.is_some() { 2 } else { 0 };
    let part = |n: u8| move |p: u8, m: &str| progress(n * (100 / share) + p / share, m);
    let frames = measure_frames(state, doc.clone(), &sample, region, &part(0), cancel)?;
    let per_frame = |frames: &[Measured]| -> Vec<Value> { frames.iter().map(|f| json!({ "t": f.t, "frame": f.frame, "stats": ColorStats::of(f.pixels.iter().map(unit)) })).collect() };
    let mut out = json!({ "sequence": sequence, "region": region, "samples": per_frame(&frames), "stats": ColorStats::of(pooled(&frames)) });
    if let Some(before) = &req.before {
        let earlier = measure_frames(state, before.document(state, &doc)?, &sample, region, &part(1), cancel)?;
        out["before"] = json!({ "samples": per_frame(&earlier), "stats": ColorStats::of(pooled(&earlier)) });
    }
    let Some((m, at)) = target else { return Ok(out) };

    if let Some(off) = frames.iter().find(|f| f.t < at.start - 1e-9 || f.t >= at.end) {
        anyhow::bail!("clip `{}` is on screen from {:.2} to {:.2} s in `{}`; {:.2} s is outside it", m.clip, at.start, at.end, sequence, off.t);
    }
    // The clip as it is before its grade: the same project with the grade taken out — a
    // copy, so the document never holds it. A clip with no grade already is that.
    let bare_frames;
    let ungraded = match at.graded_at {
        None => &frames,
        Some(ei) => {
            let mut bare = doc.clone();
            if let Some(BinSource::Sequence(seq)) = bare.bin.get_mut(&at.sequence).map(|i| &mut i.source) {
                seq.tracks[at.track].clips[at.clip].effects.remove(ei);
            }
            bare_frames = measure_frames(state, bare, &sample, region, &part(1 + b), cancel)?;
            &bare_frames
        }
    };
    anyhow::ensure!(!cancel.load(Ordering::SeqCst), "cancelled");

    let (aim, reference) = match &m.reference {
        Reference::Neutral { neutral: true } => (Target::Neutral, json!({ "neutral": true })),
        Reference::Neutral { neutral: false } => anyhow::bail!("`reference` is `{{\"neutral\": true}}`, a colour or a moment"),
        Reference::Colour { color } => (Target::colour(color).ok_or_else(|| anyhow::anyhow!("`{color}` is not a CSS hex colour"))?, json!({ "color": color })),
        Reference::At { t, region: r, sequence: s } => {
            let seq = doc.sequence_or_master(Some(s.as_deref().unwrap_or(&sequence))).map_err(|e| anyhow::anyhow!(e))?.clone();
            let r = region_in(&doc, Some(r.unwrap_or(region)))?;
            let at_ref = SampleRequest { times: Some(vec![*t]), rect: Some(r), sequence: Some(seq.clone()), ..sample.clone() };
            let px: Vec<[f64; 3]> = pooled(&measure_frames(state, doc.clone(), &at_ref, r, &part(2 + b), cancel)?).collect();
            let stats = ColorStats::of(&px);
            (Target::like(&px).ok_or_else(|| anyhow::anyhow!("the reference region is empty"))?, json!({ "t": t, "sequence": seq, "region": r, "stats": stats }))
        }
    };
    let matched = match_light(&at.grade, lut(state, at.grade.input_lut.as_deref())?, lut(state, at.grade.look.as_deref())?, &spread(ungraded, MATCH_PIXELS), aim)
        .ok_or_else(|| anyhow::anyhow!("the region has no pixels"))?;
    let effects = format!("/bin/{}/source/tracks/{}/clips/{}/effects", at.sequence, at.track, at.clip);
    let value = serde_json::to_value(Effect::Grade(matched.grade.clone()))?;
    let patch = match at.graded_at {
        Some(ei) => json!([{ "op": "replace", "path": format!("{effects}/{ei}"), "value": value }]),
        None => json!([{ "op": "add", "path": format!("{effects}/-"), "value": value }]),
    };
    out["match"] = json!({
        "clip": m.clip,
        "reference": reference,
        "before": ColorStats::of(pooled(ungraded)),
        "after": matched.after,
        "reached": matched.reached,
        "grade": matched.grade,
        "patch": patch,
    });
    progress(100, "measured");
    Ok(out)
}
