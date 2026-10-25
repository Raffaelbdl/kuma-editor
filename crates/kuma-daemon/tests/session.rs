//! The session state the editor reads beside the document: the agent's activity, the
//! revision history (with refusals and restores), and the markers' answers. None of it is in the project
//! file, all of it is over HTTP, and every change of it is broadcast.
//!
//! Needs no browser and no engine: a daemon on a loopback port and raw requests.

use std::io::{Read as _, Write as _};
use std::sync::Arc;

use kuma_daemon::{serve, AppState, Config, Libraries};
use serde_json::{json, Value};

const PROJECT: &str = r##"{
  "version": 9, "name": "t", "master": "main",
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
    let config = Config { project_file, repo_root: dir.path().join("repo"), libraries: Libraries::new(dir.path().join("library")), bind: "127.0.0.1:0".parse().unwrap() };
    // Its note for `kuma servers` goes beside the project, not among this machine's daemons.
    std::env::set_var("KUMA_DAEMONS_DIR", dir.path().join("daemons"));
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

/// A call that started no job is reported once it has ended, and kept finished: with the
/// revision before the one it made, and what the report said (a sample's moments and
/// frames); the list is newest first, each report is broadcast, and the old start/end
/// protocol is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_is_reported_once_it_has_ended() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let rev = revision(&state);
    let samples = state.config.samples_dir();

    let frames = json!({ "paths": [samples.join("x/1.png"), samples.join("x/2.png")], "sheet": null });
    let args = json!({ "t0": 46.0, "t1": 49.0, "rect": { "x": 0, "y": 0, "w": 960, "h": 540 } });
    let sample = post(&addr, "/api/activity", json!({ "phase": "call", "tool": "sample_frames", "client": "claude-code", "args": args, "ok": true, "error": null, "result": frames, "revision": null, "started_at": 1 }));
    let edit = post(&addr, "/api/activity", json!({ "phase": "call", "tool": "apply_edits", "args": { "patch": { "ops": 2 }, "note": "Moved + trimmed the umbrella", "answers": ["main@8.600"] }, "ok": false, "error": "409: stale", "result": null }));
    let (a, b) = (sample["id"].as_u64().unwrap(), edit["id"].as_u64().unwrap());
    assert!(b > a);

    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    assert_eq!(all.iter().map(|r| r["id"].as_u64().unwrap()).collect::<Vec<_>>(), [b, a], "newest first");
    let (e, s) = (&all[0], &all[1]);
    assert_eq!((e["state"].as_str(), e["client"].as_str(), e["summary"].as_str()), (Some("failed"), Some("agent"), Some("Moved + trimmed the umbrella")));
    assert_eq!((e["error"].as_str(), &e["args"]["answers"], &e["job"]), (Some("409: stale"), &json!(["main@8.600"]), &Value::Null));
    assert_eq!((s["state"].as_str(), s["client"].as_str(), s["revision_before"].as_u64(), s["revision_after"].as_u64()), (Some("done"), Some("claude-code"), Some(rev), Some(rev)));
    assert_eq!(s["sample"], json!({ "sequence": null, "overlay": null, "times": [46.0, 49.0], "rect": { "x": 0, "y": 0, "w": 960, "h": 540 } }));
    assert_eq!(s["frames"], json!(["x/1.png", "x/2.png"]));
    assert_eq!(s["started_at"].as_u64(), Some(1), "when the call started, not when it was reported");
    assert!(s["ended_at"].as_u64().unwrap() >= s["started_at"].as_u64().unwrap());
    assert_eq!(get(&addr, "/api/activity?limit=1")["activity"].as_array().unwrap().len(), 1);

    for old in [json!({ "phase": "start", "tool": "get_project", "args": {} }), json!({ "phase": "end", "id": a, "ok": true })] {
        let (status, _) = call(&addr, "POST", "/api/activity", old);
        assert!(status.contains("422") || status.contains("400"), "{status}");
    }
    assert_eq!(get(&addr, "/api/activity")["activity"].as_array().unwrap().len(), 2);

    let ev = events.recv().await.unwrap();
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!((v["type"].as_str(), v["tool"].as_str(), v["state"].as_str()), (Some("activity"), Some("sample_frames"), Some("done")), "{v}");
}

