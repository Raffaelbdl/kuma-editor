//! `measure_color` through melt: what a region is, and a match whose patch, applied, makes
//! the render look like the reference. Needs melt and ffmpeg; skips without them.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use kuma_daemon::color::{measure, ColorRequest};
use kuma_daemon::{AppState, Config, Libraries};
use serde_json::{json, Value};

/// A grey, a warm clip and a cool dark one, a second each.
const PROJECT: &str = r##"{
  "version": 11, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 64, "height": 36, "sample_rate": 48000,
  "bin": {
    "grey": { "name": "Grey", "source": { "color": "#808080" } },
    "warm": { "name": "Warm", "source": { "color": "#a08060" } },
    "cool": { "name": "Cool", "source": { "color": "#405068" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "clips": [
        { "id": "g", "start": 0, "end": 1, "bin": "grey" },
        { "id": "w", "start": 1, "end": 2, "bin": "warm" },
        { "id": "c", "start": 2, "end": 3, "bin": "cool", "effects": [ { "type": "grade", "saturation": 0.9 } ] }
      ] }
    ], "markers": [] } }
  }
}"##;

/// Whether melt and ffmpeg are here, as the daemon's other rendering tests ask.
fn renders() -> bool {
    kuma_overlay::media::ffprobe_available() && kuma_mlt::melt::melt_available()
}

fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        libraries: Libraries::new(dir.path().join("library")),
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    (Arc::new(AppState::load(config).unwrap()), dir)
}

fn run(state: &AppState, req: Value) -> anyhow::Result<Value> {
    let req: ColorRequest = serde_json::from_value(req).unwrap();
    measure(state, &req, &|_, _| {}, &AtomicBool::new(false))
}

fn apply(state: &AppState, patch: &Value) {
    let ops: Vec<json_patch::PatchOperation> = serde_json::from_value(patch.clone()).unwrap();
    state.apply_patch(&ops, None, "test", &[]).unwrap();
}

fn mean(stats: &Value) -> [f64; 3] {
    let m = &stats["mean"];
    [0, 1, 2].map(|i| m[i].as_f64().unwrap())
}

/// The middle of the 64×36 frame.
fn centre() -> Value {
    json!({ "x": 16, "y": 9, "w": 32, "h": 18 })
}

#[test]
fn a_region_reads_as_what_it_shows() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let out = run(&state, json!({ "times": [0.5], "region": centre() })).unwrap();
    let s = &out["stats"];
    assert_eq!(s["pixels"], 32 * 18);
    for v in mean(s) {
        assert!((v - 128.0 / 255.0).abs() < 3.0 / 255.0, "{s}");
    }
    assert!(s["balance"]["temperature"].as_f64().unwrap().abs() < 0.05, "a grey has no cast: {s}");
    // The warm clip reads warm, in the grade's own units.
    let warm = run(&state, json!({ "times": [1.5] })).unwrap();
    assert!(warm["stats"]["balance"]["temperature"].as_f64().unwrap() > 0.5, "{}", warm["stats"]);
    // Outside the frame is refused, not clipped.
    assert!(run(&state, json!({ "region": { "x": 60, "y": 0, "w": 10, "h": 10 } })).unwrap_err().to_string().contains("outside"));
}

/// The patch a match answers with, applied, renders the clip as the reference shows: the
/// loop the editor and an agent run.
#[test]
fn a_match_applied_renders_like_its_reference() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let before = serde_json::to_string(&*state.project.read().unwrap()).unwrap();

    // The warm clip made neutral: a new grade, added.
    let out = run(&state, json!({ "times": [1.5], "region": centre(), "match": { "clip": "w", "reference": { "neutral": true } } })).unwrap();
    let m = &out["match"];
    assert_eq!(m["reached"], true, "{m}");
    assert_eq!(m["patch"][0]["op"], "add");
    assert!(m["grade"]["temperature"].as_f64().unwrap() < -0.5, "{m}");
    assert_eq!(serde_json::to_string(&*state.project.read().unwrap()).unwrap(), before, "a measurement is not an edit");
    apply(&state, &m["patch"]);
    let now = run(&state, json!({ "times": [1.5], "region": centre() })).unwrap();
    let [r, g, b] = mean(&now["stats"]);
    assert!((r - g).abs() < 4.0 / 255.0 && (b - g).abs() < 4.0 / 255.0, "neutral after the grade: {}", now["stats"]);

    // The cool, dark clip matched to the grey one: its grade replaced, its saturation kept.
    let out = run(&state, json!({ "times": [2.5], "region": centre(), "match": { "clip": "c", "reference": { "t": 0.5 } } })).unwrap();
    let m = &out["match"];
    assert_eq!((m["patch"][0]["op"].as_str(), m["grade"]["saturation"].as_f64()), (Some("replace"), Some(0.9)), "{m}");
    assert_eq!(m["reached"], true, "{m}");
    apply(&state, &m["patch"]);
    let now = mean(&run(&state, json!({ "times": [2.5], "region": centre() })).unwrap()["stats"]);
    let grey = mean(&run(&state, json!({ "times": [0.5], "region": centre() })).unwrap()["stats"]);
    assert!((0..3).all(|i| (now[i] - grey[i]).abs() < 4.0 / 255.0), "{now:?} vs the reference {grey:?}");
    // What the match said the clip would look like is what the render shows: the grade's
    // maths and the LUT melt applies agree.
    let said = mean(&m["after"]);
    assert!((0..3).all(|i| (now[i] - said[i]).abs() < 3.0 / 255.0), "rendered {now:?}, predicted {said:?}");

    // A moment the clip is not on screen at is refused, saying when it is.
    let e = run(&state, json!({ "times": [0.5], "match": { "clip": "c", "reference": { "neutral": true } } })).unwrap_err().to_string();
    assert!(e.contains("from 2.00 to 3.00"), "{e}");
}
