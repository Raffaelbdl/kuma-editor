//! Project → MLT XML.
//!
//! Layout of the emitted document (flat form, what MLT's own `xml` consumer writes):
//!
//! ```text
//! <mlt LC_NUMERIC="C" root=… producer="main_tractor">
//!   <profile …/>
//!   <producer id="black"/>          <playlist id="background"/>      track 0
//!   <chain id="clip_c1">…filters…</chain>   one producer copy per timeline clip
//!   <playlist id="v1_a"/> [<playlist id="v1_b"/> <tractor id="track_v1"> luma… </tractor>]
//!   <tractor id="main_tractor">
//!     <multitrack> background, track_v1|v1_a, … </multitrack>
//!     <transition mix  a=0 b=i/>  <transition qtblend a=0 b=i/>   per track
//!   </tractor>
//! </mlt>
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use kuma_core::template::Region;
use kuma_core::*;

use crate::lumas;
use crate::xml::{num, XmlWriter};

/// A baked overlay instance, produced by `kuma-overlay`.
#[derive(Debug, Clone)]
pub struct OverlayBake {
    /// File path, or an image-sequence pattern (`…/frame_%06d.png`).
    pub resource: PathBuf,
    /// Where the bake sits in the composition; `None` means full frame.
    pub region: Option<Region>,
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
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("project is invalid: {0:?}")]
    Invalid(Vec<Issue>),
    #[error("overlay clip `{0}` has no bake")]
    Unbaked(ClipId),
    #[error("luma: {0}")]
    Luma(String),
    #[error("empty timeline")]
    Empty,
}

/// Compile a project to MLT XML.
pub fn compile(project: &Project, opts: &CompileOptions) -> Result<String, CompileError> {
    let issues = validate(project, &ValidationContext { check_files: false, ..Default::default() });
    if kuma_core::validate::has_errors(&issues) {
        return Err(CompileError::Invalid(issues));
    }
    let fps = project.fps;
    let total = fps.length(project.duration());
    if project.duration() <= 0.0 {
        return Err(CompileError::Empty);
    }

    let (pw, ph) = opts.size.unwrap_or((project.width, project.height));
    let scale = pw as f64 / project.width as f64;
    let ctx = Ctx { project, opts, fps, pw, ph, scale, total };

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
            ("description", format!("kuma {}x{} {}", pw, ph, fps)),
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

    // Track 0: black background.
    w.open("producer", &[("id", "black".into()), ("in", "0".into()), ("out", (total - 1).to_string())]);
    w.property("mlt_service", "color");
    w.property("resource", "black");
    w.property("length", total.to_string());
    w.property("mlt_image_format", "rgba");
    w.close("producer");
    w.open("playlist", &[("id", "background".into())]);
    w.empty("entry", &[("producer", "black".into()), ("in", "0".into()), ("out", (total - 1).to_string())]);
    w.close("playlist");

    let mut compiled_tracks: Vec<CompiledTrack> = Vec::new();
    for track in &project.tracks {
        if let Some(ct) = compile_track(&ctx, track, &mut w)? {
            compiled_tracks.push(ct);
        }
    }

    // Main tractor.
    w.open("tractor", &[("id", "main_tractor".into()), ("in", "0".into()), ("out", (total - 1).to_string())]);
    w.open("multitrack", &[]);
    w.empty("track", &[("producer", "background".into())]);
    for ct in &compiled_tracks {
        let mut attrs = vec![("producer", ct.producer_id.clone())];
        if let Some(h) = ct.hide {
            attrs.push(("hide", h.into()));
        }
        w.empty("track", &attrs);
    }
    w.close("multitrack");
    for (i, ct) in compiled_tracks.iter().enumerate() {
        let b = (i + 1).to_string();
        if ct.has_audio {
            w.open("transition", &[("id", format!("mix_{}", ct.id))]);
            w.property("mlt_service", "mix");
            w.property("a_track", "0");
            w.property("b_track", &b);
            w.property("always_active", "1");
            w.property("sum", "1");
            w.close("transition");
        }
        if ct.has_video {
            w.open("transition", &[("id", format!("comp_{}", ct.id))]);
            w.property("mlt_service", "qtblend");
            w.property("a_track", "0");
            w.property("b_track", &b);
            w.property("always_active", "1");
            if !ct.blends.is_empty() {
                w.property("compositing", &blend_anim(&ct.blends, total));
            }
            w.close("transition");
        }
    }
    w.close("tractor");
    w.close("mlt");
    Ok(w.finish())
}

struct Ctx<'a> {
    project: &'a Project,
    opts: &'a CompileOptions,
    fps: Fps,
    pw: u32,
    ph: u32,
    scale: f64,
    total: i64,
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

