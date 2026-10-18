//! The text published beside a render, over HTTP: subtitles as SRT and chapters as YouTube
//! timestamps, of the master unless a sequence is named — whatever the editor has open —
//! returned, or written beside the renders.
//!
//! The project's subtitles folder is the shipped `subtitles` blueprint, so the names the
//! export reads off it (`cues`, `offset`, `words_per_line`) are held to the real manifest.
//! Needs no browser and no engine: a daemon on a loopback port and raw requests.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;

use kuma_daemon::{serve, AppState, Config, Libraries};
use serde_json::{json, Value};

const PROJECT: &str = r##"{
  "version": 9, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "black": { "name": "Black", "source": { "color": "#000000" } },
    "subs": { "name": "Talk subs", "source": { "overlay": "overlays/subtitles", "vars": { "words_per_line": 2, "offset": 100, "cues": [
      { "start": 100.5, "end": 101.0, "text": "Hello" }, { "start": 101.0, "end": 101.5, "text": "and" },
      { "start": 101.5, "end": 102.0, "text": "welcome." }, { "start": 114.0, "end": 115.0, "text": "Later." }
    ] } } },
    "talk": { "name": "The talk", "source": { "tracks": [
      { "id": "v1", "kind": "video", "clips": [ { "id": "c1", "start": 0, "end": 20, "bin": "black" } ] },
      { "id": "o1", "kind": "overlay", "clips": [ { "id": "s1", "start": 0, "end": 20, "bin": "subs" } ] }
    ] } },
    "outro": { "name": "Outro", "source": { "tracks": [ { "id": "v2", "kind": "video", "clips": [ { "id": "c2", "start": 0, "end": 15, "bin": "black" } ] } ] } },
    "main": { "name": "Main", "source": { "tracks": [ { "id": "v3", "kind": "video", "clips": [
      { "id": "m1", "start": 0, "end": 12, "bin": "talk", "label": "Getting started" },
      { "id": "m2", "start": 12, "end": 24, "bin": "outro" },
      { "id": "m3", "start": 24, "end": 30, "in": 12, "bin": "talk" }
    ] } ] } }
  }
}"##;

async fn daemon() -> (Arc<AppState>, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/blueprints/subtitles");
    let folder = dir.path().join("overlays/subtitles");
    std::fs::create_dir_all(&folder).unwrap();
    for f in ["overlay.json", "index.html"] {
        std::fs::copy(shipped.join(f), folder.join(f)).unwrap();
    }
    let config = Config { project_file, repo_root: dir.path().join("repo"), libraries: Libraries::new(dir.path().join("library")), bind: "127.0.0.1:0".parse().unwrap() };
    let (state, addr) = serve(config).await.unwrap();
    (state, addr.to_string(), dir)
}

/// One HTTP/1.1 POST: the status line and the JSON body.
fn post(addr: &str, path: &str, body: Value) -> (String, Value) {
    let body = body.to_string();
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(s, "POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head.lines().next().unwrap_or_default().to_string(), serde_json::from_str(body).unwrap_or(Value::Null))
}

fn ok(addr: &str, path: &str, body: Value) -> Value {
    let (status, v) = post(addr, path, body.clone());
    assert!(status.contains("200"), "{path} {body}: {status} {v}");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn subtitles_come_out_in_master_seconds_whatever_is_open() {
    let (state, addr, dir) = daemon().await;
    // What the editor has open is session state, and must not leak into an export.
    state.open_sequence(Some("talk".into())).unwrap();

    let v = ok(&addr, "/api/export/subtitles", json!({}));
    assert_eq!(v["sequence"], "main");
    // Two words a line, as the entry says; held to the next across the short pause. The
    // talk's 12..20 is cut from the first placement, and its 12..18 is placed again at 24.
    assert_eq!(
        v["text"],
        "1\n00:00:00,500 --> 00:00:01,500\nHello and\n\n2\n00:00:01,500 --> 00:00:02,000\nwelcome.\n\n3\n00:00:26,000 --> 00:00:27,000\nLater.\n\n"
    );
    assert_eq!((v["cues"].as_u64(), v["entries"].clone()), (Some(3), json!(["subs"])));
    assert_eq!(v["warnings"], json!([]));

    // Named, a sequence is in its own seconds; and longer captions are asked for.
    let v = ok(&addr, "/api/export/subtitles", json!({ "sequence": "talk", "words_per_line": 8 }));
    assert_eq!(v["text"], "1\n00:00:00,500 --> 00:00:02,000\nHello and welcome.\n\n2\n00:00:14,000 --> 00:00:15,000\nLater.\n\n");

    // Written, beside the renders, and the answer says where rather than repeating it.
    let v = ok(&addr, "/api/export/subtitles", json!({ "write": true }));
    assert_eq!(v["path"], "renders/t.srt");
    assert!(v.get("text").is_none(), "{v}");
    assert!(std::fs::read_to_string(dir.path().join("renders/t.srt")).unwrap().starts_with("1\n00:00:00,500 --> "));

    let (status, v) = post(&addr, "/api/export/subtitles", json!({ "sequence": "nope" }));
    assert!(status.contains("400"), "{status} {v}");
    let (status, v) = post(&addr, "/api/export/subtitles", json!({ "entries": ["black"] }));
    assert!(status.contains("400") && v["error"].as_str().unwrap().contains("not a subtitles entry"), "{status} {v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chapters_are_the_masters_sequence_clips() {
    let (state, addr, dir) = daemon().await;
    state.open_sequence(Some("outro".into())).unwrap();

    let v = ok(&addr, "/api/export/chapters", json!({}));
    assert_eq!(v["sequence"], "main");
    assert_eq!(v["text"], "0:00 Getting started\n0:12 Outro\n0:24 The talk\n");
    // The last runs to the end of the master, 6 s: YouTube would not take it.
    assert_eq!(v["warnings"], json!(["“The talk” (0:24) lasts 6 s; YouTube needs every chapter to last at least 10 s"]));

    let v = ok(&addr, "/api/export/chapters", json!({ "output": "desc/chapters.txt" }));
    assert_eq!(v["path"], "desc/chapters.txt");
    assert_eq!(std::fs::read_to_string(dir.path().join("desc/chapters.txt")).unwrap(), "0:00 Getting started\n0:12 Outro\n0:24 The talk\n");

    // A sequence that places none has none, and says how to have them.
    let v = ok(&addr, "/api/export/chapters", json!({ "sequence": "talk" }));
    assert_eq!(v["text"], "");
    assert!(v["warnings"][0].as_str().unwrap().contains("places no sequence"), "{v}");
}
