//! Proxies: small copies of heavy footage that the preview's picture decodes at ½.
//!
//! Phone and camera footage (long-GOP H.264/HEVC) stores a whole picture only every few
//! seconds, so showing any one frame means decoding from the last of them — 2–3 s a seek on
//! 4K HEVC, measured. A proxy is the same footage at the size the preview at ½ shows it,
//! every frame a keyframe, so a seek decodes one small frame.
//!
//! A proxy is cache, never document: named by what it was made from ([`key`]), made by a
//! `proxy` job in the background one file at a time, and read only by the preview's picture
//! at ½ (`CompileOptions::proxies`) — never by the sound, a render, a sample or the preview
//! at full. A file that does not need one gets a `.none` marker instead, so it is probed
//! once. Until its proxy is there a file is decoded as it is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Context;
use kuma_core::{BinId, BinSource, MediaKind, Project};
use kuma_overlay::media::Probe;
use sha2::{Digest, Sha256};

use crate::preview::has_bin;
use crate::state::{lock, read, AppState};

/// In every key: changing how proxies are made makes new ones rather than reading old ones.
const VERSION: &str = "kuma-proxy-v1";

/// Below this many pixels a file decodes fast enough as it is: 720p.
const SMALL: u64 = 1280 * 720;

/// Codecs whose every frame is a keyframe already: a seek decodes one frame, so a proxy
/// would only cost space.
const INTRA: &[&str] = &[
    "prores", "dnxhd", "mjpeg", "jpeg2000", "png", "rawvideo", "qtrle", "ffv1", "utvideo", "huffyuv", "ffvhuff", "cfhd", "v210", "v410", "r210", "magicyuv",
    "hap", "gif", "tiff", "bmp", "dpx", "exr",
];

/// Which proxy jobs run and which files are not to be tried again this session.
#[derive(Default)]
pub struct Making {
    running: bool,
    /// Keys whose proxy failed or was cancelled. A forced reload clears it.
    skip: HashSet<String>,
}

impl Making {
    pub fn clear(&mut self) {
        self.skip.clear();
    }
}

/// The frame a proxy is made to fit: half the project's, which the preview at ½ shows 1:1.
pub fn target(project: &Project) -> (u32, u32) {
    let half = |v: u32| (v / 2).max(2) & !1;
    (half(project.width), half(project.height))
}

/// The key of `file`'s proxy for `target`: the file as it is now (its path, length and
/// modification time — reading gigabytes to hash them is out of the question) and how the
/// proxy is made. `None` when the file is not there.
pub fn key(file: &Path, target: (u32, u32)) -> Option<String> {
    let meta = std::fs::metadata(file).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let mut h = Sha256::new();
    h.update(format!("{VERSION}\n{}\n{} {modified}\n{}x{}", file.display(), meta.len(), target.0, target.1).as_bytes());
    Some(hex::encode(h.finalize())[..24].to_string())
}

/// What the cache holds for a key.
enum Cached {
    Ready(PathBuf),
    NotNeeded,
    Unknown,
}

fn lookup(dir: &Path, key: &str) -> Cached {
    let made = dir.join(format!("{key}.mp4"));
    if made.is_file() {
        Cached::Ready(made)
    } else if dir.join(format!("{key}.none")).is_file() {
        Cached::NotNeeded
    } else {
        Cached::Unknown
    }
}

/// A video of the bin with no proxy of its own: its entry, file and key.
type Video = (BinId, PathBuf, String);

fn videos<'p>(state: &'p AppState, project: &'p Project, target: (u32, u32)) -> impl Iterator<Item = Video> + 'p {
    project.bin.iter().filter_map(move |(id, item)| match &item.source {
        BinSource::File { path, kind: MediaKind::Video, proxy: None } => {
            let file = crate::jobs::resolve_path(state, Path::new(path));
            let key = key(&file, target)?;
            Some((id.clone(), file, key))
        }
        _ => None,
    })
}

/// The bin's videos as the cache has them, looked at once per compile.
#[derive(Default)]
pub struct Scan {
    /// The proxies to decode, by entry.
    pub ready: HashMap<BinId, PathBuf>,
    /// The files not looked at yet, those a sequence places first: they are what the
    /// preview shows.
    missing: Vec<Video>,
    target: (u32, u32),
}

pub fn scan(state: &AppState, project: &Project) -> Scan {
    let dir = state.config.proxies_dir();
    let target = target(project);
    let mut scan = Scan { target, ..Scan::default() };
    for (id, file, key) in videos(state, project, target) {
        match lookup(&dir, &key) {
            Cached::Ready(path) => drop(scan.ready.insert(id, path)),
            Cached::NotNeeded => {}
            Cached::Unknown => scan.missing.push((id, file, key)),
        }
    }
    let placed: HashSet<&str> = project.all_clips().map(|(_, _, c)| c.bin.as_str()).collect();
    scan.missing.sort_by_key(|(id, ..)| !placed.contains(id.as_str()));
    scan
}

/// Keys of every proxy (or marker) the bin can use now: what a prune keeps.
pub fn live(state: &AppState, project: &Project) -> HashSet<String> {
    videos(state, project, target(project)).map(|(.., key)| key).collect()
}

/// ffmpeg and ffprobe are there to make proxies with: looked for once.
fn tools() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| has_bin("ffmpeg") && has_bin("ffprobe"))
}

