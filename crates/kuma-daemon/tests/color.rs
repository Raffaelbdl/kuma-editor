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
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The starter set, for its camera logs.
    let libraries = Libraries { starter: Some(repo_root.join("library")), ..Libraries::new(dir.path().join("library")) };
    let config = Config { project_file, repo_root, libraries, bind: "127.0.0.1:0".parse().unwrap() };
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

/// What a frame's middle pixel is, read off its PNG.
fn middle(png: &std::path::Path) -> [u8; 3] {
    let probe = kuma_overlay::probe(png).unwrap();
    let (w, h) = (probe.meta.width.unwrap() as usize, probe.meta.height.unwrap() as usize);
    let bytes = kuma_overlay::media::rgb24(png).unwrap();
    let i = ((h / 2) * w + w / 2) * 3;
    [bytes[i], bytes[i + 1], bytes[i + 2]]
}

fn sample(state: &AppState, mut req: Value) -> anyhow::Result<Value> {
    req["t0"] = json!(0); // the request's own field; `times` say when
    kuma_daemon::jobs::sample(state, &serde_json::from_value(req).unwrap(), &|_, _| {}, None)
}

/// Sampling with `before` pairs each moment with the same moment of another version: what
/// a grade does (`without`), and what an edit did (`revision`) — the sheet a row a moment,
/// before on the left. Neither version is ever the document.
#[test]
fn a_sample_compares_before_and_after() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let doc = serde_json::to_string(&*state.project.read().unwrap()).unwrap();

    // Without its grade, the cool clip at two moments: a 2×2 sheet of 480-wide frames.
    let out = sample(&state, json!({ "times": [2.2, 2.8], "tile": true, "before": { "without": { "kinds": ["grade"] } } })).unwrap();
    let (before, after) = (out["before"].as_array().unwrap(), out["samples"].as_array().unwrap());
    assert_eq!((before.len(), after.len()), (2, 2));
    let px = |s: &Value| middle(std::path::Path::new(s["path"].as_str().unwrap()));
    // The grade takes a tenth of the saturation: the after is greyer, not the same picture.
    let (b, a) = (px(&before[0]), px(&after[0]));
    assert!(b[2] as i32 - b[0] as i32 > a[2] as i32 - a[0] as i32, "before {b:?} should be bluer than after {a:?}");
    let sheet = kuma_overlay::probe(std::path::Path::new(out["sheet"].as_str().unwrap())).unwrap();
    assert_eq!((sheet.meta.width, sheet.meta.height), (Some(960), Some(540)), "two columns, two rows");
    assert_eq!(serde_json::to_string(&*state.project.read().unwrap()).unwrap(), doc, "a comparison is not an edit");

    // After an edit, the revision before it is what the picture was.
    let r = state.project.read().unwrap().revision;
    apply(&state, &json!([{ "op": "replace", "path": "/bin/grey/source/color", "value": "#ff0000" }]));
    let out = sample(&state, json!({ "times": [0.5], "before": { "revision": r } })).unwrap();
    let (b, a) = (px(&out["before"][0]), px(&out["samples"][0]));
    assert!(b[0].abs_diff(b[1]) < 4 && a[0] > 240 && a[1] < 15, "before grey {b:?}, after red {a:?}");

    // Asking for nothing to compare with is said, not answered with two identical pictures.
    for (before, says) in [
        (json!({ "without": { "kinds": ["grad"] } }), "takes nothing out"),
        (json!({ "without": { "clips": ["nope"] } }), "no clip `nope`"),
        (json!({ "revision": 99999 }), "not in the history"),
    ] {
        let e = sample(&state, json!({ "times": [0.5], "before": before })).unwrap_err().to_string();
        assert!(e.contains(says), "{e}");
    }
}

/// `measure_color` with `before` answers the same measurement of the other version, so what
/// an edit did to a region is numbers as well as pictures.
#[test]
fn a_measurement_compares_before_and_after() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let out = run(&state, json!({ "times": [2.5], "region": centre(), "before": { "without": { "clips": ["c"] } } })).unwrap();
    let (before, after) = (&out["before"]["stats"], &out["stats"]);
    assert!(before["saturation"].as_f64().unwrap() > after["saturation"].as_f64().unwrap(), "the grade desaturates: {before} vs {after}");
    assert_eq!(out["before"]["samples"].as_array().unwrap().len(), 1);
}

