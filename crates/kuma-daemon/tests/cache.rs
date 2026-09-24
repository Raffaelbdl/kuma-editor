//! Pruning `.kuma/`: what it keeps, what it deletes, and that it waits for whatever is
//! reading the cache. No hyperframes or melt: a bake entry is only a directory named by its
//! key, and the key is computed without rendering anything.

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use kuma_daemon::cache::{live_bakes, prune, usage};
use kuma_daemon::{AppState, Config};
use kuma_overlay::BakeRequest;

/// One overlay entry placed twice at different lengths: once on the master, once on a
/// sequence nothing places — whose bake a prune must keep all the same, since `kuma bake`
/// warms it on purpose.
const PROJECT: &str = r##"{
  "version": 7, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "tag": { "name": "Tag", "source": { "overlay": "overlays/probe", "vars": {} } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "o1", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 4, "bin": "tag", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } },
    "spare": { "name": "Spare", "source": { "tracks": [
      { "id": "o2", "kind": "overlay", "name": "O", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c2", "start": 0, "end": 2, "bin": "tag", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let tpl = dir.path().join("overlays/probe");
    std::fs::create_dir_all(&tpl).unwrap();
    std::fs::write(tpl.join("overlay.json"), r#"{ "name": "Probe", "duration_default": 3 }"#).unwrap();
    std::fs::write(tpl.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    (Arc::new(AppState::load(config).unwrap()), dir)
}

/// A finished bake entry of `bytes` bytes.
fn bake(cache: &Path, name: &str, bytes: usize) {
    let d = cache.join("bakes").join(name).join("frames");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("frame_000001.png"), vec![0u8; bytes]).unwrap();
}

fn file(path: &Path, bytes: usize, age: Duration) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![0u8; bytes]).unwrap();
    std::fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - age).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// The key a clip bakes under is the key the prune keeps. Written out here rather than
/// read back from the daemon, so a change to what a clip bakes as that the prune does not
/// follow fails this test instead of deleting a render's frames.
#[test]
fn the_live_set_is_every_clip_in_every_sequence_and_each_entrys_preview() {
    let (state, dir) = fixture();
    let project = state.project.read().unwrap().clone();
    let live = live_bakes(&state, &project);
    let folder = dir.path().join("overlays/probe");
    let manifest = kuma_core::manifest::OverlayManifest::load(&folder).unwrap();
    let vars = serde_json::Map::new();
    let clip = |duration| BakeRequest { overlay_dir: &folder, manifest: &manifest, vars: &vars, fps: project.fps, width: 320, height: 240, duration, speed: 1.0 };
    let on_master = state.baker.key(&clip(4.0)).unwrap();
    let unplaced = state.baker.key(&clip(2.0)).unwrap();
    let preview = state.baker.key(&kuma_overlay::Baker::preview_request(&folder, &manifest, &vars)).unwrap();
    assert!(live.contains(&on_master), "the master's clip");
    assert!(live.contains(&unplaced), "a clip in a sequence nothing places");
    assert!(live.contains(&preview), "the bake the entry's thumbnail is cut from");
    assert_eq!(live.len(), 3);
}

#[test]
fn a_prune_deletes_what_nothing_wants_and_nothing_else() {
    let (state, dir) = fixture();
    let cache = dir.path().join(".kuma");
    let project = state.project.read().unwrap().clone();
    let live: Vec<String> = live_bakes(&state, &project).into_iter().collect();
    for key in &live {
        bake(&cache, key, 100);
    }
    bake(&cache, "0123456789abcdef01234567", 1000); // superseded
    bake(&cache, "0123456789abcdef01234567.tmp-42-1f", 10); // a bake still being written
    let samples = cache.join("samples");
    file(&samples.join("fold_1.png"), 500, Duration::from_secs(600));
    file(&samples.join("fnew_1.png"), 50, Duration::ZERO); // just handed to someone
    file(&samples.join(".tmp-AbC.png"), 5, Duration::from_secs(600)); // being written
    let thumbs = cache.join("thumbs");
    file(&thumbs.join("tag.png"), 7, Duration::ZERO);
    file(&thumbs.join("removed.png"), 70, Duration::ZERO);
    file(&thumbs.join("removed.webp"), 700, Duration::ZERO);
    file(&thumbs.join("tag.webp.tmp-"), 1, Duration::ZERO);

    let before = usage(&state).unwrap();
    assert_eq!(before.bakes.entries, 5);
    let pruned = prune(&state, false, &|_, _| {}).unwrap();

    let mut kept_bakes = live.clone();
    kept_bakes.push("0123456789abcdef01234567.tmp-42-1f".into());
    kept_bakes.sort();
    assert_eq!(names(&cache.join("bakes")), kept_bakes);
    assert_eq!(names(&samples), vec![".tmp-AbC.png", "fnew_1.png"]);
    assert_eq!(names(&thumbs), vec!["tag.png", "tag.webp.tmp-"]);

    assert_eq!((pruned.freed.bakes.entries, pruned.freed.bakes.bytes), (1, 1000));
    assert_eq!((pruned.freed.samples.entries, pruned.freed.samples.bytes), (1, 500));
    assert_eq!((pruned.freed.thumbs.entries, pruned.freed.thumbs.bytes), (2, 770));
    assert_eq!(pruned.freed.bytes, 2270);
    assert_eq!(pruned.kept, usage(&state).unwrap());
    assert_eq!(pruned.kept.bytes + pruned.freed.bytes, before.bytes);
}

/// `all` is the blunt version: every bake and sample, live or fresh — but still not what
/// is being written, and not the thumbnails of entries in the bin, which nothing
/// regenerates until someone asks.
#[test]
fn prune_all_empties_bakes_and_samples_but_not_work_in_flight() {
    let (state, dir) = fixture();
    let cache = dir.path().join(".kuma");
    let project = state.project.read().unwrap().clone();
    for key in live_bakes(&state, &project) {
        bake(&cache, &key, 100);
    }
    bake(&cache, "abc.tmp-1-2", 10);
    file(&cache.join("samples/fnew_1.png"), 50, Duration::ZERO);
    file(&cache.join("thumbs/tag.png"), 7, Duration::ZERO);
    let pruned = prune(&state, true, &|_, _| {}).unwrap();
    assert_eq!(names(&cache.join("bakes")), vec!["abc.tmp-1-2"]);
    assert!(names(&cache.join("samples")).is_empty());
    assert_eq!(names(&cache.join("thumbs")), vec!["tag.png"]);
    assert_eq!(pruned.freed.bakes.entries, 3);
}

/// A render reads its bakes for as long as melt encodes, so a prune asked for meanwhile
/// waits for it — and waiting must not hold up a sample, which the preview and agents
/// depend on while a render runs.
#[test]
fn a_prune_waits_for_readers_without_blocking_new_ones() {
    let (state, dir) = fixture();
    let cache = dir.path().join(".kuma");
    bake(&cache, "0123456789abcdef01234567", 10);
    let render = state.cache_users.read().unwrap();

    let (tx, rx) = mpsc::channel();
    let s2 = state.clone();
    std::thread::spawn(move || tx.send(prune(&s2, false, &|_, _| {}).map(|p| p.freed.bakes.entries).unwrap()).unwrap());
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "pruned under a render");
    assert!(cache.join("bakes/0123456789abcdef01234567").is_dir());
    assert!(state.cache_users.try_read().is_ok(), "a waiting prune blocked a sample");

    drop(render);
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
}

#[test]
fn an_empty_cache_prunes_to_nothing() {
    let (state, _dir) = fixture();
    let pruned = prune(&state, false, &|_, _| {}).unwrap();
    assert_eq!(pruned.freed.bytes, 0);
    assert_eq!(pruned.kept.bytes, 0);
}
