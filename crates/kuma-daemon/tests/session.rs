//! The session state the editor reads beside the document: the agent's activity, the
//! revision history (with refusals and restores), and pins. None of it is in the project
//! file, all of it is over HTTP, and every change of it is broadcast.
//!
//! Needs no browser and no engine: a daemon on a loopback port and raw requests.

use std::io::{Read as _, Write as _};
use std::sync::Arc;

use kuma_daemon::{serve, AppState, Config};
use serde_json::{json, Value};

const PROJECT: &str = r##"{
  "version": 8, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "black": { "name": "Black", "source": { "color": "#000000" } },
    "intro": { "name": "Intro", "source": { "tracks": [], "markers": [ { "t": 0.5, "label": "louder" } ] } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "black" }, { "id": "c2", "start": 2, "end": 4, "bin": "black" } ] }
    ], "markers": [ { "t": 8.6, "label": "this HUD corner fights with the subtitles" }, { "t": 1, "label": "too dark" }, { "t": 3, "label": "" } ] } }
  }
}"##;

async fn daemon() -> (Arc<AppState>, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let config = Config { project_file, repo_root: dir.path().join("repo"), library_dirs: vec![dir.path().join("library")], bind: "127.0.0.1:0".parse().unwrap() };
    let (state, addr) = serve(config).await.unwrap();
    (state, addr.to_string(), dir)
}

/// One HTTP/1.1 request: the status line and the body (read by `content-length`).
fn request(addr: &str, method: &str, path: &str, body: &str) -> (String, Vec<u8>) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let typed = if body.is_empty() { "" } else { "Content-Type: application/json\r\n" };
    write!(s, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{typed}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => raw.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("{path}: {e}"),
        }
    }
    let head = String::from_utf8_lossy(&raw).to_string();
    let status = head.lines().next().unwrap_or_default().to_string();
    let len: usize = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    let mut out = vec![0u8; len];
    s.read_exact(&mut out).unwrap();
    (status, out)
}

fn call(addr: &str, method: &str, path: &str, body: Value) -> (String, Value) {
    let text = if body.is_null() { String::new() } else { body.to_string() };
    let (status, bytes) = request(addr, method, path, &text);
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn get(addr: &str, path: &str) -> Value {
    let (status, v) = call(addr, "GET", path, Value::Null);
    assert!(status.contains("200"), "{path}: {status} {v}");
    v
}

fn post(addr: &str, path: &str, body: Value) -> Value {
    let (status, v) = call(addr, "POST", path, body.clone());
    assert!(status.contains("200"), "{path} {body}: {status} {v}");
    v
}

fn revision(state: &AppState) -> u64 {
    state.project.read().unwrap().revision
}

/// A call is recorded when it starts and completed when it ends, with what the daemon
/// knows (the revisions either side) and what the report said (a sample's moments and
/// frames); the list is newest first, and both phases are broadcast.
#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_is_reported_from_start_to_end() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let rev = revision(&state);
    let samples = state.config.samples_dir();

    let sample = post(&addr, "/api/activity", json!({ "phase": "start", "tool": "sample_frames", "client": "claude-code", "args": { "t0": 46.0, "t1": 49.0, "rect": { "x": 0, "y": 0, "w": 960, "h": 540 } } }));
    let edit = post(&addr, "/api/activity", json!({ "phase": "start", "tool": "apply_edits", "args": { "patch": { "ops": 2 }, "note": "Moved + trimmed the umbrella", "pins": ["main@8.600"] } }));
    let (a, b) = (sample["id"].as_u64().unwrap(), edit["id"].as_u64().unwrap());
    assert!(b > a);

    let frames = json!({ "count": 2, "paths": [samples.join("x/1.png"), samples.join("x/2.png")], "sheet": null });
    post(&addr, "/api/activity", json!({ "phase": "end", "id": a, "ok": true, "error": null, "result": frames }));
    post(&addr, "/api/activity", json!({ "phase": "end", "id": b, "ok": false, "error": "409: stale", "result": null }));

    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    assert_eq!(all.iter().map(|r| r["id"].as_u64().unwrap()).collect::<Vec<_>>(), [b, a], "newest first");
    let (e, s) = (&all[0], &all[1]);
    assert_eq!((e["state"].as_str(), e["client"].as_str(), e["summary"].as_str()), (Some("failed"), Some("agent"), Some("Moved + trimmed the umbrella")));
    assert_eq!((e["error"].as_str(), &e["pins"]), (Some("409: stale"), &json!(["main@8.600"])));
    assert_eq!((s["state"].as_str(), s["client"].as_str(), s["revision_before"].as_u64(), s["revision_after"].as_u64()), (Some("done"), Some("claude-code"), Some(rev), Some(rev)));
    assert_eq!(s["sample"], json!({ "sequence": null, "overlay": null, "times": [46.0, 49.0], "rect": { "x": 0, "y": 0, "w": 960, "h": 540 } }));
    assert_eq!(s["frames"], json!(["x/1.png", "x/2.png"]));
    assert!(s["ended_at"].as_u64().unwrap() >= s["started_at"].as_u64().unwrap());
    assert_eq!(get(&addr, "/api/activity?limit=1")["activity"].as_array().unwrap().len(), 1);

    let (status, _) = call(&addr, "POST", "/api/activity", json!({ "phase": "end", "id": 999, "ok": true }));
    assert!(status.contains("404"), "{status}");

    let ev = events.recv().await.unwrap();
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!((v["type"].as_str(), v["tool"].as_str(), v["state"].as_str()), (Some("activity"), Some("sample_frames"), Some("running")), "{v}");
}