/// A call's `revision_after` is the revision it reported making and its `revision_before`
/// the one before; one that reported none is at the daemon's revision on both: an edit the
/// person made while it ran is not the call's.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_is_credited_only_with_the_revision_it_made() {
    let (state, addr, _dir) = daemon().await;
    let rev = revision(&state);
    let id = |v: Value| v["id"].as_u64().unwrap();
    let made = post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }], "origin": "mcp: rename" }))["revision"].as_u64().unwrap();
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Coal" }], "origin": "ui" }));
    assert_eq!(revision(&state), rev + 2);

    let edit = id(post(&addr, "/api/activity", json!({ "phase": "call", "tool": "apply_edits", "args": {}, "ok": true, "result": null, "revision": made })));
    let look = id(post(&addr, "/api/activity", json!({ "phase": "call", "tool": "get_project", "args": {}, "ok": true, "result": null })));
    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    let after = |id: u64| all.iter().find(|r| r["id"] == id).map(|r| (r["revision_before"].as_u64(), r["revision_after"].as_u64())).unwrap();
    assert_eq!(after(edit), (Some(rev), Some(rev + 1)), "its own revision, not the person's after it");
    assert_eq!(after(look), (Some(rev + 2), Some(rev + 2)), "no revision reported: it edited nothing");
}

