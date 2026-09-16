//! What the daemon promises over HTTP beyond the body: nothing it serves may be reused
//! without asking first. Without a `Cache-Control` a browser invents a freshness lifetime
//! from the file's age, so a rebuilt `ui/dist` had no effect on a window that had cached
//! the old one — it kept running month-old JavaScript against a current daemon.
//!
//! Needs no browser and no engine: a raw request against the router is enough.

use std::io::Write as _;
use std::sync::Arc;

use kuma_daemon::{serve, AppState, Config};

const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "tag": { "name": "Tag", "source": { "template": "probe", "vars": {} } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "o1", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "o1c1", "start": 0, "end": 2, "bin": "tag", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

/// A daemon on a loopback port, with a one-template library and a stand-in `ui/dist`, so
/// the test needs nothing built and nothing installed. The tests run on a multi-thread
/// runtime because `get` blocks its thread, which on the default one starves the server.
async fn daemon() -> (Arc<AppState>, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();

    let tpl = dir.path().join("library/overlays/probe");
    std::fs::create_dir_all(&tpl).unwrap();
    std::fs::write(tpl.join("template.json"), r#"{ "id": "probe", "name": "Probe" }"#).unwrap();
    std::fs::write(tpl.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();

    let ui = dir.path().join("repo/ui/dist");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join("index.html"), "<!doctype html><title>stand-in</title>").unwrap();

    let config = Config {
        project_file,
        repo_root: dir.path().join("repo"),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    let (state, addr) = serve(config).await.unwrap();
    (state, addr.to_string(), dir)
}

/// One HTTP/1.1 GET: the status line and the headers, lowercased. Hand-rolled rather than
/// adding a client crate for a test. Stops at the blank line ending the headers — reading
/// to EOF waits for a close the server need not do, and hangs instead of failing.
fn get(addr: &str, path: &str) -> (String, std::collections::HashMap<String, String>) {
    use std::io::Read as _;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => raw.push(byte[0]),
            Err(e) => panic!("{path}: reading the response headers: {e}"),
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let head = text.trim_end().to_string();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase()))
        .collect();
    (status, headers)
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_served_without_a_cache_control() {
    let (_state, addr, _dir) = daemon().await;
    // The bundle, live project state, and a template being authored: all reloaded
    // constantly, all cacheable by heuristic before this.
    for path in ["/", "/api/project", "/api/health", "/overlays/probe/index.html"] {
        let (status, headers) = get(&addr, path);
        assert!(status.contains("200"), "{path}: {status}");
        assert_eq!(
            headers.get("cache-control").map(String::as_str),
            Some("no-cache"),
            "{path} may be reused without revalidating (headers: {headers:?})",
        );
    }
}

/// The blanket header is `if_not_present`, so a handler that asked for something stricter
/// keeps it. The overlay preview is live DOM the editor re-reads on every edit.
#[tokio::test(flavor = "multi_thread")]
async fn a_handler_that_says_no_store_keeps_it() {
    let (_state, addr, _dir) = daemon().await;
    let (status, headers) = get(&addr, "/overlay-preview/o1c1");
    assert!(status.contains("200"), "{status}");
    assert_eq!(headers.get("cache-control").map(String::as_str), Some("no-store"), "{headers:?}");
}
