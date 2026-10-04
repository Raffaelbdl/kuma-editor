//! Driving `melt` as a subprocess. Wraps in `xvfb-run` when there is no display, because
//! MLT's Qt module (qtblend, qtcrop) refuses to load without X11.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::presets::{ExportPreset, Tuning};

#[derive(Debug, thiserror::Error)]
pub enum MeltError {
    #[error("melt not found on PATH (install the `melt` package)")]
    NotFound,
    #[error("melt failed (exit {code:?}): {stderr}")]
    Failed { code: Option<i32>, stderr: String },
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct RenderJob {
    pub mlt_file: PathBuf,
    pub output: PathBuf,
    pub preset: &'static ExportPreset,
    /// Optional frame range (inclusive) to render.
    pub range: Option<(i64, i64)>,
    /// What this render changes of the preset.
    pub tuning: Tuning,
    /// Consumer properties after the preset's, which they override.
    pub extra: Vec<(String, String)>,
}

/// How many things at once `var` asks for, else half the hardware threads, at most eight:
/// frames of a render (`KUMA_RENDER_THREADS`) and overlay bakes (`KUMA_BAKE_JOBS`), measured
/// in *Bake and render speed* (`docs/decisions.md`).
pub fn threads_from_env(var: &str) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 8))
        .max(1)
}

/// Frames a render builds at once when its tuning does not say.
pub fn render_threads() -> usize {
    threads_from_env("KUMA_RENDER_THREADS")
}

#[derive(Debug, Clone, Copy)]
pub struct RenderProgress {
    pub frame: i64,
    pub percent: u8,
}