#[derive(Clone)]
enum Item {
    Blank(i64),
    Entry { producer: String, in_: i64, out: i64 },
}

/// Per-clip frame plan after transition handles are applied.
struct Plan<'a> {
    clip: &'a Clip,
    lane: usize,
    /// Timeline in/out (inclusive), including any transition extension.
    tl_in: i64,
    tl_out: i64,
    /// Producer in/out (inclusive).
    p_in: i64,
    p_out: i64,
    producer_id: String,
}

fn compile_track(ctx: &Ctx, track: &Track, w: &mut XmlWriter) -> Result<Option<CompiledTrack>, CompileError> {
    let fps = ctx.fps;
    let clips = ctx.project.sorted_clips(track);
    let tid = ident(&track.id);

    // Which clips are the `to` of a transition, and by how much each side extends.
    let mut ext_after: HashMap<&str, i64> = HashMap::new();
    let mut ext_before: HashMap<&str, i64> = HashMap::new();
    let mut is_to: HashMap<&str, &Transition> = HashMap::new();
    for x in &track.transitions {
        let d = fps.length(x.duration);
        let before = d / 2;
        let after = d - before;
        ext_after.insert(x.from.as_str(), after);
        ext_before.insert(x.to.as_str(), before);
        is_to.insert(x.to.as_str(), x);
    }

    // Plan lanes and frame ranges.
    let mut plans: Vec<Plan> = Vec::new();
    let mut prev_lane = 0usize;
    for clip in clips {
        // Overlays without a bake are skipped (they are live DOM in preview).
        if let ClipSource::Overlay { .. } = clip.src {
            if !ctx.opts.bakes.contains_key(&clip.id) {
                match ctx.opts.unbaked {
                    UnbakedPolicy::Skip => continue,
                    UnbakedPolicy::Error => return Err(CompileError::Unbaked(clip.id.clone())),
                }
            }
        }
        let lane = if is_to.contains_key(clip.id.as_str()) { 1 - prev_lane } else { 0 };
        // Lane 0 is the default; a clip only goes to lane 1 to overlap its predecessor.
        let lane = if is_to.contains_key(clip.id.as_str()) { lane } else { 0 };
        prev_lane = lane;

        let cf = fps.clip_frames(clip.start, clip.end);
        let before = *ext_before.get(clip.id.as_str()).unwrap_or(&0);
        let after = *ext_after.get(clip.id.as_str()).unwrap_or(&0);
        let tl_in = cf.in_ - before;
        let tl_out = cf.out + after;
        let len = tl_out - tl_in + 1;

        let (p_in, p_out) = match &clip.src {
            ClipSource::Asset { source_in, .. } => {
                let asset = &ctx.project.assets[match &clip.src {
                    ClipSource::Asset { asset, .. } => asset,
                    _ => unreachable!(),
                }];
                if asset.kind == AssetKind::Image {
                    (0, len - 1)
                } else {
                    let sin = fps.frame(source_in / clip.speed.abs()) - before;
                    let sin = sin.max(0);
                    (sin, sin + len - 1)
                }
            }
            _ => (0, len - 1),
        };
        let producer_id = format!("clip_{}", ident(&clip.id));
        plans.push(Plan { clip, lane, tl_in, tl_out, p_in, p_out, producer_id });
    }
    if plans.is_empty() {
        return Ok(None);
    }

    // Emit one producer per clip.
    let mut blends = Vec::new();
    let mut has_audio = false;
    for p in &plans {
        emit_clip_producer(ctx, p, w)?;
        if let ClipSource::Asset { asset, .. } = &p.clip.src {
            let a = &ctx.project.assets[asset];
            if a.kind != AssetKind::Image && !p.clip.muted && !track.muted {
                has_audio = true;
            }
        }
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
        if p.tl_in > cursor[l] {
            lanes[l].push(Item::Blank(p.tl_in - cursor[l]));
        }
        lanes[l].push(Item::Entry { producer: p.producer_id.clone(), in_: p.p_in, out: p.p_out });
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
        w.open("tractor", &[("id", tr_id.clone()), ("in", "0".into()), ("out", (ctx.total - 1).to_string())]);
        w.open("multitrack", &[]);
        w.empty("track", &[("producer", format!("{tid}_a"))]);
        w.empty("track", &[("producer", format!("{tid}_b"))]);
        w.close("multitrack");
        for x in &track.transitions {
            let Some(from) = plans.iter().find(|p| p.clip.id == x.from) else { continue };
            let Some(to) = plans.iter().find(|p| p.clip.id == x.to) else { continue };
            let in_ = to.tl_in;
            let out = from.tl_out;
            let base = [("in", in_.to_string()), ("out", out.to_string())];
            // Video.
            w.open("transition", &[("id", format!("luma_{}_{}", ident(&x.from), ident(&x.to))), base[0].clone(), base[1].clone()]);
            w.property("mlt_service", "luma");
            w.property("a_track", from.lane.to_string());
            w.property("b_track", to.lane.to_string());
            w.property("progressive", "1");
            if x.kind == TransitionKind::Wipe {
                let name = x.luma.as_deref().unwrap_or("wipe_left");
                let path = lumas::resolve(name, &ctx.opts.cache_dir).map_err(|e| CompileError::Luma(e.to_string()))?;
                w.property("resource", path.to_string_lossy());
                w.property("softness", num(x.softness));
            }
            w.close("transition");
            // Audio crossfade.
            w.open("transition", &[("id", format!("xfade_{}_{}", ident(&x.from), ident(&x.to))), base[0].clone(), base[1].clone()]);
            w.property("mlt_service", "mix");
            w.property("a_track", from.lane.to_string());
            w.property("b_track", to.lane.to_string());
            w.property("start", "0");
            w.property("end", "1");
            w.close("transition");
        }
        w.close("tractor");
        tr_id
    } else {
        format!("{tid}_a")
    };

    let has_video = track.kind != TrackKind::Audio;
    let hide = match (track.hidden || !has_video, track.muted || !has_audio) {
        (true, true) => Some("both"),
        (true, false) => Some("video"),
        (false, true) => Some("audio"),
        (false, false) => None,
    };
    Ok(Some(CompiledTrack {
        id: tid,
        producer_id,
        hide,
        has_video: has_video && !track.hidden,
        has_audio: has_audio && !track.muted,
        blends,
    }))
}

