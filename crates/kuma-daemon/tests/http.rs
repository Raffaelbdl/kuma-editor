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
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "tag": { "name": "Tag", "source": { "overlay": "overlays/probe", "vars": {} } },
    "clips": { "name": "Clips", "source": { "folder": true } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "o1", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "o1c1", "start": 0, "end": 2, "bin": "tag", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

/// A daemon on a loopback port, with one overlay folder in the project, a one-blueprint
/// library and a stand-in `ui/dist`, so
/// the test needs nothing built and nothing installed. The tests run on a multi-thread
/// runtime because `get` blocks its thread, which on the default one starves the server.
async fn daemon() -> (Arc<AppState>, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();

    // The same folder twice: the project's own overlay, and a blueprint in the library.
    for folder in ["overlays/probe", "library/blueprints/probe"] {
        let tpl = dir.path().join(folder);
        std::fs::create_dir_all(&tpl).unwrap();
        std::fs::write(tpl.join("overlay.json"), r#"{ "name": "Probe" }"#).unwrap();
        std::fs::write(tpl.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    }
    // The shared `_vendor/` every overlay references, as `kuma library` installs it.
    // Without one, a scaffold is (rightly) told its `_vendor/…` files are missing.
    let vendor = dir.path().join("library/blueprints/_vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    for f in ["gsap.min.js", "kuma.js"] {
        std::fs::write(vendor.join(f), "").unwrap();
    }
    // The real `kuma.css` is what turns the `size` var into `--u`; a stub that did not
    // read `--var-size` would make the scaffold's `size` look unused, which it is not.
    std::fs::write(vendor.join("kuma.css"), ":root { --u: calc(1vh * var(--var-size, 1)); }").unwrap();

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

/// One HTTP/1.1 request: the status line, the headers (lowercased) and the body.
/// Hand-rolled rather than adding a client crate for a test. The body is read by
/// `content-length`, exactly — reading to EOF would wait on a close the server need not
/// do, and hang instead of failing.
fn request(addr: &str, method: &str, path: &str, body: &str) -> (String, std::collections::HashMap<String, String>, String) {
    use std::io::Read as _;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => raw.push(byte[0]),
            // A handler that spawns a subprocess (the lint does) reaps it with a signal,
            // and a signal interrupts this blocking read. `read_exact` below retries for
            // itself; this loop has to say so. Without it the suite fails only when the
            // tests that spawn and the tests that read overlap, which is to say randomly.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("{path}: reading the response headers: {e}"),
        }
    }
    let head = String::from_utf8_lossy(&raw).trim_end().to_string();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default().to_string();
    let headers: std::collections::HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).unwrap_or_else(|e| panic!("{path}: reading {len} bytes of body: {e}"));
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn get(addr: &str, path: &str) -> (String, std::collections::HashMap<String, String>) {
    let (status, headers, _) = request(addr, "GET", path, "");
    (status, headers)
}

fn get_body(addr: &str, path: &str) -> (String, String) {
    let (status, _, body) = request(addr, "GET", path, "");
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_served_without_a_cache_control() {
    let (_state, addr, _dir) = daemon().await;
    // The bundle, live project state, and a blueprint's files: all reloaded constantly,
    // all cacheable by heuristic before this.
    for path in ["/", "/api/project", "/api/health", "/blueprints/probe/index.html"] {
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

/// The overlay workbench iterates on a bin **entry**, which exists before any clip places
/// it — so previewing one must not need a clip, and the length it is looking at comes
/// from the request rather than from a placement.
#[tokio::test(flavor = "multi_thread")]
async fn an_overlay_entry_previews_without_a_placement() {
    let (_state, addr, _dir) = daemon().await;
    let (status, headers) = get(&addr, "/overlay-entry/tag");
    assert!(status.contains("200"), "{status}");
    // Live DOM being iterated on: the same "never reuse this" the clip route promises.
    assert_eq!(headers.get("cache-control").map(String::as_str), Some("no-store"), "{headers:?}");

    let (_, body) = get_body(&addr, "/overlay-entry/tag?duration=3");
    assert!(body.contains("\"duration\":3.0"), "the requested length did not reach the contract: {body}");
    // An overlay must look right in either orientation, so the workbench can ask for a
    // frame the project is not in. Nothing about the project changes with it.
    let (_, body) = get_body(&addr, "/overlay-entry/tag?width=1080&height=1920");
    assert!(body.contains("\"width\":1080") && body.contains("\"height\":1920"), "the requested frame did not reach the contract: {body}");

    // Nothing else may be reachable through it: the base href and the file route are the
    // overlay's folder, and only an overlay entry has one.
    let (status, _) = get(&addr, "/overlay-entry/main");
    assert!(status.contains("400"), "a sequence entry is not an overlay: {status}");
    let (status, _) = get(&addr, "/overlay-entry/nope");
    assert!(status.contains("404"), "{status}");
}

fn post(addr: &str, path: &str, body: &str) -> (String, serde_json::Value) {
    let (status, _, body) = request(addr, "POST", path, body);
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

/// The authoring routes, over the wire: a check that finds nothing on the fixture, a
/// refusal for a blueprint that is not there, and a scaffold that lands in the project's
/// own `overlays/`, is **not** a blueprint and **not** in the bin until it is imported,
/// and refuses to be repeated.
#[tokio::test(flavor = "multi_thread")]
async fn an_overlay_is_scaffolded_checked_and_imported_over_http() {
    let (state, addr, _dir) = daemon().await;
    let (status, body) = get_body(&addr, "/api/blueprints/probe/lint");
    assert!(status.contains("200"), "{status}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["overlay"], "probe");
    assert!(report["findings"].is_array());
    let (status, _) = get(&addr, "/api/blueprints/nope/lint");
    assert!(status.contains("400"), "{status}");
    let (status, _) = get(&addr, "/api/overlays/lint?path=overlays/probe");
    assert!(status.contains("200"), "{status}");

    let (status, made) = post(&addr, "/api/overlays/new", r#"{ "id": "fresh", "name": "Fresh" }"#);
    assert!(status.contains("200"), "{status}: {made}");
    assert_eq!(made["overlay"], "overlays/fresh");
    assert!(state.config.project_root().join("overlays/fresh/index.html").is_file());
    assert_eq!(made["lint"]["findings"].as_array().map(Vec::len), Some(0), "the scaffold is not clean: {}", made["lint"]);
    // Written is not imported, and a project's overlay is never a blueprint.
    assert!(!state.blueprint_infos().iter().any(|t| t.id == "fresh"), "a project overlay was loaded as a blueprint");
    assert!(!state.project.read().unwrap().bin.values().any(|i| matches!(&i.source, kuma_core::BinSource::Overlay { overlay, .. } if overlay == "overlays/fresh")));
    let (status, _) = post(&addr, "/api/overlays/new", r#"{ "id": "fresh" }"#);
    assert!(status.contains("400"), "a second scaffold overwrote the first: {status}");

    // The editor reads each folder's manifest here, not from a library listing.
    let (_, body) = get_body(&addr, "/api/overlays");
    let manifests: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(manifests["overlays/probe"]["manifest"]["name"], "Probe", "{manifests}");
    assert!(manifests.get("overlays/fresh").is_none(), "only folders the bin uses: {manifests}");

    // Imported by its manifest's path: an entry named after the folder, titled by the manifest.
    let (status, added) = post(&addr, "/api/overlays/import", r#"{ "path": "overlays/fresh/overlay.json" }"#);
    assert!(status.contains("200"), "{status}: {added}");
    assert_eq!(added["id"], "fresh");
    let entry = state.project.read().unwrap().bin["fresh"].clone();
    assert_eq!(entry.name, "Fresh");
    assert!(matches!(&entry.source, kuma_core::BinSource::Overlay { overlay, .. } if overlay == "overlays/fresh"), "{entry:?}");
    // A var the manifest does not declare is refused before anything is written.
    let (status, _) = post(&addr, "/api/overlays/import", r#"{ "path": "overlays/fresh", "vars": { "nope": 1 } }"#);
    assert!(status.contains("400"), "{status}");

    // A folder outside the project is not an overlay of it.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere.path().join("probe");
    kuma_overlay::new_overlay(elsewhere.path(), "probe", "", "").unwrap();
    let (status, reply) = post(&addr, "/api/overlays/import", &format!(r#"{{ "path": {:?} }}"#, outside.to_string_lossy()));
    assert!(status.contains("400") && reply["error"].as_str().is_some_and(|e| e.contains("outside the project")), "{status}: {reply}");
}

/// A blueprint reaches the bin by being copied: the entry points into the project, never
/// into the library, and a copy never lands on a folder the project already has.
#[tokio::test(flavor = "multi_thread")]
async fn a_blueprint_is_copied_into_the_project_not_referenced() {
    let (state, addr, dir) = daemon().await;
    let (status, added) = post(&addr, "/api/blueprints/probe/add", "{}");
    assert!(status.contains("200"), "{status}: {added}");
    // `overlays/probe` is the project's own, so the copy goes beside it.
    assert_eq!(added["overlay"], "overlays/probe-2");
    let id = added["id"].as_str().unwrap().to_string();
    assert!(matches!(&state.project.read().unwrap().bin[&id].source, kuma_core::BinSource::Overlay { overlay, .. } if overlay == "overlays/probe-2"));
    assert!(dir.path().join("overlays/probe-2/overlay.json").is_file());

    // The blueprint changing afterwards does not reach the project.
    std::fs::write(dir.path().join("library/blueprints/probe/index.html"), "changed").unwrap();
    assert_ne!(std::fs::read_to_string(dir.path().join("overlays/probe-2/index.html")).unwrap(), "changed");

    // The editor's copy route: a folder, and no edit.
    let before = state.project.read().unwrap().revision;
    let (status, copied) = post(&addr, "/api/blueprints/probe/copy", r#"{ "folder": "title" }"#);
    assert!(status.contains("200"), "{status}: {copied}");
    assert_eq!(copied["overlay"], "overlays/title");
    assert_eq!(state.project.read().unwrap().revision, before);
    let (status, _) = post(&addr, "/api/blueprints/nope/copy", "{}");
    assert!(status.contains("400"), "{status}");
}

/// Two entries of one folder share its HTML until one is made unique; then editing the
/// copy leaves the other alone. And a project overlay goes back to the library as a
/// blueprint, refusing to replace one unless told to.
#[tokio::test(flavor = "multi_thread")]
async fn an_entry_is_made_unique_and_an_overlay_saved_as_a_blueprint() {
    let (state, addr, dir) = daemon().await;
    let (status, twin) = post(&addr, "/api/overlays/import", r#"{ "path": "overlays/probe", "id": "twin" }"#);
    assert!(status.contains("200"), "{status}: {twin}");

    let (status, unique) = post(&addr, "/api/overlays/unique", r#"{ "bin": "twin" }"#);
    assert!(status.contains("200"), "{status}: {unique}");
    assert_eq!(unique["overlay"], "overlays/twin");
    let folder = |id: &str| match &state.project.read().unwrap().bin[id].source {
        kuma_core::BinSource::Overlay { overlay, .. } => overlay.clone(),
        _ => unreachable!(),
    };
    assert_eq!((folder("twin").as_str(), folder("tag").as_str()), ("overlays/twin", "overlays/probe"));
    assert!(dir.path().join("overlays/twin/index.html").is_file());
    let (status, _) = post(&addr, "/api/overlays/unique", r#"{ "bin": "main" }"#);
    assert!(status.contains("400"), "a sequence has no folder to copy: {status}");

    // Saved from an entry: its vars become the blueprint's defaults, the rest survives.
    std::fs::write(dir.path().join("overlays/twin/overlay.json"), r#"{ "name": "Twin", "vars": { "label": { "type": "string", "default": "Hello" } } }"#).unwrap();
    let edit = serde_json::from_value::<Vec<json_patch::PatchOperation>>(serde_json::json!([{ "op": "add", "path": "/bin/twin/source/vars/label", "value": "Paris" }])).unwrap();
    state.apply_patch(&edit, None, "test").unwrap();
    let save = |body: &str| post(&addr, "/api/overlays/save-blueprint", body).0;
    assert!(save(r#"{ "bin": "twin", "id": "twin" }"#).contains("200"));
    let saved = kuma_core::OverlayManifest::load(&dir.path().join("library/blueprints/twin")).unwrap();
    assert_eq!((saved.vars["label"].default.as_str(), saved.name.as_str()), (Some("Paris"), "Twin"));
    assert!(state.blueprint_infos().iter().any(|b| b.id == "twin"), "not listed after saving");
    assert!(save(r#"{ "bin": "twin", "id": "twin" }"#).contains("400"), "replaced without asking");
    assert!(save(r#"{ "bin": "twin", "id": "twin", "force": true }"#).contains("200"));
}

/// Media added into a folder is one edit; a parent not in the bin is refused.
#[tokio::test(flavor = "multi_thread")]
async fn media_is_added_into_a_folder_in_one_edit() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping add-media test");
        return;
    }
    let (state, addr, dir) = daemon().await;
    let media = dir.path().join("beep.wav");
    kuma_overlay::media::ffmpeg(&["-f".into(), "lavfi".into(), "-i".into(), "sine=duration=1".into(), media.to_string_lossy().into_owned()]).unwrap();
    let add = |id: &str, parent: &str| request(&addr, "POST", "/api/bin", &format!(r#"{{ "path": {:?}, "id": "{id}", "parent": "{parent}" }}"#, media.to_string_lossy()));
    let before = state.project.read().unwrap().revision;

    let (status, _, reply) = add("beep", "clips");
    assert!(status.contains("200"), "{status}: {reply}");
    let project = state.project.read().unwrap();
    assert_eq!(project.bin["beep"].parent.as_deref(), Some("clips"));
    assert_eq!(project.revision, before + 1, "one edit, not two");
    drop(project);

    let (status, _, _) = add("boop", "nope");
    assert!(status.starts_with("HTTP/1.1 4"), "a dangling parent was accepted: {status}");
    assert!(!state.project.read().unwrap().bin.contains_key("boop"));
}

/// The queue's routes: a listing a fresh window can rebuild from, and a cancel that tells
/// an unknown job from one that can no longer be stopped. A bad preset is refused when
/// the render is asked for, not discovered as a failed job.
#[tokio::test(flavor = "multi_thread")]
async fn jobs_are_listed_and_cancels_are_checked() {
    let (_state, addr, _dir) = daemon().await;
    let (status, body) = get_body(&addr, "/api/jobs?kind=render");
    assert!(status.contains("200"), "{status}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["jobs"], serde_json::json!([]));

    let (status, _, _) = request(&addr, "POST", "/api/jobs/42/cancel", "{}");
    assert!(status.contains("404"), "{status}");

    let (status, _, body) = request(&addr, "POST", "/api/render", r#"{"preset":"nope"}"#);
    assert!(status.contains("400") && body.contains("unknown preset"), "{status} {body}");
}

/// What a render would overwrite is said before it is asked for: the file on disk, and a
/// render still to finish that writes the same path.
#[tokio::test(flavor = "multi_thread")]
async fn a_render_target_says_what_it_would_overwrite() {
    let (_state, addr, dir) = daemon().await;
    let target = |q: &str| -> serde_json::Value { serde_json::from_str(&get_body(&addr, &format!("/api/render/target?{q}")).1).unwrap() };

    let t = target("preset=draft");
    assert_eq!(t["output"], serde_json::json!(dir.path().join("renders/t-draft.mp4")));
    assert_eq!((t["exists"].as_bool(), t["pending"].as_array().map(Vec::len)), (Some(false), Some(0)));

    std::fs::create_dir_all(dir.path().join("renders")).unwrap();
    std::fs::write(dir.path().join("renders/t-draft.mp4"), "").unwrap();
    assert_eq!(target("preset=draft")["exists"], true);
    assert_eq!(target("preset=draft&output=renders/other.mp4")["exists"], false);

    let (status, _) = get_body(&addr, "/api/render/target?preset=nope");
    assert!(status.contains("400"), "{status}");
}