/// The activity record of `job`, polled until it is final, beside the job as it then is.
fn finished(addr: &str, job: u64) -> (Value, Value) {
    let start = std::time::Instant::now();
    loop {
        let j = get(addr, &format!("/api/jobs/{job}"));
        let all = get(addr, "/api/activity")["activity"].as_array().unwrap().clone();
        let r = all.into_iter().find(|r| r["job"] == job).unwrap_or_else(|| panic!("no activity for job {job}"));
        if matches!(j["state"].as_str(), Some("done" | "failed" | "cancelled")) && r["state"] != "running" {
            return (r, j);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(60), "job {job} or its activity never ended: {j} {r}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A job the agent asked for (`origin: "mcp: …"` and `call` in its request) is that call's
/// activity: recorded running when the job is created, with the job's params, and finished
/// when the job ends, however it ends. Every job says who asked; one the editor, the command
/// line or an old client asked for has no activity, and clearing finished jobs leaves the
/// activity alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_an_agent_asked_for_is_its_activity() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let rev = revision(&state);

    // Fails at once: a colour has no audio.
    let failing = post(&addr, "/api/transcribe", json!({ "bin": "black", "origin": "mcp: transcribe", "call": { "tool": "transcribe", "client": "claude-code" } }))["job"].as_u64().unwrap();
    let v = serde_json::to_value(events.recv().await.unwrap()).unwrap();
    assert_eq!((v["type"].as_str(), v["id"].as_u64()), (Some("job"), Some(failing)), "{v}");
    let v = serde_json::to_value(events.recv().await.unwrap()).unwrap();
    assert_eq!((v["type"].as_str(), v["tool"].as_str(), v["job"].as_u64(), v["state"].as_str()), (Some("activity"), Some("transcribe"), Some(failing), Some("running")), "{v}");
    let (r, j) = finished(&addr, failing);
    assert_eq!(j["asked_by"], json!({ "author": "agent", "origin": "mcp: transcribe", "tool": "transcribe", "client": "claude-code" }));
    assert_eq!((r["state"].as_str(), r["client"].as_str(), &r["args"]), (Some("failed"), Some("claude-code"), &json!({ "bin": "black", "clip": null })));
    assert_eq!((r["error"].as_str(), r["revision_before"].as_u64(), r["revision_after"].as_u64()), (j["message"].as_str(), Some(rev), Some(rev)));

    let pruned = post(&addr, "/api/cache/prune", json!({ "all": false, "call": { "tool": "prune_cache", "client": null } }))["job"].as_u64().unwrap();
    let (r, j) = finished(&addr, pruned);
    assert_eq!((r["state"].as_str(), r["client"].as_str(), j["state"].as_str()), (Some("done"), Some("agent"), Some("done")));
    assert_eq!((j["asked_by"]["author"].as_str(), j["asked_by"]["origin"].as_str()), (Some("agent"), Some("agent: prune_cache")), "a call alone is the agent's");
    assert!(r["ended_at"].as_u64().unwrap() >= r["started_at"].as_u64().unwrap());

    // Done or failed (there may be no melt here), but what it looked at is known from the start.
    let rect = json!({ "x": 0, "y": 0, "w": 160, "h": 120 });
    let sampled = post(&addr, "/api/sample/job", json!({ "t0": 1, "t1": 3, "count": 2, "rect": rect, "origin": "mcp: sample_frames", "call": { "tool": "sample_frames", "client": "claude-code" } }))["job"].as_u64().unwrap();
    let (r, j) = finished(&addr, sampled);
    assert_eq!(r["sample"], json!({ "sequence": "main", "overlay": null, "times": [1.0, 3.0], "rect": rect }));
    assert_eq!(r["state"], j["state"]);
    assert_eq!(r["frames"].as_array().unwrap().is_empty(), j["state"] != "done", "{r}");

    // Stamped as the editor's, the command line's, or not at all (an old client).
    let settled = |body: Value| {
        let id = post(&addr, "/api/cache/prune", body)["job"].as_u64().unwrap();
        let start = std::time::Instant::now();
        while !matches!(get(&addr, &format!("/api/jobs/{id}"))["state"].as_str(), Some("done" | "failed")) {
            assert!(start.elapsed() < std::time::Duration::from_secs(60));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        (id, get(&addr, &format!("/api/jobs/{id}"))["asked_by"].clone())
    };
    let (by_ui, asked) = settled(json!({ "all": false, "origin": "ui: prune" }));
    assert_eq!(asked, json!({ "author": "user", "origin": "ui: prune", "tool": null, "client": null }));
    let (by_cli, asked) = settled(json!({ "all": false, "origin": "cli: cache prune" }));
    assert_eq!((asked["author"].as_str(), asked["origin"].as_str()), (Some("other"), Some("cli: cache prune")));
    let (quiet, asked) = settled(json!({ "all": false }));
    assert_eq!(asked, Value::Null);
    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    assert!(all.iter().all(|r| ![by_ui, by_cli, quiet].contains(&r["job"].as_u64().unwrap_or_default())), "only the agent's jobs are its activity: {all:?}");

    post(&addr, "/api/jobs/clear", Value::Null);
    assert!(state.jobs.get(failing).is_none());
    assert_eq!(get(&addr, "/api/activity")["activity"].as_array().unwrap().clone(), all, "clearing jobs forgets no activity");
}

/// A cancel finishes the record of the job it stops, queued or running: no call the agent
/// abandons, nor any it aborts with `cancel_job`, leaves a record running.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_job_finishes_its_activity() {
    use std::sync::atomic::Ordering;
    let (state, addr, _dir) = daemon().await;
    let call = || kuma_daemon::jobs::AskedBy::new(Some("mcp: render".into()), Some(kuma_daemon::activity::Call { tool: "render".into(), client: "claude-code".into() }));
    // A stand-in render: holds the lane until cancelled.
    let lane = |state: &Arc<AppState>| {
        let s2 = state.clone();
        kuma_daemon::jobs::spawn_cancellable(state.clone(), "render", json!({ "preset": "draft" }), call(), move |progress, cancel| {
            kuma_daemon::jobs::in_render_lane(&s2, cancel, progress, || {
                progress(10, "encoding");
                while !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(json!({}))
            })
        })
    };
    let running = lane(&state);
    let start = std::time::Instant::now();
    while get(&addr, &format!("/api/jobs/{running}"))["message"] != "encoding" {
        assert!(start.elapsed() < std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let queued = lane(&state);
    assert_eq!(get(&addr, &format!("/api/jobs/{queued}"))["state"], "queued");
    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    assert_eq!(all.iter().filter(|r| r["state"] == "running").count(), 2, "{all:?}");

    post(&addr, &format!("/api/jobs/{queued}/cancel"), json!({}));
    let (r, j) = finished(&addr, queued);
    assert_eq!((r["state"].as_str(), j["state"].as_str(), &r["error"]), (Some("cancelled"), Some("cancelled"), &Value::Null));
    post(&addr, &format!("/api/jobs/{running}/cancel"), json!({}));
    let (r, _) = finished(&addr, running);
    assert_eq!((r["state"].as_str(), &r["args"]), (Some("cancelled"), &json!({ "preset": "draft" })));
    let all = get(&addr, "/api/activity")["activity"].as_array().unwrap().clone();
    assert!(all.iter().all(|r| r["state"] != "running"), "{all:?}");
}

/// A job that panics fails, and so does the call that asked for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_panics_ends_its_activity() {
    let (state, addr, _dir) = daemon().await;
    let call = kuma_daemon::jobs::AskedBy::new(Some("mcp: bake_overlay".into()), Some(kuma_daemon::activity::Call { tool: "bake_overlay".into(), client: "claude-code".into() }));
    let id = kuma_daemon::jobs::spawn_with(state.clone(), "bake", Value::Null, call, |_| panic!("a bug"));
    let (r, j) = finished(&addr, id);
    assert_eq!((r["state"].as_str(), j["state"].as_str(), r["error"].as_str()), (Some("failed"), Some("failed"), Some("the job panicked")));
}

/// An edit, its undo (authored by whoever asked) and a refused stale patch are each one
/// entry, newest first, naming what they touched; each is broadcast.
#[tokio::test(flavor = "multi_thread")]
async fn the_history_names_edits_steps_and_refusals() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let rev = revision(&state);
    let patch = json!([{ "op": "replace", "path": "/bin/main/source/tracks/0/clips/1/start", "value": 2.5 }]);
    let edited = post(&addr, "/api/project/patch", json!({ "patch": patch, "base_revision": rev, "origin": "mcp: Moved + trimmed the umbrella" }));
    assert_eq!(edited["revision"], rev + 1);

    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    post(&addr, "/api/project/redo", Value::Null);
    let (status, _) = call(&addr, "POST", "/api/project/patch", json!({ "patch": patch, "base_revision": rev, "origin": "mcp: again" }));
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

/// The undo stack and the history keep documents as JSON, shared between them: undo walks
/// back as far as the stack goes (the oldest steps past its 200 dropped), redo forward, and a
/// revision an undo made restores to what that undo left.
#[tokio::test(flavor = "multi_thread")]
async fn undo_walks_back_through_many_edits() {
    let (state, _addr, _dir) = daemon().await;
    let name = |s: &AppState| s.project.read().unwrap().bin["black"].name.clone();
    let rename = |i: usize| serde_json::from_value::<Vec<json_patch::PatchOperation>>(json!([{ "op": "replace", "path": "/bin/black/name", "value": format!("n{i}") }])).unwrap();
    for i in 0..205 {
        state.apply_patch(&rename(i), None, "ui", &[]).unwrap();
    }
    let edited = revision(&state);
    for back in 1..=200 {
        let r = state.undo(Some("ui")).unwrap();
        assert_eq!((r, name(&state)), (Some(edited + back), format!("n{}", 204 - back)), "undo {back}");
    }
    assert_eq!(state.undo(None).unwrap(), None, "the oldest steps are dropped");
    let undone = revision(&state);
    assert_eq!(state.redo(None).unwrap(), Some(undone + 1));
    assert_eq!(name(&state), "n5");

    // r(edited + 10) is what the tenth undo left; its snapshot holds that document.
    let (rev, _) = state.restore(edited + 10, Some("ui")).unwrap();
    assert_eq!((rev, name(&state)), (undone + 2, "n194".to_string()));
    state.undo(None).unwrap();
    assert_eq!(name(&state), "n5", "the restore undoes");
}

/// The lists are written from their locks rather than copied into a `Value` first, and
/// read byte for byte as the `json!` they used to be built into.
#[tokio::test(flavor = "multi_thread")]
async fn the_lists_read_as_they_always_did() {
    let (state, addr, _dir) = daemon().await;
    post(&addr, "/api/activity", json!({ "phase": "call", "tool": "apply_edits", "client": "claude-code", "args": { "note": "rename", "answers": ["main@8.600"] }, "ok": true, "revision": 1 }));
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }], "origin": "mcp: rename", "answers": ["main@8.600"] }));
    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    let (status, _) = call(&addr, "POST", "/api/project/patch", json!({ "patch": [{ "op": "remove", "path": "/bin/nowhere" }] }));
    assert!(status.contains("422") || status.contains("409") || status.contains("400"), "{status}");

    let raw = |path: &str| {
        let (status, body) = request(&addr, "GET", path, "");
        assert!(status.contains("200"), "{path}: {status}");
        String::from_utf8(body).unwrap()
    };
    let history = json!({ "current": revision(&state), "entries": state.history.lock().unwrap().newest(kuma_daemon::history::ENTRIES) }).to_string();
    assert_eq!(raw("/api/history"), history);
    let activity = json!({ "activity": state.activity.lock().unwrap().newest(kuma_daemon::activity::RECORDS) }).to_string();
    assert_eq!(raw("/api/activity"), activity);
    assert_eq!(raw("/api/markers"), json!({ "markers": state.marker_list() }).to_string());
}