pub fn melt_available() -> bool {
    Command::new("melt").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// `melt`, wrapped in `xvfb-run -a` when there is no display.
fn melt_command() -> Command {
    let headless = std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    let mut cmd = if headless {
        let mut c = Command::new("xvfb-run");
        c.arg("-a").arg("melt");
        c
    } else {
        Command::new("melt")
    };
    cmd.env("LC_NUMERIC", "C");
    cmd
}

fn command(job: &RenderJob) -> Command {
    let mut args: Vec<String> = Vec::new();
    // Not `-silent`: melt 7.22 takes it to mean no `-progress` lines either, and a render
    // then sits at "rendering with melt" until it is done.
    args.push("-progress".into());
    args.push(job.mlt_file.to_string_lossy().into_owned());
    if let Some((in_, out)) = job.range {
        args.push(format!("in={in_}"));
        args.push(format!("out={out}"));
    }
    args.push("-consumer".into());
    args.push(format!("avformat:{}", job.output.to_string_lossy()));
    // Several frames at once, never dropping one (`real_time` below zero).
    args.push(format!("real_time=-{}", job.tuning.threads.unwrap_or_else(render_threads)));
    args.extend(job.preset.tuned(&job.tuning).map(|(k, v)| format!("{k}={v}")));
    // Later keys win on melt's command line.
    args.extend(job.extra.iter().map(|(k, v)| format!("{k}={v}")));
    let mut cmd = melt_command();
    cmd.args(&args);
    cmd
}

/// Encode five black frames with `preset`, all its properties, at a small progressive size:
/// whether this machine can render with it, or the encoder's own reason why not. melt
/// reports a failure by crashing, so the reason is read from its stderr: the last line
/// naming the codec (`[h264_nvenc @ 0x…] … CUDA_ERROR_NO_DEVICE …`).
pub fn try_preset(preset: &ExportPreset) -> Result<(), String> {
    let out = std::env::temp_dir().join(format!("kuma-try-{}-{}.{}", std::process::id(), preset.id, preset.extension));
    // Progressive, as every kuma profile is: NVENC refuses melt's default, interlaced PAL.
    let mut args: Vec<String> = ["-silent", "-profile", "atsc_720p_25", "color:black", "in=0", "out=4", "-consumer"].map(String::from).to_vec();
    args.push(format!("avformat:{}", out.to_string_lossy()));
    args.extend(preset.properties.iter().map(|(k, v)| format!("{k}={v}")));
    args.push("an=1".into());
    let run = melt_command().args(&args).output();
    let _ = std::fs::remove_file(&out);
    let run = run.map_err(|e| format!("melt could not start: {e}"))?;
    if run.status.success() {
        return Ok(());
    }
    let codec = preset.properties.iter().find(|(k, _)| *k == "vcodec").map_or("", |(_, v)| v);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let said = stderr.lines().rfind(|l| !codec.is_empty() && l.contains(codec)).map(|l| l.split_once("] ").map_or(l, |(_, rest)| rest).trim().to_string());
    Err(said.unwrap_or_else(|| format!("a test encode failed ({})", run.status)))
}

/// Render synchronously, reporting progress lines as they arrive. Setting `cancel` stops
/// melt and returns [`MeltError::Cancelled`]; the partial output is the caller's to remove.
pub fn render(job: &RenderJob, cancel: &AtomicBool, mut on_progress: impl FnMut(RenderProgress)) -> Result<(), MeltError> {
    if !melt_available() {
        return Err(MeltError::NotFound);
    }
    if let Some(parent) = job.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Keep the last lines of diagnostics for the error message.
    let mut tail: VecDeque<String> = VecDeque::new();
    let code = run_cancellable(command(job), cancel, |line| {
        if let Some(p) = parse_progress(line) {
            on_progress(p);
        } else {
            tail.push_back(line.to_string());
            if tail.len() > 100 {
                tail.pop_front();
            }
        }
    })?;
    if code != Some(0) {
        return Err(MeltError::Failed { code, stderr: tail.into_iter().collect::<Vec<_>>().join("\n") });
    }
    Ok(())
}

/// Run `cmd`, handing each non-empty stderr line to `on_line`, and return its exit code.
/// Public for the other long subprocesses a job can stop: ffmpeg metering a long file.
pub fn run_cancellable(mut cmd: Command, cancel: &AtomicBool, mut on_line: impl FnMut(&str)) -> Result<Option<i32>, MeltError> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let stderr = child.stderr.take().expect("piped stderr");
    let finished = AtomicBool::new(false);
    // Set however the reading ends, a panic in `on_line` included: the watcher waits for it,
    // and the scope for the watcher, so a callback that panicked used to hang the caller.
    struct Finished<'a>(&'a AtomicBool);
    impl Drop for Finished<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    std::thread::scope(|scope| -> Result<(), MeltError> {
        // The reader blocks on stderr, so the flag is watched beside it. Killing the child
        // closes the pipe, which ends the read.
        scope.spawn(|| {
            let done = || finished.load(Ordering::SeqCst);
            let tick = || std::thread::sleep(Duration::from_millis(100));
            while !done() && !cancel.load(Ordering::SeqCst) {
                tick();
            }
            if done() {
                return;
            }
            kill_tree(pid, "TERM");
            for _ in 0..30 {
                if done() {
                    return;
                }
                tick();
            }
            kill_tree(pid, "KILL");
        });
        // melt writes progress as "Current Frame: N, percentage: P" on stderr, using \r.
        let _finished = Finished(&finished);
        for chunk in BufReader::new(stderr).split(b'\r') {
            for line in chunk?.split(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(line);
                if !line.trim().is_empty() {
                    on_line(line.trim());
                }
            }
        }
        Ok(())
    })?;
    let status = child.wait()?;
    if cancel.load(Ordering::SeqCst) {
        return Err(MeltError::Cancelled);
    }
    Ok(status.code())
}

/// Signal a process's children, then the process (`pkill`/`kill`, no `libc`). Children
/// first: melt may run under `xvfb-run`. Not a process group of its own, which would stop
/// a terminal's Ctrl-C from reaching melt.
fn kill_tree(pid: u32, signal: &str) {
    let quiet = |c: &mut Command| {
        let _ = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
    };
    quiet(Command::new("pkill").arg(format!("-{signal}")).arg("-P").arg(pid.to_string()));
    quiet(Command::new("kill").arg(format!("-{signal}")).arg(pid.to_string()));
}

fn parse_progress(line: &str) -> Option<RenderProgress> {
    let rest = line.strip_prefix("Current Frame:")?;
    let (frame, pct) = rest.split_once(", percentage:")?;
    Some(RenderProgress {
        frame: frame.trim().parse().ok()?,
        percent: pct.trim().parse().ok()?,
    })
}

