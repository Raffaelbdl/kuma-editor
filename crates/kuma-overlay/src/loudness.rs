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

/// Meter `wav` with ffmpeg.
pub fn scan(wav: &Path) -> Result<Scan, String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-v", "verbose", "-i"])
        .arg(wav)
        .args(["-af", "ebur128=peak=true:framelog=verbose", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        let lines: Vec<&str> = log.lines().collect();
        return Err(format!("ffmpeg could not meter {}: {}", wav.display(), lines[lines.len().saturating_sub(5)..].join("\n")));
    }
    parse(&log).ok_or_else(|| format!("ffmpeg's ebur128 printed nothing this version of kuma can read for {}", wav.display()))
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

impl Scan {
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
