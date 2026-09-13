//! ffmpeg/ffprobe helpers shared by the bake driver and the daemon.

use std::path::Path;
use std::process::Command;

use kuma_core::{AssetKind, AssetMeta, Fps};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Probe {
    pub meta: AssetMeta,
    pub kind: AssetKind,
}

#[derive(Debug, Deserialize)]
struct FfStream {
    codec_type: String,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfFormat {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    format_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfOut {
    #[serde(default)]
    streams: Vec<FfStream>,
    #[serde(default)]
    format: Option<FfFormat>,
}

pub fn ffprobe_available() -> bool {
    Command::new("ffprobe").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Probe a media file: duration, size, fps, audio presence, and a best-effort kind.
pub fn probe(path: &Path) -> Result<Probe, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let ff: FfOut = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let video = ff.streams.iter().find(|s| s.codec_type == "video");
    let audio = ff.streams.iter().any(|s| s.codec_type == "audio");
    let fmt_dur = ff.format.as_ref().and_then(|f| f.duration.as_deref()).and_then(|d| d.parse::<f64>().ok());
    let format_name = ff.format.as_ref().and_then(|f| f.format_name.clone()).unwrap_or_default();

    let is_image = format_name.contains("image2") || format_name == "png_pipe" || format_name == "jpeg_pipe" || format_name == "webp_pipe";
    let kind = match (video, audio, is_image) {
        (Some(_), _, true) => AssetKind::Image,
        (Some(_), _, false) => AssetKind::Video,
        (None, true, _) => AssetKind::Audio,
        _ => AssetKind::Video,
    };
    let fps = video
        .and_then(|v| v.r_frame_rate.as_deref())
        .and_then(|r| {
            let (n, d) = r.split_once('/')?;
            Some(Fps::new(n.parse().ok()?, d.parse().ok()?))
        })
        .filter(|f| f.num > 0 && f.den > 0);
    let meta = AssetMeta {
        duration: if kind == AssetKind::Image { None } else { fmt_dur.or_else(|| video.and_then(|v| v.duration.as_deref()).and_then(|d| d.parse().ok())) },
        width: video.and_then(|v| v.width),
        height: video.and_then(|v| v.height),
        fps: if kind == AssetKind::Image { None } else { fps },
        has_audio: audio,
        codec: video.and_then(|v| v.codec_name.clone()),
    };
    Ok(Probe { meta, kind })
}

/// Pixel format of the first video stream — used to assert alpha survived an encode.
pub fn pix_fmt(path: &Path) -> Result<String, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run ffmpeg with `-v error -y` and the given args.
pub fn ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// Generate a low-resolution H.264 proxy for smooth scrubbing.
pub fn make_proxy(src: &Path, dst: &Path, height: u32) -> Result<(), String> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    ffmpeg(&[
        "-i".into(),
        src.to_string_lossy().into_owned(),
        "-vf".into(),
        format!("scale=-2:{height}"),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "23".into(),
        "-g".into(),
        "15".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        dst.to_string_lossy().into_owned(),
    ])
}