fn emit_clip_producer(ctx: &Ctx, p: &Plan, w: &mut XmlWriter) -> Result<(), CompileError> {
    let clip = p.clip;
    let len = p.p_out - p.p_in + 1;
    let base = [("id", p.producer_id.clone()), ("in", p.p_in.to_string()), ("out", p.p_out.to_string())];

    match &clip.src {
        ClipSource::Asset { asset, .. } => {
            let a = &ctx.project.assets[asset];
            let path = resource_path(&ctx.opts.root, a.proxy.as_deref().unwrap_or(&a.path));
            if a.kind == AssetKind::Image {
                w.open("producer", &base);
                w.property("mlt_service", "pixbuf");
                w.property("resource", &path);
                w.property("length", len.to_string());
                w.property("ttl", "1");
            } else if (clip.speed - 1.0).abs() > 1e-9 {
                w.open("producer", &base);
                w.property("mlt_service", "timewarp");
                w.property("resource", format!("{}:{}", num(clip.speed), path));
                w.property("warp_pitch", "1");
            } else {
                w.open("chain", &base);
                w.property("mlt_service", "avformat-novalidate");
                w.property("resource", &path);
            }
            if clip.muted {
                w.property("audio_index", "-1");
            }
            let tag = if a.kind != AssetKind::Image && (clip.speed - 1.0).abs() <= 1e-9 { "chain" } else { "producer" };
            emit_filters(ctx, p, w);
            w.close(tag);
        }
        ClipSource::Color { color } => {
            w.open("producer", &base);
            w.property("mlt_service", "color");
            w.property("resource", mlt_color(color));
            w.property("length", len.to_string());
            w.property("mlt_image_format", "rgba");
            emit_filters(ctx, p, w);
            w.close("producer");
        }
        ClipSource::Overlay { .. } => {
            let bake = &ctx.opts.bakes[&clip.id];
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
            if let Some(r) = bake.region {
                // Place the region-sized bake at its offset within the frame.
                w.open("filter", &[("id", format!("{}_place", p.producer_id))]);
                w.property("mlt_service", "qtblend");
                w.property(
                    "rect",
                    format!(
                        "{} {} {} {} 1",
                        num(r.x as f64 * ctx.scale),
                        num(r.y as f64 * ctx.scale),
                        num(r.w as f64 * ctx.scale),
                        num(r.h as f64 * ctx.scale)
                    ),
                );
                w.property("distort", "1");
                w.close("filter");
            }
            emit_filters(ctx, p, w);
            w.close("producer");
        }
    }
    Ok(())
}

