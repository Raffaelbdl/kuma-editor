//! Render jobs are queued and cancellable: one encodes at a time, the rest wait `queued`,
//! and a cancel stops either kind — a queued one without it ever starting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use kuma_daemon::jobs::{self, CancelError, Job, JobState, RenderRequest};
use kuma_daemon::{AppState, Config};

const PROJECT: &str = r##"{
  "version": 1, "name": "t", "master": "main",
  "fps": { "num": 30, "den": 1 }, "width": 320, "height": 240, "sample_rate": 48000,
  "bin": {
    "navy": { "name": "Navy", "source": { "color": "#102030" } },
    "main": { "name": "Main", "source": { "tracks": [
      { "id": "v1", "kind": "video", "name": "V", "muted": false, "hidden": false, "transitions": [],
        "clips": [ { "id": "c1", "start": 0, "end": 3600, "bin": "navy", "in": 0,
                     "speed": 1, "volume": 1, "muted": false, "effects": [], "label": "" } ] }
    ], "markers": [] } }
  }
}"##;

fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let project_file = dir.path().join("t.kuma.json");
    std::fs::write(&project_file, PROJECT).unwrap();
    let config = Config {
        project_file,
        repo_root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        library_dirs: vec![dir.path().join("library")],
        bind: "127.0.0.1:0".parse().unwrap(),
    };
    (Arc::new(AppState::load(config).unwrap()), dir)
}

