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

/// A project of one colour clip: enough to compile, and it needs neither media on disk nor
/// ffmpeg, melt or hyperframes to get through a preview compile.
const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "navy": { "name": "Navy", "source": { "color": "#102030" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "navy", "in": 0,
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
    let config = Config {
        project_file,
        repo_root: repo,
        library_dirs: vec![dir.path().join("empty-library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    Some((Arc::new(AppState::load(config).unwrap()), dir))
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
