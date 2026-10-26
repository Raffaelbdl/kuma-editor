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
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kuma_core::{BinId, BinSource, MediaKind, Project};
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
    let path = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let mut h = Sha256::new();
    h.update(format!("{VERSION}\n{}\n{} {modified}\n{}x{}", path.display(), meta.len(), target.0, target.1).as_bytes());
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

/// A video entry of the bin with no proxy of its own: its id, file and key. Entries some
/// sequence places come first, since those are what the preview shows.
fn videos(state: &AppState, project: &Project, target: (u32, u32)) -> Vec<(BinId, PathBuf, String)> {
    let placed: HashSet<&str> = project.all_clips().map(|(_, _, c)| c.bin.as_str()).collect();
    let mut out: Vec<(BinId, PathBuf, String)> = project
        .bin
        .iter()
        .filter_map(|(id, item)| match &item.source {
            BinSource::File { path, kind: MediaKind::Video, proxy: None } => {
                let file = crate::jobs::resolve_path(state, Path::new(path));
                let key = key(&file, target)?;
                Some((id.clone(), file, key))
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|(id, ..)| !placed.contains(id.as_str()));
    out
}

/// The proxies the cache has for the bin, by entry: what the preview's picture decodes at ½.
pub fn ready(state: &AppState, project: &Project) -> HashMap<BinId, PathBuf> {
    let dir = state.config.proxies_dir();
    videos(state, project, target(project))
        .into_iter()
        .filter_map(|(id, _, key)| match lookup(&dir, &key) {
            Cached::Ready(path) => Some((id, path)),
            _ => None,
        })
        .collect()
}

/// Keys of every proxy (or marker) the bin can use now: what a prune keeps.
pub fn live(state: &AppState, project: &Project) -> HashSet<String> {
    videos(state, project, target(project)).into_iter().map(|(.., key)| key).collect()
}

/// Start a `proxy` job for the next video the cache knows nothing about, unless one is
/// running, the preview is at full (which reads no proxies), or ffmpeg is missing. Each
/// job starts the next as it ends, so the cache fills whether or not an editor is open.
pub fn make_next(state: &Arc<AppState>) {
    if *lock(&state.preview.scale) >= 1.0 || !has_bin("ffmpeg") || !has_bin("ffprobe") {
        return;
    }
    let project = read(&state.project).clone();
    let target = target(&project);
    let dir = state.config.proxies_dir();
    let (id, file, key) = {
        let mut making = lock(&state.preview.making);
        if making.running {
            return;
        }
        let next = videos(state, &project, target).into_iter().find(|(.., key)| !making.skip.contains(key) && matches!(lookup(&dir, key), Cached::Unknown));
        let Some(next) = next else { return };
        making.running = true;
        next
    };
    let st = state.clone();
    let params = serde_json::json!({ "bin": id, "path": file });
    crate::jobs::spawn_cancellable(state.clone(), "proxy", params, None, move |progress, cancel| {
        let made = make(&dir, &file, &key, target, progress, cancel);
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
    let video = probe(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    if let Some(why) = video.needs_none(target) {
        std::fs::write(dir.join(format!("{key}.none")), &why)?;
        return Ok(serde_json::json!({ "proxy": null, "why": why }));
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let scratch = Scratch(dir.join(format!("{key}.tmp-{}-{nanos:x}.mp4", std::process::id())));
    encode(file, &scratch.0, &video, target, progress, cancel)?;
    let done = dir.join(format!("{key}.mp4"));
    std::fs::rename(&scratch.0, &done)?;
    Ok(serde_json::json!({ "proxy": done }))
}

/// Scratch output: removed unless it was renamed into place.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What deciding on and making a proxy needs to know of a file's first video stream.
struct Video {
    codec: String,
    pix_fmt: String,
    pixels: u64,
    /// The rate the proxy is made at: constant, since phones record a variable one.
    fps: String,
    duration: f64,
}

impl Video {
    /// Why this file needs no proxy, or `None` when it does.
    fn needs_none(&self, target: (u32, u32)) -> Option<String> {
        if INTRA.contains(&self.codec.as_str()) {
            Some(format!("{} has every frame a keyframe", self.codec))
        } else if kuma_overlay::media::has_alpha(&self.pix_fmt) {
            Some("it has an alpha channel, which a proxy would lose".into())
        } else if self.pixels <= SMALL || self.pixels <= target.0 as u64 * target.1 as u64 {
            Some("it is small enough to decode as it is".into())
        } else {
            None
        }
    }
}

fn probe(file: &Path) -> Result<Video, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-print_format", "json"])
        .args(["-show_entries", "stream=codec_name,pix_fmt,width,height,avg_frame_rate,r_frame_rate:format=duration"])
        .arg(file)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let s = v["streams"].get(0).ok_or("no video stream")?;
    let text = |k: &str| s[k].as_str().unwrap_or_default().to_string();
    let rate = |k: &str| Some(text(k)).filter(|r| !r.is_empty() && !r.starts_with('0'));
    Ok(Video {
        codec: text("codec_name"),
        pix_fmt: text("pix_fmt"),
        pixels: s["width"].as_u64().unwrap_or(0) * s["height"].as_u64().unwrap_or(0),
        fps: rate("avg_frame_rate").or_else(|| rate("r_frame_rate")).unwrap_or_else(|| "30".into()),
        duration: v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0),
    })
}

/// Encode the proxy: the first video stream, no sound (the sound graph reads the media),
/// fitted inside `target` as the preview fits the file into the frame, at a constant
/// rate, every frame a keyframe. Quality 28 was measured at about 4 Mbit/s at 540p, 8% of a
/// phone's 4K. Run at low priority: the preview engine is what the person is waiting on.
fn encode(file: &Path, out: &Path, video: &Video, target: (u32, u32), progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<()> {
    let (tw, th) = target;
    let fit = format!("min(1,min({tw}/iw,{th}/ih))");
    let scale = format!("scale=w='trunc(iw*{fit}/2)*2':h='trunc(ih*{fit}/2)*2'");
    let mut cmd = if has_bin("nice") {
        let mut c = Command::new("nice");
        c.args(["-n", "10", "ffmpeg"]);
        c
    } else {
        Command::new("ffmpeg")
    };
    cmd.args(["-v", "error", "-y", "-nostdin", "-nostats", "-progress", "pipe:1", "-i"])
        .arg(file)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", &scale, "-fps_mode", "cfr", "-r", &video.fps])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "28", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p", "-f", "mp4"])
        .arg(out)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("ffmpeg: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            break;
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
            let percent = if video.duration > 0.0 { (us / 1e6 / video.duration * 100.0).clamp(0.0, 99.0) as u8 } else { 0 };
            progress(percent, "making a proxy");
        }
    }
    let out = child.wait_with_output()?;
    if cancel.load(Ordering::SeqCst) {
        anyhow::bail!("cancelled");
    }
    if !out.status.success() {
        anyhow::bail!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
