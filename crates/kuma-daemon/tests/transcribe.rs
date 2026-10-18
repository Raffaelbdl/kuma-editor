//! The `transcribe` job end to end, with a shell script standing in for whisper.cpp: it
//! writes a fixed transcript where `-of` points. That exercises the argument plumbing,
//! the WAV extraction (real ffmpeg, so this skips without one), the parse, and the patch
//! that lands the cues in the bin as a validated `subtitles` entry that edits like any
//! other var and undoes as one step.

use std::path::PathBuf;
use std::sync::Arc;

use kuma_core::BinSource;
use kuma_daemon::jobs::{self, TranscribeRequest};
use kuma_daemon::{AppState, Config, Libraries};
use serde_json::json;

const FAKE_WHISPER: &str = r#"#!/bin/sh
# Stand-in for whisper-cli: answers the `--help` probe, refuses a missing WAV, and writes
# a fixed transcript where `-of` says. Records its arguments next to it.
printf '%s\n' "$@" > "$0.args"
while [ $# -gt 0 ]; do
  case "$1" in --help) exit 0 ;; -of) base=$2; shift ;; -f) [ -f "$2" ] || exit 3; shift ;; esac
  shift
done
[ -n "$base" ] || exit 4
# Armed by the test, this blocks until released, so "the document changed while the model
# ran" is a fact rather than a matter of timing.
if [ -f "$0.wait" ]; then
  printf 'started\n' > "$0.started"
  n=0
  while [ ! -f "$0.go" ] && [ $n -lt 400 ]; do sleep 0.05; n=$((n+1)); done
