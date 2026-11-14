//! Lining a mic up with its camera, and bringing an entry to a loudness: jobs that measure,
//! then make one edit. Need ffmpeg (and melt, for the loudness), and skip without them.

use std::sync::Arc;

use kuma_daemon::sound::{normalize, sync, NormalizeRequest, SyncPair, SyncRequest};
use kuma_daemon::{AppState, Config, Libraries};

const PROJECT: &str = r##"{
  "version": 12, "name": "t", "master": "main",
  "fps": { "num": 25, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "clips": [] },
      { "id": "a1", "kind": "audio", "clips": [] }
    ] } }
  }
}"##;

/// A scene 20 s long, something like speech (pink noise, its level rising and falling); the
/// mic heard all of it, the camera 12 s of it from 3.3 s, quieter and with its own hiss;
/// a second camera started 2 s before a second mic.
fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
    let ff = |args: &[&str]| kuma_overlay::media::ffmpeg(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    ff(&["-f", "lavfi", "-i", "anoisesrc=color=pink:duration=20:sample_rate=48000:seed=7,volume='0.15+0.6*abs(sin(2*PI*0.9*t))':eval=frame", &path("scene.wav")]);
    ff(&["-i", &path("scene.wav"), "-c", "copy", &path("mic.wav")]);
    ff(&["-ss", "2", "-i", &path("scene.wav"), &path("mic2.wav")]);
    ff(&["-f", "lavfi", "-i", "anoisesrc=color=pink:duration=20:sample_rate=48000:seed=99", &path("other.wav")]);
    let camera = |from: &str, out: &str| {
        ff(&[
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=12",
            "-ss", from, "-t", "12", "-i", &path("scene.wav"),
            "-f", "lavfi", "-i", "anoisesrc=color=white:amplitude=0.03:duration=12:sample_rate=48000:seed=3",
            "-filter_complex", "[1:a]volume=0.3[a];[a][2:a]amix=inputs=2:duration=first[o]",
            "-map", "0:v", "-map", "[o]", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", &path(out),
        ]);
    };
    camera("3.3", "cam.mp4");
    camera("0", "cam2.mp4");
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        libraries: Libraries::new(dir.path().join("library")),
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    let state = Arc::new(AppState::load(config).unwrap());
    let files: Vec<std::path::PathBuf> = ["cam.mp4", "cam2.mp4", "mic.wav", "mic2.wav", "other.wav"].map(std::path::PathBuf::from).to_vec();
    kuma_daemon::media::add(&state, &files, Default::default(), None, "test").unwrap();
    (state, dir)
}

fn place_cameras(state: &AppState) {
    let clips = serde_json::json!([
        { "id": "cam", "start": 2, "end": 12, "bin": "cam", "in": 1 },
        { "id": "cam2", "start": 14, "end": 24, "bin": "cam2" }
    ]);
    let ops: Vec<json_patch::PatchOperation> = serde_json::from_value(serde_json::json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips", "value": clips }])).unwrap();
    state.apply_patch(&ops, None, "test", &[]).unwrap();
}

/// Whether the tools these tests need are here; says so when not.
fn tools(melt: bool) -> bool {
    let here = kuma_overlay::media::ffprobe_available() && (!melt || kuma_mlt::melt::melt_available());
    if !here {
        eprintln!("ffmpeg{} not available; skipping", if melt { "/melt" } else { "" });
    }
    here
}

fn num(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("a number: {v}"))
}

fn level(state: &AppState, bin: &str, target: f64) -> anyhow::Result<serde_json::Value> {
    normalize(state, &NormalizeRequest { bin: bin.into(), target, origin: "test".into() }, &|_, _| {}, &Default::default())
}

fn run(state: &AppState, pairs: &[(&str, &str)], dry_run: bool) -> anyhow::Result<serde_json::Value> {
    let pairs = pairs.iter().map(|(c, a)| SyncPair { clip: c.to_string(), audio: a.to_string(), track: None }).collect();
    sync(state, &SyncRequest { pairs, dry_run, origin: "test".into() }, &|_, _| {}, &Default::default())
}

