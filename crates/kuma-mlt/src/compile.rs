//! Project → MLT XML.
//!
//! Layout of the emitted document (flat form, what MLT's own `xml` consumer writes):
//!
//! ```text
//! <mlt LC_NUMERIC="C" root=… producer="main_tractor">
//!   <profile …/>
//!   …one block per nested sequence, innermost first…           see `emit_sequence`
//!   <producer id="black"/>          <playlist id="background"/>      track 0
//!   <chain id="clip_c1">…filters…</chain>   one producer copy per timeline clip
//!   <playlist id="v1_a"/> [<playlist id="v1_b"/> <tractor id="track_v1"> luma… </tractor>]
//!   <tractor id="main_tractor">
//!     <multitrack> background, track_v1|v1_a, … </multitrack>
//!     <transition mix  a=0 b=i/>  <transition qtblend a=0 b=i/>   per track
//!   </tractor>
//! </mlt>
//! ```
//!
//! ## Nested sequences
//!
//! MLT nests natively: a sequence is a tractor and a tractor is a producer. Each sequence
//! is emitted **once**, innermost first, and a clip placing it is a wrapper playlist:
//!
//! ```text
//! <tractor id="seq_part">…</tractor>              emitted once, however often it is used
//! <playlist id="clip_c1">                         one per placement
//!   <entry producer="seq_part" in="60" out="209"/>     the trim into the sequence
//!   <filter …/>                                        this placement's effects
//! </playlist>
//! ```
//!
//! The wrapper is what keeps one tractor shareable, and it starts at its own zero, so the
//! lane cuts it at `0..len-1` — see [`Plan::entry`]. A nested sequence's background is
//! transparent, not black, so its gaps show the tracks underneath; only the master gets an
//! opaque base. Speed on a sequence clip is refused by the validator. `docs/decisions.md`
//! records why each of those is the shape it is, and what was measured to settle it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use kuma_core::*;

use crate::lumas;
use crate::xml::{num, XmlWriter};

/// A baked overlay instance, produced by `kuma-overlay`.
#[derive(Debug, Clone)]
pub struct OverlayBake {
    /// File path, or an image-sequence pattern (`…/frame_%06d.png`).
    pub resource: PathBuf,
    /// True for a single-image bake (static template).
    pub is_still: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnbakedPolicy {
    /// Leave the overlay out (preview: it's live DOM instead).
    #[default]
    Skip,
    /// Fail compilation.
    Error,
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Project directory: `root` attribute and base for relative asset paths.
    pub root: PathBuf,
    /// Where generated lumas and other scratch files go.
    pub cache_dir: PathBuf,
    /// Overlay bakes keyed by clip id.
    pub bakes: HashMap<ClipId, OverlayBake>,
    pub unbaked: UnbakedPolicy,
    /// Override the output size (preview at half resolution). Transforms scale with it.
    pub size: Option<(u32, u32)>,
    /// Which sequence to compile as the root. `None` means the project's master — what a
    /// render always wants. The preview sets it to whichever sequence is open.
    pub sequence: Option<BinId>,
}

impl CompileOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        CompileOptions {
            cache_dir: root.join(".kuma").join("cache"),
            root,
            bakes: HashMap::new(),
            unbaked: UnbakedPolicy::Skip,
            size: None,
            sequence: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("project is invalid: {0:?}")]
    Invalid(Vec<Issue>),
    #[error("overlay clip `{0}` has no bake")]
    Unbaked(ClipId),
    /// Anything else: an empty timeline, a missing luma, or a frame plan the validator
    /// should have rejected.
    #[error("{0}")]
    Internal(String),
}

/// Compile a project to MLT XML.
pub fn compile(project: &Project, opts: &CompileOptions) -> Result<String, CompileError> {
    let issues = validate(project, &ValidationContext { check_files: false, ..Default::default() });
    if kuma_core::validate::has_errors(&issues) {
        return Err(CompileError::Invalid(issues));
    }
    let root = opts.sequence.clone().unwrap_or_else(|| project.master.clone());
    if project.seq(&root).is_none() {
        return Err(CompileError::Internal(format!("`{root}` is not a sequence in this project")));
    }
    // Innermost first, so every reference below points at a tractor already written. The
    // validator refuses cycles, so this only fails on a document it never saw.
    let order = project.compile_order(&root).map_err(|ring| CompileError::Internal(format!("sequence cycle: {}", ring.join(" → "))))?;
    let duration = project.seq_duration(&root);
    if duration <= 0.0 {
        return Err(CompileError::Internal("empty timeline".into()));
    }
    let fps = project.fps;

    let (pw, ph) = opts.size.unwrap_or((project.width, project.height));
    let scale = pw as f64 / project.width as f64;
    let ctx = Ctx { project, opts, fps, pw, ph, scale };

    let mut w = XmlWriter::new();
    w.open(
        "mlt",
        &[
            ("LC_NUMERIC", "C".into()),
            ("version", "7.22.0".into()),
            ("root", opts.root.to_string_lossy().into_owned()),
            ("producer", "main_tractor".into()),
        ],
    );
    w.empty(
        "profile",
        &[
            ("description", format!("kuma {pw}x{ph} {}/{}", fps.num, fps.den)),
            ("width", pw.to_string()),
            ("height", ph.to_string()),
            ("progressive", "1".into()),
            ("sample_aspect_num", "1".into()),
            ("sample_aspect_den", "1".into()),
            ("display_aspect_num", pw.to_string()),
            ("display_aspect_den", ph.to_string()),
            ("frame_rate_num", fps.num.to_string()),
            ("frame_rate_den", fps.den.to_string()),
            ("colorspace", "709".into()),
        ],
    );

    // `order` ends on the root, which becomes `main_tractor`; the rest are nested.
    for id in &order {
        let seq = project.seq(id).ok_or_else(|| CompileError::Internal(format!("`{id}` is not a sequence")))?;
        emit_sequence(&ctx, id, seq, id == &root, &mut w)?;
    }
    w.close("mlt");
    Ok(w.finish())
}