/// An edit, its undo (authored by whoever asked) and a refused stale patch are each one
/// entry, newest first, naming what they touched; each is broadcast.
#[tokio::test(flavor = "multi_thread")]
async fn the_history_names_edits_steps_and_refusals() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let rev = revision(&state);
    let patch = json!([{ "op": "replace", "path": "/bin/main/source/tracks/0/clips/1/start", "value": 2.5 }]);
    let edited = post(&addr, "/api/project/patch", json!({ "patch": patch, "base_revision": rev, "origin": "claude: Moved + trimmed the umbrella" }));
    assert_eq!(edited["revision"], rev + 1);

    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    post(&addr, "/api/project/redo", Value::Null);
    let (status, _) = call(&addr, "POST", "/api/project/patch", json!({ "patch": patch, "base_revision": rev, "origin": "claude: again" }));
    assert!(status.contains("409"), "{status}");

    let h = get(&addr, "/api/history");
    assert_eq!(h["current"], rev + 3);
    let e = h["entries"].as_array().unwrap();
    assert_eq!(e.iter().map(|e| e["kind"].as_str().unwrap()).collect::<Vec<_>>(), ["refused", "redo", "undo", "edit"]);
    let (refused, redo, undo, edit) = (&e[0], &e[1], &e[2], &e[3]);

    assert_eq!((edit["revision"].as_u64(), edit["author"].as_str(), edit["summary"].as_str()), (Some(rev + 1), Some("agent"), Some("Moved + trimmed the umbrella")));
    assert_eq!((&edit["targets"], edit["ops"].as_u64(), edit["base_revision"].as_u64()), (&json!(["c2"]), Some(1), Some(rev)));
    assert_eq!(edit["changes"], json!([{ "target": "c2", "field": "start", "from": 2.0, "to": 2.5 }]));

    assert_eq!((undo["revision"].as_u64(), undo["author"].as_str(), undo["summary"].as_str()), (Some(rev + 2), Some("user"), Some("undo: Moved + trimmed the umbrella")));
    assert_eq!((&undo["targets"], &undo["changes"][0]["to"]), (&json!(["c2"]), &json!(2.0)));
    assert_eq!(redo["author"], "other", "no origin given");

    assert_eq!((refused["revision"].clone(), refused["author"].as_str()), (Value::Null, Some("agent")));
    assert_eq!(refused["error"].as_str(), Some(format!("stale base r{rev} (current r{})", rev + 3).as_str()));
    assert_eq!(refused["targets"], json!(["c2"]));
    assert_eq!(get(&addr, "/api/history?limit=2")["entries"].as_array().unwrap().len(), 2);

    let v = serde_json::to_value(events.recv().await.unwrap()).unwrap();
    assert_eq!(v["type"], "project_changed");
    let v = serde_json::to_value(events.recv().await.unwrap()).unwrap();
    assert_eq!((v["type"].as_str(), v["kind"].as_str(), v["revision"].as_u64()), (Some("history"), Some("edit"), Some(rev + 1)), "{v}");
}

