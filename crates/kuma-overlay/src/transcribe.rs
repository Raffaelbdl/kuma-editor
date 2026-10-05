//! Speech to text for subtitles: whisper.cpp as a subprocess, the way melt and ffmpeg are.
//!
//! `whisper-cli` reads 16 kHz mono WAV, so the media is extracted with ffmpeg first; the
//! JSON it writes (`-oj`) becomes a list of cues in **source seconds**. That list is the
//! `cues` var of the `subtitles` overlay, and nothing else about the model
//! reaches the project: a cue is `{ start, end, text }` and is edited like any other var.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::loudness::{self, Quiet};
use crate::media::ffmpeg;

/// One subtitle line. Seconds, `end` exclusive, like everything else in the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TranscribeOptions {
    /// ISO 639-1 code (`en`, `fr`, …) or `auto` (the default).
    #[serde(default)]
    pub language: Option<String>,
    /// Longest cue in characters: whisper re-splits its segments on word boundaries to
    /// fit. The default, `1`, is one cue per word — the finest timing whisper gives, and
    /// the form an overlay can group into lines of any length. `0` keeps whisper's own
    /// segments (sentences); anything else, lines of about that many characters.
    #[serde(default)]
    pub max_chars: Option<u32>,
    /// Which model: a file in the models folder (`ggml-large-v3.bin`) or its size
    /// (`large-v3`, `medium`, `small`). The default model when absent.
    #[serde(default)]
    pub model: Option<String>,
    /// Move cue edges onto the pauses in the sound (the default): whisper's word cues
    /// are contiguous, so a pause is absorbed into a word beside it. `false` keeps
    /// whisper's own times.
    #[serde(default)]
    pub snap: Option<bool>,
    /// What a pause is, for that: every sample under this many dBFS (default -35)…
    #[serde(default)]
    pub silence_db: Option<f64>,
    /// …for at least this many seconds (default 0.15).
    #[serde(default)]
    pub silence_min: Option<f64>,
}

impl TranscribeOptions {
    /// The pauses cues are snapped to, unless snapping is off.
    pub fn pauses(&self) -> Option<Quiet> {
        let d = Quiet::default();
        (self.snap != Some(false)).then(|| Quiet { db: self.silence_db.unwrap_or(d.db), min: self.silence_min.unwrap_or(d.min) })
    }
}

/// Where whisper.cpp is. Discovered once at daemon start, like the baker.
#[derive(Debug, Clone)]
pub struct Transcriber {
    /// `whisper-cli` from whisper.cpp: `KUMA_WHISPER_BIN`, else the checkout's own build
    /// (`npm run rebuild` clones and builds it under `whisper.cpp/`), else `PATH`.
    pub cli: PathBuf,
    /// A ggml model file: `KUMA_WHISPER_MODEL`, else the first `*.bin` in `model_dir`.
    pub model: Option<PathBuf>,
    pub model_dir: PathBuf,
}

impl Transcriber {
    /// `repo_root` is the checkout; `model_dir` is the library's `models/` folder
    /// (`~/.local/share/kuma/models`, where the rebuild downloads one).
    pub fn discover(repo_root: &Path, model_dir: PathBuf) -> Self {
        let cli = std::env::var_os("KUMA_WHISPER_BIN").map(PathBuf::from).unwrap_or_else(|| {
            let own = repo_root.join("whisper.cpp/build/bin/whisper-cli");
            if own.is_file() { own } else { "whisper-cli".into() }
        });
        let mut t = Transcriber { cli, model: std::env::var_os("KUMA_WHISPER_MODEL").map(PathBuf::from), model_dir };
        if t.model.is_none() {
            t.model = t.models().first().map(|name| t.model_dir.join(name));
        }
        t
    }

    /// The model path when everything needed is there, else what to install and where.
    pub fn available(&self) -> Result<&Path, String> {
        self.runnable()?;
        match &self.model {
            Some(m) if m.is_file() => Ok(m),
            Some(m) => Err(format!("whisper model not found: {}", m.display())),
            None => Err(format!("no whisper model: put a ggml `*.bin` (e.g. ggml-base.en.bin) in {} or set KUMA_WHISPER_MODEL", self.model_dir.display())),
        }
    }

