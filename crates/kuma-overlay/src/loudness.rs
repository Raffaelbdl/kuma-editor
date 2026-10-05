//! Loudness of a mix, read by ffmpeg's EBU R128 meter (`ebur128`): the whole, any part of
//! it, where it peaks and where it is silent.
//!
//! One pass of the meter logs every 100 ms: the momentary loudness (the 400 ms ending
//! there), the short-term loudness (3 s) and the true peak of those 100 ms. The integrated
//! loudness of a part is computed from the momentary values the way BS.1770 gates it, since
//! those are its 400 ms blocks at a 100 ms hop; the log prints them to 0.1 dB, which
//! costs a part a few hundredths of a LU. The whole comes from the meter's own summary.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// One 100 ms step of the meter, ending at `t` seconds into the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub t: f64,
    /// LUFS over the 400 ms ending at `t`; `-inf` for digital silence.
    pub momentary: f64,
    /// LUFS over the 3 s ending at `t`.
    pub short_term: f64,
    /// dBTP, the loudest channel's true peak within these 100 ms.
    pub true_peak: f64,
}

/// A file's loudness: the meter's summary and every step.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub steps: Vec<Step>,
    /// Integrated loudness, LUFS; `None` when nothing passes the absolute gate.
    pub integrated: Option<f64>,
    /// Loudness range, LU.
    pub range: f64,
    /// dBTP over the whole file.
    pub true_peak: f64,
}

/// BS.1770's absolute gate: a block quieter than this is not programme, it is silence.
pub const SILENT_LUFS: f64 = -70.0;
/// Its relative gate: blocks this far under the ungated loudness do not count either.
const RELATIVE_GATE: f64 = -10.0;
/// How often the meter reports: each step is the 100 ms ending at its `t`.
pub const STEP: f64 = 0.1;
const BLOCK: f64 = 0.4;
/// The meter, logging every step.
const EBUR128: &str = "ebur128=peak=true:framelog=verbose";

/// Meter `wav` with ffmpeg.
pub fn scan(wav: &Path) -> Result<Scan, String> {
    let log = run_ffmpeg(wav, None, EBUR128, &AtomicBool::new(false), &|_| {})?;
    parse(&log).ok_or_else(|| unreadable(wav))
}

fn unreadable(file: &Path) -> String {
    format!("ffmpeg's ebur128 printed nothing this version of kuma can read for {}", file.display())
}

/// The meter's log: a line per step, `t: 2.5 TARGET:-23 LUFS M: -21.7 S:-120.7 I: … FTPK:
/// -20.4 -20.4 dBFS TPK: …`, then a summary with `I:`, `LRA:` and the true `Peak:`.
pub fn parse(log: &str) -> Option<Scan> {
    // The number after `key`, and every number after it up to `unit`.
    fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        line.split_once(key).map(|(_, rest)| rest.trim_start())
    }
    fn number(s: &str) -> Option<f64> {
        s.split_whitespace().next()?.parse().ok()
    }
    fn numbers(s: &str, unit: &str) -> Vec<f64> {
        s.split(unit).next().unwrap_or_default().split_whitespace().filter_map(|x| x.parse().ok()).collect()
    }
    let mut steps = Vec::new();
    let (summary, per_step) = match log.rfind("Summary:") {
        Some(i) => (&log[i..], &log[..i]),
        None => ("", log),
    };
    let quiet = |v: f64| if v.is_nan() { f64::NEG_INFINITY } else { v };
    for line in per_step.lines().filter(|l| l.contains("ebur128") && l.contains(" t: ")) {
        let peaks = numbers(after(line, "FTPK:")?, "dBFS");
        steps.push(Step {
            // Printed a hair early (`2.49998`); the steps are on a 100 ms grid.
            t: (number(after(line, " t:")?)? / STEP).round() * STEP,
            momentary: quiet(number(after(line, " M:")?)?),
            short_term: quiet(number(after(line, " S:")?)?),
            true_peak: peaks.into_iter().map(quiet).fold(f64::NEG_INFINITY, f64::max),
        });
    }
    let field = |text: &str, key: &str| text.lines().find_map(|l| number(after(l, key)?));
    let integrated = field(summary, "I:")?;
    Some(Scan {
        integrated: (integrated > SILENT_LUFS).then_some(integrated),
        range: field(summary, "LRA:")?,
        true_peak: summary.split("True peak:").nth(1).and_then(|s| field(s, "Peak:")).unwrap_or(f64::NEG_INFINITY),
        steps,
    })
}

/// Where a source is quiet: every sample under `db` dBFS for at least `min` seconds, as
/// ffmpeg's `silencedetect` reads it. The absolute gate above finds only digital silence,
/// which a recording never has; a pause in speech is about -35 dB for 0.15 s or more.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quiet {
    pub db: f64,
    pub min: f64,
}

