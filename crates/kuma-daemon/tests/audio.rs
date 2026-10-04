//! Measuring a sequence's sound: rendered through the real graph, then metered. Needs
//! ffmpeg and melt, and skips (saying so) without them.

use std::sync::Arc;

use kuma_daemon::jobs::{measure_audio, measure_media, MeasureRequest};
use serde_json::json;
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

/// One source, measured where it is rather than through a timeline: by bin entry or by
/// path, whole or a stretch, in its own seconds, and nothing written to the document.
#[test]
fn a_source_is_measured_in_its_own_seconds() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let (state, dir) = fixture();
    let make = |name: &str, lavfi: &str| {
        let out = dir.path().join(name).to_string_lossy().into_owned();
        kuma_overlay::media::ffmpeg(&["-f", "lavfi", "-i", lavfi, &out].map(String::from)).unwrap();
    };
    // Pauses at 2–2.5 s and 5–5.2 s, 6 dB down after the first.
    make("talk.wav", "sine=frequency=440:duration=8:sample_rate=48000:samples_per_frame=48,volume='if(between(t,2,2.5)+between(t,5,5.2),0,if(gt(t,2.5),0.5,1))':eval=frame");
    make("room.wav", "anullsrc=channel_layout=mono:sample_rate=8000:duration=130");
    kuma_daemon::jobs::add_media(&state, std::path::Path::new("talk.wav"), None, None, None, "test").unwrap();
    let before = serde_json::to_value(&*state.project.read().unwrap()).unwrap();
    let measure = |req: serde_json::Value| measure_media(&state, &serde_json::from_value(req).unwrap(), &|_, _| {}, &Default::default());
    let pairs = |v: &serde_json::Value| -> Vec<(f64, f64)> { v["silences"].as_array().unwrap().iter().map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())).collect() };
    let near = |a: &[(f64, f64)], b: &[(f64, f64)]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.0 - y.0).abs() < 0.03 && (x.1 - y.1).abs() < 0.03);

    let whole = measure(json!({ "bin": "talk" })).unwrap();
    assert!(near(&pairs(&whole), &[(2.0, 2.5), (5.0, 5.2)]), "{whole:#}");
    assert!(whole["integrated"].is_f64() && whole["true_peak"].is_f64() && whole["range"].is_f64(), "{whole:#}");
    assert_eq!((&whole["curve"]["of"], &whole["curve"]["step"], whole["curve"]["lufs"].as_array().unwrap().len()), (&json!("short_term"), &json!(0.1), 80), "8 s at the finest step stays under the default size: {whole:#}");

    // A stretch, by path, at a chosen step: the pause in it, still in the file's seconds.
    let part = measure(json!({ "path": "talk.wav", "range": [3.0, 7.0], "curve": "momentary", "step": 1.0 })).unwrap();
    assert!(near(&pairs(&part), &[(5.0, 5.2)]), "{part:#}");
    assert_eq!((&part["start"], &part["end"], &part["curve"]["start"], &part["curve"]["window"]), (&json!(3.0), &json!(7.0), &json!(4.0), &json!(0.4)), "{part:#}");
    let lufs: Vec<f64> = part["curve"]["lufs"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert_eq!(lufs.len(), 4, "{part:#}");
    assert!((lufs[0] - lufs[3]).abs() < 0.3, "the same level either side of the short pause: {lufs:?}");
    // A longer minimum skips the short pause, and a higher threshold is still the same tone.
    let gaps = measure(json!({ "bin": "talk", "silence_min": 0.3, "curve": "none" })).unwrap();
    assert!(near(&pairs(&gaps), &[(2.0, 2.5)]) && gaps["curve"].is_null(), "{gaps:#}");

    // Over two minutes the default step widens, and a curve too long to read is refused.
    let room = measure(json!({ "path": "room.wav" })).unwrap();
    assert_eq!((&room["curve"]["step"], room["curve"]["lufs"].as_array().unwrap().len()), (&json!(2.0), 65), "{room:#}");
    assert!(room["integrated"].is_null() && room["curve"]["lufs"][0].is_null(), "digital silence: {room:#}");
    assert!(near(&pairs(&room), &[(0.0, 130.0)]), "{room:#}");
    let refused = |req: serde_json::Value| measure(req).unwrap_err().to_string();
    assert!(refused(json!({ "path": "room.wav", "step": 0.1 })).contains("the most is 1200"));
    assert!(refused(json!({ "path": "../elsewhere.wav" })).contains("cannot read"));
    assert!(refused(json!({ "path": "/etc/hostname" })).contains("not inside the project"));
    assert!(refused(json!({ "bin": "talk", "path": "talk.wav" })).contains("exactly one"));
    assert!(refused(json!({ "bin": "talk", "range": [9.0, 12.0] })).contains("nothing after 9"));
    assert!(refused(json!({ "bin": "talk", "curve": "loud" })).contains("`curve` is"));

    assert_eq!(serde_json::to_value(&*state.project.read().unwrap()).unwrap(), before, "nothing is stored");
}