    fn runnable(&self) -> Result<(), String> {
        if !Command::new(&self.cli).arg("--help").output().map(|o| o.status.success()).unwrap_or(false) {
            return Err(format!("`{}` is not runnable: install whisper.cpp (its `whisper-cli`) on PATH, or set KUMA_WHISPER_BIN", self.cli.display()));
        }
        Ok(())
    }

    /// The model files in the models folder, by name: what `model` can ask for.
    pub fn models(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.model_dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "bin")).filter_map(|p| Some(p.file_name()?.to_string_lossy().into_owned())).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The model `name` asks for, in the models folder: the file of that name, or
    /// `ggml-<name>.bin`, so `large-v3` is `ggml-large-v3.bin` as whisper.cpp's downloads
    /// name it. The default model when `None`. A name is never a path: the folder is the
    /// one place models are found.
    pub fn model_named(&self, name: Option<&str>) -> Result<PathBuf, String> {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else { return self.available().map(Path::to_path_buf) };
        self.runnable()?;
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(format!("`model` is the name of a file in {}, not a path", self.model_dir.display()));
        }
        let candidates = [name.to_string(), format!("ggml-{name}.bin"), format!("{name}.bin")];
        if let Some(found) = candidates.iter().map(|c| self.model_dir.join(c)).find(|p| p.is_file()) {
            return Ok(found);
        }
        let installed = self.models();
        let listed = if installed.is_empty() { "none".to_string() } else { installed.join(", ") };
        Err(format!(
            "no whisper model `{name}` in {}: installed are {listed}. Download one into that folder from https://huggingface.co/ggerganov/whisper.cpp (e.g. ggml-{name}.bin), or `WHISPER_MODEL={name} npm run rebuild` with the folder empty",
            self.model_dir.display()
        ))
    }

    /// Transcribe a media file's audio. `work_dir` receives the WAV and the JSON, under a
    /// fresh directory per call so two jobs on one file never share a name. `progress` is
    /// called with the model's own percentage as it goes: whisper is minutes of work on a
    /// long recording, so the caller must be able to say more than "started".
    pub fn transcribe(&self, media: &Path, work_dir: &Path, opts: &TranscribeOptions, progress: &dyn Fn(u8)) -> Result<Vec<Cue>, String> {
        let model = self.model_named(opts.model.as_deref())?;
        let model = model.as_path();
        std::fs::create_dir_all(work_dir).map_err(|e| format!("{}: {e}", work_dir.display()))?;
        let scratch = tempfile::Builder::new().prefix("stt-").tempdir_in(work_dir).map_err(|e| e.to_string())?;
        let base = scratch.path().join("audio");
        let wav = base.with_extension("wav");
        let arg = |p: &Path| p.to_string_lossy().into_owned();
        ffmpeg(&["-i".into(), arg(media), "-vn".into(), "-ac".into(), "1".into(), "-ar".into(), "16000".into(), "-c:a".into(), "pcm_s16le".into(), arg(&wav)])
            .map_err(|e| format!("extracting audio: {e}"))?;

        progress(0);
        let mut cmd = Command::new(&self.cli);
        // `-pp` prints a percentage on stderr as it works; `-np` silences everything else
        // it would say there, which is why the two go together rather than fighting.
        cmd.args(["-m", &arg(model), "-f", &arg(&wav), "-oj", "-of", &arg(&base), "-np", "-pp", "-l", opts.language.as_deref().unwrap_or("auto")]);
        let max_chars = opts.max_chars.unwrap_or(1);
        if max_chars > 0 {
            cmd.args(["-ml", &max_chars.to_string(), "-sow"]);
        }
        let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{}: {e}", self.cli.display()))?;
        let stderr = child.stderr.take().expect("stderr is piped");
        // Read it line by line rather than waiting for the process: that is the whole
        // point of asking for progress. Whatever is not progress is kept for the error
        // message, since a failure is explained by the last thing it said.
        let mut tail: VecDeque<String> = VecDeque::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            match parse_progress(&line) {
                Some(p) => progress(p),
                None => {
                    if tail.len() == 10 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            }
        }
        let status = child.wait().map_err(|e| format!("{}: {e}", self.cli.display()))?;
        if !status.success() {
            return Err(format!("whisper failed: {}", Vec::from(tail).join("\n").trim()));
        }
        let json = std::fs::read_to_string(base.with_extension("json")).map_err(|e| format!("whisper wrote no transcript: {e}"))?;
        let mut cues = parse_whisper_json(&json)?;
        if let Some(quiet) = opts.pauses() {
            // The WAV is already here, and finding its pauses is one quick ffmpeg pass.
            snap_to_pauses(&mut cues, &loudness::silences(&wav, quiet)?, SNAP_REACH);
        }
        Ok(cues)
    }
}

