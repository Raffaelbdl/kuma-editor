//! Sampling an overlay **entry**: the agent's half of the overlay workbench.
//!
//! An entry is a whole thing before any clip places it, so checking what it looks like
//! must not require putting one in the cut — and must not leave anything behind in the
//! document if it does. That invariant is what these tests pin; they need no melt, no
//! hyperframes and no browser, because what they check happens before any of them run.

use std::sync::Arc;

use kuma_daemon::jobs::{overlay_project, sample_frames, SampleRequest};
use kuma_daemon::{AppState, Config};

/// `tag` is an overlay entry **no clip places** — the case the whole feature is about.
const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "tag": { "name": "Tag", "source": { "template": "probe", "vars": {} } },
    "navy": { "name": "Navy", "source": { "color": "#102030" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "navy", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();

    let tpl = dir.path().join("library/overlays/probe");
    std::fs::create_dir_all(&tpl).unwrap();
    std::fs::write(tpl.join("template.json"), r#"{ "id": "probe", "name": "Probe", "duration_default": 3 }"#).unwrap();
    std::fs::write(tpl.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();

    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    (Arc::new(AppState::load(config).unwrap()), dir)
}

fn req(overlay: Option<&str>, sequence: Option<&str>) -> SampleRequest {
    SampleRequest { t0: 1.0, sequence: sequence.map(str::to_string), overlay: overlay.map(str::to_string), ..Default::default() }
}

/// The reason this is a synthetic project rather than a clip added and removed around the
/// sample: a look must never be an edit. Whether the frames render here (they need
/// hyperframes and melt) is beside the point — either way the document is untouched.
#[test]
fn sampling_an_overlay_entry_leaves_the_document_alone() {
    let (state, _dir) = fixture();
    let before = serde_json::to_string(&*state.project.read().unwrap()).unwrap();
    let _ = sample_frames(&state, &req(Some("tag"), None), &|_, _| {});
    let after = serde_json::to_string(&*state.project.read().unwrap()).unwrap();
    assert_eq!(before, after, "sampling an entry changed the project");
}

/// Each refusal happens before anything is rendered, so these run on any machine.
#[test]
fn an_entry_sample_refuses_what_it_cannot_mean() {
    let (state, _dir) = fixture();
    let why = |r: &SampleRequest| format!("{:#}", sample_frames(&state, r, &|_, _| {}).expect_err("should have been refused"));

    assert!(why(&req(Some("nope"), None)).contains("no bin entry"), "{}", why(&req(Some("nope"), None)));
    // A colour is a bin entry and is not an overlay; so is a sequence.
    assert!(why(&req(Some("navy"), None)).contains("not an overlay entry"));
    assert!(why(&req(Some("main"), None)).contains("not an overlay entry"));
    // One timeline or one entry, never both: the times would mean different things.
    assert!(why(&req(Some("tag"), Some("main"))).contains("one or the other"));
}

/// How far a valid ask gets on a machine with no renderer: past every guard, into the
/// bake. Without this the test above could pass by refusing the sample outright.
#[test]
fn a_valid_entry_sample_reaches_the_renderer() {
    let (state, _dir) = fixture();
    match sample_frames(&state, &req(Some("tag"), None), &|_, _| {}) {
        Ok(samples) => assert_eq!(samples.len(), 1, "one time asked for, one frame back"),
        // hyperframes, a browser or melt is missing — the ask was still understood, and
        // the failure names the renderer rather than the request.
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                !msg.contains("no bin entry") && !msg.contains("not an overlay entry") && !msg.contains("one or the other"),
                "refused before rendering: {msg}"
            );
            eprintln!("no renderer on this machine, so only the request was checked: {msg}");
        }
    }
}

/// What the throwaway project is: one overlay clip over one colour, on the real project's
/// rate and revision, in the asked-for frame — and a different sequence id for a
/// different view, because that id is the frame cache's key.
#[test]
fn the_synthetic_project_has_the_shape_the_sample_relies_on() {
    let (state, _dir) = fixture();
    let doc = state.project.read().unwrap().clone();

    let (p, seq) = overlay_project(&state, &doc, "tag", &req(Some("tag"), None)).unwrap();
    assert_eq!(p.master, seq);
    assert_eq!(p.bin.len(), 3, "the entry, a backdrop and the sequence: {:?}", p.bin.keys());
    assert_eq!((p.fps, p.revision), (doc.fps, doc.revision), "sampled at the real rate, cached by the real revision");
    assert_eq!((p.width, p.height), (doc.width, doc.height));
    let clips: Vec<_> = p.all_clips().map(|(_, _, c)| c).collect();
    assert_eq!(clips.len(), 2);
    assert!(clips.iter().all(|c| c.start == 0.0 && c.end == 3.0), "both clips run the template's default 3 s: {clips:?}");
    assert!(clips.iter().any(|c| c.bin == "tag"));
    // The entry itself travels unchanged: same template, same vars, same name.
    assert_eq!(serde_json::to_value(&p.bin["tag"]).unwrap(), serde_json::to_value(&doc.bin["tag"]).unwrap());

    // The frame the caller asked for, and a length; the document is untouched by both.
    let portrait = SampleRequest { frame_width: Some(1080), frame_height: Some(1920), duration: Some(6.0), ..req(Some("tag"), None) };
    let (q, seq2) = overlay_project(&state, &doc, "tag", &portrait).unwrap();
    assert_eq!((q.width, q.height), (1080, 1920));
    assert!(q.all_clips().all(|(_, _, c)| c.end == 6.0));
    assert_ne!(seq, seq2, "a different view is a different cache key");
    assert_eq!((doc.width, doc.height), (320, 240));

    // The backdrop is a colour entry; `transparent` is spelled the way MLT reads it.
    let clear = SampleRequest { backdrop: Some("transparent".into()), ..req(Some("tag"), None) };
    let (r, seq3) = overlay_project(&state, &doc, "tag", &clear).unwrap();
    let colours: Vec<_> = r.bin.values().filter_map(|i| match &i.source { kuma_core::BinSource::Color { color } => Some(color.clone()), _ => None }).collect();
    assert_eq!(colours, ["#00000000"]);
    assert_ne!(seq, seq3);
}