fn wait_for(state: &AppState, id: u64, what: &str, done: impl Fn(&Job) -> bool) -> Job {
    let start = Instant::now();
    loop {
        let job = state.jobs.get(id).unwrap();
        if done(&job) {
            return job;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "job {id} never {what}: {job:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A stand-in render: holds the lane until told to let go.
fn lane_job(state: &Arc<AppState>, release: mpsc::Receiver<()>, ran: Arc<AtomicBool>) -> u64 {
    let s2 = state.clone();
    jobs::spawn_cancellable(state.clone(), "render", serde_json::Value::Null, move |progress, cancel| {
        jobs::in_render_lane(&s2, cancel, progress, || {
            ran.store(true, Ordering::SeqCst);
            progress(10, "encoding");
            let _ = release.recv();
            Ok(serde_json::json!({}))
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_queue_and_a_queued_one_cancels_without_starting() {
    let (state, _dir) = fixture();
    let (release_a, a_rx) = mpsc::channel();
    let (_release_b, b_rx) = mpsc::channel();
    let (release_c, c_rx) = mpsc::channel();
    let (ran_a, ran_b, ran_c) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));

    let a = lane_job(&state, a_rx, ran_a.clone());
    wait_for(&state, a, "started", |j| j.message == "encoding");
    let b = lane_job(&state, b_rx, ran_b.clone());
    let c = lane_job(&state, c_rx, ran_c.clone());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(state.jobs.get(b).unwrap().state, JobState::Queued);
    assert_eq!(state.jobs.get(c).unwrap().state, JobState::Queued);

    jobs::cancel(&state, b).unwrap();
    let job = wait_for(&state, b, "finished", |j| j.state.is_final());
    assert_eq!(job.state, JobState::Cancelled);
    assert!(!ran_b.load(Ordering::SeqCst), "a render cancelled in the queue must never start");

    release_a.send(()).unwrap();
    wait_for(&state, a, "finished", |j| j.state == JobState::Done);
    wait_for(&state, c, "started", |j| j.message == "encoding");
    release_c.send(()).unwrap();
    wait_for(&state, c, "finished", |j| j.state == JobState::Done);
    assert!(ran_a.load(Ordering::SeqCst) && ran_c.load(Ordering::SeqCst));

    let listed: Vec<u64> = state.jobs.list(Some("render")).iter().map(|j| j.id).collect();
    assert_eq!(listed, [a, b, c]);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_running_cancellable_job_can_be_cancelled() {
    let (state, _dir) = fixture();
    assert!(matches!(jobs::cancel(&state, 999), Err(CancelError::NotFound)));

    let (tx, rx) = mpsc::channel::<()>();
    let plain = jobs::spawn(state.clone(), "bake", move |_| {
        let _ = rx.recv();
        Ok(serde_json::Value::Null)
    });
    assert!(matches!(jobs::cancel(&state, plain), Err(CancelError::NotCancellable(_))));
    tx.send(()).unwrap();

    let quick = jobs::spawn_cancellable(state.clone(), "render", serde_json::Value::Null, |_, _| Ok(serde_json::Value::Null));
    wait_for(&state, quick, "finished", |j| j.state.is_final());
    assert!(matches!(jobs::cancel(&state, quick), Err(CancelError::Finished(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn clearing_forgets_finished_jobs_and_keeps_live_ones() {
    let (state, _dir) = fixture();
    let finished = jobs::spawn(state.clone(), "bake", |_| Ok(serde_json::Value::Null));
    wait_for(&state, finished, "finished", |j| j.state.is_final());
    let (tx, rx) = mpsc::channel::<()>();
    let live = jobs::spawn(state.clone(), "bake", move |_| {
        let _ = rx.recv();
        Ok(serde_json::Value::Null)
    });
    assert_eq!(state.jobs.clear_finished(), [finished]);
    assert!(state.jobs.get(finished).is_none());
    assert!(state.jobs.get(live).is_some(), "a running job is not the queue's to forget");
    tx.send(()).unwrap();
}

/// The real thing: melt is stopped mid-encode and leaves no half-written file.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_render_stops_melt_and_leaves_nothing() {
    if !kuma_mlt::melt::melt_available() {
        eprintln!("skipping: no melt");
        return;
    }
    let (state, dir) = fixture();
    let output = dir.path().join("renders/out.mp4");
    let req = RenderRequest { preset: "draft".into(), output: Some(output.clone()), ..Default::default() };
    let (_, preset, _) = jobs::render_target(&state, &req).unwrap();
    let s2 = state.clone();
    let id = jobs::spawn_cancellable(state.clone(), "render", serde_json::json!({ "preset": preset.id }), move |p, c| jobs::render(&s2, &req, p, c));
    let job = wait_for(&state, id, "encoding", |j| j.state.is_final() || j.percent >= 50);
    assert!(!job.state.is_final(), "finished before it could be cancelled: {job:?}");
    jobs::cancel(&state, id).unwrap();
    let start = Instant::now();
    let job = wait_for(&state, id, "finished", |j| j.state.is_final());
    assert_eq!(job.state, JobState::Cancelled);
    assert!(start.elapsed() < Duration::from_secs(20), "melt outlived its cancel");
    assert!(!output.exists(), "a cancelled render left {}", output.display());
}

fn request(preset: &str) -> RenderRequest {
    RenderRequest { preset: preset.into(), ..Default::default() }
}

/// A tuning the preset does not offer is refused when the render is asked for, not when
/// its turn comes (the checks themselves are `kuma-mlt`'s).
#[test]
fn a_bad_tuning_is_refused_when_asked_for() {
    let (state, _dir) = fixture();
    let refused = jobs::render_target(&state, &RenderRequest { quality: Some(10), ..request("prores-master") });
    assert_eq!(refused.unwrap_err().to_string(), "preset `prores-master` has no quality to set");
    assert!(jobs::render_target(&state, &RenderRequest { quality: Some(20), speed: Some("medium".into()), threads: Some(2), ..request("youtube-1080p") }).is_ok());
}

/// The quality and speed asked for are what x264 encoded with: it writes its settings into
/// the stream, `crf=` as given and `subme=` by preset (0 for `ultrafast`, 2 for `veryfast`).
#[tokio::test(flavor = "multi_thread")]
async fn a_render_encodes_with_the_quality_and_speed_asked_for() {
    if !kuma_mlt::melt::melt_available() {
        eprintln!("skipping: no melt");
        return;
    }
    let (state, dir) = fixture();
    let settings = |name: &str, req: RenderRequest| {
        let req = RenderRequest { output: Some(dir.path().join(format!("renders/{name}.mp4"))), range: Some((0.0, 1.0)), ..req };
        jobs::render(&state, &req, &|_, _| {}, &AtomicBool::new(false)).expect("render");
        // The settings are one NUL-terminated string in the stream.
        let bytes = std::fs::read(req.output.unwrap()).unwrap();
        let at = bytes.windows(11).position(|w| w == b"x264 - core").expect("x264 settings in the stream");
        let end = bytes[at..].iter().position(|&b| b == 0).map_or(bytes.len(), |n| at + n);
        let options = String::from_utf8_lossy(&bytes[at..end]);
        let get = |k: &str| options.split_whitespace().find_map(|w| w.strip_prefix(&format!("{k}="))).unwrap_or("?").to_string();
        (get("crf"), get("subme"))
    };
    assert_eq!(settings("preset", request("draft")), ("26.0".into(), "0".into()), "the preset's own: crf 26, ultrafast");
    let asked = RenderRequest { quality: Some(30), speed: Some("veryfast".into()), threads: Some(2), ..request("draft") };
    assert_eq!(settings("asked", asked), ("30.0".into(), "2".into()));
}

/// A preset this machine cannot run is refused when asked for, with the encoder's reason,
/// not handed to melt to crash on; one it can run is accepted. Which of the two depends on
/// the machine, so both are checked against what `available` says here.
#[test]
fn a_preset_this_machine_cannot_run_is_refused() {
    let (state, _dir) = fixture();
    let nvenc = kuma_mlt::presets::find("youtube-nvenc").unwrap();
    let asked = jobs::render_target(&state, &request("youtube-nvenc"));
    match kuma_mlt::presets::available(nvenc) {
        Ok(()) => assert!(asked.is_ok(), "{asked:?}"),
        Err(why) => {
            let refused = asked.expect_err("refused").to_string();
            assert!(refused.starts_with("preset `youtube-nvenc` cannot run on this machine: ") && refused.ends_with(&why), "{refused}");
        }
    }
    assert!(jobs::render_target(&state, &request("youtube-1080p")).is_ok(), "software presets are always there");
}