/// How far a cue's edge may be from a pause and still be moved onto it: whisper's word
/// edges can be a third of a second off.
pub const SNAP_REACH: f64 = 0.4;

/// Give cues the gaps the sound has. whisper's word cues are contiguous, so a pause is
/// absorbed into a word beside it, and a boundary taken from them can be off by a third
/// of a second: a cut there clips a first word or keeps the next sentence's.
///
/// So an edge within `reach` of a pause moves onto it: a cue's end to where the pause
/// starts, a cue's start to where it ends — inside the pause or beside it, since whisper
/// puts a boundary late or early as often as in it. The nearest pause wins, and one the
/// edge is in is nearest. A move that would empty a cue is not made, and neither is one
/// that would run a cue into the next: that pair keeps whisper's edges. `pauses` are
/// `(start, end)` in order, in the cues' seconds.
pub fn snap_to_pauses(cues: &mut [Cue], pauses: &[(f64, f64)], reach: f64) {
    if pauses.is_empty() {
        return;
    }
    // The pause nearest `t` within reach that `fits` accepts: in it first, then the closest,
    // then the longest.
    let nearest = |t: f64, fits: &dyn Fn(&(f64, f64)) -> bool| -> Option<(f64, f64)> {
        let away = |p: &(f64, f64)| if t < p.0 { p.0 - t } else if t > p.1 { t - p.1 } else { 0.0 };
        pauses.iter().filter(|p| away(p) <= reach && fits(p)).min_by(|a, b| away(a).total_cmp(&away(b)).then((b.1 - b.0).total_cmp(&(a.1 - a.0)))).copied()
    };
    let before: Vec<Cue> = cues.to_vec();
    // To the millisecond, as whisper gives them.
    let ms = |t: f64| (t * 1000.0).round() / 1000.0;
    for c in cues.iter_mut() {
        let (start, end) = (c.start, c.end);
        if let Some(p) = nearest(end, &|p| ms(p.0) > start) {
            c.end = ms(p.0);
        }
        if let Some(p) = nearest(start, &|p| ms(p.1) < c.end.min(end)) {
            c.start = ms(p.1);
        }
        if c.end <= c.start {
            (c.start, c.end) = (start, end);
        }
    }
    // A cue may not run into the one after it: trim its end back to that cue's start, and
    // where that would empty it, give both their own edges back. Twice, since a cue given
    // its start back can then be run into by the one before it.
    for _ in 0..2 {
        for i in 1..cues.len() {
            let next = cues[i].start;
            if cues[i - 1].end > next {
                cues[i - 1].end = next;
                if cues[i - 1].end <= cues[i - 1].start {
                    cues[i - 1] = before[i - 1].clone();
                    cues[i] = before[i].clone();
                }
            }
        }
    }
}

/// One stretch of a transcript cut at pauses: its own entry, baked as its own clip.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    /// The source seconds it stands for: from the middle of the pause before it (0 for the
    /// first) to the middle of the pause after it (`f64::INFINITY` for the last).
    pub start: f64,
    pub end: f64,
    /// Its cues, as indices into the transcript.
    pub cues: std::ops::Range<usize>,
}

