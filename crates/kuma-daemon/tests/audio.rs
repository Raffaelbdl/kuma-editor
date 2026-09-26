//! Measuring a sequence's sound: rendered through the real graph, then metered. Needs
//! ffmpeg and melt, and skips (saying so) without them.

use std::sync::Arc;

use kuma_daemon::jobs::{measure_audio, MeasureRequest};
use kuma_daemon::{AppState, Config};

/// Two chapters of the same tone, the second 6 dB down (`volume` 0.5), with two seconds of
/// nothing between them. The first opens on a fade that rounds to one frame, which used to
/// silence the whole clip.
const PROJECT: &str = r##"{
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "tone": { "name": "Tone", "source": { "path": "tone.wav", "kind": "audio" } },
    "one": { "name": "One", "source": { "tracks": [
      { "id": "a1", "kind": "audio", "clips": [ { "id": "t1", "start": 0, "end": 4, "bin": "tone",
        "effects": [ { "type": "audio_fade", "in": 0.02 } ] } ] }
    ] } },
    "two": { "name": "Two", "source": { "tracks": [
      { "id": "a2", "kind": "audio", "clips": [ { "id": "t2", "start": 0, "end": 4, "bin": "tone", "volume": 0.5 } ] }
    ] } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "clips": [
        { "id": "ch1", "start": 0, "end": 4, "bin": "one", "label": "Opening" },
        { "id": "ch2", "start": 6, "end": 10, "bin": "two" } ] }
    ] } }
  }
}"##;

fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let tone = dir.path().join("tone.wav").to_string_lossy().into_owned();
    kuma_overlay::media::ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=440:duration=5:sample_rate=48000", "-ac", "2", &tone].map(String::from)).unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    (Arc::new(AppState::load(config).unwrap()), dir)
}

#[test]
fn a_sequence_is_measured_whole_by_chapter_and_where_it_is_silent() {
    if !kuma_overlay::media::ffprobe_available() || !kuma_mlt::melt::melt_available() {
        eprintln!("ffmpeg/melt not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let measure = |req: MeasureRequest| measure_audio(&state, &req, &|_, _| {}, &Default::default()).expect("measure");
    let m = measure(MeasureRequest::default());
    let f = |v: &serde_json::Value| v.as_f64().unwrap_or_else(|| panic!("a number: {v} in {m:#}"));

    let sections = m["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 2, "one per chapter: {m:#}");
    assert_eq!((&sections[0]["label"], &sections[0]["clip"], &sections[1]["label"]), (&serde_json::json!("Opening"), &serde_json::json!("ch1"), &serde_json::json!("Two")));
    let (one, two) = (f(&sections[0]["integrated"]), f(&sections[1]["integrated"]));
    assert!((one - two - 6.0).abs() < 0.3, "half the gain is 6 dB quieter: {one} and {two}");
    assert!((f(&sections[0]["true_peak"]) - f(&sections[1]["true_peak"]) - 6.0).abs() < 0.3, "{m:#}");
    assert!((f(&m["true_peak"]) - f(&sections[0]["true_peak"])).abs() < 0.05, "the loudest chapter holds the peak: {m:#}");
    // Gated: the quiet chapter is under the relative gate of neither, so the whole lies
    // between the two.
    assert!(f(&m["integrated"]) < one && f(&m["integrated"]) > two, "{m:#}");

    // The gap between the chapters, and nothing else: not the opening, which a one-frame
    // fade used to silence for the whole clip.
    let silences = m["silences"].as_array().unwrap();
    assert_eq!(silences.len(), 1, "{m:#}");
    assert!((f(&silences[0]["start"]) - 4.0).abs() < 0.15 && (f(&silences[0]["end"]) - 6.0).abs() < 0.15, "{m:#}");
    let hottest = m["hottest"].as_array().unwrap();
    assert_eq!(hottest.len(), 5, "{m:#}");
    assert!(f(&hottest[0]["t"]) < 4.0 && f(&hottest[0]["true_peak"]) == f(&m["true_peak"]), "the loudest moment first, in the loud chapter: {m:#}");
    let times: Vec<f64> = hottest.iter().map(|h| f(&h["t"])).collect();
    assert!(times.iter().all(|a| times.iter().filter(|b| (*a - **b).abs() < 2.0).count() == 1), "at least 2 s apart: {times:?}");

    // Part of it, in the sequence's own seconds.
    let part = measure(MeasureRequest { range: Some((5.0, 10.0)), ..Default::default() });
    let s = part["sections"].as_array().unwrap();
    assert_eq!(s.len(), 1, "only the chapter in the range: {part:#}");
    assert!((f(&s[0]["integrated"]) - two).abs() < 0.2, "{part:#}");
    let quiet = part["silences"].as_array().unwrap();
    assert!(quiet.len() == 1 && (f(&quiet[0]["start"]) - 5.0).abs() < 0.15 && (f(&quiet[0]["end"]) - 6.0).abs() < 0.15, "{part:#}");
}
