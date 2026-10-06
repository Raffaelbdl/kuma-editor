//! Media in the bin: added by the list or by the folder as one edit, probed again when a
//! file is replaced, and the entries nothing uses. Real ffmpeg makes the files, so these
//! skip without one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kuma_core::{BinSource, MediaKind};
use kuma_daemon::media::{self, AddHow};
use kuma_daemon::{AppState, Config};
use serde_json::json;

fn state(dir: &Path) -> Arc<AppState> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let config = Config { project_file: dir.join("t.kuma.json"), repo_root: repo.clone(), library_dirs: vec![repo.join("library")], bind: "127.0.0.1:0".parse().unwrap() };
    Arc::new(AppState::load(config).unwrap())
}

/// A second of sound (or `seconds`) at `path`, made by ffmpeg.
fn beep(path: &Path, seconds: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), format!("sine=duration={seconds}"), path.to_string_lossy().into_owned()]).unwrap();
}

fn patch(state: &AppState, ops: serde_json::Value) {
    let ops: Vec<json_patch::PatchOperation> = serde_json::from_value(ops).unwrap();
    state.apply_patch(&ops, None, "test", &[]).unwrap();
}

fn revision(state: &AppState) -> u64 {
    state.project.read().unwrap().revision
}

fn skipped(added: &media::Added) -> Vec<(String, String)> {
    added.skipped.iter().map(|s| (s.path.clone(), s.reason.clone())).collect()
}

/// A folder adds the media directly in it, ids minted apart even when two names slug
/// alike, as one revision that one undo takes back; what is not media is said, not added.
/// Added again it adds only what is new, and `recursive` reaches into subfolders.
#[test]
fn a_folder_of_media_is_added_in_one_edit_and_says_what_it_passed_over() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping add-media test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let media = dir.path().join("media");
    for name in ["01 intro.wav", "take-b.wav", "take_b.wav", ".hidden.wav", "more/deep.wav"] {
        beep(&media.join(name), 1);
    }
    std::fs::write(media.join("notes.txt"), "not media").unwrap();
    std::fs::write(media.join("logo.svg"), "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
    std::fs::create_dir_all(media.join("card")).unwrap();
    std::fs::write(media.join("card/overlay.json"), "{}").unwrap();
    patch(&state, json!([{ "op": "add", "path": "/bin/clips", "value": { "source": { "folder": true } } }]));
    let before = revision(&state);

    let how = AddHow { parent: Some("clips".into()), ..Default::default() };
    let added = media::add(&state, &[PathBuf::from("media")], how.clone(), Some(before), "test").unwrap();
    assert_eq!(added.ids, ["audio_01_intro", "take_b", "take_b-2"], "a stem that starts with a digit is prefixed with its kind");
    assert_eq!((added.revision, revision(&state)), (before + 1, before + 1), "one edit for the folder");
    let p = state.project.read().unwrap().clone();
    assert!(added.ids.iter().all(|id| p.bin[id].parent.as_deref() == Some("clips")));
    assert!(matches!(&p.bin["take_b-2"].source, BinSource::File { path, kind: MediaKind::Audio, .. } if path == "media/take_b.wav"));
    let reasons = skipped(&added);
    let reason = |path: &str| reasons.iter().find(|(p, _)| p == path).map(|(_, r)| r.as_str()).unwrap_or_else(|| panic!("{path} not skipped: {reasons:?}"));
    assert!(reason("media/card").contains("import_overlay"));
    assert!(reason("media/logo.svg").contains("rasterise"));
    assert!(reason("media/more").contains("recursive"));
    assert!(reason("media/notes.txt").contains("not named like a media file"));
    assert_eq!(reasons.len(), 4, "a hidden file is passed over without a word: {reasons:?}");

    state.undo(None).unwrap();
    assert!(!state.project.read().unwrap().bin.contains_key("take_b"), "one undo takes the whole folder back");
    state.redo(None).unwrap();

    let again = media::add(&state, &[media.clone()], AddHow { recursive: true, ..Default::default() }, None, "test").unwrap();
    assert_eq!(again.ids, ["deep"], "only what is new");
    assert!(skipped(&again).iter().any(|(p, r)| p == "media/take_b.wav" && r.contains("`take_b-2`")), "{:?}", again.skipped);
    let before = revision(&state);
    let nothing = media::add(&state, &[media], AddHow::default(), None, "test").unwrap_err().to_string();
    assert!(nothing.contains("no media to add") && nothing.contains("already in the bin"), "{nothing}");
    assert_eq!(revision(&state), before, "nothing to add is no edit");

    // The editor's drop passes over a folder and adds the files dropped with it.
    beep(&dir.path().join("loose.wav"), 1);
    let dropped = media::add(&state, &[PathBuf::from("media"), PathBuf::from("loose.wav")], AddHow { files_only: true, ..Default::default() }, None, "test").unwrap();
    assert_eq!(dropped.ids, ["loose"]);
    assert_eq!(skipped(&dropped), [("media".to_string(), "a folder: add the files in it".to_string())]);
}

