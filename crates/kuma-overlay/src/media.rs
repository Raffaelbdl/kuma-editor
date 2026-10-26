//! ffmpeg/ffprobe helpers shared by the bake driver and the daemon.

use std::path::Path;
use std::process::Command;

use kuma_core::{MediaKind, MediaMeta, Fps};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Probe {
    pub meta: MediaMeta,
    pub kind: MediaKind,
}

#[derive(Debug, Deserialize)]
struct FfStream {
    codec_type: String,
    #[serde(default)]
    disposition: Disposition,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    pix_fmt: Option<String>,
}

/// Cover art in audio files is a "video" stream with `attached_pic`; it is not footage.
#[derive(Debug, Default, Deserialize)]
struct Disposition {
    #[serde(default)]
    attached_pic: u8,
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
    let video = ff.streams.iter().find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0);
    let audio = ff.streams.iter().any(|s| s.codec_type == "audio");
    let fmt_dur = ff.format.as_ref().and_then(|f| f.duration.as_deref()).and_then(|d| d.parse::<f64>().ok());
    let format_name = ff.format.as_ref().and_then(|f| f.format_name.clone()).unwrap_or_default();

    let is_image = format_name.contains("image2") || format_name == "png_pipe" || format_name == "jpeg_pipe" || format_name == "webp_pipe";
    let kind = match (video, audio, is_image) {
        (Some(_), _, true) => MediaKind::Image,
        (Some(_), _, false) => MediaKind::Video,
        (None, true, _) => MediaKind::Audio,
        _ => MediaKind::Video,
    };
    let (fps, variable_fps) = video
        .filter(|_| kind != MediaKind::Image)
        .map_or((None, false), |v| frame_rate(v.r_frame_rate.as_deref(), v.avg_frame_rate.as_deref()));
    let meta = MediaMeta {
        duration: if kind == MediaKind::Image { None } else { fmt_dur.or_else(|| video.and_then(|v| v.duration.as_deref()).and_then(|d| d.parse().ok())) },
        width: video.and_then(|v| v.width),
        height: video.and_then(|v| v.height),
        fps,
        variable_fps,
        has_audio: audio,
        alpha: video.and_then(|v| v.pix_fmt.as_deref()).is_some_and(has_alpha),
    };
    Ok(Probe { meta, kind })
}

/// Whether frames in pixel format `pix_fmt` carry an alpha channel, read from the name the
/// way ffmpeg spells its formats. `pal8` is left out: its palette always has room for
/// alpha, so the name cannot say whether a picture uses it. VP8/VP9 alpha travels beside
/// the frames (`alpha_mode`), not in them, and is not seen here either.
pub fn has_alpha(pix_fmt: &str) -> bool {
    const ALPHA: &[&str] = &["rgba", "bgra", "argb", "abgr", "ya8", "ya16", "yuva", "gbrap", "ayuv", "vuya", "rgbaf", "rgba64", "bgra64"];
    ALPHA.iter().any(|a| pix_fmt.starts_with(a))
}

/// Extensions a folder is scanned for when media is added from one. By name, because
/// ffprobe reads more than media — a `.txt` probes as a video (its `tty` demuxer) — and a
/// folder of footage usually holds other things too. A file added by its own path is
/// probed whatever it is called.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    // video
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "mts", "m2ts", "ts", "mxf", "flv", "wmv", "ogv", "3gp",
    // audio
    "wav", "mp3", "aac", "m4a", "flac", "ogg", "oga", "opus", "aif", "aiff", "wma",
    // stills
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "tga", "exr",
];

/// Whether `path` is named like media (see [`MEDIA_EXTENSIONS`]).
pub fn is_media_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

/// The rate to report, and whether the source is variable-rate: ffprobe's `r_frame_rate`
/// when `avg_frame_rate` agrees with it to within 0.5%, else the average, flagged.
/// `docs/decisions.md` ("Probed frame rate") has why.
fn frame_rate(r: Option<&str>, avg: Option<&str>) -> (Option<Fps>, bool) {
    match (r.and_then(Fps::parse_ratio), avg.and_then(Fps::parse_ratio)) {
        (Some(r), Some(avg)) if (r.as_f64() - avg.as_f64()).abs() > r.as_f64() * 0.005 => (Some(avg), true),
        (Some(r), _) => (Some(r), false),
        (None, avg) => (avg, false),
    }
}

/// Run ffmpeg with `-v error -y` and the given args.
pub fn ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// The pixels of a picture as RGB bytes, row by row: what a measurement of it reads.
pub fn rgb24(src: &Path) -> Result<Vec<u8>, String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

