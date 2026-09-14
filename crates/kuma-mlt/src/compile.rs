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
    let duration = project.duration();
    if duration <= 0.0 {
        return Err(CompileError::Internal("empty timeline".into()));
    }
    let fps = project.fps;
    let total = fps.length(duration);

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
    /// Producer in/out (inclusive), including any transition extension.
    p_in: i64,
    p_out: i64,
    /// Frames added before/after the clip's own range for a dissolve. Effects are
    /// measured from the clip's own start, so keyframes are offset by `pre`.
    pre: i64,
    post: i64,
    producer_id: String,
}

fn compile_track(ctx: &Ctx, track: &Track, w: &mut XmlWriter) -> Result<Option<CompiledTrack>, CompileError> {
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
        // Overlays without a bake are skipped (they are live DOM in preview).
        if ctx.item(clip)?.is_overlay() && !ctx.opts.bakes.contains_key(&clip.id) {
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
        let (p_in, p_out) = if ctx.item(clip)?.is_timed_media() {
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
        plans.push(Plan { clip, lane, tl_in, tl_out, p_in, p_out, pre: e.before, post: e.after, producer_id });
    }
    if plans.is_empty() {
        return Ok(None);
    }

    // Emit one producer per clip.
    let mut blends = Vec::new();
    let mut has_audio = false;
    for p in &plans {
        emit_clip_producer(ctx, p, w)?;
        has_audio |= ctx.item(p.clip)?.is_timed_media() && !p.clip.muted;
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
        BinSource::Color { color } => {
            w.open("producer", &base);
            w.property("mlt_service", "color");
            w.property("resource", mlt_color(color));
            w.property("length", len.to_string());
            w.property("mlt_image_format", "rgba");
            emit_filters(ctx, p, w);
            w.close("producer");
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

fn emit_filters(ctx: &Ctx, p: &Plan, w: &mut XmlWriter) {
    let clip = p.clip;
    let fps = ctx.fps;
    let len = p.p_out - p.p_in + 1;
    let n = std::cell::Cell::new(0usize);
    // Open a `<filter>` with a unique id over an explicit frame range; the caller adds
    // properties and closes it.
    let open_at = |w: &mut XmlWriter, name: &str, service: &str, in_: i64, out: i64| {
        n.set(n.get() + 1);
        w.open("filter", &[("id", format!("{}_{name}{}", p.producer_id, n.get())), ("in", in_.to_string()), ("out", out.to_string())]);
        w.property("mlt_service", service);
    };
    let open = |w: &mut XmlWriter, name: &str, service: &str| open_at(w, name, service, p.p_in, p.p_out);
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
                let (own_in, own_out) = (p.p_in + p.pre, p.p_out - p.post);
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

    #[test]
    fn transitions_split_into_two_lanes() {
        let p: Project = serde_json::from_value(json!({
            "fps": 30, "bin": { "x": { "source": { "path": "x.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [
                { "id": "a", "start": 0, "end": 5, "bin": "x" },
                { "id": "b", "start": 5, "end": 8, "bin": "x", "in": 2 }
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