/// The table the editor's live preview applies is the compiler's maths, a byte a channel:
/// a neutral grade is the identity, and a grade's table is its `.cube` at the same points.
#[test]
fn a_grade_table_is_the_grade_at_every_point() {
    let (state, _dir) = fixture();
    let n = 5;
    let identity = kuma_daemon::color::table(&state, &kuma_core::Grade::default(), n).unwrap();
    assert_eq!(identity.len(), n * n * n * 3);
    let at = |i: usize| (i as f64 / (n - 1) as f64 * 255.0).round() as u8;
    for (k, px) in identity.chunks(3).enumerate() {
        assert_eq!(px, [at(k % n), at(k / n % n), at(k / (n * n))], "point {k}");
    }
    let warm = kuma_core::Grade { temperature: 0.5, ..Default::default() };
    let table = kuma_daemon::color::table(&state, &warm, n).unwrap();
    let grading = kuma_core::grade::Grading::new(&warm, Default::default());
    let want = grading.apply([0.5, 0.5, 0.5]).map(|v| (v * 255.0).round() as u8);
    let mid = (2 * n * n + 2 * n + 2) * 3;
    assert_eq!(&table[mid..mid + 3], &want);
    assert!(kuma_daemon::color::table(&state, &warm, 1).is_err() && kuma_daemon::color::table(&state, &warm, 999).is_err());
}

/// `version` samples another version of the document instead of it: the editor's before
/// view, which is the same frame a comparison's `before` is.
#[test]
fn a_sampled_version_is_the_before_of_a_comparison() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    let without = json!({ "without": { "kinds": ["grade"], "clips": ["c"] } });
    let version = sample(&state, json!({ "times": [2.5], "version": without })).unwrap();
    let compared = sample(&state, json!({ "times": [2.5], "before": without })).unwrap();
    assert_eq!(version["samples"][0]["path"], compared["before"][0]["path"], "the same frame, from the frame cache");
    assert_ne!(version["samples"][0]["path"], compared["samples"][0]["path"]);
}

/// A grey card shot in S-Log3 (code 420, which is #686868 once the limited range is expanded)
/// renders as mid grey once the grade names the log: the conversion is in the render, not
/// only in the grade's maths.
#[test]
fn a_log_grey_card_renders_mid_grey() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    apply(&state, &json!([
        { "op": "add", "path": "/bin/slog", "value": { "name": "S-Log3 card", "source": { "color": "#686868" } } },
        { "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/bin", "value": "slog" },
    ]));
    let flat = run(&state, json!({ "times": [0.5], "region": centre() })).unwrap();
    assert!(flat["stats"]["exposure"].as_f64().unwrap() < -0.5, "log grey is darker than mid grey on screen: {}", flat["stats"]);
    apply(&state, &json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/0/effects/-", "value": { "type": "grade", "log": "s-log3-cine" } }]));
    let s = &run(&state, json!({ "times": [0.5], "region": centre() })).unwrap()["stats"];
    assert!(s["exposure"].as_f64().unwrap().abs() < 0.1, "converted, it is mid grey: {s}");
    assert!(s["balance"]["temperature"].as_f64().unwrap().abs() < 0.05 && s["balance"]["tint"].as_f64().unwrap().abs() < 0.05, "and still grey: {s}");
}

/// A masked grade renders inside its window and nowhere else: the window darker by its
/// exposure, the corners as they were — and a second, whole-picture grade on the same clip
/// is the one a match reads and writes.
#[test]
fn a_masked_grade_renders_inside_its_window_only() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    apply(&state, &json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/0/effects", "value": [
        { "type": "grade", "exposure": -2, "mask": { "shape": "rectangle", "x": 16, "y": 9, "w": 32, "h": 18 } }
    ] }]));
    let inside = run(&state, json!({ "times": [0.5], "region": { "x": 24, "y": 13, "w": 16, "h": 10 } })).unwrap();
    let corner = run(&state, json!({ "times": [0.5], "region": { "x": 0, "y": 0, "w": 8, "h": 6 } })).unwrap();
    let ev = |s: &Value| s["stats"]["exposure"].as_f64().unwrap();
    assert!((ev(&inside) - ev(&corner) + 2.0).abs() < 0.15, "two stops darker inside: {} against {}", inside["stats"], corner["stats"]);
    assert!((mean(&corner["stats"])[0] - 128.0 / 255.0).abs() < 3.0 / 255.0, "the corner untouched: {}", corner["stats"]);
    // The window alone taken out, by its index: what it did, as a before.
    let compared = run(&state, json!({ "times": [0.5], "region": { "x": 24, "y": 13, "w": 16, "h": 10 }, "before": { "without": { "clips": ["g"], "effects": [0] } } })).unwrap();
    assert!((compared["before"]["stats"]["exposure"].as_f64().unwrap() - ev(&corner)).abs() < 0.05, "{compared}");
    for (without, says) in [
        (json!({ "clips": ["g"], "effects": [3] }), "none at 3"),
        (json!({ "effects": [0] }), "effects of one clip"),
    ] {
        let e = run(&state, json!({ "times": [0.5], "before": { "without": without } })).unwrap_err().to_string();
        assert!(e.contains(says), "{e}");
    }
    // A match adds the whole-picture grade rather than rewriting the window.
    let out = run(&state, json!({ "times": [0.5], "region": { "x": 0, "y": 0, "w": 8, "h": 6 }, "match": { "clip": "g", "reference": { "neutral": true } } })).unwrap();
    assert_eq!(out["match"]["patch"][0]["op"], "add", "{}", out["match"]);
}