/// Cut a transcript into pieces of about `every` seconds, at pauses: each cut is the best
/// gap between cues whose piece would be between ⅔ and 4⁄3 of `every` long. A gap is better
/// the longer it is, up to a second (any pause that long is a fine place to cut), and a
/// sentence ending before it counts as 0.3 s more; ties go to the cut nearest `every`.
/// A transcript, or a remainder, of up to 4⁄3 of `every` stays whole. `every` of 0 is one
/// piece. Long pieces bake as long clips, and every correction re-bakes the whole clip.
pub fn split_at_pauses(cues: &[Cue], every: f64) -> Vec<Piece> {
    let whole = |range: std::ops::Range<usize>| Piece { start: 0.0, end: f64::INFINITY, cues: range };
    if every <= 0.0 || cues.is_empty() {
        return vec![whole(0..cues.len())];
    }
    let (shortest, longest) = (every * 2.0 / 3.0, every * 4.0 / 3.0);
    let ends_sentence = |c: &Cue| c.text.trim_end_matches(['"', '\'', '»', '”', ')']).ends_with(['.', '?', '!', '…', '。', '？', '！']);
    let mut cuts = Vec::new();
    let mut first = 0;
    while cues[cues.len() - 1].end - cues[first].start > longest {
        let from = cues[first].start;
        let length = |j: usize| cues[j - 1].end - from;
        let score = |j: usize| (cues[j].start - cues[j - 1].end).clamp(0.0, 1.0) + if ends_sentence(&cues[j - 1]) { 0.3 } else { 0.0 };
        let best = (first + 1..cues.len())
            .filter(|&j| (shortest..=longest).contains(&length(j)))
            .max_by(|&a, &b| score(a).total_cmp(&score(b)).then((length(b) - every).abs().total_cmp(&(length(a) - every).abs())));
        // No gap in the window — one long cue, or a monologue without a pause — means the
        // first gap after it.
        let Some(j) = best.or_else(|| (first + 1..cues.len()).find(|&j| length(j) >= shortest)) else { break };
        cuts.push(j);
        first = j;
    }
    let mut pieces = Vec::new();
    let mut from = 0;
    // To the hundredth: an `offset` is set in those.
    let middle = |j: usize| ((cues[j - 1].end + cues[j].start) * 50.0).round() / 100.0;
    for &j in cuts.iter().chain([&cues.len()]) {
        let start = if from == 0 { 0.0 } else { middle(from) };
        let end = if j == cues.len() { f64::INFINITY } else { middle(j) };
        pieces.push(Piece { start, end, cues: from..j });
        from = j;
    }
    pieces
}

/// The percentage out of a `-pp` line (`…: progress =  40%`), clamped: whisper measures
/// against the seek range and overshoots past 100 on the last window.
fn parse_progress(line: &str) -> Option<u8> {
    let pct = line.split("progress =").nth(1)?.trim().strip_suffix('%')?;
    Some(pct.trim().parse::<u32>().ok()?.min(100) as u8)
}

/// The `transcription` list of whisper.cpp's `-oj` output. Offsets are milliseconds.
#[derive(Deserialize)]
struct WhisperOut {
    #[serde(default)]
    transcription: Vec<Segment>,
}
#[derive(Deserialize)]
struct Segment {
    offsets: Offsets,
    text: String,
}
#[derive(Deserialize)]
struct Offsets {
    from: f64,
    to: f64,
}

