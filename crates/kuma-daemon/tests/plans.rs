//! `plans::apply`: a document a script wrote becomes the project in one edit, and the next
//! one keeps what a person changed in the editor in between, with no base for the script
//! to carry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kuma_core::EditError;
use kuma_daemon::plans::{self, PlanRequest};
use kuma_daemon::{AppState, Config, Libraries};
use serde_json::{json, Value};

fn state(dir: &Path) -> Arc<AppState> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let config = Config { project_file: dir.join("t.kuma.json"), repo_root: repo.clone(), libraries: Libraries::new(repo.join("library")), bind: "127.0.0.1:0".parse().unwrap() };
    Arc::new(AppState::load(config).unwrap())
}

/// What a script writes: colour clips on `main`'s one track, and the markers given.
fn plan(clips: &[(&str, f64, f64)], markers: Value) -> String {
    let clips: Vec<Value> = clips.iter().map(|(id, s, e)| json!({ "id": id, "start": s, "end": e, "bin": "black" })).collect();
    json!({ "bin": {
        "black": { "source": { "color": "#000000" } },
        "main": { "source": { "tracks": [{ "id": "v1", "clips": clips }], "markers": markers } }
    } })
    .to_string()
}

fn write(dir: &Path, text: &str) -> PlanRequest {
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("build/plan.json"), text).unwrap();
    PlanRequest { path: "build/plan.json".into(), origin: Some("test".into()), ..Default::default() }
}

fn clips(state: &AppState) -> Vec<(String, f64)> {
    let p = state.project.read().unwrap();
    p.seq("main").unwrap().tracks[0].clips.iter().map(|c| (c.id.clone(), c.end)).collect()
}

fn revision(state: &AppState) -> u64 {
    state.project.read().unwrap().revision
}

/// The loop the field reports found agents building out of `kuma diff`, `apply_edits` and
/// a base they copied by hand, as one call: a rebuild after a person's edit keeps it.
#[test]
fn a_plan_is_applied_and_the_next_keeps_the_editors_changes() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let req = write(dir.path(), &plan(&[("c1", 0.0, 2.0), ("c2", 2.0, 4.0)], json!([])));

    let first = plans::apply(&st, &req).unwrap();
    assert!(first.applied);
    assert_eq!(first.base, "none", "the first plan from a path has nothing to merge with");
    assert_eq!(clips(&st), [("c1".to_string(), 2.0), ("c2".to_string(), 4.0)]);
    assert!(plans::base_file(&st, "build/plan.json").exists(), "the plan is kept as the next base");

    // The same plan again: nothing to do, and no revision for it.
    let rev = revision(&st);
    let again = plans::apply(&st, &req).unwrap();
    assert_eq!((again.ops, again.applied, again.revision), (0, false, rev));
    assert_eq!(revision(&st), rev);

    // In the editor: c1 shortened and a marker left. The script then moves c2 only.
    let person: Vec<json_patch::PatchOperation> = serde_json::from_value(json!([
        { "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/end", "value": 1.5 },
        { "op": "add", "path": "/bin/main/source/markers/-", "value": { "t": 3.0, "label": "too long" } }
    ]))
    .unwrap();
    st.apply_patch(&person, None, "ui", &[]).unwrap();
    let req = write(dir.path(), &plan(&[("c1", 0.0, 2.0), ("c2", 2.0, 3.0)], json!([])));
    let out = plans::apply(&st, &req).unwrap();
    assert_eq!(out.base, "last-applied");
    assert!(out.kept.is_empty(), "{:?}", out.kept);
    assert_eq!(clips(&st), [("c1".to_string(), 1.5), ("c2".to_string(), 3.0)], "the script's change and the person's");
    assert_eq!(st.project.read().unwrap().seq("main").unwrap().markers.len(), 1, "the marker stays");

    // Both change c1's end: the editor's stays, and the answer says where.
    st.apply_patch(&serde_json::from_value::<Vec<json_patch::PatchOperation>>(json!([{ "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/end", "value": 1.25 }])).unwrap(), None, "ui", &[]).unwrap();
    let req = write(dir.path(), &plan(&[("c1", 0.0, 1.0), ("c2", 2.0, 3.0)], json!([])));
    let out = plans::apply(&st, &req).unwrap();
    assert_eq!(out.kept, ["/bin/main/source/tracks/0/clips/0/end"]);
    assert_eq!(clips(&st)[0].1, 1.25);
    // The same plan again: nothing to send, but the two still differ there, and it says so
    // until one side gives way. (It used to answer 0 ops and no `kept`, as if they agreed.)
    let again = plans::apply(&st, &req).unwrap();
    assert_eq!((again.ops, again.kept.clone()), (0, vec!["/bin/main/source/tracks/0/clips/0/end".to_string()]));

    // `exact` makes the project the plan, the person's marker and end included.
    let out = plans::apply(&st, &PlanRequest { exact: true, ..req }).unwrap();
    assert_eq!((out.base, out.lost_markers), ("none", 1));
    assert_eq!(clips(&st)[0].1, 1.0);
    assert!(st.project.read().unwrap().seq("main").unwrap().markers.is_empty());
}

/// A dry run says what would change and changes nothing, base included; a plan the
/// validator refuses makes no revision and leaves the base as it was, so the next plan is
/// still merged against what the project really came from.
#[test]
fn a_dry_run_and_a_refused_plan_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let req = write(dir.path(), &plan(&[("c1", 0.0, 2.0)], json!([])));

    let dry = plans::apply(&st, &PlanRequest { dry_run: true, ..req.clone() }).unwrap();
    assert!(!dry.applied && dry.ops > 0);
    assert!(dry.patch.as_ref().is_some_and(|p| p.as_array().is_some_and(|a| a.len() == dry.ops)), "the patch comes back");
    assert_eq!(clip_count(&st), 0, "nothing was applied");
    assert!(!plans::base_file(&st, "build/plan.json").exists(), "a dry run is no base");

    plans::apply(&st, &req).unwrap();
    let base = std::fs::read_to_string(plans::base_file(&st, "build/plan.json")).unwrap();
    let rev = revision(&st);
    // A clip ending before it starts.
    let bad = write(dir.path(), &plan(&[("c1", 2.0, 1.0)], json!([])));
    match plans::apply(&st, &bad).map_err(|e| e.downcast::<EditError>()) {
        Err(Ok(EditError::Invalid(issues))) => assert!(!issues.is_empty()),
        other => panic!("expected the validator's refusal, got {other:?}"),
    }
    assert_eq!(revision(&st), rev);
    assert_eq!(std::fs::read_to_string(plans::base_file(&st, "build/plan.json")).unwrap(), base, "the base is the plan that landed");

    // Not a project document, and a file outside the project.
    let junk = write(dir.path(), "[1, 2]");
    let err = plans::apply(&st, &junk).unwrap_err();
    assert!(err.downcast_ref::<EditError>().is_none() && format!("{err:#}").contains("not a project document"), "{err:#}");
    let outside = tempfile::NamedTempFile::new().unwrap();
    assert!(plans::apply(&st, &PlanRequest { path: outside.path().to_string_lossy().into_owned(), ..Default::default() }).is_err());
}

fn clip_count(state: &AppState) -> usize {
    state.project.read().unwrap().seq("main").map_or(0, |s| s.tracks.iter().map(|t| t.clips.len()).sum())
}