/// Every marker is listed, numbered in time order per sequence, the master's first; an
/// edit naming one answers it, and a tick by hand marks or clears it. Each of those sends
/// the whole list, as `GET /api/markers` has it.
#[tokio::test(flavor = "multi_thread")]
async fn markers_are_listed_answered_and_ticked() {
    let (state, addr, _dir) = daemon().await;
    let markers = || get(&addr, "/api/markers")["markers"].as_array().unwrap().clone();
    let all = markers();
    let listed: Vec<(&str, u64, bool)> = all.iter().map(|m| (m["id"].as_str().unwrap(), m["index"].as_u64().unwrap(), m["waiting"].as_bool().unwrap())).collect();
    assert_eq!(listed, [("main@1.000", 1, true), ("main@3.000", 2, false), ("main@8.600", 3, true), ("intro@0.500", 1, true)], "an empty label waits for nothing");
    assert_eq!((&all[2]["label"], &all[2]["done"], &all[2]["t"]), (&json!("this HUD corner fights with the subtitles"), &Value::Null, &json!(8.6)));
    assert_eq!((&all[2]["origin"], &all[2]["author"]), (&Value::Null, &json!("user")), "an older file's marker is a person's");

    let mut events = state.events.subscribe();
    let r = post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }], "origin": "mcp: quieter HUD", "answers": ["main@8.600"] }));
    let hud = &markers()[2];
    assert_eq!((hud["done"].as_u64(), hud["done_by"].as_str(), hud["waiting"].as_bool()), (r["revision"].as_u64(), Some("agent"), Some(false)));
    assert_eq!(get(&addr, "/api/history?limit=1")["entries"][0]["answers"], json!(["main@8.600"]));

    post(&addr, "/api/markers/done", json!({ "id": "main@1.000", "done": true }));
    let dark = &markers()[0];
    assert_eq!((dark["done"].as_u64(), dark["done_by"].as_str()), (Some(revision(&state)), Some("user")));
    post(&addr, "/api/markers/done", json!({ "id": "main@8.600", "done": false }));
    assert_eq!((&markers()[2]["done"], &markers()[2]["waiting"]), (&Value::Null, &json!(true)), "an untick forgets the answer too");
    post(&addr, "/api/markers/done", json!({ "id": "main@3.000", "done": true }));
    assert_eq!(markers()[1]["done_by"], "user", "any marker can be ticked");
    post(&addr, "/api/markers/done", json!({ "id": "main@3.000", "done": true, "origin": "cli: tick" }));
    assert_eq!(markers()[1]["done_by"], "other", "a tick says who ticked it, as an edit does");
    let (status, _) = call(&addr, "POST", "/api/markers/done", json!({ "id": "main@9.000", "done": true }));
    assert!(status.contains("404"), "no marker there: {status}");

    let sent = drain(&mut events);
    let types: Vec<&str> = sent.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["project_changed", "history", "markers", "markers", "markers", "markers", "markers"]);
    assert_eq!(sent[2]["markers"][2]["done"], r["revision"], "the answer is in the list sent");
    assert_eq!(sent[6], json!({ "type": "markers", "markers": markers() }), "the last list sent is the list");
}