/// Cues from whisper.cpp's JSON: trimmed, in seconds, without the non-speech markers
/// (`[BLANK_AUDIO]`, `[Music]`, `(applause)`) and zero-length segments the model emits.
pub fn parse_whisper_json(text: &str) -> Result<Vec<Cue>, String> {
    let out: WhisperOut = serde_json::from_str(text).map_err(|e| format!("whisper output: {e}"))?;
    Ok(out
        .transcription
        .into_iter()
        .map(|s| Cue { start: s.offsets.from / 1000.0, end: s.offsets.to / 1000.0, text: s.text.trim().to_string() })
        .filter(|c| c.end > c.start && !c.text.is_empty())
        .filter(|c| !((c.text.starts_with('[') && c.text.ends_with(']')) || (c.text.starts_with('(') && c.text.ends_with(')'))))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whisper_cpp_output_into_seconds_and_drops_markers() {
        let json = r#"{ "systeminfo": "x", "model": { "type": "base" }, "result": { "language": "en" }, "transcription": [
          { "timestamps": { "from": "00:00:00,000", "to": "00:00:01,250" }, "offsets": { "from": 0, "to": 1250 }, "text": " Hello there" },
          { "timestamps": { "from": "00:00:01,250", "to": "00:00:03,000" }, "offsets": { "from": 1250, "to": 3000 }, "text": " [BLANK_AUDIO]" },
          { "timestamps": { "from": "00:00:03,000", "to": "00:00:03,000" }, "offsets": { "from": 3000, "to": 3000 }, "text": " glitch" },
          { "timestamps": { "from": "00:00:03,000", "to": "00:00:04,500" }, "offsets": { "from": 3000, "to": 4500 }, "text": "  general Kenobi.  " },
          { "timestamps": { "from": "00:00:04,500", "to": "00:00:05,000" }, "offsets": { "from": 4500, "to": 5000 }, "text": " (applause)" }
        ] }"#;
        let cues = parse_whisper_json(json).unwrap();
        assert_eq!(cues, vec![
            Cue { start: 0.0, end: 1.25, text: "Hello there".into() },
            Cue { start: 3.0, end: 4.5, text: "general Kenobi.".into() },
        ]);
        assert!(parse_whisper_json("{}").unwrap().is_empty(), "no transcription list is an empty transcript");
        assert!(parse_whisper_json("nope").is_err());
    }

    #[test]
    fn reads_the_percentage_whisper_prints() {
        assert_eq!(parse_progress("whisper_print_progress_callback: progress =  31%"), Some(31));
        assert_eq!(parse_progress("whisper_print_progress_callback: progress = 126%"), Some(100), "it overshoots on the last window");
        assert_eq!(parse_progress("read_audio_data: reading audio data from 'x.wav' ..."), None);
        assert_eq!(parse_progress("progress = soon%"), None);
    }

    /// whisper's word cues are contiguous: each pause is absorbed into a word beside it,
    /// and the boundary can sit a third of a second from it either way.
    #[test]
    fn cue_edges_move_onto_the_pauses_beside_them() {
        let cue = |start: f64, end: f64, text: &str| Cue { start, end, text: text.into() };
        let mut cues = vec![
            cue(0.1, 2.25, "Bonjour"), // starts in the pause before speech, runs 0.35 s past the next one
            cue(2.25, 2.7, "tout"),    // so starts 0.35 s late
            cue(2.7, 3.0, "le"),       // far from any pause
            cue(3.0, 4.3, "monde."),   // runs into a pause
            cue(4.3, 5.2, "Ensuite"),  // starts in it
            cue(5.2, 5.7, "il"),       // ends 0.1 s before a pause, but the next word is there
            cue(5.7, 6.0, "faut"),     // which runs into the pause
            cue(6.0, 7.0, "partir."),
        ];
        let pauses = [(0.0, 0.3), (1.5, 1.9), (4.0, 4.6), (5.8, 6.1)];
        snap_to_pauses(&mut cues, &pauses, SNAP_REACH);
        let got: Vec<(f64, f64)> = cues.iter().map(|c| (c.start, c.end)).collect();
        assert_eq!(got, [(0.3, 1.5), (1.9, 2.7), (2.7, 3.0), (3.0, 4.0), (4.6, 5.2), (5.2, 5.7), (5.7, 5.8), (6.1, 7.0)]);
        // `il` reached for the pause too, and stops where `faut` starts instead of running
        // into it; `faut` keeps its start, since the pause's end would be past its own.
        assert!(cues.windows(2).all(|w| w[0].end <= w[1].start + 1e-9), "{got:?}");

        let mut word = vec![cue(6.0, 6.2, "euh")];
        snap_to_pauses(&mut word, &[(5.9, 6.5)], SNAP_REACH);
        assert_eq!((word[0].start, word[0].end), (6.0, 6.2), "a cue wholly inside a pause is not emptied");

        let mut far = vec![cue(1.0, 2.0, "loin")];
        snap_to_pauses(&mut far, &[(2.5, 3.0)], SNAP_REACH);
        assert_eq!((far[0].start, far[0].end), (1.0, 2.0), "a pause 0.5 s away is not this word's");
    }

    /// Three minutes of words, 0.5 s each with a short gap, and a few real pauses: the cuts
    /// fall on the pauses nearest a minute, and a sentence end wins over a slightly longer
    /// gap mid-sentence.
    #[test]
    fn a_transcript_is_cut_at_pauses_about_every_n_seconds() {
        let mut cues = Vec::new();
        let mut t = 0.0;
        while t < 180.0 {
            let pause = match t {
                t if (55.0..55.5).contains(&t) => 0.6,  // a pause mid-sentence…
                t if (62.0..62.5).contains(&t) => 0.5,  // …and a sentence end just after it
                t if (121.0..121.5).contains(&t) => 0.8,
                _ => 0.05,
            };
            let text = if (62.0..62.5).contains(&t) { "end." } else { "word" };
            cues.push(Cue { start: t, end: t + 0.5, text: text.into() });
            t += 0.5 + pause;
        }
        let pieces = split_at_pauses(&cues, 60.0);
        assert_eq!(pieces.len(), 3, "{pieces:?}");
        let cut = |p: &Piece| cues[p.cues.end - 1].end;
        assert!((cut(&pieces[0]) - 62.5).abs() < 0.6 && cues[pieces[0].cues.end - 1].text == "end.", "the sentence end: {:?}", cues[pieces[0].cues.end - 1]);
        assert!((cut(&pieces[1]) - 121.5).abs() < 0.6, "the long pause: {}", cut(&pieces[1]));
        assert_eq!((pieces[0].start, pieces[2].end, pieces[2].cues.end), (0.0, f64::INFINITY, cues.len()));
        assert!(pieces.windows(2).all(|w| w[0].end == w[1].start && w[0].cues.end == w[1].cues.start), "contiguous, in the middle of the pause");
        assert!((pieces[1].start - (cut(&pieces[0]) + 0.25)).abs() < 0.006, "{:?}", pieces[1]);

        assert_eq!(split_at_pauses(&cues, 0.0).len(), 1, "0 is one piece");
        assert_eq!(split_at_pauses(&cues[..140], 60.0).len(), 1, "about 75 s is not worth two");
        assert_eq!(split_at_pauses(&[], 60.0), vec![Piece { start: 0.0, end: f64::INFINITY, cues: 0..0 }]);
    }

    #[test]
    fn a_model_is_named_by_file_or_by_size_and_never_by_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("models");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["ggml-base.bin", "ggml-large-v3.bin", "notes.txt"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let t = Transcriber { cli: "true".into(), model: Some(dir.join("ggml-base.bin")), model_dir: dir.clone() };
        assert_eq!(t.models(), ["ggml-base.bin", "ggml-large-v3.bin"]);
        assert_eq!(t.model_named(None).unwrap(), dir.join("ggml-base.bin"), "the default");
        assert_eq!(t.model_named(Some("large-v3")).unwrap(), dir.join("ggml-large-v3.bin"));
        assert_eq!(t.model_named(Some("ggml-large-v3.bin")).unwrap(), dir.join("ggml-large-v3.bin"));
        let missing = t.model_named(Some("medium")).unwrap_err();
        assert!(missing.contains("installed are ggml-base.bin, ggml-large-v3.bin") && missing.contains("ggml-medium.bin"), "{missing}");
        assert!(t.model_named(Some("../models/ggml-base.bin")).unwrap_err().contains("not a path"));
    }

    #[test]
    fn availability_names_what_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let t = Transcriber { cli: tmp.path().join("no-such-whisper"), model: None, model_dir: tmp.path().join("models") };
        assert!(t.available().unwrap_err().contains("KUMA_WHISPER_BIN"));
        let t = Transcriber { cli: "true".into(), ..t };
        assert!(t.available().unwrap_err().contains("KUMA_WHISPER_MODEL"));
        let t = Transcriber { model: Some(tmp.path().join("gone.bin")), ..t };
        assert!(t.available().unwrap_err().contains("not found"));
    }
}