/// A master whose picture track places two scenes, each with its own voice track, and a
/// music bed under both that runs on after them; the first scene also has a sound effect.
/// Three tones at distinct pitches, so each part is its own level: the voice in the second
/// scene 6 dB under the first, the music 12 dB under the voice.
const NESTED: &str = r##"{
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "voice": { "name": "Voice", "source": { "path": "voice.wav", "kind": "audio" } },
    "music": { "name": "Music", "source": { "path": "music.wav", "kind": "audio" } },
    "sfx": { "name": "Sfx", "source": { "path": "sfx.wav", "kind": "audio" } },
    "sc1": { "name": "Scene one", "source": { "tracks": [
      { "id": "va", "kind": "audio", "clips": [ { "id": "v1", "start": 0, "end": 4, "bin": "voice" } ] },
      { "id": "fx", "kind": "audio", "clips": [ { "id": "f1", "start": 2, "end": 3, "bin": "sfx" } ] }
    ] } },
    "sc2": { "name": "Scene two", "source": { "tracks": [
      { "id": "vb", "kind": "audio", "clips": [ { "id": "v2", "start": 0, "end": 4, "bin": "voice", "volume": 0.5 } ] }
    ] } },
    "unused": { "name": "Unused", "source": { "tracks": [
      { "id": "ux", "kind": "audio", "clips": [ { "id": "u1", "start": 0, "end": 1, "bin": "voice" } ] }
    ] } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "pic", "kind": "video", "clips": [
        { "id": "ch1", "start": 0, "end": 4, "bin": "sc1" },
        { "id": "ch2", "start": 5, "end": 9, "bin": "sc2" } ] },
      { "id": "bed", "kind": "audio", "clips": [ { "id": "m1", "start": 0, "end": 12, "bin": "music", "volume": 0.25 } ] },
      { "id": "o1", "kind": "overlay", "clips": [] }
    ] } }
  }
}"##;

/// Solo and mute are for one measurement: applied to the copy that is compiled, never to
/// the document. Soloing a track inside a nested sequence hears it through the track that
/// places that sequence, and nothing else that track or any other carries.
#[test]
fn a_track_is_heard_alone_wherever_it_is_nested() {
    if !kuma_overlay::media::ffprobe_available() || !kuma_mlt::melt::melt_available() {
        eprintln!("ffmpeg/melt not available; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (name, tone) in [("voice", "sine=frequency=440:duration=6"), ("music", "sine=frequency=660:duration=20"), ("sfx", "sine=frequency=880:duration=2")] {
        let out = dir.path().join(format!("{name}.wav")).to_string_lossy().into_owned();
        kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), format!("{tone}:sample_rate=48000"), "-ac".into(), "2".into(), out]).unwrap();
    }
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, NESTED).unwrap();
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = Config { project_file, repo_root, library_dirs: vec![dir.path().join("library")], bind: "127.0.0.1:0".parse().unwrap() };
    let state = Arc::new(AppState::load(config).unwrap());
    let before = serde_json::to_value(&*state.project.read().unwrap()).unwrap();

    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let measure = |mute: &[&str], solo: &[&str]| measure_audio(&state, &MeasureRequest { mute: ids(mute), solo: ids(solo), ..Default::default() }, &|_, _| {}, &Default::default());
    let chapters = |m: &serde_json::Value| -> Vec<Option<f64>> { m["sections"].as_array().unwrap().iter().map(|s| s["integrated"].as_f64()).collect() };
    let silent = |m: &serde_json::Value, a: f64, b: f64| m["silences"].as_array().unwrap().iter().any(|s| s["start"].as_f64().unwrap() <= a + 0.15 && s["end"].as_f64().unwrap() >= b - 0.15);
    let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 0.3);

    // One scene's voice alone: the picture track still carries it, but not the other
    // scene, whose voice is on another track, nor the music or the effect.
    let one = measure(&[], &["va"]).unwrap();
    let [voice, none] = chapters(&one)[..] else { panic!("{one:#}") };
    let voice = voice.unwrap_or_else(|| panic!("the soloed voice is heard: {one:#}"));
    assert_eq!(none, None, "the other scene is silent: {one:#}");
    assert!(silent(&one, 4.0, 12.0), "and so is the music after the first scene: {one:#}");
    assert_eq!(one["silenced"], serde_json::json!({ "tracks": 2, "clips": 1 }), "`fx` and `bed`, and the clip placing the other scene: {one:#}");

    // Both voices: each scene at its own level, and still no music.
    let both = measure(&[], &["va", "vb"]).unwrap();
    let c = chapters(&both);
    assert!(near(c[0], voice) && near(c[1], voice - 6.0), "{both:#}");
    assert!(silent(&both, 9.0, 12.0), "{both:#}");

    // The music alone, under both scenes and after them.
    let bed = measure(&[], &["bed"]).unwrap();
    let c = chapters(&bed);
    assert!(near(c[0], voice - 12.0) && near(c[1], voice - 12.0), "the bed is 12 dB under the voice: {bed:#}");
    assert!(bed["silences"].as_array().unwrap().is_empty(), "{bed:#}");

    // The effect alone is only its one second.
    let fx = measure(&[], &["fx"]).unwrap();
    assert!(silent(&fx, 0.0, 2.0) && silent(&fx, 3.0, 12.0), "{fx:#}");

    // Muting the bed leaves the scenes as they are mixed: the first louder than its voice,
    // for the effect in it.
    let muted = measure(&["bed"], &[]).unwrap();
    let c = chapters(&muted);
    assert!(c[0].unwrap() > voice + 0.3 && near(c[1], voice - 6.0), "{muted:#}");
    assert!(silent(&muted, 9.0, 12.0), "{muted:#}");
    let whole = measure(&[], &[]).unwrap();
    assert!(whole.get("silenced").is_none() && whole["silences"].as_array().unwrap().is_empty(), "{whole:#}");

    // A typo would measure the whole mix and look like an answer, so it is refused.
    let refused = |mute: &[&str], solo: &[&str]| measure(mute, solo).unwrap_err().to_string();
    assert!(refused(&[], &["nope"]).contains("no track `nope`"));
    assert!(refused(&["ux"], &[]).contains("does not play"));
    assert!(refused(&[], &["o1"]).contains("overlay track"));
    assert!(refused(&["va"], &["va"]).contains("both muted and soloed"));

    assert_eq!(serde_json::to_value(&*state.project.read().unwrap()).unwrap(), before, "the document is untouched, revision and all");
}
