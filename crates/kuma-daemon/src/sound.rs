//! Sound that needs measuring before it can be edited: lining a mic's recording up with the
//! camera that heard the same scene, and bringing a source to a loudness.
//!
//! Both are jobs that end in one ordinary edit — clips placed and a camera muted, an
//! entry's `volume` — revision-checked, validated and undoable like any other, composed
//! against the project as it is when the measuring is done ([`AppState::apply_built`]).
//! Nothing here is a runtime layer: what they measure is written into the document, and
//! the compiler reads only the document.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kuma_core::{BinSource, Clip, MediaKind, Project, Track, TrackKind};
use kuma_overlay::sync::Alignment;
use serde::{Deserialize, Serialize};

use crate::jobs::{bin_op, bin_remove_op, bin_replace_op, db, default_origin, resolve_path, scan_sound, secs};
use crate::state::{read, AppState};

/// How many pairs are decoded and correlated at once: each is two ffmpeg decodes and an FFT
/// of up to a few hundred MB.
const SYNC_AT_ONCE: usize = 4;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncRequest {
    /// Each camera clip and the recording to line up with it.
    pub pairs: Vec<SyncPair>,
    /// Only measure: answer the offsets and change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// The edit's origin, which the handler sets from the request's.
    #[serde(skip, default = "default_origin")]
    pub origin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncPair {
    /// A clip whose source recorded the scene with its own sound: the camera. At speed 1.
    pub clip: String,
    /// The bin entry that recorded the same scene better: the mic, an audio or video file.
    pub audio: String,
    /// The audio track the mic's clip goes on, in the camera clip's sequence. By default
    /// the first audio track with room over that stretch, else a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
}

/// A pair, checked: the two files it lines up and how long they are.
struct Work<'a> {
    pair: &'a SyncPair,
    camera: PathBuf,
    mic: PathBuf,
    camera_len: Option<f64>,
    mic_len: f64,
}

/// What a sync answers for one pair.
#[derive(Serialize)]
struct PairReport<'a> {
    clip: &'a str,
    audio: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<String>,
    #[serde(flatten)]
    placed: Option<Placed>,
}

impl<'a> PairReport<'a> {
    fn new(w: &'a Work, alignment: &Result<Alignment, String>, placed: Option<Placed>) -> Self {
        let (offset, confidence, skipped) = match alignment {
            Ok(a) => (
                Some((a.offset * 10000.0).round() / 10000.0),
                Some((a.confidence * 100.0).round() / 100.0),
                (!a.is_match()).then(|| {
                    format!("the two sounds do not match (confidence {:.2}, under {}): not the same take, or one of them is mostly silence", a.confidence, kuma_overlay::sync::MIN_CONFIDENCE)
                }),
            ),
            Err(e) => (None, None, Some(e.clone())),
        };
        PairReport { clip: &w.pair.clip, audio: &w.pair.audio, offset, confidence, skipped, placed }
    }
}

/// The alignment, when it is one to act on: measured, and a match.
fn hit(alignment: &Result<Alignment, String>) -> Option<Alignment> {
    alignment.as_ref().ok().copied().filter(Alignment::is_match)
}

