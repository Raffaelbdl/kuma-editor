//! Proxies, made the way the preview at ½ asks for them: in the background, one file at a
//! time, only for footage that is slow to seek, every frame a keyframe. Skips (with a
//! message) without ffmpeg.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kuma_daemon::{preview, proxies};
use kuma_daemon::{AppState, Config, Libraries};

const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 1920, "height": 1080, "sample_rate": 48000,
  "bin": {
    "phone": { "name": "Phone", "source": { "path": "phone.mp4", "kind": "video" } },
    "intra": { "name": "Intra", "source": { "path": "intra.mov", "kind": "video" } },
    "small": { "name": "Small", "source": { "path": "small.mp4", "kind": "video" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "phone", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

fn ffmpeg(dir: &Path, out: &str, size: &str, codec: &[&str]) {
    let mut args: Vec<String> = ["-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate=30:duration=2")].iter().map(|s| s.to_string()).collect();
    args.extend(codec.iter().map(|s| s.to_string()));
    args.push(dir.join(out).to_string_lossy().into_owned());
    kuma_overlay::media::ffmpeg(&args).expect("ffmpeg");
}

fn fixture() -> Option<(Arc<AppState>, tempfile::TempDir)> {
    if !preview::has_bin("ffmpeg") || !preview::has_bin("ffprobe") {
        eprintln!("ffmpeg missing; skipping");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    // Phone-like: H.264 with a keyframe every 60 frames and B-frames. An all-intra codec at
    // the same size, and a long-GOP file no bigger than 720p.
    ffmpeg(dir.path(), "phone.mp4", "1920x1080", &["-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-pix_fmt", "yuv420p"]);
    ffmpeg(dir.path(), "intra.mov", "1920x1080", &["-c:v", "mjpeg", "-q:v", "8"]);
    ffmpeg(dir.path(), "small.mp4", "1280x720", &["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]);
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = Config { project_file, repo_root: repo, libraries: Libraries::new(dir.path().join("empty-library")), bind: "127.0.0.1:0".parse().unwrap() };
    Some((Arc::new(AppState::load(config).unwrap()), dir))
}

/// Wait until `n` proxy jobs have run and none is still going.
fn settle(state: &AppState, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let jobs = state.jobs.list(Some("proxy"));
        if jobs.len() >= n && jobs.iter().all(|j| j.state.is_final()) {
            return;
        }
        assert!(Instant::now() < deadline, "proxy jobs did not finish: {jobs:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn probe(path: &Path, entries: &str) -> String {
    let out = std::process::Command::new("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-of", "csv=p=0"]).args(entries.split(' ')).arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn only_footage_slow_to_seek_gets_a_proxy_every_frame_a_keyframe() {
    let Some((state, _dir)) = fixture() else { return };
    // Jobs run on the runtime's blocking threads, as in the daemon.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _rt = rt.enter();
    // At full nothing is made: the preview decodes the media.
    *state.preview.scale.lock().unwrap() = 1.0;
    proxies::make_next(&state);
    assert!(state.jobs.list(Some("proxy")).is_empty(), "a proxy was made for the preview at full");

    // At ½ one job looks at each video in turn, each starting the next.
    *state.preview.scale.lock().unwrap() = 0.5;
    proxies::make_next(&state);
    settle(&state, 3);
    let jobs = state.jobs.list(Some("proxy"));
    assert_eq!(jobs.len(), 3, "{jobs:?}");
    assert!(jobs.iter().all(|j| j.state == kuma_daemon::jobs::JobState::Done), "{jobs:?}");
    assert_eq!(jobs[0].params["bin"], "phone", "what a sequence places comes first");

    let project = state.project.read().unwrap().clone();
    let ready = proxies::ready(&state, &project);
    assert_eq!(ready.keys().collect::<Vec<_>>(), vec!["phone"], "only the long-GOP 1080p file needs one");
    let proxy = &ready["phone"];
    assert_eq!(probe(proxy, "-show_entries stream=codec_name,width,height"), "h264,960,540", "half the frame");
    let frames = probe(proxy, "-count_frames -show_entries stream=nb_read_frames");
    let keys = probe(proxy, "-skip_frame nokey -count_frames -show_entries stream=nb_read_frames");
    assert_eq!((frames.as_str(), keys.as_str()), ("60", "60"), "every frame a keyframe");
    assert_eq!(probe(proxy, "-show_entries stream=nb_frames -select_streams a"), "", "no sound: the sound reads the media");

    // Nothing more to do, and the next compile starts nothing.
    proxies::make_next(&state);
    assert_eq!(state.jobs.list(Some("proxy")).len(), 3);

    // A prune keeps what the bin uses; a file that changed makes its proxy a stale one.
    let pruned = kuma_daemon::cache::prune(&state, false, &|_, _| {}).unwrap();
    assert_eq!(pruned.freed.proxies.entries, 0);
    assert_eq!(pruned.kept.proxies.entries, 3, "one proxy and two markers");
    let phone = _dir.path().join("phone.mp4");
    let f = std::fs::File::options().append(true).open(&phone).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
    assert!(proxies::ready(&state, &project).is_empty(), "a changed file is not read through its old proxy");
    let pruned = kuma_daemon::cache::prune(&state, false, &|_, _| {}).unwrap();
    assert_eq!(pruned.freed.proxies.entries, 1);
}