impl Default for Quiet {
    fn default() -> Self {
        Quiet { db: -35.0, min: 0.15 }
    }
}

impl Quiet {
    fn filter(&self) -> String {
        format!("silencedetect=noise={}dB:d={}", self.db, self.min)
    }
}

/// A source file measured over a stretch of it: its loudness and where it is quiet, both
/// in the file's own seconds.
#[derive(Debug, Clone, Default)]
pub struct Measured {
    pub scan: Scan,
    /// `(start, end)` of each quiet stretch, in order.
    pub silences: Vec<(f64, f64)>,
}

/// Meter `file` over `range` (its own seconds; all of it when `None`) and find where it is
/// quiet, in one ffmpeg pass: `ebur128` passes the sound on to `silencedetect`. Times come
/// back in the file's seconds. `progress` hears how many seconds of the range are done, and
/// `cancel` stops ffmpeg: a long recording takes a while to decode.
pub fn measure(file: &Path, range: Option<(f64, f64)>, quiet: Quiet, cancel: &AtomicBool, progress: &dyn Fn(f64)) -> Result<Measured, String> {
    let filters = format!("{EBUR128},{}", quiet.filter());
    let log = run_ffmpeg(file, range, &filters, cancel, &|line| {
        if line.contains("ebur128") {
            if let Some(t) = line.split_once(" t:").and_then(|(_, r)| r.split_whitespace().next()?.parse().ok()) {
                progress(t);
            }
        }
    })?;
    let from = range.map_or(0.0, |r| r.0);
    let mut scan = parse(&log).ok_or_else(|| unreadable(file))?;
    let end = scan.steps.last().map_or(0.0, |s| s.t);
    for s in &mut scan.steps {
        s.t += from;
    }
    let silences = parse_silences(&log, end).into_iter().map(|(a, b)| (a + from, b + from)).collect();
    Ok(Measured { scan, silences })
}

/// Only where `file` is quiet, in its own seconds: `silencedetect` alone, which is quick.
pub fn silences(file: &Path, quiet: Quiet) -> Result<Vec<(f64, f64)>, String> {
    let log = run_ffmpeg(file, None, &quiet.filter(), &AtomicBool::new(false), &|_| {})?;
    // A stretch still open at the end runs to it, which the last line ffmpeg prints says.
    let end = log.lines().filter_map(|l| l.split_once("time=")?.1.split_whitespace().next().and_then(clock)).last().unwrap_or(f64::INFINITY);
    Ok(parse_silences(&log, end))
}

/// `HH:MM:SS.ss` as seconds.
fn clock(s: &str) -> Option<f64> {
    s.split(':').try_fold(0.0, |acc, part| Some(acc * 60.0 + part.parse::<f64>().ok()?))
}

/// ffmpeg reading the audio of `file` (over `range`) through `filters`, discarding the
/// result: its log, every line of which `on_line` also hears as it comes.
fn run_ffmpeg(file: &Path, range: Option<(f64, f64)>, filters: &str, cancel: &AtomicBool, on_line: &dyn Fn(&str)) -> Result<String, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-v", "verbose"]);
    // Before `-i`, so ffmpeg seeks rather than decoding up to the start, and the filters'
    // clocks start at 0 there.
    if let Some((a, b)) = range {
        cmd.args(["-ss", &a.to_string(), "-t", &(b - a).to_string()]);
    }
    cmd.arg("-i").arg(file).args(["-vn", "-af", filters, "-f", "null", "-"]);
    let mut log = String::new();
    let code = kuma_mlt::melt::run_cancellable(cmd, cancel, |line| {
        on_line(line);
        log.push_str(line);
        log.push('\n');
    })
    .map_err(|e| match e {
        kuma_mlt::MeltError::Cancelled => "cancelled".to_string(),
        e => format!("ffmpeg: {e}"),
    })?;
    if code != Some(0) {
        let lines: Vec<&str> = log.lines().collect();
        return Err(format!("ffmpeg could not read the sound of {}: {}", file.display(), lines[lines.len().saturating_sub(5)..].join("\n")));
    }
    Ok(log)
}

/// `silencedetect`'s log: `silence_start: 1.2` then `silence_end: 1.6 | silence_duration: 0.4`.
/// A stretch the log leaves open runs to `end`.
pub fn parse_silences(log: &str, end: f64) -> Vec<(f64, f64)> {
    let value = |line: &str, key: &str| -> Option<f64> { line.split_once(key)?.1.split_whitespace().next()?.parse().ok() };
    let mut out = Vec::new();
    let mut open = None;
    for line in log.lines().filter(|l| l.contains("silencedetect")) {
        if let Some(a) = value(line, "silence_start:") {
            open = Some(a.max(0.0));
        } else if let (Some(b), Some(a)) = (value(line, "silence_end:"), open) {
            out.push((a, b));
            open = None;
        }
    }
    if let Some(a) = open.filter(|&a| end > a) {
        out.push((a, end));
    }
    out
}