/// Line each mic recording up with its camera clip: cross-correlate the two sounds
/// (`kuma_overlay::sync`), then, in one edit, place the mic on an audio track so that it
/// plays what the camera heard at the same instant, mute the camera clip's own sound, and
/// give both one `group` so they are moved and cut together. A pair already synced (a clip
/// of the mic in the camera clip's group) is moved, not placed twice. A pair whose sounds
/// do not match is left as it is and said so; the rest still land.
///
/// The offset is found to a fraction of a millisecond, and the compiler then rounds the
/// mic clip's `in` to a frame: `error_ms` is what that leaves, under half a frame.
pub fn sync(state: &AppState, req: &SyncRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(!req.pairs.is_empty(), "`pairs` is empty: give each camera `clip` and the `audio` entry to line up with it");
    // Everything checked before anything is decoded: a typo should not cost a minute.
    let work: Vec<Work> = {
        let project = read(&state.project);
        req.pairs
            .iter()
            .map(|pair| {
                let (_, _, clip) = project.find_clip(&pair.clip).ok_or_else(|| anyhow::anyhow!("no clip `{}`", pair.clip))?;
                anyhow::ensure!(clip.speed == 1.0, "clip `{}` plays at speed {}: sync it at speed 1, then change its speed", clip.id, clip.speed);
                let (camera, camera_len) = sounding_file(&project, &clip.bin).map_err(|e| anyhow::anyhow!("clip `{}`: {e}", clip.id))?;
                let (mic, mic_len) = sounding_file(&project, &pair.audio)?;
                anyhow::ensure!(clip.bin != pair.audio, "clip `{}` already plays `{}`", clip.id, pair.audio);
                Ok(Work { pair, camera: resolve_path(state, &camera), mic: resolve_path(state, &mic), camera_len, mic_len: known_length(mic_len, &pair.audio)? })
            })
            .collect::<anyhow::Result<_>>()?
    };

    // Each two files listened to once, however many clips of one take are synced: the
    // offset is the files', not the clip's. Workers take the next pair as they finish one.
    let mut seen = HashSet::new();
    let distinct: Vec<&Work> = work.iter().filter(|w| seen.insert((&w.camera, &w.mic))).collect();
    let total = distinct.len();
    progress(0, &format!("listening to {total} pair{}", if total == 1 { "" } else { "s" }));
    let mut found: HashMap<(&PathBuf, &PathBuf), Result<Alignment, String>> = HashMap::new();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..SYNC_AT_ONCE.min(total) {
            let (tx, next, distinct) = (tx.clone(), &next, &distinct);
            s.spawn(move || {
                while let Some(w) = distinct.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let _ = tx.send((*w, kuma_overlay::sync::align(&w.camera, &w.mic, w.camera_len.unwrap_or(0.0) + w.mic_len, cancel)));
                }
            });
        }
        drop(tx);
        for (n, (w, alignment)) in rx.into_iter().enumerate() {
            found.insert((&w.camera, &w.mic), alignment);
            progress(((n + 1) * 90 / total) as u8, &format!("lined up {} of {total}", n + 1));
        }
    });
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
    let measured: Vec<(&Work, &Result<Alignment, String>)> = work.iter().map(|w| (w, &found[&(&w.camera, &w.mic)])).collect();
    let matched: Vec<(&Work, Alignment)> = measured.iter().filter_map(|&(w, a)| hit(a).map(|a| (w, a))).collect();
    if req.dry_run || matched.is_empty() {
        let pairs: Vec<PairReport> = measured.iter().map(|&(w, a)| PairReport::new(w, a, None)).collect();
        anyhow::ensure!(req.dry_run, "no pair lined up: {}", serde_json::to_string(&pairs).unwrap_or_default());
        return Ok(serde_json::json!({ "pairs": pairs, "applied": false, "note": "`offset` is seconds into the audio at the camera's 0: positive when the audio started recording first." }));
    }

    progress(95, "placing the audio");
    let (placed, rev, warnings) = state.apply_built(&req.origin, None, |p| place(p, &matched))?;
    let mut placed = placed.into_iter();
    let pairs: Vec<PairReport> = measured.iter().map(|&(w, a)| PairReport::new(w, a, hit(a).and_then(|_| placed.next()))).collect();
    Ok(serde_json::json!({ "pairs": pairs, "applied": true, "revision": rev, "warnings": warnings }))
}

/// Where one pair's mic landed.
#[derive(Debug, Serialize)]
struct Placed {
    /// The mic's clip.
    audio_clip: String,
    track: String,
    start: f64,
    end: f64,
    #[serde(rename = "in")]
    source_in: f64,
    group: String,
    /// It was synced before, and was moved rather than placed again.
    moved: bool,
    /// Milliseconds the mic plays late (positive) or early against the camera once the
    /// compiler has rounded both clips to frames.
    error_ms: f64,
}