/// A restore makes the document what it was, at a new revision, as an edit that undoes;
/// a revision never kept is a 404.
#[tokio::test(flavor = "multi_thread")]
async fn a_revision_is_restored_as_one_undoable_edit() {
    let (state, addr, _dir) = daemon().await;
    let start = revision(&state);
    let bin = |s: &AppState| serde_json::to_value(&s.project.read().unwrap().bin).unwrap();
    let original = bin(&state);
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }] }));
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "remove", "path": "/bin/main/source/tracks/0/clips/0" }] }));
    let edited = bin(&state);

    let r = post(&addr, "/api/history/restore", json!({ "revision": start, "origin": "ui" }));
    assert_eq!(r["revision"], start + 3, "forward, as undo goes");
    assert_eq!(bin(&state), original);
    let entry = &get(&addr, "/api/history?limit=1")["entries"][0];
    assert_eq!((entry["kind"].as_str(), entry["summary"].as_str(), entry["author"].as_str()), (Some("restore"), Some(format!("restore r{start}").as_str()), Some("user")));
    assert_eq!(entry["targets"], json!(["c1", "black"]));

    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    assert_eq!(bin(&state), edited, "the restore undoes");
    assert_eq!(revision(&state), start + 4);

    let (status, v) = call(&addr, "POST", "/api/history/restore", json!({ "revision": 9999 }));
    assert!(status.contains("404") && v["error"].as_str().unwrap().contains("9999"), "{status} {v}");
}

/// Pins are labelled markers, numbered in time order per sequence, the master's first; an
/// edit naming one answers it, and a tick by hand marks or clears it.
#[tokio::test(flavor = "multi_thread")]
async fn pins_are_listed_answered_and_ticked() {
    let (state, addr, _dir) = daemon().await;
    let pins = || get(&addr, "/api/pins")["pins"].as_array().unwrap().clone();
    let all = pins();
    let listed: Vec<(&str, u64)> = all.iter().map(|p| (p["id"].as_str().unwrap(), p["index"].as_u64().unwrap())).collect();
    assert_eq!(listed, [("main@1.000", 1), ("main@8.600", 2), ("intro@0.500", 1)]);
    assert_eq!((&all[1]["label"], &all[1]["done"], &all[1]["t"]), (&json!("this HUD corner fights with the subtitles"), &Value::Null, &json!(8.6)));

    let mut events = state.events.subscribe();
    let r = post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }], "origin": "claude: quieter HUD", "pins": ["main@8.600"] }));
    let hud = &pins()[1];
    assert_eq!((hud["done"].as_u64(), hud["done_by"].as_str()), (r["revision"].as_u64(), Some("agent")));
    assert_eq!(get(&addr, "/api/history?limit=1")["entries"][0]["pins"], json!(["main@8.600"]));

    post(&addr, "/api/pins/done", json!({ "id": "main@1.000", "done": true }));
    let dark = &pins()[0];
    assert_eq!((dark["done"].as_u64(), dark["done_by"].as_str()), (Some(revision(&state)), Some("user")));
    post(&addr, "/api/pins/done", json!({ "id": "main@8.600", "done": false }));
    assert_eq!(pins()[1]["done"], Value::Null, "an untick forgets the answer too");
    let (status, _) = call(&addr, "POST", "/api/pins/done", json!({ "id": "main@3.000", "done": true }));
    assert!(status.contains("404"), "an unlabelled marker is no pin: {status}");

    let mut types = Vec::new();
    while let Ok(ev) = events.try_recv() {
        types.push(serde_json::to_value(ev).unwrap()["type"].as_str().unwrap().to_string());
    }
    assert_eq!(types, ["project_changed", "history", "pins", "pins", "pins"]);
}

/// Sampled frames are served from `.kuma/samples/` and from nowhere else.
#[tokio::test(flavor = "multi_thread")]
async fn sampled_frames_are_served_from_the_samples_folder_only() {
    let (state, addr, _dir) = daemon().await;
    let dir = state.config.samples_dir().join("x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.png"), b"\x89PNG").unwrap();
    let (status, body) = request(&addr, "GET", "/api/samples/x/1.png", "");
    assert!(status.contains("200") && body == b"\x89PNG", "{status}");
    for path in ["/api/samples/../../t.kuma.json", "/api/samples/x/%2e%2e/%2e%2e/%2e%2e/t.kuma.json", "/api/samples/x/missing.png"] {
        let (status, body) = request(&addr, "GET", path, "");
        assert!(status.contains("400") || status.contains("404"), "{path}: {status}");
        assert!(!String::from_utf8_lossy(&body).contains("\"bin\""), "{path} leaked the project");
    }
}