/// A list is one edit too, and a file it names that cannot be added refuses the call: it
/// was asked for, so it is not skipped. An SVG is refused with how to rasterise it.
#[test]
fn a_list_of_files_is_one_edit_and_one_bad_file_refuses_it_whole() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping add-media test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    for name in ["a.wav", "b.wav", "c.wav"] {
        beep(&dir.path().join(name), 1);
    }
    std::fs::write(dir.path().join("logo.svg"), "<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'/>").unwrap();
    let before = revision(&state);
    let added = media::add(&state, &["a.wav".into(), dir.path().join("b.wav")], AddHow::default(), None, "test").unwrap();
    assert_eq!((added.ids.clone(), added.revision), (vec!["a".to_string(), "b".into()], before + 1));

    for paths in [vec![PathBuf::from("c.wav"), "gone.wav".into()], vec!["c.wav".into(), "logo.svg".into()]] {
        let e = media::add(&state, &paths, AddHow::default(), None, "test").unwrap_err().to_string();
        assert!(e.contains("not found") || e.contains("rasterise"), "{e}");
        assert!(!state.project.read().unwrap().bin.contains_key("c"), "half a list was added");
    }
    let e = media::add(&state, &["c.wav".into(), "a.wav".into()], AddHow { id: Some("x".into()), ..Default::default() }, None, "test").unwrap_err().to_string();
    assert!(e.contains("`id` names one entry"), "{e}");
    // One file by its own name, again, is a second entry under a free id, as one always was.
    let again = media::add(&state, &["a.wav".into()], AddHow::default(), None, "test").unwrap();
    assert_eq!(again.ids, ["a-2"]);
}

/// A file replaced on disk is corrected only when asked: the entries whose probe says
/// something new, and only those fields, in one undoable revision that names each change.
/// Asking again finds nothing and writes nothing. A kind change that would break a
/// placement is refused whole, like any edit.
#[test]
fn a_replaced_file_is_probed_again_on_asking_and_only_what_changed_is_written() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping re-probe test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    beep(&dir.path().join("a.wav"), 1);
    beep(&dir.path().join("b.wav"), 1);
    media::add(&state, &["a.wav".into(), "b.wav".into()], AddHow::default(), None, "test").unwrap();
    beep(&dir.path().join("a.wav"), 2);
    let before = revision(&state);

    let reprobe = |bins: Option<&[String]>, base| serde_json::to_value(media::reprobe(&state, bins, base, "test").unwrap()).unwrap();
    let out = reprobe(None, Some(before));
    assert_eq!(out["revision"], before + 1);
    assert_eq!(out["changed"], json!([{ "id": "a", "changes": { "meta.duration": { "from": 1.0, "to": 2.0 } } }]));
    assert_eq!(out["unchanged"], json!(["b"]));
    assert_eq!(state.project.read().unwrap().bin["a"].meta.as_ref().and_then(|m| m.duration), Some(2.0));
    state.undo(None).unwrap();
    assert_eq!(state.project.read().unwrap().bin["a"].meta.as_ref().and_then(|m| m.duration), Some(1.0), "one undo");
    state.redo(None).unwrap();
    let before = revision(&state);
    let out = reprobe(Some(&["a".into()]), None);
    assert_eq!((out["changed"].clone(), out["revision"].clone()), (json!([]), json!(before)), "nothing new, nothing written");
    assert!(media::reprobe(&state, Some(&["main".into()]), None, "test").is_err(), "a sequence has no file");

    // A video placed from 0.5 s in, replaced by a still: a still has no `in`, so the
    // correction is refused and says why; placed from its start, the kind follows the file.
    let clip = dir.path().join("v.mp4");
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), "testsrc=duration=2:size=64x48".into(), "-pix_fmt".into(), "yuv420p".into(), clip.to_string_lossy().into_owned()]).unwrap();
    media::add(&state, &[clip.clone()], AddHow::default(), None, "test").unwrap();
    let place = json!([{ "op": "add", "path": "/bin/main/source/tracks/0/clips/-", "value": { "id": "c1", "start": 0, "end": 1, "bin": "v", "in": 0.5 } }]);
    patch(&state, place);
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), "color=size=64x48".into(), "-frames:v".into(), "1".into(), "-c:v".into(), "png".into(), "-f".into(), "image2".into(), clip.to_string_lossy().into_owned()]).unwrap();
    let before = revision(&state);
    let e = media::reprobe(&state, Some(&["v".into()]), None, "test").unwrap_err();
    let Some(kuma_core::EditError::Invalid(issues)) = e.downcast_ref() else { panic!("{e:#}") };
    assert!(issues.iter().any(|i| i.path.ends_with("/clips/0/in")), "{issues:?}");
    assert_eq!(revision(&state), before);
    let unplace = json!([{ "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/in", "value": 0 }]);
    patch(&state, unplace);
    let out = reprobe(Some(&["v".into()]), None);
    assert_eq!(out["changed"][0]["changes"]["kind"], json!({ "from": "video", "to": "image" }), "{out}");
    assert!(matches!(state.project.read().unwrap().bin["v"].source, BinSource::File { kind: MediaKind::Image, .. }));
}

#[test]
fn unused_entries_say_what_each_is_and_where_it_is_placed() {
    let project: kuma_core::Project = serde_json::from_value(json!({
        "bin": {
            "song": { "name": "Song", "source": { "path": "song.wav", "kind": "audio" } },
            "red": { "source": { "color": "#f00" } },
            "draft": { "source": { "tracks": [ { "id": "d", "clips": [ { "id": "d1", "start": 0, "end": 1, "bin": "red" } ] } ] } },
            "main": { "source": { "tracks": [] } }
        }
    }))
    .unwrap();
    assert_eq!(
        media::unused(&project),
        json!({
            "unplaced": [ { "id": "draft", "name": "draft", "kind": "sequence" }, { "id": "song", "name": "Song", "kind": "audio" } ],
            "unreached": [ { "id": "red", "name": "red", "kind": "color", "placed_in": ["draft"] } ]
        })
    );
}