/// One timeline: a background, its tracks' producers and playlists, then the tractor that
/// composites them. The root is the bottom of the render, so it gets an opaque black base
/// and the name the document points at; a nested sequence gets a transparent one, so it
/// composites over whatever it was laid on like any other clip.
fn emit_sequence(ctx: &Ctx, seq_id: &str, seq: &Sequence, is_root: bool, w: &mut XmlWriter) -> Result<(), CompileError> {
    let total = ctx.fps.length(seq.duration());
    let (tractor_id, bg, bg_pl) = if is_root {
        ("main_tractor".to_string(), "black".to_string(), "background".to_string())
    } else {
        (format!("seq_{seq_id}"), format!("bg_{seq_id}"), format!("bgpl_{seq_id}"))
    };
    let span = [("id", bg.clone()), ("in", "0".into()), ("out", (total - 1).to_string())];
    emit_color(w, &span, if is_root { "black" } else { "0x00000000" }, total, |_| {});
    w.open("playlist", &[("id", bg_pl.clone())]);
    w.empty("entry", &[("producer", bg), ("in", "0".into()), ("out", (total - 1).to_string())]);
    w.close("playlist");

    let mut compiled_tracks: Vec<CompiledTrack> = Vec::new();
    for track in &seq.tracks {
        if let Some(ct) = compile_track(ctx, track, total, w)? {
            compiled_tracks.push(ct);
        }
    }

    w.open("tractor", &[("id", tractor_id), ("in", "0".into()), ("out", (total - 1).to_string())]);
    w.open("multitrack", &[]);
    w.empty("track", &[("producer", bg_pl)]);
    for ct in &compiled_tracks {
        let mut attrs = vec![("producer", ct.producer_id.clone())];
        if let Some(h) = ct.hide {
            attrs.push(("hide", h.into()));
        }
        w.empty("track", &attrs);
    }
    w.close("multitrack");
    for (i, ct) in compiled_tracks.iter().enumerate() {
        // Every track composites against track 0, the background, for the whole timeline.
        let against_background = |w: &mut XmlWriter, prefix: &str, service: &str| {
            w.open("transition", &[("id", format!("{prefix}_{}", ct.id))]);
            w.property("mlt_service", service);
            w.property("a_track", "0");
            w.property("b_track", (i + 1).to_string());
            w.property("always_active", "1");
        };
        if ct.has_audio {
            against_background(w, "mix", "mix");
            w.property("sum", "1");
            w.close("transition");
        }
        if ct.has_video {
            against_background(w, "comp", "qtblend");
            if !ct.blends.is_empty() {
                w.property("compositing", blend_anim(&ct.blends, total));
            }
            w.close("transition");
        }
    }
    w.close("tractor");
    Ok(())
}

struct Ctx<'a> {
    project: &'a Project,
    opts: &'a CompileOptions,
    fps: Fps,
    pw: u32,
    ph: u32,
    scale: f64,
}

impl Ctx<'_> {
    /// The validator rejects unknown bin ids, but a project loaded from disk is only
    /// validated on edit, so never index the map.
    fn item(&self, clip: &Clip) -> Result<&BinItem, CompileError> {
        self.project.item(clip).ok_or_else(|| CompileError::Internal(format!("clip `{}`: unknown bin entry `{}`", clip.id, clip.bin)))
    }
}

struct CompiledTrack {
    id: String,
    producer_id: String,
    hide: Option<&'static str>,
    has_video: bool,
    has_audio: bool,
    /// (timeline in, timeline out, mode) for non-normal blend modes.
    blends: Vec<(i64, i64, BlendMode)>,
}

enum Item {
    Blank(i64),
    Entry { producer: String, in_: i64, out: i64 },
}

/// How a dissolve extends a clip: frames added before (as the `to` side) and after (as
/// the `from` side) its own range.
#[derive(Default, Clone, Copy)]
struct Ext {
    before: i64,
    after: i64,
    is_to: bool,
}