/// A grade's light keyed: two stops up over the clip renders two stops up at its end, and a
/// match on it answers with a key at the moment measured rather than a static value the
/// keys would override.
#[test]
fn a_keyed_grade_moves_its_light_in_the_render() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    apply(&state, &json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/0/effects", "value": [
        { "type": "grade", "keyframes": [{ "t": 0, "exposure": -1 }, { "t": 0.5, "exposure": 1 }] }
    ] }]));
    let ev = |t: f64| run(&state, json!({ "times": [t], "region": centre() })).unwrap()["stats"]["exposure"].as_f64().unwrap();
    let (start, end) = (ev(0.0), ev(0.9));
    assert!((end - start - 2.0).abs() < 0.15, "two stops between the keys: {start} → {end}");
    let out = run(&state, json!({ "times": [0.8], "region": centre(), "match": { "clip": "g", "reference": { "neutral": true } } })).unwrap();
    let keys = out["match"]["grade"]["keyframes"].as_array().unwrap().clone();
    assert_eq!(keys.len(), 3, "a key added at 0.8 s: {keys:?}");
    assert!((keys[2]["t"].as_f64().unwrap() - 0.8).abs() < 0.02 && keys[2]["exposure"].is_number(), "{keys:?}");
}

/// A mask keyed to move goes where its keys say, counted from its clip's start — on a clip
/// that starts a second into the sequence, so a key read from the sequence's start would
/// show here.
#[test]
fn a_keyed_mask_moves_with_its_clip() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, _dir) = fixture();
    apply(&state, &json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/1/effects", "value": [
        { "type": "grade", "exposure": -3, "mask": { "shape": "rectangle", "x": 0, "y": 0, "w": 16, "h": 36,
          "keyframes": [{ "t": 0.1 }, { "t": 0.8, "x": 48 }] } }
    ] }]));
    let ev = |t: f64, x: u32| run(&state, json!({ "times": [t], "region": { "x": x, "y": 10, "w": 8, "h": 16 } })).unwrap()["stats"]["exposure"].as_f64().unwrap();
    let (left0, right0, left1, right1) = (ev(1.05, 4), ev(1.05, 52), ev(1.9, 4), ev(1.9, 52));
    assert!(left0 < right0 - 2.0, "the window on the left at first: {left0} {right0}");
    assert!(right1 < left1 - 2.0, "and on the right by the end: {left1} {right1}");
}

/// An entry's own grade is what every placement starts from: an S-Log3 grey card as a file,
/// its entry naming the log, renders mid grey wherever it is placed — and a clip's grade
/// then acts on that.
#[test]
fn an_entrys_grade_is_what_its_clips_start_from() {
    if !renders() {
        eprintln!("melt/ffmpeg not available; skipping");
        return;
    }
    let (state, dir) = fixture();
    let card = dir.path().join("card.png");
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "color=c=0x686868:s=64x36", "-frames:v", "1", "-y"])
        .arg(&card)
        .status()
        .unwrap();
    assert!(made.success());
    apply(&state, &json!([
        { "op": "add", "path": "/bin/card", "value": { "name": "S-Log3 card", "source": { "path": "card.png", "kind": "image" },
          "effects": [{ "type": "grade", "log": "s-log3-cine" }] } },
        { "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/bin", "value": "card" },
    ]));
    let ev = || run(&state, json!({ "times": [0.5], "region": centre() })).unwrap()["stats"]["exposure"].as_f64().unwrap();
    assert!(ev().abs() < 0.1, "the entry's log converted: {}", ev());
    apply(&state, &json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/0/effects", "value": [{ "type": "grade", "exposure": 1 }] }]));
    assert!((ev() - 1.0).abs() < 0.15, "and the clip's grade on that: {}", ev());
}