fi
# whisper reports its own progress on stderr as it works.
printf 'whisper_print_progress_callback: progress =  40%%\n' >&2
printf 'whisper_print_progress_callback: progress = 100%%\n' >&2
# A transcript the test wrote beside the script, else a fixed one.
if [ -f "$0.json" ]; then cp "$0.json" "$base.json"; exit 0; fi
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

    let config = Config { project_file: dir.path().join("t.kuma.json"), repo_root: repo.clone(), libraries: Libraries::new(repo.join("library")), bind: "127.0.0.1:0".parse().unwrap() };
    let state = Arc::new(AppState::load(config).unwrap());
    kuma_daemon::media::add(&state, &[media.clone()], Default::default(), None, "test").unwrap();

    let req = TranscribeRequest { bin: Some("talk".into()), clip: None, id: None, replace: false, split: None, options: Default::default(), origin: "test".into() };
    // Whisper is minutes of work on a real recording, so the job must report its
    // percentage while it runs rather than jumping from "started" to "done".
    let seen = std::sync::Mutex::new(Vec::<u8>::new());
    let out = jobs::transcribe(&state, &req, &|p, _| seen.lock().unwrap().push(p)).unwrap();
    let seen = seen.into_inner().unwrap();
    assert!(seen.iter().any(|p| (10..85).contains(p)), "no progress between starting and finishing: {seen:?}");
    assert_eq!(out["bin"], "talk-subs");
    assert_eq!(out["cues"], 2, "the blank-audio marker is not a cue");
    assert_eq!(out["placed"], false);
    let args = std::fs::read_to_string(fake.with_extension("args")).unwrap();
    assert!(args.contains("-ml\n1\n-sow"), "the default is one cue per word: {args}");

    let p = state.project.read().unwrap().clone();
    let BinSource::Overlay { overlay, vars } = &p.bin["talk-subs"].source else { panic!("not an overlay entry") };
    // The blueprint is copied into the project, and the entry points at the copy.
    assert_eq!(overlay, "overlays/subtitles");
    assert!(dir.path().join("overlays/subtitles/overlay.json").is_file());
    assert_eq!(p.bin["talk-subs"].parent.as_deref(), Some("talk"), "listed under the media it came from");
    assert_eq!(p.children(Some("talk")).len(), 1);
    assert_eq!(vars["cues"], json!([{ "start": 0.0, "end": 0.9, "text": "Hello" }, { "start": 2.0, "end": 2.8, "text": "world" }]));
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty(), "the entry validates against the real blueprint's copy");
    let named = TranscribeRequest { id: Some("talk-subs".into()), ..req.clone() };
    assert!(jobs::transcribe(&state, &named, &|_, _| {}).unwrap_err().to_string().contains("already exists"), "naming an entry that is there is refused");
    assert!(jobs::transcribe(&state, &TranscribeRequest { id: Some("talk".into()), replace: true, ..req.clone() }, &|_, _| {}).unwrap_err().to_string().contains("not an overlay"),
        "`replace` overwrites a transcript, never some other entry");

    // Fixing a misheard word is one var patch, and the validator still reads the cues.
    let edit = |op: serde_json::Value| state.apply_patch(&serde_json::from_value::<Vec<json_patch::PatchOperation>>(json!([op])).unwrap(), None, "test", &[]);
    edit(json!({ "op": "replace", "path": "/bin/talk-subs/source/vars/cues/1/text", "value": "World!" })).unwrap();
    assert!(edit(json!({ "op": "replace", "path": "/bin/talk-subs/source/vars/cues/1/end", "value": 1.0 })).is_err(), "a cue ending before it starts is refused");
    let now = serde_json::to_value(&state.project.read().unwrap().bin["talk-subs"].source).unwrap();
    assert_eq!(now["vars"]["cues"][1]["text"], "World!");
    assert_eq!(now["vars"]["cues"][1]["end"], 2.8, "the refused patch changed nothing");
    // Removing the media while its transcript is listed under it would leave a dangling parent.
    assert!(edit(json!({ "op": "remove", "path": "/bin/talk" })).is_err());
    // Two undo steps: the edit, then the transcript itself.
    state.undo(None).unwrap();
    state.undo(None).unwrap();
    assert!(!state.project.read().unwrap().bin.contains_key("talk-subs"));

    // From a clip: the subtitles are placed over it, aligned — same start, end and speed,
    // and the entry's `offset` is the clip's `in` — so the cues land on the words.
    let seq = "/bin/main/source/tracks/2/clips/-"; // the audio track: `talk.wav` has no picture
    edit(json!({ "op": "add", "path": seq, "value": { "id": "c1", "start": 3.0, "end": 3.8, "bin": "talk", "in": 0.2, "speed": 0.5 } })).unwrap();
    let from_clip = TranscribeRequest { bin: None, clip: Some("c1".into()), ..req.clone() };
    let out = jobs::transcribe(&state, &from_clip, &|_, _| {}).unwrap();
    assert_eq!(out["placed"], true);
    let p = state.project.read().unwrap().clone();
    let subs = serde_json::to_value(&p.bin["talk-subs"]).unwrap();
    assert_eq!(subs["source"]["vars"]["offset"], 0.2);
    let (seq_id, track, placed) = p.all_clips().find(|(_, _, c)| c.bin == "talk-subs").expect("placed on a track");
    assert_eq!((seq_id.as_str(), track.kind), ("main", kuma_core::TrackKind::Overlay));
    assert_eq!((placed.start, placed.end, placed.speed), (3.0, 3.8, 0.5));
    let issues = kuma_core::validate(&p, &state.validation_context());
    assert!(issues.is_empty(), "{issues:?}");
    // Both ways are one undo step: the clip goes with the entry.
    state.undo(None).unwrap();
    assert!(state.project.read().unwrap().all_clips().all(|(_, _, c)| c.bin != "talk-subs"));

    // Where every overlay track is busy at that range, alignment wins over reuse: a new
    // overlay track is added rather than the clip being pushed along an old one.
    let before = p.master_seq().unwrap().tracks.len();
    jobs::transcribe(&state, &from_clip, &|_, _| {}).unwrap();
    let p = state.project.read().unwrap().clone();
    assert_eq!(p.master_seq().unwrap().tracks.len(), before, "the overlay track had room, so no track was added");
    let again = TranscribeRequest { id: Some("talk-subs-2".into()), ..from_clip.clone() };
    jobs::transcribe(&state, &again, &|_, _| {}).unwrap();
    let p = state.project.read().unwrap().clone();
    let tracks = &p.master_seq().unwrap().tracks;
    assert_eq!(tracks.len(), before + 1, "no overlay track had room at {}–{}, so one was added", placed.start, placed.end);
    let last = tracks.last().unwrap();
    assert_eq!((last.kind, last.name.as_str(), last.clips[0].bin.as_str()), (kuma_core::TrackKind::Overlay, "Subtitles", "talk-subs-2"));
    // Transcripts share the one copy of the blueprint rather than each taking another.
    assert!(!dir.path().join("overlays/subtitles-2").exists(), "a second transcript copied the blueprint again");
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty());
    assert!(subs["source"]["vars"].get("lang").is_none(), "a detected language is not the page's");
    // A language asked for is the page's too, so kanji come in that language's forms.
    let japanese = TranscribeRequest { bin: Some("talk".into()), clip: None, id: Some("talk-ja".into()), options: kuma_overlay::TranscribeOptions { language: Some("ja".into()), ..Default::default() }, ..req.clone() };
    jobs::transcribe(&state, &japanese, &|_, _| {}).unwrap();
    let p = state.project.read().unwrap().clone();
    assert_eq!(serde_json::to_value(&p.bin["talk-ja"].source).unwrap()["vars"]["lang"], "ja");
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty());

    // Re-running with `replace` overwrites the words in place: the same entry, its
    // styling, and the clip the first run placed all survive, and the language asked for
    // reaches whisper.
    edit(json!({ "op": "add", "path": "/bin/talk-subs/source/vars/style", "value": "outline" })).unwrap();
    let before = state.project.read().unwrap().clone();
    let french = TranscribeRequest {
        replace: true,
        options: kuma_overlay::TranscribeOptions { language: Some("fr".into()), ..Default::default() },
        ..from_clip.clone()
    };
    let out = jobs::transcribe(&state, &french, &|_, _| {}).unwrap();
    assert_eq!((out["bin"].as_str(), out["replaced"].as_bool(), out["placed"].as_bool()), (Some("talk-subs"), Some(true), Some(false)));
    let args = std::fs::read_to_string(fake.with_extension("args")).unwrap();
    assert!(args.contains("-l\nfr"), "the language is forced, not detected: {args}");
    let p = state.project.read().unwrap().clone();
    assert_eq!(p.bin.len(), before.bin.len(), "a replacement is not a second entry");
    assert_eq!(p.all_clips().count(), before.all_clips().count(), "nor a second clip");
    let subs = serde_json::to_value(&p.bin["talk-subs"]).unwrap();
    assert_eq!(subs["source"]["vars"]["style"], "outline", "the entry's own styling survives");
    assert_eq!(subs["source"]["vars"]["cues"][0]["text"], "Hello");
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty());

    // The model runs for minutes and the document is edited while it does, so nothing
    // about where the words go may be decided before they exist. Both of these used to
    // bite: an unrelated edit threw the whole transcription away on a stale
    // `base_revision`, and an entry created at the name it had picked was silently
    // overwritten, because a JSON Patch `add` replaces a member that is already there.
    std::fs::write(fake.with_extension("wait"), "").unwrap();
    let (s2, req2) = (state.clone(), req.clone());
    let running = std::thread::spawn(move || jobs::transcribe(&s2, &req2, &|_, _| {}));
    let started = fake.with_extension("started");
    while !started.is_file() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    edit(json!({ "op": "replace", "path": "/bin/talk/name", "value": "Renamed mid-transcription" })).unwrap();
    edit(json!({ "op": "add", "path": "/bin/talk-subs-3", "value": { "name": "Someone else's work", "source": { "color": "#ff0000" } } })).unwrap();
    std::fs::write(fake.with_extension("go"), "").unwrap();

    let out = running.join().unwrap().unwrap();
    assert_eq!(out["bin"], "talk-subs-4", "the name is minted with the words in hand, after `talk-subs-3` was taken");
    let p = state.project.read().unwrap().clone();
    assert_eq!(p.bin["talk-subs-3"].name, "Someone else's work", "the entry that took the name is untouched");
    assert_eq!(p.bin["talk"].name, "Renamed mid-transcription", "and the unrelated edit stands");
    assert_eq!(p.bin["talk-subs-4"].parent.as_deref(), Some("talk"));
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty());
}