/// Per-clip frame plan after transition handles are applied.
struct Plan<'a> {
    clip: &'a Clip,
    lane: usize,
    /// Timeline in/out (inclusive), including any transition extension.
    tl_in: i64,
    tl_out: i64,
    /// Producer in/out (inclusive), including any transition extension: the cut into the
    /// thing this clip plays.
    p_in: i64,
    p_out: i64,
    /// True when this clip is emitted as a wrapper playlist that has already made the cut
    /// (a sequence placement), rather than a producer the lane cuts directly. A wrapper
    /// starts at its own zero, which is the whole of what [`Plan::entry`] decides.
    wrapped: bool,
    /// Frames added before/after the clip's own range for a dissolve. Effects are
    /// measured from the clip's own start, so keyframes are offset by `pre`.
    pre: i64,
    post: i64,
    producer_id: String,
}

impl Plan<'_> {
    /// The range to ask this clip's element for: what the lane playlist's entry cuts, and
    /// what its filters span. Derived rather than stored, so no site can reach for the
    /// wrong one of two ranges that are the same length at different offsets.
    fn entry(&self) -> (i64, i64) {
        if self.wrapped { (0, self.p_out - self.p_in) } else { (self.p_in, self.p_out) }
    }
}

fn compile_track(ctx: &Ctx, track: &Track, total: i64, w: &mut XmlWriter) -> Result<Option<CompiledTrack>, CompileError> {
    let fps = ctx.fps;
    let tid = &track.id;

    // How each clip is extended by the transitions it takes part in.
    let mut ext: HashMap<&str, Ext> = HashMap::new();
    for x in &track.transitions {
        let d = fps.length(x.duration);
        let before = d / 2;
        ext.entry(x.from.as_str()).or_default().after = d - before;
        let to = ext.entry(x.to.as_str()).or_default();
        to.before = before;
        to.is_to = true;
    }

    // Plan lanes and frame ranges.
    let mut plans: Vec<Plan> = Vec::new();
    let mut prev_lane = 0usize;
    for (_, clip) in track.sorted_clips() {
        let item = ctx.item(clip)?;
        // Overlays without a bake are skipped (they are live DOM in preview).
        if item.is_overlay() && !ctx.opts.bakes.contains_key(&clip.id) {
            match ctx.opts.unbaked {
                UnbakedPolicy::Skip => continue,
                UnbakedPolicy::Error => return Err(CompileError::Unbaked(clip.id.clone())),
            }
        }
        let e = ext.get(clip.id.as_str()).copied().unwrap_or_default();
        // Lane 0 is the default; a clip only takes the other lane to overlap the clip it
        // dissolves from.
        let lane = if e.is_to { 1 - prev_lane } else { 0 };
        prev_lane = lane;

        let cf = fps.clip_frames(clip.start, clip.end);
        let tl_in = cf.in_ - e.before;
        let tl_out = cf.out + e.after;
        let len = tl_out - tl_in + 1;

        // Stills, colours and overlays are generated to fit and start at their own zero.
        let (p_in, p_out) = if item.is_timed_media() {
            // Producer time is warped by the speed; the handle is already in source time,
            // so it is subtracted after the warp.
            let sin = fps.frame(clip.source_in / clip.speed.abs()) - e.before;
            if sin < 0 {
                return Err(CompileError::Internal(format!("clip `{}` has no handle before its in point", clip.id)));
            }
            (sin, sin + len - 1)
        } else {
            (0, len - 1)
        };
        let producer_id = format!("clip_{}", clip.id);
        plans.push(Plan { clip, lane, tl_in, tl_out, p_in, p_out, wrapped: item.is_sequence(), pre: e.before, post: e.after, producer_id });
    }
    if plans.is_empty() {
        return Ok(None);
    }

    // Emit one producer per clip.
    let mut blends = Vec::new();
    let mut has_audio = false;
    for p in &plans {
        emit_clip_producer(ctx, p, w)?;
        // Short-circuit: one audible clip settles the track, and for a sequence clip the
        // question costs a walk of everything it nests.
        has_audio = has_audio || ctx.project.clip_has_audio(p.clip);
        for e in &p.clip.effects {
            if let Effect::Blend { mode } = e {
                if *mode != BlendMode::Normal {
                    blends.push((p.tl_in, p.tl_out, *mode));
                }
            }
        }
    }

    // Build lane playlists.
    let mut lanes: [Vec<Item>; 2] = [Vec::new(), Vec::new()];
    let mut cursor = [0i64, 0i64];
    for p in &plans {
        let l = p.lane;
        if p.tl_in < cursor[l] {
            return Err(CompileError::Internal(format!("clip `{}` overlaps the previous clip on its lane", p.clip.id)));
        }
        if p.tl_in > cursor[l] {
            lanes[l].push(Item::Blank(p.tl_in - cursor[l]));
        }
        let (in_, out) = p.entry();
        lanes[l].push(Item::Entry { producer: p.producer_id.clone(), in_, out });
        cursor[l] = p.tl_out + 1;
    }
    let two_lanes = !lanes[1].is_empty();
    for (l, items) in lanes.iter().enumerate() {
        if l == 1 && !two_lanes {
            break;
        }
        let suffix = if l == 0 { "a" } else { "b" };
        w.open("playlist", &[("id", format!("{tid}_{suffix}"))]);
        for it in items {
            match it {
                Item::Blank(n) => w.empty("blank", &[("length", n.to_string())]),
                Item::Entry { producer, in_, out } => w.empty(
                    "entry",
                    &[("producer", producer.clone()), ("in", in_.to_string()), ("out", out.to_string())],
                ),
            }
        }
        w.close("playlist");
    }

    let producer_id = if two_lanes {
        let tr_id = format!("track_{tid}");
        w.open("tractor", &[("id", tr_id.clone()), ("in", "0".into()), ("out", (total - 1).to_string())]);
        w.open("multitrack", &[]);
        w.empty("track", &[("producer", format!("{tid}_a"))]);
        w.empty("track", &[("producer", format!("{tid}_b"))]);
        w.close("multitrack");
        for x in &track.transitions {
            let Some(from) = plans.iter().find(|p| p.clip.id == x.from) else { continue };
            let Some(to) = plans.iter().find(|p| p.clip.id == x.to) else { continue };
            let span = [("in", to.tl_in.to_string()), ("out", from.tl_out.to_string())];
            let open = |w: &mut XmlWriter, prefix: &str, service: &str| {
                w.open("transition", &[("id", format!("{prefix}_{}_{}", x.from, x.to)), span[0].clone(), span[1].clone()]);
                w.property("mlt_service", service);
                w.property("a_track", from.lane.to_string());
                w.property("b_track", to.lane.to_string());
            };
            // Video.
            open(w, "luma", "luma");
            w.property("progressive", "1");
            if x.kind == TransitionKind::Wipe {
                let name = x.luma.as_deref().unwrap_or("wipe_left");
                let path = lumas::resolve(name, &ctx.opts.root, &ctx.opts.cache_dir).map_err(|e| CompileError::Internal(format!("luma: {e}")))?;
                w.property("resource", path.to_string_lossy());
                w.property("softness", num(x.softness));
            }
            w.close("transition");
            // Audio crossfade.
            open(w, "xfade", "mix");
            w.property("start", "0");
            w.property("end", "1");
            w.close("transition");
        }
        w.close("tractor");
        tr_id
    } else {
        format!("{tid}_a")
    };

    let has_video = track.kind != TrackKind::Audio && !track.hidden;
    let has_audio = has_audio && !track.muted;
    let hide = match (has_video, has_audio) {
        (false, false) => Some("both"),
        (false, true) => Some("video"),
        (true, false) => Some("audio"),
        (true, true) => None,
    };
    Ok(Some(CompiledTrack { id: tid.clone(), producer_id, hide, has_video, has_audio, blends }))
}

