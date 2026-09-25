//! `kuma compile` prints the graph a render would use, overlays included: every bake the
//! cache already holds, and — asked to bake — the rest. It used to leave every overlay out,
//! which is the one thing someone debugging a composite needs to see. A stand-in renderer
//! does the baking, so this needs no hyperframes and no browser.

use kuma_daemon::jobs::{self, OverlayScope};
use kuma_daemon::{AppState, Config};

const PROJECT: &str = r##"{
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 6, "den": 1 }, "width": 64, "height": 36, "sample_rate": 48000,
  "bin": {
    "tag": { "name": "Tag", "source": { "overlay": "overlays/probe", "vars": {} } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "o1", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "o1c1", "start": 0, "end": 1, "bin": "tag", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

#[test]
fn a_compile_includes_the_overlays_already_baked() {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let probe = dir.path().join("overlays/probe");
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::write(probe.join("overlay.json"), r#"{ "name": "Probe" }"#).unwrap();
    std::fs::write(probe.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    // `render <src> --format png-sequence -o <dir> …`: six empty frames in `$6`.
    let fake = dir.path().join("hyperframes");
    std::fs::write(&fake, "#!/bin/sh\nmkdir -p \"$6\"\nfor i in 1 2 3 4 5 6; do : > \"$6/f$i.png\"; done\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    let mut state = AppState::load(config).unwrap();
    state.baker.hyperframes = fake;
    state.baker.version = Some("stand-in".into());
    let project = state.project.read().unwrap().clone();

    let (bakes, missing) = jobs::cached_bakes(&state, &project, "main");
    assert!(bakes.is_empty(), "nothing is baked yet, and nothing was baked by looking");
    assert_eq!(missing, ["o1c1"]);

    jobs::bake_for(&state, &project, "main", OverlayScope::All, None, &|_, _| {}, None).expect("bake");
    let (bakes, missing) = jobs::cached_bakes(&state, &project, "main");
    assert!(missing.is_empty(), "{missing:?}");
    let mut opts = jobs::compile_options(&state);
    opts.bakes = bakes;
    let xml = jobs::compile_xml(&project, &opts).unwrap();
    assert!(xml.contains("frame_%06d.png"), "the baked overlay is in the graph:\n{xml}");
}
