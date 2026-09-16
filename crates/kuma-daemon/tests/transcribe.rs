//! The `transcribe` job end to end, with a shell script standing in for whisper.cpp: it
//! writes a fixed transcript where `-of` points. That exercises the argument plumbing,
//! the WAV extraction (real ffmpeg, so this skips without one), the parse, and the patch
//! that lands the cues in the bin as a validated `subtitles` entry that edits like any
//! other var and undoes as one step.

use std::path::PathBuf;
use std::sync::Arc;

use kuma_core::BinSource;
use kuma_daemon::jobs::{self, TranscribeRequest};
use kuma_daemon::{AppState, Config};
use serde_json::json;

const FAKE_WHISPER: &str = r#"#!/bin/sh
# Stand-in for whisper-cli: answers the `--help` probe, refuses a missing WAV, and writes
# a fixed transcript where `-of` says.
while [ $# -gt 0 ]; do
  case "$1" in --help) exit 0 ;; -of) base=$2; shift ;; -f) [ -f "$2" ] || exit 3; shift ;; esac
  shift
done
[ -n "$base" ] || exit 4
printf '%s' '{ "transcription": [
  { "offsets": { "from": 0, "to": 900 }, "text": " Hello" },
  { "offsets": { "from": 900, "to": 2000 }, "text": " [BLANK_AUDIO]" },
  { "offsets": { "from": 2000, "to": 2800 }, "text": " world" } ] }' > "$base.json"
"#;

#[test]
fn a_transcript_becomes_an_editable_subtitles_entry() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping transcribe test");
        return;
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("talk.wav");
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), "sine=frequency=440:duration=1".into(), media.to_string_lossy().into_owned()]).unwrap();
    let fake = dir.path().join("whisper-cli");
    std::fs::write(&fake, FAKE_WHISPER).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // Read once, at load, so this test file holds the only test that sets them.
    std::env::set_var("KUMA_WHISPER_BIN", &fake);
    std::env::set_var("KUMA_WHISPER_MODEL", &fake);

    let config = Config { project_file: dir.path().join("t.kuma.json"), repo_root: repo.clone(), library_dirs: vec![repo.join("library")], bind: "127.0.0.1:0".parse().unwrap() };
    let state = Arc::new(AppState::load(config).unwrap());
    jobs::add_media(&state, &media, None, None, "test").unwrap();

    let req = TranscribeRequest { bin: "talk".into(), id: None, options: Default::default(), base_revision: None, origin: "test".into() };
    let out = jobs::transcribe(&state, &req, &|_, _| {}).unwrap();
    assert_eq!(out["bin"], "talk-subs");
    assert_eq!(out["cues"], 2, "the blank-audio marker is not a cue");

    let p = state.project.read().unwrap().clone();
    let BinSource::Overlay { template, vars } = &p.bin["talk-subs"].source else { panic!("not an overlay entry") };
    assert_eq!(template, "subtitles");
    assert_eq!(vars["cues"], json!([{ "start": 0.0, "end": 0.9, "text": "Hello" }, { "start": 2.0, "end": 2.8, "text": "world" }]));
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty(), "the entry validates against the real template");
    assert!(jobs::transcribe(&state, &req, &|_, _| {}).unwrap_err().to_string().contains("already exists"), "a second run must not rebind the entry");

    // Fixing a misheard word is one var patch, and the validator still reads the cues.
    let edit = |op: serde_json::Value| state.apply_patch(&serde_json::from_value::<Vec<json_patch::PatchOperation>>(json!([op])).unwrap(), None, "test");
    edit(json!({ "op": "replace", "path": "/bin/talk-subs/source/vars/cues/1/text", "value": "World!" })).unwrap();
    assert!(edit(json!({ "op": "replace", "path": "/bin/talk-subs/source/vars/cues/1/end", "value": 1.0 })).is_err(), "a cue ending before it starts is refused");
    let now = serde_json::to_value(&state.project.read().unwrap().bin["talk-subs"].source).unwrap();
    assert_eq!(now["vars"]["cues"][1]["text"], "World!");
    assert_eq!(now["vars"]["cues"][1]["end"], 2.8, "the refused patch changed nothing");
    // Two undo steps: the edit, then the transcript itself.
    state.undo().unwrap();
    state.undo().unwrap();
    assert!(!state.project.read().unwrap().bin.contains_key("talk-subs"));
}