fn emit_clip_producer(ctx: &Ctx, p: &Plan, w: &mut XmlWriter) -> Result<(), CompileError> {
    let clip = p.clip;
    let len = p.p_out - p.p_in + 1;
    // The producer spans 0..=p_out; the playlist entry cuts it to p_in..=p_out. Putting
    // `in` on the producer too would clamp its playtime and freeze the clip on one frame.
    let base = [("id", p.producer_id.clone()), ("in", "0".into()), ("out", p.p_out.to_string())];

    match &ctx.item(clip)?.source {
        BinSource::File { path: media, kind, proxy } => {
            // Proxies stand in for the media only in a scaled (preview) compile.
            let source = proxy.as_deref().filter(|_| ctx.opts.size.is_some()).unwrap_or(media);
            let path = resource_path(&ctx.opts.root, source);
            let warped = (clip.speed - 1.0).abs() > 1e-9;
            let still = *kind == MediaKind::Image;
            // Plain media goes through a `chain` (MLT 7 avformat with link filters);
            // stills and time-warped media are classic producers.
            let tag = if still || warped { "producer" } else { "chain" };
            w.open(tag, &base);
            if still {
                w.property("mlt_service", "pixbuf");
                w.property("resource", &path);
                w.property("length", len.to_string());
                w.property("ttl", "1");
            } else if warped {
                w.property("mlt_service", "timewarp");
                w.property("resource", format!("{}:{}", num(clip.speed), path));
                w.property("warp_pitch", "1");
            } else {
                w.property("mlt_service", "avformat-novalidate");
                w.property("resource", &path);
            }
            if clip.muted {
                w.property("audio_index", "-1");
            }
            emit_filters(ctx, p, w);
            w.close(tag);
        }
        BinSource::Color { color } => emit_color(w, &base, &mlt_color(color), len, |w| emit_filters(ctx, p, w)),
        // A sequence is already a producer (its tractor), emitted once for the whole
        // document. This placement is a playlist that cuts it and hangs its own filters,
        // so a second placement of the same sequence is unaffected by them.
        BinSource::Sequence(_) => {
            w.open("playlist", &[("id", p.producer_id.clone())]);
            w.empty("entry", &[("producer", format!("seq_{}", clip.bin)), ("in", p.p_in.to_string()), ("out", p.p_out.to_string())]);
            emit_filters(ctx, p, w);
            w.close("playlist");
        }
        BinSource::Overlay { .. } => {
            let bake = ctx.opts.bakes.get(&clip.id).ok_or_else(|| CompileError::Unbaked(clip.id.clone()))?;
            w.open("producer", &base);
            if bake.is_still {
                w.property("mlt_service", "pixbuf");
                w.property("ttl", "1");
            } else {
                w.property("mlt_service", "avformat");
            }
            w.property("resource", bake.resource.to_string_lossy());
            w.property("length", len.to_string());
            w.property("mlt_image_format", "rgba");
            emit_filters(ctx, p, w);
            w.close("producer");
        }
    }
    Ok(())
}

