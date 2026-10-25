//! What the daemon promises over HTTP beyond the body: nothing it serves may be reused
//! without asking first. Without a `Cache-Control` a browser invents a freshness lifetime
//! from the file's age, so a rebuilt `ui/dist` had no effect on a window that had cached
//! the old one — it kept running month-old JavaScript against a current daemon.
//!
//! Needs no browser and no engine: a raw request against the router is enough.

use std::io::Write as _;
use std::sync::Arc;

use kuma_daemon::{serve, AppState, Config, Libraries};

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
        libraries: Libraries::new(dir.path().join("library")),
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    // Its note for `kuma servers` goes beside the project, not among this machine's daemons.
    std::env::set_var("KUMA_DAEMONS_DIR", dir.path().join("daemons"));
    let (state, addr) = serve(config).await.unwrap();
    (state, addr.to_string(), dir)
}

/// One HTTP/1.1 request: the status line, the headers (lowercased) and the body.
/// Hand-rolled rather than adding a client crate for a test. The body is read by
/// `content-length`, exactly — reading to EOF would wait on a close the server need not
/// do, and hang instead of failing.
fn request(addr: &str, method: &str, path: &str, body: &str) -> (String, std::collections::HashMap<String, String>, String) {
    request_with(addr, method, path, body, "")
}