#[test]
fn a_mic_is_lined_up_under_its_camera_muted_and_grouped() {
    if !tools(false) {
        return;
    }
    let (state, _dir) = fixture();
    place_cameras(&state);

    // Measuring changes nothing.
    let rev = state.project.read().unwrap().revision;
    let dry = run(&state, &[("cam", "mic"), ("cam2", "mic2")], true).unwrap();
    assert_eq!(state.project.read().unwrap().revision, rev, "{dry:#}");
    assert!((num(&dry["pairs"][0]["offset"]) - 3.3).abs() < 0.001, "the mic started 3.3 s first: {dry:#}");
    assert!((num(&dry["pairs"][1]["offset"]) + 2.0).abs() < 0.001, "the second mic 2 s after its camera: {dry:#}");

    let out = run(&state, &[("cam", "mic"), ("cam2", "mic2")], false).unwrap();
    assert_eq!(out["applied"], true, "{out:#}");
    let p = state.project.read().unwrap().clone();
    let (_, _, cam) = p.find_clip("cam").unwrap();
    assert!(cam.muted && cam.group == "sync-cam", "the camera's own sound is muted and grouped: {cam:?}");
    let mic_id = out["pairs"][0]["audio_clip"].as_str().unwrap();
    let (_, track, mic) = p.find_clip(mic_id).unwrap();
    assert_eq!((track.id.as_str(), mic.bin.as_str(), mic.group.as_str()), ("a1", "mic", "sync-cam"));
    // The camera is at 1 s at its clip's start, so the mic is at 4.3 s, over the same stretch.
    assert!((mic.start - 2.0).abs() < 1e-9 && (mic.end - 12.0).abs() < 1e-9 && (mic.source_in - 4.3).abs() < 0.001, "{mic:?}");
    assert!(num(&out["pairs"][0]["error_ms"]).abs() <= 20.0, "under half a frame at 25 fps: {out:#}");
    // The second mic started later, so its clip starts later, from its own beginning.
    let (_, _, mic2) = p.find_clip(out["pairs"][1]["audio_clip"].as_str().unwrap()).unwrap();
    assert!((mic2.start - 16.0).abs() < 1e-9 && mic2.source_in.abs() < 0.02 && (mic2.end - 24.0).abs() < 1e-9, "{mic2:?}");
    // Two pairs on one track, side by side: the edit kept them apart.
    assert_eq!(track.clips.len(), 2, "{track:?}");

    // Again: the mic is moved where it belongs, not placed a second time.
    let again = run(&state, &[("cam", "mic")], false).unwrap();
    assert_eq!(again["pairs"][0]["moved"], true, "{again:#}");
    assert_eq!(state.project.read().unwrap().all_clips().filter(|(_, _, c)| c.bin == "mic").count(), 1);
}

#[test]
fn a_recording_of_something_else_is_refused() {
    if !tools(false) {
        return;
    }
    let (state, _dir) = fixture();
    place_cameras(&state);
    let rev = state.project.read().unwrap().revision;
    let err = run(&state, &[("cam", "other")], false).unwrap_err().to_string();
    assert!(err.contains("no pair lined up") && err.contains("do not match"), "{err}");
    // One that matches still lands beside one that does not, which says why it did not.
    let out = run(&state, &[("cam", "other"), ("cam2", "mic2")], false).unwrap();
    assert!(out["pairs"][0]["skipped"].is_string() && out["pairs"][1]["audio_clip"].is_string(), "{out:#}");
    assert_eq!(state.project.read().unwrap().revision, rev + 1);
    // Refused before anything is decoded.
    assert!(run(&state, &[("nope", "mic")], false).unwrap_err().to_string().contains("no clip `nope`"));
    assert!(run(&state, &[("cam", "cam")], false).unwrap_err().to_string().contains("already plays"));
}

#[test]
fn an_entry_is_brought_to_a_loudness_through_its_own_chain() {
    if !tools(true) {
        return;
    }
    let (state, _dir) = fixture();
    let set_chain = |chain: serde_json::Value| {
        let ops: Vec<json_patch::PatchOperation> = serde_json::from_value(serde_json::json!([
            { "op": "add", "path": "/bin/mic/effects", "value": chain }, { "op": "add", "path": "/bin/mic/volume", "value": 1.0 }
        ])).unwrap();
        state.apply_patch(&ops, None, "test", &[]).unwrap();
    };

    // The voice-over macros: a high-pass, the gain, a limiter. Linear up to the limiter, so
    // the second pass confirms the first.
    set_chain(serde_json::json!([{ "type": "high_pass", "freq": 80 }, { "type": "limiter", "ceiling": -2, "attack": 1, "release": 20 }]));
    let out = level(&state, "mic", -20.0).unwrap();
    assert!((num(&out["after"]) + 20.0).abs() <= 0.1 && out["passes"].as_u64() <= Some(3), "{out:#}");
    assert!(num(&out["true_peak"]) < -0.5, "the limiter after the gain holds the peaks: {out:#}");
    let volume = state.project.read().unwrap().bin["mic"].volume;
    assert!((volume - num(&out["volume"])).abs() < 1e-9 && volume != 1.0, "the entry's volume is the edit: {volume}");

    // A compressor before the gain, the gain as its makeup: still linear, still two passes.
    set_chain(serde_json::json!([{ "type": "compressor", "threshold": -30, "ratio": 3 }, { "type": "volume" }, { "type": "limiter", "ceiling": -2 }]));
    let out = level(&state, "mic", -18.0).unwrap();
    assert!((num(&out["after"]) + 18.0).abs() <= 0.15 && out["passes"].as_u64() <= Some(3), "{out:#}");

    // The gain in front of the compressor, which answers a dB with a third of one: the
    // secant gets there anyway...
    set_chain(serde_json::json!([{ "type": "compressor", "threshold": -30, "ratio": 3 }, { "type": "limiter", "ceiling": -2 }]));
    let out = level(&state, "mic", -22.0).unwrap();
    assert!((num(&out["after"]) + 22.0).abs() <= 0.15, "{out:#}");
    // ...short of what the limiter after it lets through, where more gain changes nothing:
    // refused as it is, with no edit, rather than chased with ever more gain.
    let rev = state.project.read().unwrap().revision;
    let err = level(&state, "mic", -16.0).unwrap_err();
    assert!(err.to_string().contains("holds it there"), "{err}");
    assert_eq!(state.project.read().unwrap().revision, rev);

    let err = level(&state, "main", -20.0).unwrap_err();
    assert!(err.to_string().contains("not a media file"), "{err}");
}