/// A `color` producer with an alpha channel: a sequence's background, or a colour clip.
fn emit_color(w: &mut XmlWriter, attrs: &[(&str, String)], resource: &str, length: i64, body: impl FnOnce(&mut XmlWriter)) {
    w.open("producer", attrs);
    w.property("mlt_service", "color");
    w.property("resource", resource);
    w.property("length", length.to_string());
    w.property("mlt_image_format", "rgba");
    body(w);
    w.close("producer");
}

fn emit_filters(ctx: &Ctx, p: &Plan, w: &mut XmlWriter) {
    let clip = p.clip;
    let fps = ctx.fps;
    let (f_in, f_out) = p.entry();
    let len = f_out - f_in + 1;
    let n = std::cell::Cell::new(0usize);
    // Open a `<filter>` with a unique id over an explicit frame range; the caller adds
    // properties and closes it.
    let open_at = |w: &mut XmlWriter, name: &str, service: &str, in_: i64, out: i64| {
        n.set(n.get() + 1);
        w.open("filter", &[("id", format!("{}_{name}{}", p.producer_id, n.get())), ("in", in_.to_string()), ("out", out.to_string())]);
        w.property("mlt_service", service);
    };
    let open = |w: &mut XmlWriter, name: &str, service: &str| open_at(w, name, service, f_in, f_out);
    // Keyframe positions are relative to the clip's own start, i.e. `pre` frames in.
    let at = |t: f64| (fps.frame(t) + p.pre).clamp(0, len - 1);

    // Clip-level volume (static) unless a Volume effect provides keyframes.
    let has_volume_kf = clip.effects.iter().any(|e| matches!(e, Effect::Volume { .. }));
    if !has_volume_kf && (clip.volume - 1.0).abs() > 1e-9 {
        open(w, "vol", "volume");
        w.property("level", num(db(clip.volume)));
        w.close("filter");
    }

    for e in &clip.effects {
        match e {
            Effect::Transform { x, y, scale, rotation, opacity, keyframes } => {
                open(w, "xform", "qtblend");
                w.property("compositing", "0");
                if keyframes.is_empty() {
                    w.property("rect", rect_str(ctx, *x, *y, *scale, *opacity));
                    w.property("rotation", num(*rotation));
                } else {
                    let mut rect = Vec::new();
                    let mut rot = Vec::new();
                    for k in keyframes {
                        let pos = at(k.t);
                        rect.push(format!(
                            "{pos}={}",
                            rect_str(ctx, k.x.unwrap_or(*x), k.y.unwrap_or(*y), k.scale.unwrap_or(*scale), k.opacity.unwrap_or(*opacity))
                        ));
                        rot.push(format!("{pos}={}", num(k.rotation.unwrap_or(*rotation))));
                    }
                    w.property("rect", rect.join(";"));
                    w.property("rotation", rot.join(";"));
                }
                w.close("filter");
            }
            Effect::Crop { left, right, top, bottom, radius } => {
                open(w, "crop", "qtcrop");
                w.property(
                    "rect",
                    format!(
                        "{}%/{}%:{}%x{}%",
                        num(left * 100.0),
                        num(top * 100.0),
                        num((1.0 - left - right) * 100.0),
                        num((1.0 - top - bottom) * 100.0)
                    ),
                );
                if *radius > 0.0 {
                    w.property("radius", num(*radius));
                }
                w.close("filter");
            }
            Effect::Blur { radius } => {
                open(w, "blur", "avfilter.gblur");
                w.property("av.sigma", num((radius * ctx.scale / 2.0).max(0.01)));
                w.property("av.steps", "2");
                w.close("filter");
            }
            Effect::Color { brightness, contrast, saturation, gamma } => {
                open(w, "color", "avfilter.eq");
                w.property("av.brightness", num(*brightness));
                w.property("av.contrast", num(*contrast));
                w.property("av.saturation", num(*saturation));
                w.property("av.gamma", num(*gamma));
                w.close("filter");
            }
            Effect::Fade { fade_in, fade_out } => {
                // Fades span the clip's own range, not the dissolve handles.
                let (own_in, own_out) = (f_in + p.pre, f_out - p.post);
                let own_len = own_out - own_in + 1;
                if *fade_in > 0.0 {
                    let fi = fps.length(*fade_in).min(own_len);
                    open_at(w, "fadein", "brightness", own_in, own_in + fi - 1);
                    w.property("alpha", "0=0;-1=1");
                    w.close("filter");
                }
                if *fade_out > 0.0 {
                    let fo = fps.length(*fade_out).min(own_len);
                    open_at(w, "fadeout", "brightness", own_out - fo + 1, own_out);
                    w.property("alpha", "0=1;-1=0");
                    w.close("filter");
                }
            }
            Effect::Volume { keyframes } => {
                open(w, "vol", "volume");
                let s: Vec<String> = keyframes.iter().map(|k| format!("{}={}", at(k.t), num(db(k.gain * clip.volume)))).collect();
                w.property("level", s.join(";"));
                w.close("filter");
            }
            Effect::Mlt { service, properties } => {
                open(w, "mlt", service);
                for (k, v) in properties {
                    w.property(k, v);
                }
                w.close("filter");
            }
            Effect::Blend { .. } => {} // handled on the track's compositing transition
        }
    }
}