/// Real pauses in real sound, and a transcript of contiguous words whose boundaries sit
/// 0.3 s past each pause, as whisper's do: the cues come out with the pauses as gaps, cut
/// into pieces of about a minute at the best of them, each its own entry; from a clip, the
/// pieces it plays, each placed over its stretch. And the model is the one asked for.
#[test]
fn a_long_transcript_has_real_gaps_and_is_cut_into_pieces_of_about_a_minute() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping transcribe test");
        return;
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pauses = [(10.0, 10.3), (30.0, 30.4), (61.0, 61.6), (90.0, 90.3), (121.0, 121.8)];
    let quiet: Vec<String> = pauses.iter().map(|(a, b)| format!("between(t,{a},{b})")).collect();
    let media = dir.path().join("talk.wav");
    let tone = format!("sine=frequency=300:duration=150:sample_rate=16000:samples_per_frame=160,volume='if({},0,1)':eval=frame", quiet.join("+"));
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), tone, media.to_string_lossy().into_owned()]).unwrap();

    // Words every half second, and around each pause one boundary 0.3 s past its end: the
    // word before runs over the pause, the word after starts late.
    let mut bounds: Vec<f64> = (0..=300).map(|i| i as f64 * 0.5).filter(|t| pauses.iter().all(|(a, b)| *t < a - 0.25 || *t > b + 0.55)).collect();
    bounds.extend(pauses.iter().map(|(_, b)| b + 0.3));
    bounds.sort_by(f64::total_cmp);
    let words: Vec<serde_json::Value> = bounds
        .windows(2)
        .map(|w| {
            let ends_sentence = (w[1] - 61.9).abs() < 1e-9;
            json!({ "offsets": { "from": (w[0] * 1000.0).round(), "to": (w[1] * 1000.0).round() }, "text": if ends_sentence { " fin." } else { " mot" } })
        })
        .collect();

    let fake = dir.path().join("whisper-cli");
    std::fs::write(&fake, FAKE_WHISPER).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::write(fake.with_extension("json"), json!({ "transcription": words }).to_string()).unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    for m in ["ggml-base.bin", "ggml-large-v3.bin"] {
        std::fs::write(models.join(m), "").unwrap();
    }
    let config = Config { project_file: dir.path().join("t.kuma.json"), repo_root: repo.clone(), libraries: Libraries::new(repo.join("library")), bind: "127.0.0.1:0".parse().unwrap() };
    let mut state = AppState::load(config).unwrap();
    // Set here rather than through the environment, which the other test in this file owns.
    state.transcriber = kuma_overlay::Transcriber { cli: fake.clone(), model: Some(models.join("ggml-base.bin")), model_dir: models.clone() };
    let state = Arc::new(state);
    kuma_daemon::media::add(&state, &[media.clone()], Default::default(), None, "test").unwrap();

    let req = TranscribeRequest {
        bin: Some("talk".into()),
        clip: None,
        id: None,
        replace: false,
        split: None,
        options: kuma_overlay::TranscribeOptions { model: Some("large-v3".into()), ..Default::default() },
        origin: "test".into(),
    };
    let out = jobs::transcribe(&state, &req, &|_, _| {}).unwrap();
    let args = std::fs::read_to_string(fake.with_extension("args")).unwrap();
    assert!(args.contains(&format!("-m\n{}\n", models.join("ggml-large-v3.bin").display())), "the model asked for: {args}");

    // Three pieces: cut at the sentence end after 61.6 s, then at the long pause at 121 s.
    let pieces = out["pieces"].as_array().unwrap_or_else(|| panic!("{out:#}"));
    let ids: Vec<&str> = pieces.iter().map(|p| p["bin"].as_str().unwrap()).collect();
    assert_eq!((ids.as_slice(), &out["bin"]), (["talk-subs-1", "talk-subs-2", "talk-subs-3"].as_slice(), &json!("talk-subs-1")), "{out:#}");
    let p = state.project.read().unwrap().clone();
    let entry = |id: &str| serde_json::to_value(&p.bin[id]).unwrap();
    let offsets: Vec<f64> = ids.iter().map(|id| entry(id)["source"]["vars"]["offset"].as_f64().unwrap()).collect();
    // Pauses come out of the sound to within a hundredth.
    let near = |a: f64, b: f64| (a - b).abs() < 0.02;
    assert!(offsets.len() == 3 && near(offsets[0], 0.0) && near(offsets[1], 61.3) && near(offsets[2], 121.4), "each from the middle of the pause before it: {offsets:?}");
    let cues: Vec<Vec<(f64, f64)>> = ids.iter().map(|id| entry(id)["source"]["vars"]["cues"].as_array().unwrap().iter().map(|c| (c["start"].as_f64().unwrap(), c["end"].as_f64().unwrap())).collect()).collect();
    assert_eq!(cues.iter().map(Vec::len).sum::<usize>(), out["cues"].as_u64().unwrap() as usize, "every word in one piece");
    assert!(cues[0].last().unwrap().1 <= 61.0 + 0.02 && cues[1][0].0 >= 61.6 - 0.02, "{:?} {:?}", cues[0].last(), cues[1][0]);
    // Every pause is a gap between two cues now, where it is in the sound.
    let all: Vec<(f64, f64)> = cues.concat();
    for (a, b) in pauses {
        let gap = all.windows(2).find(|w| w[0].1 <= a + 0.05 && w[1].0 >= b - 0.05 && w[1].0 - w[0].1 > 0.1).unwrap_or_else(|| panic!("no gap at {a}–{b}"));
        assert!((gap[0].1 - a).abs() < 0.02 && (gap[1].0 - b).abs() < 0.02, "{a}–{b} came out {}–{}", gap[0].1, gap[1].0);
    }
    assert_eq!(p.bin["talk-subs-2"].parent.as_deref(), Some("talk"));
    assert!(p.bin["talk-subs-2"].name.ends_with("(2/3)"), "{}", p.bin["talk-subs-2"].name);
    assert!(!p.all_clips().any(|(_, _, c)| c.bin.starts_with("talk-subs")), "from a bin entry nothing is placed");
    assert!(kuma_core::validate(&p, &state.validation_context()).is_empty());
    state.undo(None).unwrap();

    // From a clip playing 50–100 s of the source at 10–60 s: the two pieces it plays, side
    // by side on one overlay track, each with the source second it starts at as `offset`.
    let edit = |op: serde_json::Value| state.apply_patch(&serde_json::from_value::<Vec<json_patch::PatchOperation>>(json!([op])).unwrap(), None, "test", &[]);
    edit(json!({ "op": "add", "path": "/bin/main/source/tracks/2/clips/-", "value": { "id": "c1", "start": 10.0, "end": 60.0, "bin": "talk", "in": 50.0 } })).unwrap();
    let out = jobs::transcribe(&state, &TranscribeRequest { bin: None, clip: Some("c1".into()), ..req.clone() }, &|_, _| {}).unwrap();
    let p = state.project.read().unwrap().clone();
    let placed: Vec<(String, f64, f64, f64)> = p
        .all_clips()
        .filter(|(_, t, _)| t.kind == kuma_core::TrackKind::Overlay)
        .map(|(_, _, c)| (c.bin.clone(), c.start, c.end, serde_json::to_value(&p.bin[&c.bin]).unwrap()["source"]["vars"]["offset"].as_f64().unwrap()))
        .collect();
    let cut = offsets[1];
    assert_eq!(placed, [("talk-subs-1".to_string(), 10.0, 10.0 + cut - 50.0, 50.0), ("talk-subs-2".to_string(), 10.0 + cut - 50.0, 60.0, cut)], "{out:#}");
    assert_eq!(out["pieces"].as_array().unwrap().len(), 2, "a piece the clip does not play is not made: {out:#}");
    let issues = kuma_core::validate(&p, &state.validation_context());
    assert!(issues.is_empty(), "{issues:?}");
    state.undo(None).unwrap();

    // One entry when asked; and what is not supported is refused before the model runs.
    let one = jobs::transcribe(&state, &TranscribeRequest { split: Some(0.0), ..req.clone() }, &|_, _| {}).unwrap();
    assert!(one.get("pieces").is_none() && one["bin"] == "talk-subs", "{one:#}");
    let refused = |r: TranscribeRequest| jobs::transcribe(&state, &r, &|_, _| {}).unwrap_err().to_string();
    assert!(refused(TranscribeRequest { split: Some(60.0), replace: true, ..req.clone() }).contains("`replace` overwrites"));
    assert!(refused(TranscribeRequest { split: Some(2.0), ..req.clone() }).contains("5 or more"));
    let other = TranscribeRequest { options: kuma_overlay::TranscribeOptions { model: Some("medium".into()), ..Default::default() }, ..req.clone() };
    assert!(refused(other).contains("installed are ggml-base.bin, ggml-large-v3.bin"));
    // `snap: false` keeps whisper's times, pause and all.
    let raw = jobs::transcribe(&state, &TranscribeRequest { split: Some(0.0), options: kuma_overlay::TranscribeOptions { snap: Some(false), ..Default::default() }, ..req.clone() }, &|_, _| {}).unwrap();
    let p = state.project.read().unwrap().clone();
    let raw_cues = serde_json::to_value(&p.bin[raw["bin"].as_str().unwrap()]).unwrap()["source"]["vars"]["cues"].clone();
    assert!(raw_cues.as_array().unwrap().windows(2).all(|w| w[0]["end"] == w[1]["start"]), "contiguous, as whisper wrote them");
}