/// The markers are sent when an edit, an undo or a redo changes one, and not for an edit
/// that leaves them alone.
#[tokio::test(flavor = "multi_thread")]
async fn the_markers_are_sent_when_they_change() {
    let (state, addr, _dir) = daemon().await;
    let mut events = state.events.subscribe();
    let types = |sent: &[Value]| sent.iter().map(|v| v["type"].as_str().unwrap().to_string()).collect::<Vec<_>>();

    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/black/name", "value": "Ink" }] }));
    assert_eq!(types(&drain(&mut events)), ["project_changed", "history"], "no marker changed");

    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/main/source/markers/1/label", "value": "far too dark" }] }));
    let sent = drain(&mut events);
    assert_eq!(types(&sent), ["project_changed", "history", "markers"]);
    assert_eq!(sent[2]["markers"][0]["label"], "far too dark");

    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    let sent = drain(&mut events);
    assert_eq!(types(&sent), ["project_changed", "history", "markers"]);
    assert_eq!(sent[2]["markers"][0]["label"], "too dark");

    post(&addr, "/api/project/undo", json!({ "origin": "ui" }));
    assert_eq!(types(&drain(&mut events)), ["project_changed", "history"], "undoing the rename");
}

/// A marker says who left it: the daemon stamps the adding edit's origin into the
/// document, keeps it whoever edits the marker after, and carries it over when a client
/// writes the whole array back without it — so the agent's own markers are never read back
/// to it as requests.
#[tokio::test(flavor = "multi_thread")]
async fn a_marker_says_who_left_it() {
    let (state, addr, dir) = daemon().await;
    let at = |t: f64| get(&addr, "/api/markers")["markers"].as_array().unwrap().iter().find(|m| m["t"] == json!(t)).cloned().unwrap();
    let add = |t: f64, label: &str, origin: &str| post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "add", "path": "/bin/main/source/markers/-", "value": { "t": t, "label": label } }], "origin": origin }));

    add(5.0, "cut the pause", "ui: marker at 0:05");
    add(6.0, "TODO: the zone starts here", "mcp: mark the zone");
    add(7.0, "a note", "");
    let ours = at(5.0);
    assert_eq!((&ours["origin"], &ours["author"], &ours["waiting"]), (&json!("ui"), &json!("user"), &json!(true)));
    let theirs = at(6.0);
    assert_eq!((&theirs["origin"], &theirs["author"], &theirs["waiting"]), (&json!("mcp"), &json!("agent"), &json!(false)), "the agent's own waits for nobody");
    assert_eq!(at(7.0)["origin"], "ui", "an origin-less edit is the editor's");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("t.kuma.json")).unwrap()).unwrap();
    assert_eq!(saved["bin"]["main"]["source"]["markers"][4]["origin"], "mcp", "stamped in the file");
    assert!(saved["bin"]["main"]["source"]["markers"][0].get("origin").is_none(), "an older marker is left unattributed");

    // The other party edits each: the creator stays.
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/main/source/markers/3/label", "value": "cut the whole pause" }], "origin": "mcp: reword" }));
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/main/source/markers/4/color", "value": "#0f0" }], "origin": "ui" }));
    assert_eq!((&at(5.0)["origin"], &at(6.0)["origin"]), (&json!("ui"), &json!("mcp")));

    // A client rewriting the whole array without origins re-attributes nothing.
    let bare: Vec<Value> = state.project.read().unwrap().seq("main").unwrap().markers.iter().map(|m| json!({ "t": m.t, "label": m.label, "color": m.color })).collect();
    post(&addr, "/api/project/patch", json!({ "patch": [{ "op": "replace", "path": "/bin/main/source/markers", "value": bare }], "origin": "ui" }));
    assert_eq!((&at(5.0)["origin"], &at(6.0)["origin"], &at(1.0)["origin"]), (&json!("ui"), &json!("mcp"), &Value::Null));
    let kept = state.history.lock().unwrap().snapshot(revision(&state)).unwrap();
    let kept = kuma_daemon::history::parse(&kept).unwrap();
    assert_eq!(kept.seq("main").unwrap().markers[4].origin.as_deref(), Some("mcp"), "the document the history kept is the stamped one");
}

/// Every event sent so far, as JSON.
fn drain(events: &mut tokio::sync::broadcast::Receiver<kuma_daemon::state::Event>) -> Vec<Value> {
    std::iter::from_fn(|| events.try_recv().ok()).map(|ev| serde_json::to_value(ev).unwrap()).collect()
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