/// The edit for the matched pairs, against the project as it is now; one `Placed` per pair,
/// in order.
fn place(p: &Project, matched: &[(&Work, Alignment)]) -> anyhow::Result<(Vec<json_patch::PatchOperation>, Vec<Placed>)> {
    let fps = p.fps;
    let mut ops = Vec::new();
    let mut placed = Vec::new();
    // What this edit has put on each track so far, so two pairs do not share a stretch, and
    // the tracks it adds to each sequence: their indexes follow the sequence's own.
    let mut taken: HashMap<(String, usize), Vec<(f64, f64)>> = HashMap::new();
    let mut added: HashMap<String, Vec<String>> = HashMap::new();
    let mut new_clip_ids = p.free_track_or_clip_ids("mic", matched.len()).into_iter();
    let mut new_track_ids = p.free_track_or_clip_ids("a", matched.len()).into_iter();
    // Clips moved off their track, removed once everything else is written: every other op
    // names clips by their index as it is now, and adds only append.
    let mut removals: Vec<(String, usize, usize)> = Vec::new();
    for (w, alignment) in matched {
        let (cam_id, audio) = (&w.pair.clip, &w.pair.audio);
        // The camera clip as it is now: it may have been moved while the sound was read.
        let (seq_id, ti, ci) = p.clip_index(cam_id).ok_or_else(|| anyhow::anyhow!("clip `{cam_id}` is gone"))?;
        let seq = p.seq(&seq_id).ok_or_else(|| anyhow::anyhow!("`{seq_id}` is not a sequence"))?;
        let cam = &seq.tracks[ti].clips[ci];
        let group = if cam.group.is_empty() { format!("sync-{}", cam.id) } else { cam.group.clone() };

        // At the camera clip's start the camera is at `cam.in`, and the mic at that plus
        // the offset; a mic that started later begins later on the timeline.
        let mut source_in = cam.source_in + alignment.offset;
        let mut start = cam.start;
        if source_in < 0.0 {
            start = fps.snap(cam.start - source_in);
            source_in += start - cam.start;
        }
        let end = cam.end.min(fps.snap(start + (w.mic_len - source_in)));
        anyhow::ensure!(end - start >= 1.0 / fps.as_f64(), "`{audio}` does not overlap clip `{cam_id}`: the offset is {:.2} s", alignment.offset);
        let source_in = (source_in * 1e6).round() / 1e6;

        // Synced before: the mic's clip in this group is moved, wherever it is.
        let existing = seq.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|c| &c.bin == audio && !c.group.is_empty() && c.group == group).map(|ci| (ti, ci)));
        let clash = |v: &[(f64, f64)]| v.iter().any(|&(a, b)| a < end && b > start);
        let reserved = |ti: usize| taken.get(&(seq_id.clone(), ti)).map_or(&[][..], Vec::as_slice);
        let fits = |ti: usize, t: &Track| t.kind == TrackKind::Audio && !t.clips.iter().enumerate().any(|(i, c)| Some((ti, i)) != existing && c.start < end && c.end > start) && !clash(reserved(ti));
        let base = seq.tracks.len();
        let new_tracks = added.entry(seq_id.clone()).or_default();
        let track_index = if let Some(id) = &w.pair.track {
            let ti = seq.tracks.iter().position(|t| &t.id == id).ok_or_else(|| anyhow::anyhow!("no track `{id}` on `{seq_id}`, where clip `{cam_id}` is"))?;
            anyhow::ensure!(seq.tracks[ti].kind == TrackKind::Audio, "track `{id}` is not an audio track");
            anyhow::ensure!(fits(ti, &seq.tracks[ti]), "track `{id}` has a clip between {start:.2} and {end:.2} s already");
            ti
        } else if let Some(ti) = existing.map(|(ti, _)| ti).filter(|&ti| fits(ti, &seq.tracks[ti])).or_else(|| seq.tracks.iter().enumerate().position(|(ti, t)| fits(ti, t))) {
            ti
        } else if let Some(k) = (0..new_tracks.len()).find(|&k| !clash(reserved(base + k))) {
            base + k
        } else {
            let id = new_track_ids.next().unwrap_or_default();
            ops.push(bin_op(&seq_id, "/source/tracks/-", serde_json::to_value(Track::empty(id.clone(), TrackKind::Audio, "Synced sound"))?)?);
            new_tracks.push(id);
            base + new_tracks.len() - 1
        };
        let track_id = seq.tracks.get(track_index).map_or_else(|| new_tracks[track_index - base].clone(), |t| t.id.clone());
        taken.entry((seq_id.clone(), track_index)).or_default().push((start, end));

        let at = |ti: usize, ci: usize| format!("/source/tracks/{ti}/clips/{ci}");
        ops.push(bin_op(&seq_id, &format!("{}/muted", at(ti, ci)), true.into())?);
        ops.push(bin_op(&seq_id, &format!("{}/group", at(ti, ci)), group.clone().into())?);
        let (audio_clip, moved) = match existing {
            Some((eti, eci)) => {
                let clip = Clip { start, end, source_in, speed: 1.0, ..seq.tracks[eti].clips[eci].clone() };
                if eti == track_index {
                    ops.push(bin_replace_op(&seq_id, &at(eti, eci), serde_json::to_value(&clip)?)?);
                } else {
                    ops.push(bin_op(&seq_id, &format!("/source/tracks/{track_index}/clips/-"), serde_json::to_value(&clip)?)?);
                    removals.push((seq_id.clone(), eti, eci));
                }
                (clip.id, true)
            }
            None => {
                let clip = Clip { source_in, group: group.clone(), ..Clip::placing(new_clip_ids.next().unwrap_or_default(), audio.as_str(), start, end) };
                ops.push(bin_op(&seq_id, &format!("/source/tracks/{track_index}/clips/-"), serde_json::to_value(&clip)?)?);
                (clip.id, false)
            }
        };
        // What the compiler will play: each clip's source frame at a timeline frame.
        let cam_at = fps.frame(cam.source_in) - fps.frame(cam.start);
        let mic_at = fps.frame(source_in) - fps.frame(start);
        let error_ms = (fps.secs(mic_at - cam_at) - alignment.offset) * 1000.0;
        placed.push(Placed { audio_clip, track: track_id, start: secs(start), end: secs(end), source_in, group, moved, error_ms: (error_ms * 10.0).round() / 10.0 });
    }
    removals.sort_by(|a, b| b.cmp(a));
    for (seq_id, ti, ci) in removals {
        ops.push(bin_remove_op(&seq_id, &format!("/source/tracks/{ti}/clips/{ci}"))?);
    }
    Ok((ops, placed))
}

