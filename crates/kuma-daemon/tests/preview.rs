//! The preview's reload contract, against a real daemon and a real engine process.
//!
//! What is being pinned is that *the client never has to know* what the compiler depends
//! on: it asks on every edit, and the daemon reloads the engine only when the compiled
//! document actually differs. Skips (with a message) when the engine is not built, the way
//! the render tests skip without melt.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kuma_daemon::preview;
use kuma_daemon::{AppState, Config};
use serde_json::{json, Value};

/// A project of one colour clip and one sound: enough to compile, and it needs neither
/// ffmpeg, melt nor hyperframes to get through a preview compile. The sound is two seconds
/// of silence that [`wav`] writes, since an edit is validated against files on disk.
const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "navy": { "name": "Navy", "source": { "color": "#102030" } },
    "tone": { "name": "Tone", "source": { "path": "tone.wav", "kind": "audio" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "navy", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] },
      { "id": "a1", "kind": "audio", "name": "A", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "t1", "start": 0, "end": 2, "bin": "tone", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

/// `None` when this machine cannot run the engine, which is the signal to skip.
fn fixture() -> Option<(Arc<AppState>, tempfile::TempDir)> {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !repo.join("engine/build/kuma-preview").is_file() {
        eprintln!("preview engine not built; skipping");
        return None;
    }
    let headless = std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    if headless && !preview::has_bin("xvfb-run") {
        eprintln!("no display and no xvfb-run; skipping");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    std::fs::write(dir.path().join("tone.wav"), wav(2)).unwrap();
    let config = Config {
        project_file,
        repo_root: repo,
        library_dirs: vec![dir.path().join("empty-library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    Some((Arc::new(AppState::load(config).unwrap()), dir))
}

/// `secs` of 48 kHz mono 16-bit silence as a WAV file.
fn wav(secs: u32) -> Vec<u8> {
    let data = 48_000 * 2 * secs;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&48_000u32.to_le_bytes());
    w.extend_from_slice(&96_000u32.to_le_bytes()); // bytes per second
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(w.len() + data as usize, 0);
    w
}

fn patch(state: &AppState, path: &str, value: Value) {
    let ops: json_patch::Patch = serde_json::from_value(json!([{ "op": "replace", "path": path, "value": value }])).unwrap();
    state.apply_patch(&ops.0, None, "test").expect("patch accepted");
}

/// The whole point of moving the decision into the daemon: an edit that cannot reach the
/// compiled document must not cost the engine a graph rebuild, and the client does not
/// have to know which edits those are.
#[tokio::test]
async fn the_engine_is_reloaded_only_when_the_compiled_document_changes() {
    let Some((state, _dir)) = fixture() else { return };
    let clip = "/bin/main/source/tracks/0/clips/0";

    assert!(preview::reload(&state).await.unwrap().changed, "a fresh engine holds nothing, so it must be given the timeline");
    assert!(!preview::reload(&state).await.unwrap().changed, "nothing changed in between");

    // `label` is the editor's own annotation; it never reaches the XML.
    patch(&state, &format!("{clip}/label"), json!("renamed"));
    assert!(!preview::reload(&state).await.unwrap().changed, "a label is not in the compiled document");

    // Moving a cut is.
    patch(&state, &format!("{clip}/end"), json!(3.0));
    assert!(preview::reload(&state).await.unwrap().changed, "the clip's out point moved");
    assert!(!preview::reload(&state).await.unwrap().changed, "and then settles");

    // The Reload button hands it over anyway: a file replaced on disk is not in the document.
    let forced = preview::reload_forced(&state).await.unwrap();
    assert!(forced.video && forced.audio, "both halves, unchanged or not");
    assert_eq!((forced.baked, forced.baking.len()), (0, 0), "nothing nested, nothing to bake");
}

/// The picture and the sound are two graphs, and an edit reloads only the one it reaches:
/// a colour is picture, a volume is sound, a cut is both.
#[tokio::test]
async fn an_edit_reloads_only_the_graph_it_changes() {
    let Some((state, _dir)) = fixture() else { return };
    let halves = |r: preview::Reloaded| (r.video, r.audio);
    assert_eq!(halves(preview::reload(&state).await.unwrap()), (true, true), "a fresh engine needs both");

    patch(&state, "/bin/navy/source/color", json!("#304050"));
    assert_eq!(halves(preview::reload(&state).await.unwrap()), (true, false), "a colour is only picture");

    patch(&state, "/bin/main/source/tracks/1/clips/0/volume", json!(0.5));
    assert_eq!(halves(preview::reload(&state).await.unwrap()), (false, true), "a volume is only sound");

    patch(&state, "/bin/main/source/tracks/0/clips/0/end", json!(3.0));
    assert_eq!(halves(preview::reload(&state).await.unwrap()), (true, true), "the timeline got longer");
}

/// A burst of edits — an agent applying patches, undo held down — must not queue one
/// compile per request. They collapse into the pass already running, plus one more to pick
/// up whatever arrived while it worked.
#[tokio::test]
async fn concurrent_reloads_collapse_into_one_pass() {
    let Some((state, _dir)) = fixture() else { return };
    preview::reload(&state).await.unwrap();

    let before = state.preview.compiles.load(Ordering::SeqCst);
    let burst = 32;
    let asked: Vec<_> = (0..burst)
        .map(|_| {
            let st = state.clone();
            tokio::spawn(async move { preview::reload(&st).await.map(|r| r.changed) })
        })
        .collect();
    for task in asked {
        task.await.unwrap().expect("every caller gets an answer, not just the one that did the work");
    }
    // Two is the shape of the mechanism: the pass that was running, plus one more for
    // everything that queued behind it. The headroom is for scheduling, not for slippage —
    // one compile per request would be 32.
    let compiles = state.preview.compiles.load(Ordering::SeqCst) - before;
    assert!(compiles <= 4, "{burst} reloads cost {compiles} compiles; they are not being coalesced");
}

/// A restarted engine is holding nothing, whatever the daemon last compiled — so the
/// stored document hash cannot be what decides, and `changed` counts loads instead.
#[tokio::test]
async fn a_restarted_engine_is_given_the_timeline_again() {
    let Some((state, _dir)) = fixture() else { return };
    preview::reload(&state).await.unwrap();
    assert!(!preview::reload(&state).await.unwrap().changed);

    // Drop the engine the way its process dying does: the next `ensure` starts a new one.
    state.preview.forget();
    assert!(preview::reload(&state).await.unwrap().changed, "the new process was never given a timeline");
}

/// A reload never waits on hyperframes. Opening a sequence whose nested overlays were never
/// baked used to hang the request for as long as baking every one of them took; now the
/// compile leaves them out, says so, and hands them to one background `preview-bake` job,
/// whose end the client answers with another reload.
#[tokio::test]
async fn a_reload_leaves_unbaked_nested_overlays_to_a_background_job() {
    let Some((state, dir)) = fixture() else { return };
    let folder = dir.path().join("overlays/tag");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("overlay.json"), r#"{ "name": "Tag" }"#).unwrap();
    std::fs::write(folder.join("index.html"), "<html><body data-composition-id=\"root\" data-no-timeline></body></html>").unwrap();
    let ops: json_patch::Patch = serde_json::from_value(json!([
        { "op": "add", "path": "/bin/tag", "value": { "name": "Tag", "source": { "overlay": "overlays/tag", "vars": {} } } },
        { "op": "add", "path": "/bin/inner", "value": { "name": "Inner", "source": { "tracks": [
            { "id": "o1", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
              "clips": [ { "id": "tag1", "start": 0, "end": 1, "bin": "tag" } ] } ], "markers": [] } } },
        { "op": "add", "path": "/bin/main/source/tracks/0/clips/-", "value": { "id": "nest", "start": 2, "end": 3, "bin": "inner" } }
    ]))
    .unwrap();
    state.apply_patch(&ops.0, None, "test").expect("patch accepted");

    let r = preview::reload(&state).await.unwrap();
    assert_eq!(r.baking, vec!["tag1".to_string()], "left out of the picture, and said");
    let bakes = || state.jobs.list(Some("preview-bake")).len();
    assert_eq!(bakes(), 1, "handed to a background job");
    preview::reload(&state).await.unwrap();
    assert_eq!(bakes(), 1, "one job at a time, and not one per compile");
}