/// Extract a single frame (as PNG) at a timeline frame index, for previews and MCP
/// sampling. Uses the avformat consumer with an image2 muxer.
pub fn snapshot(mlt_file: &Path, frame: i64, out_png: &Path) -> Result<(), MeltError> {
    if !melt_available() {
        return Err(MeltError::NotFound);
    }
    if let Some(parent) = out_png.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let args = [
        "-silent".to_string(),
        mlt_file.to_string_lossy().into_owned(),
        format!("in={frame}"),
        format!("out={frame}"),
        "-consumer".into(),
        format!("avformat:{}", out_png.to_string_lossy()),
        "f=image2".into(),
        "vcodec=png".into(),
        "an=1".into(),
    ];
    let out = melt_command().args(&args).output()?;
    if !out.status.success() || !out_png.is_file() {
        return Err(MeltError::Failed {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A render's tuning reaches melt once each: its threads, and its quality and speed in
    /// place of the preset's own; the machine's threads when it does not say.
    #[test]
    fn a_render_passes_its_tuning_once() {
        let job = |tuning| RenderJob { mlt_file: "p.mlt".into(), output: "o.mp4".into(), preset: crate::presets::find("youtube-1080p").unwrap(), range: None, tuning, extra: vec![] };
        let args = |j: &RenderJob| command(j).get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let with = |args: &[String], key: &str| args.iter().filter(|a| a.starts_with(&format!("{key}="))).cloned().collect::<Vec<_>>();
        let tuned = args(&job(Tuning { threads: Some(3), quality: Some(20), speed: Some("medium".into()) }));
        assert_eq!((with(&tuned, "real_time"), with(&tuned, "crf"), with(&tuned, "preset")), (vec!["real_time=-3".into()], vec!["crf=20".into()], vec!["preset=medium".into()]));
        let own = args(&job(Tuning::default()));
        assert_eq!((with(&own, "real_time"), with(&own, "crf")), (vec![format!("real_time=-{}", render_threads())], vec!["crf=18".into()]));
    }

    /// An encoder melt does not have is not available, and says so in the encoder's words.
    #[test]
    fn a_preset_whose_encoder_is_missing_cannot_be_tried() {
        if !melt_available() {
            eprintln!("skipping: no melt");
            return;
        }
        static NONE: ExportPreset = ExportPreset {
            id: "no-such-encoder",
            description: "",
            extension: "mp4",
            properties: &[("f", "mp4"), ("vcodec", "h264_no_such_encoder")],
            quality: None,
            speed: None,
            needs: Some("nothing that exists"),
        };
        let why = try_preset(&NONE).expect_err("an encoder that does not exist");
        assert!(why.contains("h264_no_such_encoder"), "{why}");
        assert!(try_preset(crate::presets::find("draft").unwrap()).is_ok(), "a software preset encodes");
    }

    #[test]
    fn parses_progress() {
        let p = parse_progress("Current Frame:        120, percentage:         26").unwrap();
        assert_eq!((p.frame, p.percent), (120, 26));
        assert!(parse_progress("something else").is_none());
    }

    #[test]
    fn cancelling_kills_the_child_and_its_children() {
        // A shell with a child of its own, like `xvfb-run` with melt under it.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::SeqCst);
            });
            // The backgrounded sleep holds stderr open too: returning at all means it died.
            assert!(matches!(run_cancellable(cmd, &cancel, |_| {}), Err(MeltError::Cancelled)));
        });
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A callback that panics fails the run instead of hanging it: the watcher beside the
    /// reader is told the reading is over however it ended.
    #[test]
    fn a_panicking_callback_does_not_hang_the_run() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo one >&2"]);
        let started = std::time::Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_cancellable(cmd, &AtomicBool::new(false), |_| panic!("a bad callback"))));
        assert!(outcome.is_err(), "the panic reaches the caller");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn an_uncancelled_run_reports_its_exit_and_lines() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo one >&2; printf 'two\\rthree\\n' >&2; exit 3"]);
        let mut lines = vec![];
        let code = run_cancellable(cmd, &AtomicBool::new(false), |l| lines.push(l.to_string())).unwrap();
        assert_eq!(code, Some(3));
        assert_eq!(lines, ["one", "two", "three"]);
    }
}