/// The file behind a media entry that sounds, and its length when probed.
fn sounding_file(p: &Project, id: &str) -> anyhow::Result<(PathBuf, Option<f64>)> {
    let item = p.bin.get(id).ok_or_else(|| anyhow::anyhow!("no bin entry `{id}`"))?;
    let BinSource::File { path, kind, .. } = &item.source else { anyhow::bail!("`{id}` is not a media file") };
    anyhow::ensure!(*kind != MediaKind::Image, "`{id}` is a still image, which makes no sound");
    anyhow::ensure!(item.sounds(), "`{id}` has no sound");
    Ok((PathBuf::from(path), item.meta.as_ref().and_then(|m| m.duration)))
}

/// A length the probe gave, or why there is none.
fn known_length(len: Option<f64>, id: &str) -> anyhow::Result<f64> {
    len.ok_or_else(|| anyhow::anyhow!("`{id}` has no known length: probe it again (`reprobe`)"))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NormalizeRequest {
    /// The media entry: its `volume` is what is set.
    pub bin: String,
    /// The integrated loudness it is brought to, LUFS (-50..-5).
    pub target: f64,
    /// The edit's origin, which the handler sets from the request's.
    #[serde(skip, default = "default_origin")]
    pub origin: String,
}

/// The most passes a normalisation takes: one measures, the next confirms, and a
/// compressor after the gain (which answers a gain by less than the gain) needs a few more.
const PASSES: usize = 6;
/// Close enough, LU: what the meter prints is to a tenth.
const WITHIN: f64 = 0.05;

/// Bring a media entry to an integrated loudness by setting its `volume` — Audacity's
/// *Loudness Normalization*, for every placement at once. The entry is measured as it
/// sounds: its whole length through its own chain (a high-pass, an equaliser, a limiter),
/// on a one-clip timeline made for the measurement and never stored, then measured again
/// until it is within a twentieth of a LU. One edit: the entry's `volume`.
///
/// A mono file plays on both channels, so it is measured as Audacity's "dual mono" does.
/// The gain acts where the entry's chain puts it (its `volume` effect, else first), always
/// before its limiter, so the peaks it raises are held. The stored volume is the best pass's
/// gain, and `after` what that pass measured.
pub fn normalize(state: &AppState, req: &NormalizeRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(req.target.is_finite() && (-50.0..=-5.0).contains(&req.target), "`target` is LUFS, -50..-5 (a voice is usually -16 to -23)");
    let mut project = read(&state.project).clone();
    let (_, length) = sounding_file(&project, &req.bin)?;
    let duration = known_length(length, &req.bin)?;
    let was = project.bin[&req.bin].volume;
    let volume_at = |gain: f64| was * 10f64.powf(gain / 20.0);

    // The measurement's own timeline: the entry, whole, on one audio track.
    let seq_id = project.free_bin_id("normalize");
    let track = Track { clips: vec![Clip::placing(project.free_track_or_clip_id("c"), req.bin.as_str(), 0.0, duration)], ..Track::empty(project.free_track_or_clip_id("t"), TrackKind::Audio, "") };
    let seq = kuma_core::Sequence { tracks: vec![track], markers: vec![], effects: vec![] };
    project.bin.insert(seq_id.clone(), kuma_core::BinItem::of("normalize", BinSource::Sequence(seq)));
    let frames = project.fps.clip_frames(0.0, duration);

    // Gain in dB against the entry's volume as it was, and the loudness each gain gave. A
    // linear chain answers a dB with a dB, and lands on the second pass; with the gain in
    // front of a compressor it answers with about a third, so each step after the first
    // follows the slope the last two measured (a secant), kept within reason.
    let mut tried: Vec<(f64, f64, f64)> = Vec::new();
    let mut gain = 0.0f64;
    let span = 90 / PASSES as u16;
    for pass in 0..PASSES {
        if let Some(entry) = project.bin.get_mut(&req.bin) {
            entry.volume = volume_at(gain);
        }
        let base = pass as u16 * span;
        let scan = scan_sound(state, &project, &seq_id, (frames.in_, frames.out), &|p, m| progress((base + p as u16 * span / 100) as u8, &format!("pass {}: {m}", pass + 1)), cancel)?;
        let loudness = scan.integrated.ok_or_else(|| anyhow::anyhow!("`{}` is silent as it plays: nothing to bring to a loudness", req.bin))?;
        tried.push((gain, loudness, scan.true_peak));
        let gap = req.target - loudness;
        let slope = match tried[..] {
            [.., (g0, l0, _), (g1, l1, _)] => {
                // A limiter (or a compressor after the gain) caps how loud the chain can
                // make it: more gain, and the loudness stays. Said, rather than chased.
                anyhow::ensure!(
                    !(g1 - g0 > 3.0 && l1 - l0 < 0.3 && gap > 0.0),
                    "`{}` stays at {l1:.1} LUFS however loud its gain: the limiter or compressor in its chain holds it there. Lower the target, or raise the limiter's ceiling",
                    req.bin
                );
                ((l1 - l0) / (g1 - g0)).clamp(0.2, 1.5)
            }
            _ => 1.0,
        };
        if gap.abs() < WITHIN || pass + 1 == PASSES {
            break;
        }
        gain += gap / slope;
        anyhow::ensure!(gain.is_finite() && gain <= 60.0, "`{}` would need more than 60 dB to reach {} LUFS: it is nearly silent, or a compressor after the entry's gain holds it down (put a `volume` after the compressor)", req.bin, req.target);
    }
    let &(gain, measured, peak) = tried.iter().min_by(|a, b| (a.1 - req.target).abs().total_cmp(&(b.1 - req.target).abs())).expect("one pass at least");
    let before = tried[0].1;
    let volume = (volume_at(gain) * 10000.0).round() / 10000.0;
    progress(95, "setting the volume");
    let bin = req.bin.clone();
    let ((), rev, warnings) = state.apply_built(&req.origin, None, |p| {
        anyhow::ensure!(p.bin.contains_key(&bin), "`{bin}` is gone");
        Ok((vec![bin_op(&bin, "/volume", volume.into())?], ()))
    })?;
    Ok(serde_json::json!({
        "bin": req.bin,
        "target": req.target,
        "before": db(before),
        "after": db(measured),
        "true_peak": db(peak),
        "volume_before": was,
        "volume": volume,
        "gain_db": db(20.0 * (volume / was.max(1e-9)).log10()),
        "passes": tried.len(),
        "revision": rev,
        "warnings": warnings,
        "note": "LUFS of the entry alone through its own effects, before any placement's volume or fades; `after` is what the pass whose volume is kept measured.",
    }))
}