fn emit_filters(ctx: &Ctx, p: &Plan, w: &mut XmlWriter) {
    let clip = p.clip;
    let fps = ctx.fps;
    let len = p.p_out - p.p_in + 1;
    let fin = [("in", p.p_in.to_string()), ("out", p.p_out.to_string())];
    let mut n = 0usize;
    let mut next_id = |name: &str| {
        n += 1;
        format!("{}_{name}{n}", p.producer_id)
    };

    // Clip-level volume (static) unless a Volume effect provides keyframes.
    let has_volume_kf = clip.effects.iter().any(|e| matches!(e, Effect::Volume { .. }));
    if !has_volume_kf && (clip.volume - 1.0).abs() > 1e-9 {
        w.open("filter", &[("id", next_id("vol")), fin[0].clone(), fin[1].clone()]);
        w.property("mlt_service", "volume");
        w.property("level", num(db(clip.volume)));
        w.close("filter");
    }

    for e in &clip.effects {
        match e {
            Effect::Transform { x, y, scale, rotation, opacity, keyframes } => {
                w.open("filter", &[("id", next_id("xform")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", "qtblend");
                w.property("compositing", "0");
                if keyframes.is_empty() {
                    w.property("rect", rect_str(ctx, *x, *y, *scale, *opacity));
                    w.property("rotation", num(*rotation));
                } else {
                    let mut rect = Vec::new();
                    let mut rot = Vec::new();
                    for k in keyframes {
                        let pos = fps.frame(k.t).clamp(0, len - 1);
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
                w.open("filter", &[("id", next_id("crop")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", "qtcrop");
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
                w.open("filter", &[("id", next_id("blur")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", "avfilter.gblur");
                w.property("av.sigma", num((radius * ctx.scale / 2.0).max(0.01)));
                w.property("av.steps", "2");
                w.close("filter");
            }
            Effect::Color { brightness, contrast, saturation, gamma } => {
                w.open("filter", &[("id", next_id("color")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", "avfilter.eq");
                w.property("av.brightness", num(*brightness));
                w.property("av.contrast", num(*contrast));
                w.property("av.saturation", num(*saturation));
                w.property("av.gamma", num(*gamma));
                w.close("filter");
            }
            Effect::Fade { fade_in, fade_out } => {
                let fi = fps.length(*fade_in).min(len);
                if *fade_in > 0.0 {
                    w.open("filter", &[("id", next_id("fadein")), ("in", p.p_in.to_string()), ("out", (p.p_in + fi - 1).to_string())]);
                    w.property("mlt_service", "brightness");
                    w.property("alpha", "0=0;-1=1");
                    w.close("filter");
                }
                let fo = fps.length(*fade_out).min(len);
                if *fade_out > 0.0 {
                    w.open("filter", &[("id", next_id("fadeout")), ("in", (p.p_out - fo + 1).to_string()), ("out", p.p_out.to_string())]);
                    w.property("mlt_service", "brightness");
                    w.property("alpha", "0=1;-1=0");
                    w.close("filter");
                }
            }
            Effect::Volume { keyframes } => {
                w.open("filter", &[("id", next_id("vol")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", "volume");
                let s: Vec<String> = keyframes
                    .iter()
                    .map(|k| format!("{}={}", fps.frame(k.t).clamp(0, len - 1), num(db(k.gain * clip.volume))))
                    .collect();
                w.property("level", s.join(";"));
                w.close("filter");
            }
            Effect::Mlt { service, properties } => {
                w.open("filter", &[("id", next_id("mlt")), fin[0].clone(), fin[1].clone()]);
                w.property("mlt_service", service);
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

/// Discrete keyframes (`|=`) for the compositing mode so it never interpolates.
fn blend_anim(blends: &[(i64, i64, BlendMode)], total: i64) -> String {
    let mut kf: BTreeMap<i64, i32> = BTreeMap::new();
    kf.insert(0, 0);
    for (in_, out, mode) in blends {
        kf.insert(*in_, qt_mode(*mode));
        if out + 1 < total {
            kf.entry(out + 1).or_insert(0);
        }
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

/// `#rrggbb` / `#rrggbbaa` / `#rgb` → MLT `0xRRGGBBAA`.
fn mlt_color(css: &str) -> String {
    let hex = css.trim_start_matches('#');
    let full: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>() + "ff",
        4 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => format!("{hex}ff"),
        8 => hex.to_string(),
        _ => "000000ff".into(),
    };
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

/// XML ID-safe identifier.
fn ident(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if out.is_empty() || out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    out
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
    }

    #[test]
    fn blend_keyframes_are_discrete() {
        let s = blend_anim(&[(60, 239, BlendMode::Screen)], 450);
        assert_eq!(s, "0|=0;60|=14;240|=0");
    }

    #[test]
    fn transitions_split_into_two_lanes() {
        let p: Project = serde_json::from_value(json!({
            "fps": 30, "assets": { "x": { "path": "x.mp4" } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "src": { "asset": "x" } },
                { "id": "b", "start": 5, "end": 8, "src": { "asset": "x", "in": 2 } }
            ], "transitions": [ { "from": "a", "to": "b", "duration": 1.0 } ]}]
        }))
        .unwrap();
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
}