/// qtblend `rect` = "x y w h opacity" in profile pixels, from centre-relative offsets.
fn rect_str(ctx: &Ctx, x: f64, y: f64, scale: f64, opacity: f64) -> String {
    let w = ctx.pw as f64 * scale;
    let h = ctx.ph as f64 * scale;
    let rx = (ctx.pw as f64 - w) / 2.0 + x * ctx.scale;
    let ry = (ctx.ph as f64 - h) / 2.0 + y * ctx.scale;
    format!("{} {} {} {} {}", num(rx), num(ry), num(w), num(h), num(opacity.clamp(0.0, 1.0)))
}

/// Discrete keyframes (`|=`) for the compositing mode so it never interpolates. A clip's
/// mode is reset to normal after it ends unless another blended clip covers that frame.
fn blend_anim(blends: &[(i64, i64, BlendMode)], total: i64) -> String {
    let mut kf: BTreeMap<i64, i32> = BTreeMap::new();
    kf.insert(0, 0);
    for (_, out, _) in blends {
        let next = out + 1;
        if next < total && !blends.iter().any(|(i, o, _)| (*i..=*o).contains(&next)) {
            kf.insert(next, 0);
        }
    }
    for (in_, _, mode) in blends {
        kf.insert(*in_, qt_mode(*mode));
    }
    kf.iter().map(|(k, v)| format!("{k}|={v}")).collect::<Vec<_>>().join(";")
}

/// Qt `QPainter::CompositionMode` numbers used by qtblend.
fn qt_mode(m: BlendMode) -> i32 {
    match m {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 13,
        BlendMode::Screen => 14,
        BlendMode::Overlay => 15,
        BlendMode::Darken => 16,
        BlendMode::Lighten => 17,
        BlendMode::ColorDodge => 18,
        BlendMode::ColorBurn => 19,
        BlendMode::HardLight => 20,
        BlendMode::SoftLight => 21,
        BlendMode::Difference => 22,
        BlendMode::Exclusion => 23,
    }
}

fn db(gain: f64) -> f64 {
    if gain <= 0.001 {
        -60.0
    } else {
        20.0 * gain.log10()
    }
}

/// `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa` (validated upstream) → MLT `0xRRGGBBAA`.
fn mlt_color(css: &str) -> String {
    let hex = css.trim_start_matches('#');
    let mut full: String = if hex.len() <= 4 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_string() };
    if full.len() == 6 {
        full.push_str("ff");
    }
    format!("0x{full}")
}