/// Peaks per second of source in a [`peaks`] waveform.
pub const PEAK_RATE: u32 = 100;

/// A waveform for the timeline: the loudest sample in each `1 / PEAK_RATE` s of `src`,
/// mixed to mono, scaled to `0..=255`. Streamed, so an hour of audio is not held in memory.
pub fn peaks(src: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    const RATE: u32 = 8000;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    // Buffered: a read is one bucket, a few hundred bytes, far too small for a syscall each.
    let mut stdout = std::io::BufReader::with_capacity(1 << 16, child.stdout.take().expect("piped"));
    let mut buf = vec![0u8; (RATE / PEAK_RATE) as usize * 2];
    let mut out = Vec::new();
    loop {
        // One bucket per read, the last one short.
        let mut n = 0;
        while n < buf.len() {
            match stdout.read(&mut buf[n..]).map_err(|e| e.to_string())? {
                0 => break,
                k => n += k,
            }
        }
        if n < 2 {
            break;
        }
        let loudest = buf[..n].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs()).max().unwrap_or(0);
        out.push(((u32::from(loudest) * 255 + 16384) / 32768) as u8);
    }
    let done = child.wait_with_output().map_err(|e| e.to_string())?;
    if !done.status.success() {
        return Err(String::from_utf8_lossy(&done.stderr).trim().to_string());
    }
    Ok(out)
}

/// A [`filmstrip`]: `count` frames `width`×`height` side by side, the i-th taken from
/// `(i + 0.5) * step` seconds into the source. A still is one frame and `step` 0.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Strip {
    pub count: u32,
    pub step: f64,
    pub width: u32,
    pub height: u32,
}

/// Frames along a video (or the one frame of a still) in one JPEG at `out`, for drawing
/// the timeline's clips. About one frame every two seconds, at most 120.
pub fn filmstrip(src: &Path, meta: &MediaMeta, still: bool, out: &Path) -> Result<Strip, String> {
    const HEIGHT: u32 = 54;
    let aspect = match (meta.width, meta.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => f64::from(w) / f64::from(h),
        _ => 16.0 / 9.0,
    };
    // Even, for the encoder's chroma subsampling.
    let width = (((f64::from(HEIGHT) * aspect) / 2.0).round() as u32).max(1) * 2;
    let duration = meta.duration.filter(|d| *d > 0.0 && !still);
    let (count, step) = match duration {
        Some(d) => {
            let count = ((d / 2.0).ceil() as u32).clamp(1, 120);
            (count, d / f64::from(count))
        }
        None => (1, 0.0),
    };
    let mut args: Vec<String> = Vec::new();
    // A long file is read keyframes only: the strip is a sketch, and decoding an hour of
    // footage to draw sixty stamps is the difference between a second and a minute.
    if duration.is_some_and(|d| d > 120.0) {
        args.extend(["-skip_frame", "nokey"].map(String::from));
    }
    args.extend(["-i".to_string(), src.to_string_lossy().into_owned()]);
    let scale = format!("scale={width}:{HEIGHT}");
    // The first frame at or after the middle of each `step`-long stretch. `select` rather
    // than `fps`, whose rounding drops the last one at the end of the file.
    let vf = if count > 1 {
        let (o, s) = (step / 2.0, step);
        format!("select='gte(t,{o:.6})*(isnan(prev_selected_t)+gte(floor((t-{o:.6})/{s:.6}),floor((prev_selected_t-{o:.6})/{s:.6})+1))',{scale},tile={count}x1")
    } else {
        scale
    };
    args.extend(["-an", "-vf", &vf, "-frames:v", "1", "-q:v", "5"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    ffmpeg(&args)?;
    Ok(Strip { count, step, width, height: HEIGHT })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_rate_reports_its_average() {
        assert_eq!(frame_rate(Some("30/1"), Some("180000/15001")), (Some(Fps::new(180000, 15001)), true));
    }

    #[test]
    fn a_constant_rate_keeps_the_clean_rational() {
        assert_eq!(frame_rate(Some("30000/1001"), Some("30000/1001")), (Some(Fps::new(30000, 1001)), false));
        // An average a hair off the grid is still constant, and still reads as the grid.
        assert_eq!(frame_rate(Some("25/1"), Some("2499/100")), (Some(Fps::new(25, 1)), false));
    }

    #[test]
    fn an_unknown_rate_falls_back_to_the_other() {
        assert_eq!(frame_rate(Some("25/1"), Some("0/0")), (Some(Fps::new(25, 1)), false));
        assert_eq!(frame_rate(Some("0/0"), Some("12/1")), (Some(Fps::new(12, 1)), false));
        assert_eq!(frame_rate(None, None), (None, false));
    }
}