/// Look at the bin again and start the next proxy: what a job does as it ends.
pub fn make_next(state: &Arc<AppState>) {
    let scan = scan(state, &read(&state.project));
    start(state, scan);
}

/// Start a `proxy` job for the first file `scan` found nothing for, unless one is running,
/// the preview is at full (which reads no proxies), or ffmpeg is missing. Each job starts
/// the next as it ends, so the cache fills whether or not an editor is open.
pub fn start(state: &Arc<AppState>, scan: Scan) {
    if *lock(&state.preview.scale) >= 1.0 || !tools() {
        return;
    }
    let (id, file, key) = {
        let mut making = lock(&state.preview.making);
        if making.running {
            return;
        }
        let Some(next) = scan.missing.into_iter().find(|(.., key)| !making.skip.contains(key)) else { return };
        making.running = true;
        next
    };
    let (st, dir, target) = (state.clone(), state.config.proxies_dir(), scan.target);
    let params = serde_json::json!({ "bin": id, "path": file });
    crate::jobs::spawn_cancellable(state.clone(), "proxy", params, None, move |progress, cancel| {
        let made = make(&dir, &file, &key, target, progress, cancel).with_context(|| format!("{}", file.display()));
        let stopped = cancel.load(Ordering::SeqCst);
        {
            let mut making = lock(&st.preview.making);
            making.running = false;
            if made.is_err() || stopped {
                making.skip.insert(key);
            }
        }
        // A cancel is a request to stop, not to move on to the next file.
        if !stopped {
            make_next(&st);
        }
        made
    });
}

/// Probe `file`; write its proxy, or the marker saying it needs none.
fn make(dir: &Path, file: &Path, key: &str, target: (u32, u32), progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    progress(0, "probing");
    std::fs::create_dir_all(dir)?;
    let probe = kuma_overlay::media::probe(file).map_err(anyhow::Error::msg)?;
    if let Some(why) = needs_none(&probe, target) {
        std::fs::write(dir.join(format!("{key}.none")), why)?;
        return Ok(serde_json::json!({ "proxy": null, "why": why }));
    }
    // Under a `.tmp-` name until it is whole (`is_in_flight`); dropped on any error.
    let scratch = tempfile::Builder::new().prefix(&format!("{key}.tmp-")).suffix(".mp4").tempfile_in(dir)?;
    encode(file, scratch.path(), &probe, target, progress, cancel)?;
    let done = dir.join(format!("{key}.mp4"));
    scratch.persist(&done)?;
    Ok(serde_json::json!({ "proxy": done }))
}

/// Why `probe` needs no proxy, or `None` when it does.
fn needs_none(probe: &Probe, target: (u32, u32)) -> Option<&'static str> {
    let Some(codec) = probe.codec.as_deref() else { return Some("it has no picture") };
    let pixels = probe.meta.width.unwrap_or(0) as u64 * probe.meta.height.unwrap_or(0) as u64;
    if INTRA.contains(&codec) {
        Some("every frame is a keyframe already")
    } else if probe.meta.alpha {
        Some("it has an alpha channel, which a proxy would lose")
    } else if pixels <= SMALL || pixels <= target.0 as u64 * target.1 as u64 {
        Some("it is small enough to decode as it is")
    } else {
        None
    }
}

/// Encode the proxy: the first video stream, no sound (the sound graph reads the media),
/// fitted inside `target` as the preview fits the file into the frame, at a constant
/// rate, every frame a keyframe. Quality 28 was measured at about 4 Mbit/s at 540p, 8% of a
/// phone's 4K. Run at low priority: the preview engine is what the person is waiting on.
fn encode(file: &Path, out: &Path, probe: &Probe, target: (u32, u32), progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<()> {
    let (tw, th) = target;
    let fit = format!("min(1,min({tw}/iw,{th}/ih))");
    let scale = format!("scale=w='trunc(iw*{fit}/2)*2':h='trunc(ih*{fit}/2)*2'");
    let fps = probe.meta.fps.map_or_else(|| "30".to_string(), |f| format!("{}/{}", f.num, f.den));
    let duration = probe.meta.duration.unwrap_or(0.0);
    let mut cmd = if has_bin("nice") {
        let mut c = Command::new("nice");
        c.args(["-n", "10", "ffmpeg"]);
        c
    } else {
        Command::new("ffmpeg")
    };
    cmd.args(["-v", "error", "-y", "-nostdin", "-nostats", "-progress", "pipe:2", "-i"])
        .arg(file)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", &scale, "-fps_mode", "cfr", "-r", &fps])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "28", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p", "-f", "mp4"])
        .arg(out);
    // Progress comes as `key=value` lines beside ffmpeg's own errors, on stderr.
    let mut errors = Vec::new();
    let code = kuma_mlt::melt::run_cancellable(cmd, cancel, |line| match line.split_once('=') {
        Some(("out_time_us", us)) => {
            let done = us.parse::<f64>().unwrap_or(0.0) / 1e6;
            progress(if duration > 0.0 { (done / duration * 100.0).clamp(0.0, 99.0) as u8 } else { 0 }, "making a proxy");
        }
        Some((key, _)) if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {}
        _ => errors.push(line.to_string()),
    })
    .map_err(|e| match e {
        kuma_mlt::MeltError::Cancelled => anyhow::anyhow!("cancelled"),
        e => anyhow::anyhow!("ffmpeg: {e}"),
    })?;
    if code != Some(0) {
        anyhow::bail!("ffmpeg: {}", errors.join("\n"));
    }
    Ok(())
}