fn resource_path(root: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        root.join(path).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colour_conversion() {
        assert_eq!(mlt_color("#ff4f00"), "0xff4f00ff");
        assert_eq!(mlt_color("#ff4f00cc"), "0xff4f00cc");
        assert_eq!(mlt_color("#abc"), "0xaabbccff");
        assert_eq!(mlt_color("#abcd"), "0xaabbccdd");
    }

    #[test]
    fn blend_keyframes_are_discrete() {
        let s = blend_anim(&[(60, 239, BlendMode::Screen)], 450);
        assert_eq!(s, "0|=0;60|=14;240|=0");
        // A dissolve overlaps two blended clips: no reset inside the second one.
        let s = blend_anim(&[(0, 164, BlendMode::Screen), (135, 239, BlendMode::Screen)], 450);
        assert_eq!(s, "0|=14;135|=14;240|=0");
    }

    /// A project from `{bin, tracks}`, with `tracks` folded into the master sequence.
    fn project(v: serde_json::Value) -> Project {
        let mut v = v;
        if let Some(tracks) = v.as_object_mut().and_then(|o| o.remove("tracks")) {
            v["bin"]["main"] = json!({ "source": { "tracks": tracks } });
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn transitions_split_into_two_lanes() {
        let p = project(json!({
            "fps": 30, "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "x" },
                { "id": "b", "start": 5, "end": 8, "bin": "x", "in": 2 }
            ], "transitions": [ { "from": "a", "to": "b", "duration": 1.0 } ]}]
        }));
        let xml = compile(&p, &CompileOptions::new("/proj")).unwrap();
        assert!(xml.contains(r#"<playlist id="v1_b">"#));
        assert!(xml.contains(r#"<tractor id="track_v1""#));
        // a: 0..149 extended by 15 → out 164.
        // b: 90 frames starting at 150, pulled 15 earlier → timeline 135..239, source in 60-15=45.
        assert!(xml.contains(r#"<entry producer="clip_a" in="0" out="164"/>"#), "{xml}");
        assert!(xml.contains(r#"<blank length="135"/>"#), "{xml}");
        assert!(xml.contains(r#"<entry producer="clip_b" in="45" out="149"/>"#), "{xml}");
        assert!(xml.contains(r#"<transition id="luma_a_b" in="135" out="164">"#), "{xml}");
    }

    fn nested() -> Project {
        project(json!({
            "fps": 30,
            "bin": {
                "x": { "source": { "path": "x.mp4" } },
                "part": { "source": { "tracks": [{ "id": "pv", "clips": [
                    { "id": "p1", "start": 0, "end": 6, "bin": "x" }
                ]}]}}
            },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "s1", "start": 0, "end": 2, "bin": "part", "in": 1 },
                { "id": "s2", "start": 2, "end": 4, "bin": "part", "effects": [ { "type": "blur", "radius": 3 } ] }
            ]}]
        }))
    }

    /// A sequence becomes one tractor whatever the number of placements, each placement a
    /// wrapper playlist that does its own cut. Emitting a tractor per placement instead
    /// would duplicate the subtree, multiplicatively once sequences nest.
    #[test]
    fn a_sequence_is_emitted_once_and_placed_through_wrappers() {
        let xml = compile(&nested(), &CompileOptions::new("/proj")).unwrap();
        assert_eq!(xml.matches(r#"<tractor id="seq_part""#).count(), 1, "{xml}");
        // s1 is trimmed 1s into the sequence and runs 2s: source frames 30..89.
        assert!(xml.contains(r#"<playlist id="clip_s1">"#), "{xml}");
        assert!(xml.contains(r#"<entry producer="seq_part" in="30" out="89"/>"#), "{xml}");
        assert!(xml.contains(r#"<entry producer="seq_part" in="0" out="59"/>"#), "{xml}");
        // The lane cuts the wrapper from its own zero, not from the cut into the sequence.
        assert!(xml.contains(r#"<entry producer="clip_s1" in="0" out="59"/>"#), "{xml}");
        // The blur belongs to s2's placement alone.
        let s2 = xml.split(r#"<playlist id="clip_s2">"#).nth(1).unwrap();
        assert!(s2[..s2.find("</playlist>").unwrap()].contains("avfilter.gblur"), "{xml}");
        let s1 = xml.split(r#"<playlist id="clip_s1">"#).nth(1).unwrap();
        assert!(!s1[..s1.find("</playlist>").unwrap()].contains("avfilter.gblur"), "{xml}");
    }

    /// The inner tractor must be written before the outer one references it, and a nested
    /// sequence's own base must be transparent or it paints over the tracks below it.
    #[test]
    fn nested_sequences_are_ordered_and_composite_over_what_is_under_them() {
        let xml = compile(&nested(), &CompileOptions::new("/proj")).unwrap();
        assert!(xml.find(r#"<tractor id="seq_part""#) < xml.find(r#"<playlist id="clip_s1">"#), "{xml}");
        assert!(xml.find(r#"<playlist id="clip_s1">"#) < xml.find(r#"<tractor id="main_tractor""#), "{xml}");
        assert!(xml.contains(r#"<property name="resource">0x00000000</property>"#), "nested base is transparent: {xml}");
        // Only the master gets the opaque base the render sits on.
        assert_eq!(xml.matches(r#"<property name="resource">black</property>"#).count(), 1, "{xml}");
    }

    /// The preview compiles whichever sequence is open, so a sub-sequence has to stand on
    /// its own as a root — and as a root it is the bottom of the picture, hence opaque.
    #[test]
    fn a_sub_sequence_can_be_compiled_as_the_root() {
        let mut opts = CompileOptions::new("/proj");
        opts.sequence = Some("part".into());
        let xml = compile(&nested(), &opts).unwrap();
        assert!(xml.contains(r#"<tractor id="main_tractor" in="0" out="179">"#), "6s at 30fps: {xml}");
        assert!(!xml.contains("seq_part"), "nothing outside the sequence is compiled: {xml}");
        assert!(!xml.contains("0x00000000"), "as a root it gets the opaque base: {xml}");
    }

    /// Three levels deep: each tractor must be written before the one that references it,
    /// and each must be emitted once however many paths reach it. `deep` is placed by both
    /// `mid` and the master, which is where a tractor-per-placement scheme would start
    /// duplicating.
    #[test]
    fn nesting_is_ordered_at_depth_and_on_a_diamond() {
        let p = project(json!({
            "fps": 30,
            "bin": {
                "x": { "source": { "path": "x.mp4" } },
                "deep": { "source": { "tracks": [{ "id": "dv", "clips": [
                    { "id": "d1", "start": 0, "end": 4, "bin": "x" }
                ]}]}},
                "mid": { "source": { "tracks": [{ "id": "mv", "clips": [
                    { "id": "m1", "start": 0, "end": 4, "bin": "deep" }
                ]}]}}
            },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "s1", "start": 0, "end": 4, "bin": "mid" },
                { "id": "s2", "start": 4, "end": 8, "bin": "deep" }
            ]}]
        }));
        let xml = compile(&p, &CompileOptions::new("/proj")).unwrap();
        for id in ["seq_deep", "seq_mid"] {
            assert_eq!(xml.matches(&format!(r#"<tractor id="{id}""#)).count(), 1, "{id} emitted once: {xml}");
        }
        assert!(xml.find(r#"<tractor id="seq_deep""#) < xml.find(r#"<tractor id="seq_mid""#), "{xml}");
        assert!(xml.find(r#"<tractor id="seq_mid""#) < xml.find(r#"<tractor id="main_tractor""#), "{xml}");
    }

    /// A dissolve between two sequence placements. The handle extends the cut inside the
    /// wrapper while the lane still reads it from zero, and the luma spans track time —
    /// so the two frame ranges must not be confused for one another.
    #[test]
    fn a_dissolve_between_two_sequence_clips_keeps_its_two_frame_ranges() {
        let p = project(json!({
            "fps": 30,
            "bin": {
                "x": { "source": { "path": "x.mp4" } },
                "part": { "source": { "tracks": [{ "id": "pv", "clips": [
                    { "id": "p1", "start": 0, "end": 10, "bin": "x" }
                ]}]}}
            },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "s1", "start": 0, "end": 4, "bin": "part", "in": 1 },
                { "id": "s2", "start": 4, "end": 8, "bin": "part", "in": 2 }
            ], "transitions": [ { "from": "s1", "to": "s2", "duration": 1.0 } ]}]
        }));
        let xml = compile(&p, &CompileOptions::new("/proj")).unwrap();
        // s1: source 30, 120 frames + 15 of tail handle → cut 30..164, lane 0..134.
        assert!(xml.contains(r#"<entry producer="seq_part" in="30" out="164"/>"#), "{xml}");
        assert!(xml.contains(r#"<entry producer="clip_s1" in="0" out="134"/>"#), "{xml}");
        // s2: in 2 s minus the 15-frame handle → source 45, pulled 15 frames earlier.
        assert!(xml.contains(r#"<entry producer="seq_part" in="45" out="179"/>"#), "{xml}");
        assert!(xml.contains(r#"<entry producer="clip_s2" in="0" out="134"/>"#), "{xml}");
        assert!(xml.contains(r#"<transition id="luma_s1_s2" in="105" out="134">"#), "{xml}");
    }

    /// An overlay inside a nested sequence is baked (the editor cannot drive it as live
    /// DOM, since its clip times are its own sequence's), so its producer has to land
    /// inside that sequence's tractor and not in the master's.
    #[test]
    fn a_baked_overlay_inside_a_nested_sequence_lands_in_that_sequence() {
        let p = project(json!({
            "fps": 30,
            "bin": {
                "tag": { "source": { "template": "location-tag" } },
                "part": { "source": { "tracks": [{ "id": "po1", "kind": "overlay", "clips": [
                    { "id": "o_in", "start": 0, "end": 4, "bin": "tag" }
                ]}]}}
            },
            "tracks": [{ "id": "v1", "clips": [ { "id": "s1", "start": 0, "end": 4, "bin": "part" } ] }]
        }));
        let mut opts = CompileOptions::new("/proj");
        // Unbaked it is skipped, which leaves the sequence empty and the compile with
        // nothing to place.
        assert!(!compile(&p, &opts).unwrap().contains("clip_o_in"));
        opts.bakes.insert("o_in".into(), OverlayBake { resource: "/proj/.kuma/bakes/x/frame_%06d.png".into(), is_still: false });
        let xml = compile(&p, &opts).unwrap();
        assert!(xml.contains(r#"<producer id="clip_o_in""#), "{xml}");
        assert!(xml.find("clip_o_in") < xml.find(r#"<tractor id="seq_part""#), "inside the sequence it belongs to: {xml}");
    }

    /// Sound two levels down still needs an audio mix on the track carrying the sequence.
    #[test]
    fn audio_inside_a_sequence_reaches_the_outer_mix() {
        let silent = project(json!({
            "fps": 30,
            "bin": {
                "still": { "source": { "path": "logo.png", "kind": "image" } },
                "part": { "source": { "tracks": [{ "id": "pv", "clips": [
                    { "id": "p1", "start": 0, "end": 4, "bin": "still" }
                ]}]}}
            },
            "tracks": [{ "id": "v1", "clips": [ { "id": "s1", "start": 0, "end": 4, "bin": "part" } ] }]
        }));
        assert!(!compile(&silent, &CompileOptions::new("/proj")).unwrap().contains(r#"id="mix_v1""#));
        assert!(compile(&nested(), &CompileOptions::new("/proj")).unwrap().contains(r#"id="mix_v1""#));
    }
}