impl Scan {
    /// One value every `step` seconds from the first step on (`step` a multiple of
    /// [`STEP`]): the short-term loudness, or the momentary one. Value `i` is the window
    /// ending at `steps[0].t - STEP + (i + 1) · step`.
    pub fn curve(&self, step: f64, momentary: bool) -> Vec<f64> {
        let every = ((step / STEP).round() as usize).max(1);
        self.steps.iter().skip(every - 1).step_by(every).map(|s| if momentary { s.momentary } else { s.short_term }).collect()
    }

    /// The integrated loudness of `[from, to)` seconds: its 400 ms blocks, gated as BS.1770
    /// gates them. `None` for a part too short to hold a block, or silent.
    pub fn integrated_over(&self, from: f64, to: f64) -> Option<f64> {
        let blocks: Vec<f64> = self.steps.iter().filter(|s| s.t - BLOCK >= from - 1e-6 && s.t <= to + 1e-6).map(|s| s.momentary).filter(|&m| m > SILENT_LUFS).collect();
        let energy = |l: f64| 10f64.powf((l + 0.691) / 10.0);
        let loudness = |ls: &[f64]| (!ls.is_empty()).then(|| -0.691 + 10.0 * (ls.iter().map(|&l| energy(l)).sum::<f64>() / ls.len() as f64).log10());
        let gate = loudness(&blocks)? + RELATIVE_GATE;
        loudness(&blocks.iter().copied().filter(|&l| l > gate).collect::<Vec<_>>())
    }

    /// The loudest true peak within `[from, to)`, dBTP.
    pub fn peak_over(&self, from: f64, to: f64) -> f64 {
        self.steps.iter().filter(|s| s.t - STEP >= from - 1e-6 && s.t <= to + 1e-6).map(|s| s.true_peak).fold(f64::NEG_INFINITY, f64::max)
    }

    /// The loudest moments: the steps with the highest true peaks, at least `apart` seconds
    /// from each other, loudest first.
    pub fn hottest(&self, n: usize, apart: f64) -> Vec<Step> {
        let mut by_peak: Vec<&Step> = self.steps.iter().filter(|s| s.true_peak.is_finite()).collect();
        by_peak.sort_by(|a, b| b.true_peak.total_cmp(&a.true_peak).then(a.t.total_cmp(&b.t)));
        let mut out: Vec<Step> = Vec::new();
        for s in by_peak {
            if out.len() == n {
                break;
            }
            if out.iter().all(|o| (o.t - s.t).abs() >= apart) {
                out.push(*s);
            }
        }
        out
    }

    /// Stretches of at least `min` seconds where nothing reaches the absolute gate, as
    /// `(start, end)`. A step's momentary loudness is the 400 ms ending at it, so a run of
    /// silent steps from `a` to `b` is silence from `a − 0.4` to `b`.
    pub fn silences(&self, min: f64) -> Vec<(f64, f64)> {
        let silent = |s: &Step| s.momentary <= SILENT_LUFS;
        self.steps
            .chunk_by(|a, b| silent(a) == silent(b))
            .filter(|run| silent(&run[0]))
            .map(|run| ((run[0].t - BLOCK).max(0.0), run[run.len() - 1].t))
            .filter(|(a, b)| b - a >= min - 1e-6)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: f64, m: &str, s: &str, peak: &str) -> String {
        format!("[Parsed_ebur128_0 @ 0x5] t: {t:<10}  TARGET:-23 LUFS    M:{m:>6} S:{s:>6}     I: -21.7 LUFS       LRA:   0.0 LU  SPK: -20.6 -20.6 dBFS  FTPK: {peak} {peak} dBFS  TPK: -20.4 -20.4 dBFS\n")
    }

    const SUMMARY: &str = "[Parsed_ebur128_0 @ 0x5] Summary:\n\n  Integrated loudness:\n    I:         -21.9 LUFS\n    Threshold: -32.1 LUFS\n\n  Loudness range:\n    LRA:         1.6 LU\n    Threshold: -42.5 LUFS\n    LRA low:   -23.5 LUFS\n    LRA high:  -21.9 LUFS\n\n  Sample peak:\n    Peak:      -20.6 dBFS\n\n  True peak:\n    Peak:      -20.4 dBFS\n";

