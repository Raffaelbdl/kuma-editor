//! Overlay clips bake side by side, each its own one-worker renderer, and clips that come
//! to one key bake once. A stand-in renderer logs when each run starts and ends, so this
//! needs no hyperframes and no browser.

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use kuma_daemon::jobs::{self, OverlayScope};
use kuma_daemon::{AppState, Config, Libraries};

/// `c1` and `c2` place one entry at one length, so one key; `c3` places another.
const PROJECT: &str = r##"{
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 6, "den": 1 }, "width": 64, "height": 36, "sample_rate": 48000,
  "bin": {
    "a": { "name": "A", "source": { "overlay": "overlays/probe", "vars": { "label": "a" } } },
    "b": { "name": "B", "source": { "overlay": "overlays/probe", "vars": { "label": "b" } } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "o1", "kind": "overlay", "name": "O1", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 1, "bin": "a", "in": 0, "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" },
                   { "id": "c2", "start": 1, "end": 2, "bin": "a", "in": 0, "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] },
      { "id": "o2", "kind": "overlay", "name": "O2", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c3", "start": 0, "end": 1, "bin": "b", "in": 0, "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

/// The project on disk, a baker whose renderer is the stand-in, and the stand-in's log:
/// one `start <src>` and one `end <src>` line per run, the source naming the key.
fn fixture(concurrency: usize) -> (AppState, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let probe = dir.path().join("overlays/probe");
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::write(probe.join("overlay.json"), r#"{ "name": "Probe", "vars": { "label": { "type": "string", "default": "" } } }"#).unwrap();
    std::fs::write(probe.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    // `render <src> --format png-sequence -o <dir> …`: a script on the page throws, which the
    // real one prints and survives; slow enough for runs to overlap, then six empty frames in `$6`.
    let log = dir.path().join("runs");
    let fake = dir.path().join("hyperframes");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"start $2\" >> '{0}'\necho '[Browser:PAGEERROR] boom' >&2\nsleep 1\nmkdir -p \"$6\"\nfor i in 1 2 3 4 5 6; do : > \"$6/f$i.png\"; done\necho \"end $2\" >> '{0}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        libraries: Libraries::new(dir.path().join("library")),
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    let mut state = AppState::load(config).unwrap();
    state.baker.hyperframes = fake;
    state.baker.version = Some("stand-in".into());
    state.baker.concurrency = concurrency;
    (state, log, dir)
}

#[test]
fn clips_bake_side_by_side_and_a_shared_key_bakes_once() {
    let (state, log, _dir) = fixture(2);
    let project = state.project.read().unwrap().clone();
    let said = Mutex::new(Vec::new());
    let (bakes, warnings) = jobs::bake_for(&state, &project, "main", OverlayScope::All, None, &|_, m| said.lock().unwrap().push(m.to_string()), None).expect("bake");

    assert_eq!(bakes.len(), 3, "every clip has its bake: {bakes:?}");
    assert_eq!(bakes["c1"].resource, bakes["c2"].resource, "one key, one bake");
    assert_ne!(bakes["c1"].resource, bakes["c3"].resource);
    // What the page threw is said once per bake, on the entry, naming the clips it is under.
    let said_of: Vec<(&str, &str)> = warnings.iter().map(|w| (w.path.as_str(), w.message.as_str())).collect();
    assert_eq!(said_of, [("/bin/a", "overlay clip `c1`, `c2` a script on the page threw while it baked: boom"), ("/bin/b", "overlay clip `c3` a script on the page threw while it baked: boom")]);
    let lines: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
    assert_eq!(lines.len(), 4, "two renders, not three: {lines:?}");
    // With two workers, `c2` sharing `c1`'s key must not hold the second one: `c3` starts
    // beside `c1`, before either ends.
    assert!(lines[..2].iter().all(|l| l.starts_with("start")), "the two renders overlapped: {lines:?}");
    let said = said.into_inner().unwrap();
    assert!(said.iter().any(|m| m.contains("c1 (") && m.contains("c3 (")), "progress names both running bakes: {said:?}");
}

#[test]
fn a_cancelled_bake_starts_nothing() {
    let (state, log, _dir) = fixture(2);
    let project = state.project.read().unwrap().clone();
    let cancel = AtomicBool::new(true);
    let err = jobs::bake_for(&state, &project, "main", OverlayScope::All, None, &|_, _| {}, Some(&cancel)).expect_err("cancelled");
    assert_eq!(err.to_string(), "cancelled");
    assert!(!log.exists(), "no renderer ran");
}

/// A cancel reaches the renderers already running: they are killed rather than left to
/// finish a bake nobody wants, and nothing half-made reaches the cache.
#[test]
fn a_cancel_stops_the_bakes_that_are_running() {
    let (state, log, _dir) = fixture(2);
    let project = state.project.read().unwrap().clone();
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let outcome = std::thread::scope(|s| {
        s.spawn(|| {
            while !log.exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        jobs::bake_for(&state, &project, "main", OverlayScope::All, None, &|_, _| {}, Some(&cancel))
    });
    assert!(outcome.is_err(), "a cancelled bake is not a bake");
    assert!(started.elapsed() < std::time::Duration::from_millis(900), "the renderer was waited for: {:?}", started.elapsed());
    let lines = std::fs::read_to_string(&log).unwrap();
    assert!(lines.contains("start") && !lines.contains("end"), "a renderer ran to its end: {lines}");
    let (cached, missing) = jobs::cached_bakes(&state, &project, "main");
    assert!(cached.is_empty() && missing.len() == 3, "nothing reached the cache: {cached:?}");
}
