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
        let model = std::env::var_os("KUMA_WHISPER_MODEL").map(PathBuf::from).or_else(|| {
            let mut bins: Vec<PathBuf> = std::fs::read_dir(&model_dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect();
            bins.sort();
            bins.into_iter().next()
        });
        Transcriber { cli, model, model_dir }
    }

    /// The model path when everything needed is there, else what to install and where.
    pub fn available(&self) -> Result<&Path, String> {
        if !Command::new(&self.cli).arg("--help").output().map(|o| o.status.success()).unwrap_or(false) {
            return Err(format!("`{}` is not runnable: install whisper.cpp (its `whisper-cli`) on PATH, or set KUMA_WHISPER_BIN", self.cli.display()));
        }
        match &self.model {
            Some(m) if m.is_file() => Ok(m),
            Some(m) => Err(format!("whisper model not found: {}", m.display())),
            None => Err(format!("no whisper model: put a ggml `*.bin` (e.g. ggml-base.en.bin) in {} or set KUMA_WHISPER_MODEL", self.model_dir.display())),
        }
    }

    /// Transcribe a media file's audio. `work_dir` receives the WAV and the JSON, under a
    /// fresh directory per call so two jobs on one file never share a name. `progress` is
    /// called with the model's own percentage as it goes: whisper is minutes of work on a
    /// long recording, so the caller must be able to say more than "started".
    pub fn transcribe(&self, media: &Path, work_dir: &Path, opts: &TranscribeOptions, progress: &dyn Fn(u8)) -> Result<Vec<Cue>, String> {
        let model = self.available()?;
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
        parse_whisper_json(&json)
    }
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