/// [`request`] with `extra` headers, each ending in `\r\n`.
fn request_with(addr: &str, method: &str, path: &str, body: &str, extra: &str) -> (String, std::collections::HashMap<String, String>, String) {
    use std::io::Read as _;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
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

    // A folder from outside the project is copied in (beside `overlays/probe`), then imported.
    let elsewhere = tempfile::tempdir().unwrap();
    kuma_overlay::new_overlay(elsewhere.path(), "probe", "", "").unwrap();
    let manifest = elsewhere.path().join("probe/overlay.json");
    let (status, reply) = post(&addr, "/api/bin", &format!(r#"{{ "path": {:?} }}"#, manifest.to_string_lossy()));
    assert!(status.contains("200"), "{status}: {reply}");
    let id = reply["id"].as_str().unwrap().to_string();
    assert!(matches!(&state.project.read().unwrap().bin[&id].source, kuma_core::BinSource::Overlay { overlay, .. } if overlay == "overlays/probe-2"));
    assert!(state.config.project_root().join("overlays/probe-2/index.html").is_file());
}

/// A blueprint reaches the bin by being copied: the entry points into the project, never
/// into the library, and a copy never lands on a folder the project already has.
#[tokio::test(flavor = "multi_thread")]
async fn a_blueprint_is_copied_into_the_project_not_referenced() {
    let (state, addr, dir) = daemon().await;
    // Copying is not an edit, and never lands on a folder the project has.
    let before = state.project.read().unwrap().revision;
    let (status, copied) = post(&addr, "/api/blueprints/probe/copy", "{}");
    assert!(status.contains("200"), "{status}: {copied}");
    assert_eq!(copied["overlay"], "overlays/probe-2");
    assert_eq!(state.project.read().unwrap().revision, before);
    let (status, _) = post(&addr, "/api/blueprints/nope/copy", "{}");
    assert!(status.contains("400"), "{status}");
    // The copy can be named for what it holds; `null` is no name, as the MCP tool sends it.
    let (_, named) = post(&addr, "/api/blueprints/probe/copy", r#"{ "folder": "kanji-title" }"#);
    assert_eq!(named["overlay"], "overlays/kanji-title");
    let (_, unnamed) = post(&addr, "/api/blueprints/probe/copy", r#"{ "folder": null }"#);
    assert_eq!(unnamed["overlay"], "overlays/probe-3");
    let (status, _) = post(&addr, "/api/blueprints/probe/copy", r#"{ "folder": "../out" }"#);
    assert!(status.contains("400"), "a folder name is a name, not a path: {status}");

    // Adding the folder by path, as a drop does, imports it.
    let (status, added) = post(&addr, "/api/bin", r#"{ "path": "overlays/probe-2" }"#);
    assert!(status.contains("200"), "{status}: {added}");
    let id = added["id"].as_str().unwrap().to_string();
    assert!(matches!(&state.project.read().unwrap().bin[&id].source, kuma_core::BinSource::Overlay { overlay, .. } if overlay == "overlays/probe-2"));

    // The blueprint changing afterwards does not reach the project.
    std::fs::write(dir.path().join("library/blueprints/probe/index.html"), "changed").unwrap();
    assert_ne!(std::fs::read_to_string(dir.path().join("overlays/probe-2/index.html")).unwrap(), "changed");
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
    state.apply_patch(&edit, None, "test", &[]).unwrap();
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

/// What the timeline draws on a media clip: a waveform at `PEAK_RATE` per second and a
/// filmstrip with one frame per stretch of the file, made on first asking, kept, and made
/// again when the entry points at another file. Looking at them is not an edit.
#[tokio::test(flavor = "multi_thread")]
async fn a_media_entry_has_a_waveform_and_a_filmstrip() {
    if !kuma_overlay::media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping media view test");
        return;
    }
    let (state, addr, dir) = daemon().await;
    let clip = dir.path().join("clip.mp4");
    let args = "-f lavfi -i testsrc=size=160x90:rate=10:duration=6 -f lavfi -i sine=duration=6 -shortest -pix_fmt yuv420p";
    let mut args: Vec<String> = args.split(' ').map(String::from).collect();
    args.push(clip.to_string_lossy().into_owned());
    kuma_overlay::media::ffmpeg(&args).unwrap();
    let (status, _, reply) = request(&addr, "POST", "/api/bin", &format!(r#"{{ "path": {:?}, "id": "clip" }}"#, clip.to_string_lossy()));
    assert!(status.contains("200"), "{status}: {reply}");
    let revision = state.project.read().unwrap().revision;

    let (status, body) = get_body(&addr, "/api/bin/clip/peaks.json");
    assert!(status.contains("200"), "{status}: {body}");
    let peaks: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(peaks["rate"], kuma_overlay::PEAK_RATE);
    let n = peaks["peaks"].as_array().unwrap().len() as i64;
    assert!((n - 600).abs() <= 2, "six seconds at 100 a second, not {n}");
    // lavfi's sine is at an eighth of full scale.
    assert!(peaks["peaks"][300].as_u64().is_some_and(|p| (28..=36).contains(&p)), "{}", peaks["peaks"][300]);

    let (status, body) = get_body(&addr, "/api/bin/clip/strip.json");
    assert!(status.contains("200"), "{status}: {body}");
    let strip: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(strip["strip"]["count"], 3);
    assert_eq!(strip["strip"]["step"], 2.0);
    assert_eq!((strip["strip"]["width"].as_u64(), strip["strip"]["height"].as_u64()), (Some(96), Some(54)));
    let (status, headers) = get(&addr, "/api/bin/clip/strip.jpg");
    assert!(status.contains("200"), "{status}");
    assert_eq!(headers.get("content-type").map(String::as_str), Some("image/jpeg"));

    // Kept: asked again, the same file answers, untouched.
    let kept = state.config.thumb_path("clip", "strip.json");
    let made = std::fs::metadata(&kept).unwrap().modified().unwrap();
    get_body(&addr, "/api/bin/clip/strip.json");
    assert_eq!(std::fs::metadata(&kept).unwrap().modified().unwrap(), made);
    assert_eq!(state.project.read().unwrap().revision, revision, "looking is not an edit");

    // Not every entry has them: an overlay has neither.
    let (status, _) = get_body(&addr, "/api/bin/tag/peaks.json");
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
}

/// The viewer plays a media entry's own file: served whole, or in the ranges a `<video>`
/// asks for to seek, and from the proxy when the entry has one. Nothing else is a file to
/// serve, and looking is not an edit.
#[tokio::test(flavor = "multi_thread")]
async fn a_media_entry_is_served_for_the_viewer_in_ranges() {
    let (state, addr, dir) = daemon().await;
    std::fs::write(dir.path().join("take.wav"), b"0123456789").unwrap();
    std::fs::write(dir.path().join("take-proxy.wav"), b"proxy").unwrap();
    let rev = state.project.read().unwrap().revision;
    let entry = r#"{ "name": "Take", "source": { "path": "take.wav", "kind": "audio" }, "meta": { "duration": 1, "has_audio": true } }"#;
    let (status, reply) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch": [{{ "op": "add", "path": "/bin/take", "value": {entry} }}], "base_revision": {rev} }}"#));
    assert!(status.contains("200"), "{status}: {reply}");
    let revision = state.project.read().unwrap().revision;

    let (status, headers, body) = request(&addr, "GET", "/api/bin/take/source", "");
    assert!(status.contains("200"), "{status}");
    assert_eq!(body, "0123456789");
    assert_eq!(headers.get("accept-ranges").map(String::as_str), Some("bytes"), "a browser seeks only what says it can");
    assert!(headers.get("content-type").is_some_and(|t| t.starts_with("audio/")), "{headers:?}");

    let (status, headers, body) = request_with(&addr, "GET", "/api/bin/take/source", "", "Range: bytes=2-5\r\n");
    assert!(status.contains("206"), "{status}");
    assert_eq!(body, "2345");
    assert_eq!(headers.get("content-range").map(String::as_str), Some("bytes 2-5/10"));
    assert_eq!(state.project.read().unwrap().revision, revision, "looking is not an edit");

    // A proxy stands in for the file here as it does in the preview compile.
    let (status, reply) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch": [{{ "op": "add", "path": "/bin/take/source/proxy", "value": "take-proxy.wav" }}], "base_revision": {revision} }}"#));
    assert!(status.contains("200"), "{status}: {reply}");
    assert_eq!(get_body(&addr, "/api/bin/take/source").1, "proxy");

    // Only media is served: an entry pointed at any other file gets nothing of it.
    std::fs::write(dir.path().join("secret.txt"), b"key").unwrap();
    let (status, reply) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch": [{{ "op": "replace", "path": "/bin/take/source/proxy", "value": {:?} }}], "base_revision": {} }}"#, dir.path().join("secret.txt"), revision + 1));
    assert!(status.contains("200"), "{status}: {reply}");
    let (status, body) = get_body(&addr, "/api/bin/take/source");
    assert!(status.starts_with("HTTP/1.1 403") && !body.contains("key"), "{status}: {body}");

    // A file that is not there says which, so the viewer can tell it from a daemon older
    // than the editor, whose 404 for this path is a bare "not found".
    let (status, reply) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch": [{{ "op": "replace", "path": "/bin/take/source/proxy", "value": "gone.wav" }}], "base_revision": {} }}"#, revision + 2));
    assert!(status.contains("200"), "{status}: {reply}");
    let (status, body) = get_body(&addr, "/api/bin/take/source");
    assert!(status.starts_with("HTTP/1.1 404") && body.contains("gone.wav is not on disk"), "{status}: {body}");

    // An overlay has no file of its own to play, and an entry that is not there has nothing.
    for id in ["tag", "nope"] {
        let (status, _) = get_body(&addr, &format!("/api/bin/{id}/source"));
        assert!(status.starts_with("HTTP/1.1 404"), "{id}: {status}");
    }
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

/// `_vendor/<file>` is served from the first folder that has that file: the overlay's own
/// `_vendor/`, the one beside it, the library's. A project's shared files beside its
/// overlays add to kuma's runtime rather than hiding it, as the bake stages them.
#[tokio::test(flavor = "multi_thread")]
async fn a_vendor_file_is_served_from_the_first_folder_that_has_it() {
    let (state, addr, _dir) = daemon().await;
    let overlays = state.config.project_root().join("overlays");
    std::fs::create_dir_all(overlays.join("_vendor/fonts")).unwrap();
    std::fs::create_dir_all(overlays.join("probe/_vendor")).unwrap();
    std::fs::write(overlays.join("_vendor/house.css"), "beside").unwrap();
    std::fs::write(overlays.join("_vendor/fonts/house.woff2"), "beside font").unwrap();
    std::fs::write(overlays.join("_vendor/kuma.js"), "beside kuma").unwrap();
    std::fs::write(overlays.join("probe/_vendor/kuma.js"), "own kuma").unwrap();
    for (path, body) in [("house.css", "beside"), ("fonts/house.woff2", "beside font"), ("kuma.js", "own kuma"), ("gsap.min.js", "")] {
        for route in ["/overlay-entry/tag", "/overlay-preview/o1c1"] {
            let (status, got) = get_body(&addr, &format!("{route}/_vendor/{path}"));
            assert!(status.contains("200") && got == body, "{route}/_vendor/{path}: {status} {got:?}");
        }
    }
    let (status, _) = get_body(&addr, "/overlay-entry/tag/_vendor/nope.js");
    assert!(status.contains("404"), "{status}");
}

/// A font is added by path, is copied into the project's `fonts/` (or,
/// shared, the library's) and is then what a `font` var draws with: the preview document
/// carries its `@font-face` and serves the file from the path the rule names, the one a
/// bake stages it at.
#[tokio::test(flavor = "multi_thread")]
async fn a_font_is_added_and_an_overlay_draws_with_it() {
    let (state, addr, dir) = daemon().await;
    let (_, body) = get_body(&addr, "/api/fonts");
    assert_eq!(body, "[]", "no fonts in the fixture library");

    let anton = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/fonts/Anton/Anton-Regular.ttf").canonicalize().unwrap();
    let add = |path: &std::path::Path, shared: bool| post(&addr, "/api/fonts", &serde_json::json!({ "path": path, "shared": shared }).to_string());
    let (status, added) = add(&anton, false);
    assert!(status.contains("200"), "{status}: {added}");
    assert_eq!(added["family"], "Anton");
    let file = state.config.project_root().join("fonts/Anton/Anton-Regular.ttf");
    assert!(file.is_file(), "{added}");
    // The same bytes again are the same file, not a second copy.
    let (_, again) = add(&anton, false);
    assert_eq!(again["path"], added["path"]);
    // Relative to the project, as media is; refused when it is not a font, or not there.
    let (_, relative) = add(std::path::Path::new("fonts/Anton/Anton-Regular.ttf"), false);
    assert_eq!(relative["path"], added["path"], "{relative}");
    std::fs::write(dir.path().join("notes.ttf"), b"not a font").unwrap();
    let (status, _) = add(&dir.path().join("notes.ttf"), false);
    assert!(status.contains("400"), "{status}");
    std::fs::write(dir.path().join("notes.woff2"), b"wOF2 but not a font").unwrap();
    let (status, refused) = add(&dir.path().join("notes.woff2"), false);
    assert!(status.contains("400") && refused["error"].as_str().unwrap_or_default().contains("does not decode"), "{status}: {refused}");
    let (status, _) = add(&dir.path().join("nope.ttf"), false);
    assert!(status.contains("400"), "{status}");
    // Shared goes to the user library; the project's copy still wins.
    let (_, shared) = add(&anton, true);
    assert!(dir.path().join("library/fonts/Anton/Anton-Regular.ttf").is_file(), "{shared}");
    let (_, body) = get_body(&addr, "/api/fonts");
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list[0]["name"], "Anton");
    assert_eq!(list[0]["origin"], "project", "{list}");

    // An overlay with a font var, imported; its preview names and serves the face.
    let typed = state.config.project_root().join("overlays/typed");
    std::fs::create_dir_all(&typed).unwrap();
    std::fs::write(typed.join("overlay.json"), r#"{ "name": "Typed", "vars": { "font": { "type": "font", "default": "Anton" } } }"#).unwrap();
    std::fs::write(typed.join("index.html"), "<html><head></head><body data-composition-id=\"root\"></body></html>").unwrap();
    let (status, imported) = post(&addr, "/api/overlays/import", r#"{ "path": "overlays/typed" }"#);
    assert!(status.contains("200"), "{status}: {imported}");
    let (_, doc) = get_body(&addr, "/overlay-entry/typed");
    assert!(doc.contains("--var-font: \"Anton\";"), "{doc}");
    assert!(doc.contains("url(\"/fonts/anton/0.ttf\")"), "{doc}");
    let (status, headers) = get(&addr, "/fonts/anton/0.ttf");
    assert!(status.contains("200"), "{status}");
    assert_eq!(headers.get("content-type").map(String::as_str), Some("font/ttf"));
    let (status, _) = get(&addr, "/fonts/anton/7.ttf");
    assert!(status.contains("404"), "{status}");

    // The editor's own page gets every family, from the same route, for the picker.
    let (_, css) = get_body(&addr, "/api/fonts.css");
    assert!(css.contains("url(\"/fonts/anton/0.ttf\")"), "{css}");

    // A web font goes in as the font it wraps, which a font folder reads.
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kuma-overlay/tests/fixtures/fonts").canonicalize().unwrap();
    let (status, woff2) = add(&web.join("BebasNeue-Regular.woff2"), false);
    assert!(status.contains("200"), "{status}: {woff2}");
    assert_eq!(woff2["family"], "Bebas Neue");
    assert!(woff2["path"].as_str().unwrap().ends_with("fonts/Bebas Neue/BebasNeue-Regular.ttf"), "{woff2}");
    let (_, again) = add(&web.join("BebasNeue-Regular.woff2"), false);
    assert_eq!(again["path"], woff2["path"], "unwrapping is deterministic: {again}");
    let (status, woff) = add(&web.join("BebasNeue-Regular.woff"), false);
    assert!(status.contains("200") && woff["family"] == "Bebas Neue", "{status}: {woff}");
    let (_, body) = get_body(&addr, "/api/fonts");
    assert!(body.contains("\"Bebas Neue\""), "{body}");
    // A font named as a web font that is not one is kept as what it is.
    std::fs::copy(&anton, dir.path().join("Anton.woff2")).unwrap();
    let (_, misnamed) = add(&dir.path().join("Anton.woff2"), false);
    assert!(misnamed["path"].as_str().unwrap().ends_with("fonts/Anton/Anton.ttf"), "{misnamed}");
}

/// A patch a script wrote is applied from its file, exactly, as one revision-checked edit;
/// a file outside the project, one that is not a patch, and a request that names both a
/// patch and a file are refused before anything is applied.
#[tokio::test(flavor = "multi_thread")]
async fn a_patch_is_applied_from_a_file_in_the_project() {
    let (state, addr, _dir) = daemon().await;
    let root = state.config.project_root();
    std::fs::create_dir_all(root.join("patches")).unwrap();
    let ops: Vec<serde_json::Value> = (0..200).map(|i| serde_json::json!({ "op": "add", "path": format!("/bin/c{i}"), "value": { "name": format!("Colour {i}"), "source": { "color": "#102030" } } })).collect();
    std::fs::write(root.join("patches/diff.json"), serde_json::to_string(&ops).unwrap()).unwrap();
    let rev = state.project.read().unwrap().revision;

    let (status, body) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch_path": "./patches/diff.json", "base_revision": {rev} }}"#));
    assert!(status.contains("200"), "{status}: {body}");
    assert_eq!(body["ops"], 200);
    let p = state.project.read().unwrap().clone();
    assert_eq!(p.revision, rev + 1, "one edit, one undo step");
    assert_eq!(p.bin["c199"].name, "Colour 199");

    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("outside.json"), "[]").unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("outside.json"), root.join("patches/link.json")).unwrap();
    std::fs::write(root.join("patches/notes.json"), r#"{ "patch": [] }"#).unwrap();
    for (body, says) in [
        (serde_json::json!({ "patch_path": "patches/link.json" }), "not inside the project"),
        (serde_json::json!({ "patch_path": elsewhere.path().join("outside.json") }), "not inside the project"),
        (serde_json::json!({ "patch_path": "patches/notes.json" }), "not a JSON Patch"),
        (serde_json::json!({ "patch_path": "patches/missing.json" }), "cannot read"),
        (serde_json::json!({ "patch_path": "patches/diff.json", "patch": [] }), "one of the two"),
        (serde_json::json!({}), "one of the two"),
    ] {
        let (status, answer) = post(&addr, "/api/project/patch", &body.to_string());
        assert!(status.contains("400") && answer["error"].as_str().unwrap().contains(says), "{body}: {status} {answer}");
    }
    assert_eq!(state.project.read().unwrap().revision, rev + 1, "nothing refused was applied");
}

/// A script's own document comes back in the form the daemon stores one, as the text of
/// the file: the same bytes an edit writes, so the two can be diffed. Nothing is edited.
#[tokio::test(flavor = "multi_thread")]
async fn a_document_is_canonicalised_as_the_daemon_stores_it() {
    let (state, addr, _dir) = daemon().await;
    // The project as a script would write it: an old version, and a default spelled out.
    let script = PROJECT.replace(r#""version": 7"#, r#""version": 1"#).replace(r#""label": """#, r#""label": "", "group": """#);
    let rev = state.project.read().unwrap().revision;
    let (status, _, body) = request(&addr, "POST", "/api/project/canonical", &script);
    assert!(status.contains("200"), "{status}: {body}");
    assert_eq!(body, kuma_core::canonical(&script).unwrap());
    assert!(!body.contains("\"group\""), "an empty group is not stored: {body}");

    // An edit writes the file in exactly that form.
    let (status, _) = post(&addr, "/api/project/patch", &format!(r#"{{ "patch": [{{ "op": "replace", "path": "/name", "value": "t" }}], "base_revision": {rev} }}"#));
    assert!(status.contains("200"), "{status}");
    let stored = std::fs::read_to_string(&state.config.project_file).unwrap();
    assert_eq!(kuma_core::canonical(&stored).unwrap(), stored, "the file is its own canonical form");
    assert_eq!(body.replace(r#""revision": 0"#, r#""revision": 1"#), stored, "only the revision differs");

    let (status, answer) = post(&addr, "/api/project/canonical", r#"{ "fps": "fast" }"#);
    assert!(status.contains("400") && answer["error"].is_string(), "{status} {answer}");
}

/// `validate_project` says what an entry will not be drawn in, pointing at the var: a font
/// that is not installed, and text its fonts cannot set on a page in another language.
/// `check_overlay` only ever saw a folder's defaults, and a render fell back quietly.
#[tokio::test(flavor = "multi_thread")]
async fn validation_names_text_the_fonts_will_not_draw() {
    let (state, addr, _dir) = daemon().await;
    let fonts = state.config.project_root().join("fonts/Anton");
    std::fs::create_dir_all(&fonts).unwrap();
    std::fs::copy(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/fonts/Anton/Anton-Regular.ttf"), fonts.join("Anton-Regular.ttf")).unwrap();
    let label = state.config.project_root().join("overlays/label");
    std::fs::create_dir_all(&label).unwrap();
    std::fs::write(label.join("overlay.json"), r#"{ "name": "Label", "vars": { "font": { "type": "font", "default": "Anton" }, "label": { "type": "text", "default": "Hello" } } }"#).unwrap();
    std::fs::write(label.join("index.html"), "<html lang=\"en\"><head></head><body data-composition-id=\"root\"></body></html>").unwrap();
    for (id, vars) in [("kanji", r#"{ "label": "承ける" }"#), ("unset", r#"{ "font": "Nope Sans" }"#), ("fine", "{}")] {
        let (status, body) = post(&addr, "/api/overlays/import", &format!(r#"{{ "path": "overlays/label", "id": "{id}", "vars": {vars} }}"#));
        assert!(status.contains("200"), "{status}: {body}");
    }
    let (status, body) = get_body(&addr, "/api/project/validate");
    assert!(status.contains("200"), "{status}");
    let issues = serde_json::from_str::<serde_json::Value>(&body).unwrap()["issues"].clone();
    let at = |path: &str| issues.as_array().unwrap().iter().filter(|i| i["path"] == path).map(|i| i["message"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let kanji = at("/bin/kanji/source/vars/label");
    assert!(kanji.iter().any(|m| m.contains("ける承") && m.contains("Anton")), "{issues}");
    assert!(kanji.iter().any(|m| m.contains(r#"lang="en""#)), "{issues}");
    assert!(at("/bin/unset/source/vars/font").iter().any(|m| m.contains("Nope Sans")), "{issues}");
    assert!(issues.as_array().unwrap().iter().all(|i| i["severity"] == "warning" && !i["path"].as_str().unwrap().starts_with("/bin/fine")), "{issues}");
}

/// Two entries of one folder in two languages: each previews with its own `<html lang>`,
/// and validation reads the language the entry sets rather than the one the page is
/// written with, so only the entry left in English is told its kana want `ja`.
#[tokio::test(flavor = "multi_thread")]
async fn an_entry_sets_its_page_language_with_a_lang_var() {
    let (state, addr, _dir) = daemon().await;
    let label = state.config.project_root().join("overlays/caption");
    std::fs::create_dir_all(&label).unwrap();
    std::fs::write(label.join("overlay.json"), r#"{ "name": "Caption", "vars": { "label": { "type": "text", "default": "ありがとう" }, "lang": { "type": "lang", "default": "" } } }"#).unwrap();
    std::fs::write(label.join("index.html"), "<!doctype html><html lang=\"en\"><head></head><body data-composition-id=\"root\"></body></html>").unwrap();
    for (id, vars) in [("ja", r#"{ "lang": "ja" }"#), ("en", "{}")] {
        let (status, body) = post(&addr, "/api/overlays/import", &format!(r#"{{ "path": "overlays/caption", "id": "{id}", "vars": {vars} }}"#));
        assert!(status.contains("200"), "{status}: {body}");
    }
    let (status, body) = post(&addr, "/api/overlays/import", r#"{ "path": "overlays/caption", "id": "bad", "vars": { "lang": "Japanese please" } }"#);
    assert!(status.contains("400"), "not a language tag: {status} {body}");
    let (_, doc) = get_body(&addr, "/overlay-entry/ja");
    assert!(doc.starts_with("<!doctype html><html lang=\"ja\">"), "{doc}");
    let (_, doc) = get_body(&addr, "/overlay-entry/en");
    assert!(doc.starts_with("<!doctype html><html lang=\"en\">"), "{doc}");

    let (_, body) = get_body(&addr, "/api/project/validate");
    let issues = serde_json::from_str::<serde_json::Value>(&body).unwrap()["issues"].clone();
    let cjk: Vec<&str> = issues.as_array().unwrap().iter().filter(|i| i["message"].as_str().unwrap().contains("CJK")).map(|i| i["path"].as_str().unwrap()).collect();
    assert_eq!(cjk, ["/bin/en/source/vars/label"], "{issues}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cache_is_measured_and_pruned_over_http() {
    let (state, addr, dir) = daemon().await;
    std::fs::create_dir_all(dir.path().join(".kuma/bakes/stale")).unwrap();
    let (_, body) = get_body(&addr, "/api/cache");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["bakes"]["entries"], 1, "{body}");
    let (_, _, body) = request(&addr, "POST", "/api/cache/prune", "{}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["job"].as_u64().unwrap();
    let start = std::time::Instant::now();
    while !state.jobs.get(id).unwrap().state.is_final() {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(state.jobs.get(id).unwrap().result["freed"]["bakes"]["entries"], 1);
}

/// A sample as a job: what an agent asks for when the bakes it needs may outlast its
/// tool call. It answers with a job at once; the job ends with the frames `/api/sample`
/// would have returned, or with the reason it could not, word for word.
#[tokio::test(flavor = "multi_thread")]
async fn a_sample_job_ends_with_the_frames_or_the_reason() {
    let (_state, addr, _dir) = daemon().await;
    let finished = |body: &str| -> serde_json::Value {
        let (status, _, body) = request(&addr, "POST", "/api/sample/job", body);
        assert!(status.contains("200"), "{status} {body}");
        let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["job"].as_u64().expect("a job id");
        for _ in 0..600 {
            let job: serde_json::Value = serde_json::from_str(&get_body(&addr, &format!("/api/jobs/{id}")).1).unwrap();
            if matches!(job["state"].as_str(), Some("done" | "failed" | "cancelled")) {
                assert_eq!(job["kind"], "sample");
                return job;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("sample job {id} never finished");
    };

    let job = finished(r#"{"t0": 1, "overlay": "nope"}"#);
    assert_eq!(job["state"], "failed");
    assert!(job["message"].as_str().unwrap().contains("no bin entry `nope`"), "{job}");

    if std::process::Command::new("melt").arg("-version").output().is_err() {
        eprintln!("melt not available; skipping the rendered half");
        return;
    }
    let job = finished(r#"{"t0": 0.5, "overlays": false}"#);
    assert_eq!(job["state"], "done", "{job}");
    let samples = job["result"]["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 1);
    assert!(std::path::Path::new(samples[0]["path"].as_str().unwrap()).is_file(), "{job}");

    // A range with no count is several frames across it, not one frame of its start.
    let job = finished(r#"{"t0": 0.2, "t1": 1.8, "overlays": false}"#);
    let times: Vec<f64> = job["result"]["samples"].as_array().unwrap().iter().map(|s| s["t"].as_f64().unwrap()).collect();
    assert_eq!(times.len(), kuma_daemon::jobs::RANGE_COUNT, "{job}");
    assert_eq!((times[0], times[times.len() - 1]), (0.2, 1.8));
}

/// A library is the project's: naming it is an edit of the document, which an undo takes
/// back like any other, and nothing is copied or written anywhere else — so no other project
/// sees it. While named, its blueprints are listed under it and its `_vendor/` serves beside
/// the user library's; taking it off leaves its folder alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_library_is_the_projects_and_an_edit() {
    let (state, addr, dir) = daemon().await;
    let house = dir.path().join("house");
    let blueprint = house.join("blueprints/house-tag");
    std::fs::create_dir_all(&blueprint).unwrap();
    std::fs::write(blueprint.join("overlay.json"), r#"{ "name": "House tag" }"#).unwrap();
    std::fs::write(blueprint.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    std::fs::create_dir_all(house.join("blueprints/_vendor")).unwrap();
    std::fs::write(house.join("blueprints/_vendor/house.js"), "// house").unwrap();
    std::fs::create_dir_all(dir.path().join("not-one")).unwrap();
    let user_library = || std::fs::read_dir(dir.path().join("library")).unwrap().map(|e| e.unwrap().file_name()).collect::<std::collections::BTreeSet<_>>();
    let before = user_library();

    let post = |path: &str, body: serde_json::Value| {
        let (status, _, body) = request(&addr, "POST", path, &body.to_string());
        (status, serde_json::from_str::<serde_json::Value>(&body).unwrap_or_default())
    };
    let listed = || serde_json::from_str::<serde_json::Value>(&get_body(&addr, "/api/libraries").1).unwrap();
    let tagged = |out: &serde_json::Value| out["blueprints"].as_array().unwrap().iter().any(|b| b["id"] == "house-tag");
    let (status, _) = post("/api/libraries", serde_json::json!({ "path": dir.path().join("not-one") }));
    assert!(status.contains("400"), "a folder with no library in it is refused: {status}");

    // Its `blueprints/` folder names it too, and the project names it from its own folder.
    let (status, out) = post("/api/libraries", serde_json::json!({ "path": house.join("blueprints"), "origin": "ui: add library" }));
    assert!(status.contains("200"), "{status} {out}");
    assert_eq!((&out["library"]["name"], &out["library"]["source"]), (&serde_json::json!("house"), &serde_json::json!("project")));
    assert_eq!(out["library"]["contents"]["blueprints"], serde_json::json!(["house-tag"]));
    assert_eq!(state.project.read().unwrap().libraries, ["house"], "in the document, named from the project's folder");
    let all = listed();
    let names: Vec<(&str, &str)> = all["libraries"].as_array().unwrap().iter().map(|l| (l["name"].as_str().unwrap(), l["source"].as_str().unwrap())).collect();
    assert_eq!(names, [("My library", "user"), ("house", "project")]);
    assert!(tagged(&all), "its blueprint is listed");
    for file in ["house.js", "kuma.js"] {
        assert!(get(&addr, &format!("/vendor/{file}")).0.contains("200"), "{file}");
    }
    let (_, again) = post("/api/libraries", serde_json::json!({ "path": house }));
    assert!(again["revision"].is_null(), "named once: {again}");
    assert_eq!(user_library(), before, "nothing written outside the project");

    // An undo is the document without it, and the libraries follow.
    assert!(post("/api/project/undo", serde_json::json!({})).0.contains("200"));
    assert!(state.project.read().unwrap().libraries.is_empty());
    assert!(!tagged(&listed()));
    assert!(get(&addr, "/vendor/house.js").0.contains("404"));

    assert!(post("/api/project/redo", serde_json::json!({})).0.contains("200"));
    let (status, out) = post("/api/libraries/remove", serde_json::json!({ "path": "house" }));
    assert!(status.contains("200"), "{status} {out}");
    assert!(!tagged(&listed()));
    assert!(blueprint.join("overlay.json").is_file(), "its folder is left as it was");
    let (status, _) = post("/api/libraries/remove", serde_json::json!({ "path": dir.path().join("library") }));
    assert!(status.contains("400"), "the user library is not the project's to remove: {status}");
}

/// What an agent told "use this library" reads: what the library holds, the design systems
/// it offers, and its presets marked as its own; and applying a design system copies it into
/// the project, refusing to replace one unasked.
#[tokio::test(flavor = "multi_thread")]
async fn a_library_says_what_it_holds() {
    let (_state, addr, dir) = daemon().await;
    let house = dir.path().join("house");
    for (file, text) in [
        ("README.md", "# House style"),
        ("presets/house-dim.json", r#"{ "id": "house-dim", "name": "Dim", "params": {}, "effects": [] }"#),
        ("design-systems/house/DESIGN.md", "# House\n\nWarm paper."),
        ("design-systems/house/tokens.css", ":root { --paper: #f4ecd8; }"),
    ] {
        std::fs::create_dir_all(house.join(file).parent().unwrap()).unwrap();
        std::fs::write(house.join(file), text).unwrap();
    }
    let post = |path: &str, body: serde_json::Value| {
        let (status, _, body) = request(&addr, "POST", path, &body.to_string());
        (status, serde_json::from_str::<serde_json::Value>(&body).unwrap_or_default())
    };
    assert!(post("/api/libraries", serde_json::json!({ "path": house })).0.contains("200"));

    let (_, body) = get_body(&addr, "/api/libraries?detail=true");
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let contents = &listed["libraries"].as_array().unwrap().iter().find(|l| l["source"] == "project").unwrap()["contents"];
    assert_eq!(contents["presets"], serde_json::json!(["house-dim"]));
    assert_eq!(contents["design_systems"], serde_json::json!(["house"]));
    assert_eq!(contents["docs"][0].as_str().map(|d| d.ends_with("house/README.md")), Some(true), "{contents}");

    let (_, body) = get_body(&addr, "/api/presets/effects");
    let presets: serde_json::Value = serde_json::from_str(&body).unwrap();
    let dim = presets.as_array().unwrap().iter().find(|p| p["id"] == "house-dim").unwrap();
    assert!(dim["library"].as_str().is_some_and(|l| l.ends_with("house")), "{dim}");

    let (_, body) = get_body(&addr, "/api/design-systems");
    let systems: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(systems["design_systems"][0]["id"], "house");
    assert_eq!(systems["design_systems"][0]["this_project"], false);
    let (status, out) = post("/api/design-systems/house", serde_json::json!({}));
    assert!(status.contains("200"), "{status} {out}");
    assert_eq!(std::fs::read_to_string(dir.path().join("design-system/tokens.css")).unwrap(), ":root { --paper: #f4ecd8; }");
    assert!(post("/api/design-systems/house", serde_json::json!({})).0.contains("400"), "not replaced unasked");
    assert!(post("/api/design-systems/house", serde_json::json!({ "force": true })).0.contains("200"));
    let (_, body) = get_body(&addr, "/api/design-systems");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["design_systems"][0]["this_project"], true);
}