    /// A second of tone at -20 LUFS (the last 300 ms at -24), a second of silence, then half
    /// a second at -40.
    fn scan() -> Scan {
        let mut log = String::from("Stream mapping: …\n    Peak:      -99.0 dBFS\n");
        for i in 1..=25 {
            let t = i as f64 / 10.0;
            let (m, peak) = match t {
                t if t <= 1.0 => ("-20.0", "-18.0"),
                t if t <= 1.3 => ("-24.0", "-18.0"),
                t if t <= 2.0 => ("nan", "-inf"),
                _ => ("-40.0", "-37.5"),
            };
            log.push_str(&line(t - 0.00002, m, "-22.0", peak));
        }
        log.push_str(SUMMARY);
        parse(&log).unwrap()
    }

    #[test]
    fn the_log_reads_as_steps_and_a_summary() {
        let s = scan();
        assert_eq!(s.steps.len(), 25);
        assert_eq!(s.steps[0], Step { t: 0.1, momentary: -20.0, short_term: -22.0, true_peak: -18.0 });
        assert_eq!(s.steps[15].momentary, f64::NEG_INFINITY, "nan is silence");
        assert_eq!(s.steps[15].true_peak, f64::NEG_INFINITY);
        assert_eq!((s.integrated, s.range, s.true_peak), (Some(-21.9), 1.6, -20.4), "the true peak, not the sample peak or a stray line");
        assert!(parse("nothing here").is_none());
        let silent = SUMMARY.replace("-21.9 LUFS\n    Threshold", "-70.0 LUFS\n    Threshold");
        assert_eq!(parse(&silent).unwrap().integrated, None);
    }

    /// Gating is BS.1770's: silence never counts, and blocks 10 LU under the rest do not
    /// either, so a quiet tail does not drag a part down.
    #[test]
    fn a_part_is_gated_like_the_whole() {
        let s = scan();
        let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 0.05);
        assert!(near(s.integrated_over(0.0, 1.0), -20.0), "{:?}", s.integrated_over(0.0, 1.0));
        assert!(near(s.integrated_over(2.0, 2.5), -40.0), "{:?}", s.integrated_over(2.0, 2.5));
        // Together, the -40 blocks are more than 10 LU under the rest and do not count:
        // -20.87, the -20 and -24 blocks alone, where counting them gives -22.6.
        assert!(near(s.integrated_over(0.0, 2.5), -20.87), "{:?}", s.integrated_over(0.0, 2.5));
        assert_eq!(s.integrated_over(1.4, 2.0), None, "silence");
        assert_eq!(s.integrated_over(0.0, 0.3), None, "shorter than a block");
        assert_eq!(s.peak_over(0.0, 1.3), -18.0);
        assert_eq!(s.peak_over(2.0, 2.5), -37.5);
    }

    #[test]
    fn a_curve_takes_every_nth_step() {
        let s = scan();
        assert_eq!(s.curve(0.1, true).len(), 25);
        let half = s.curve(0.5, true);
        assert_eq!(half.len(), 5, "0.5 s, 1.0 s … 2.5 s");
        assert_eq!((half[1], half[2], half[4]), (-20.0, f64::NEG_INFINITY, -40.0), "{half:?}");
        assert!(s.curve(1.0, false).iter().all(|&v| v == -22.0));
    }

    #[test]
    fn quiet_stretches_read_from_the_log_and_one_left_open_runs_to_the_end() {
        let log = "[silencedetect @ 0x1] silence_start: -0.00133\n[Parsed_ebur128_0 @ 0x5] t: 0.1 …\n\
                   [silencedetect @ 0x1] silence_end: 0.5 | silence_duration: 0.5\n\
                   [silencedetect @ 0x1] silence_start: 2.25\n[silencedetect @ 0x1] silence_end: 2.5 | silence_duration: 0.25\n\
                   [silencedetect @ 0x1] silence_start: 3.75\nsize=N/A time=00:00:04.00 bitrate=N/A\n";
        assert_eq!(parse_silences(log, 4.0), vec![(0.0, 0.5), (2.25, 2.5), (3.75, 4.0)]);
        assert_eq!(clock("00:01:04.50"), Some(64.5));
    }

    #[test]
    fn silences_and_peaks_are_found_where_they_are() {
        let s = scan();
        let silences = s.silences(0.5);
        assert_eq!(silences.len(), 1);
        assert!((silences[0].0 - 1.0).abs() < 1e-9 && (silences[0].1 - 2.0).abs() < 1e-9, "{silences:?}");
        assert!(s.silences(1.5).is_empty());
        let hot = s.hottest(3, 1.0);
        assert_eq!(hot.iter().map(|h| h.true_peak).collect::<Vec<_>>(), [-18.0, -18.0, -37.5]);
        assert!((hot[0].t - 0.1).abs() < 1e-9 && (hot[1].t - 1.1).abs() < 1e-9, "the earliest of a tie first, then the next one far enough away: {hot:?}");
    }
}
