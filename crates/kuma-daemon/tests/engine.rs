//! The preview engine itself, driven over its line protocol with real media: the picture
//! and the sound are two graphs on two threads, and the sound is the clock. Skips (with a
//! message) when the engine is not built or ffmpeg is missing.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use kuma_daemon::preview;
use kuma_mlt::{compile, CompileOptions, Streams};
use serde_json::json;

#[derive(Debug)]
enum Out {
    Line(String),
    Frame(i64),
}

struct Engine {
    child: Child,
    stdin: ChildStdin,
    out: Receiver<(Instant, Out)>,
}

fn headless() -> bool {
    std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

impl Engine {
    fn start(bin: &Path) -> Engine {
        let mut cmd = if headless() {
            let mut c = Command::new("xvfb-run");
            c.arg("-a").arg(bin);
            c
        } else {
            Command::new(bin)
        };
        cmd.env("LC_NUMERIC", "C").env("SDL_VIDEODRIVER", "dummy").env("SDL_AUDIODRIVER", "dummy");
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut rd = BufReader::new(child.stdout.take().unwrap());
        let (tx, out) = channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                if rd.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let l = line.trim_end().to_string();
                let msg = if let Some(rest) = l.strip_prefix("frame ") {
                    let mut it = rest.split_whitespace();
                    let f: i64 = it.next().unwrap().parse().unwrap();
                    let n: usize = it.next().unwrap().parse().unwrap();
                    let mut jpeg = vec![0u8; n];
                    rd.read_exact(&mut jpeg).unwrap();
                    assert_eq!(&jpeg[..2], &[0xff, 0xd8], "frame {f} is not a JPEG");
                    Out::Frame(f)
                } else {
                    Out::Line(l)
                };
                if tx.send((Instant::now(), msg)).is_err() {
                    return;
                }
            }
        });
        let mut e = Engine { child, stdin, out };
        e.expect_line("ok ready", Duration::from_secs(20));
        e
    }

    fn send(&mut self, cmd: &str) {
        writeln!(self.stdin, "{cmd}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Everything up to and including the first line starting with `prefix`.
    fn expect_line(&mut self, prefix: &str, within: Duration) -> Vec<(Instant, Out)> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.out.recv_timeout(left) {
                Ok((at, Out::Line(l))) if l.starts_with(prefix) => {
                    seen.push((at, Out::Line(l)));
                    return seen;
                }
                Ok((_, Out::Line(l))) if l.starts_with("err") => panic!("engine: {l}"),
                Ok(m) => seen.push(m),
                Err(RecvTimeoutError::Timeout) => panic!("no `{prefix}` within {within:?}; saw {:?}", seen.iter().map(|m| &m.1).collect::<Vec<_>>()),
                Err(RecvTimeoutError::Disconnected) => panic!("engine exited waiting for `{prefix}`"),
            }
        }
    }

    /// Everything that arrives in the next `d`.
    fn drain(&mut self, d: Duration) -> Vec<(Instant, Out)> {
        let deadline = Instant::now() + d;
        let mut seen = Vec::new();
        while let Ok(m) = self.out.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            seen.push(m);
        }
        seen
    }

    fn frame(&mut self, want: i64) {
        match self.out.recv_timeout(Duration::from_secs(10)) {
            Ok((_, Out::Frame(f))) => assert_eq!(f, want),
            other => panic!("expected frame {want}, got {other:?}"),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
    }
}

fn frames(msgs: &[(Instant, Out)]) -> Vec<(Instant, i64)> {
    msgs.iter().filter_map(|(at, m)| if let Out::Frame(f) = m { Some((*at, *f)) } else { None }).collect()
}

/// Two seconds of moving picture and a tone, compiled as the preview compiles it: a video
/// graph, and an audio graph at each of two volumes.
fn fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let args = ["-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"]
        .into_iter()
        .chain(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .map(String::from)
        .chain([root.join("clip.mp4").to_string_lossy().into_owned()])
        .collect::<Vec<_>>();
    kuma_overlay::media::ffmpeg(&args).expect("ffmpeg");
    let write = |name: &str, volume: f64, streams: Streams| {
        let p: kuma_core::Project = serde_json::from_value(json!({
            "fps": 30, "width": 320, "height": 180,
            "bin": {
                "clip": { "source": { "path": "clip.mp4" } },
                "main": { "source": { "tracks": [
                    { "id": "v1", "clips": [ { "id": "c1", "start": 0, "end": 2, "bin": "clip", "volume": volume } ] }
                ] } }
            }
        }))
        .unwrap();
        let mut opts = CompileOptions::new(root);
        opts.streams = streams;
        let path = root.join(name);
        std::fs::write(&path, compile(&p, &opts).unwrap()).unwrap();
        path
    };
    (write("video.mlt", 1.0, Streams::Video), write("audio.mlt", 1.0, Streams::Audio), write("quiet.mlt", 0.3, Streams::Audio))
}

#[test]
fn the_sound_paces_the_picture_and_a_reload_does_not_stop_it() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = repo.join("engine/build/kuma-preview");
    if !bin.is_file() || (headless() && !preview::has_bin("xvfb-run")) || !preview::has_bin("ffmpeg") {
        eprintln!("preview engine, a display or ffmpeg missing; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (video, audio, quiet) = fixture(tmp.path());
    let mut e = Engine::start(&bin);

    // Loading the picture shows the current frame; loading the sound shows nothing.
    e.send(&format!("load-video {}", video.display()));
    e.expect_line("ok loaded video frames=60", Duration::from_secs(20));
    e.frame(0);
    e.send(&format!("load-audio {}", audio.display()));
    e.expect_line("ok loaded audio frames=60", Duration::from_secs(20));
    e.send("seek 30");
    e.frame(30);
    e.send("seek 0");
    e.frame(0);

    // Play through, swapping the sound's graph a third of the way in.
    e.send("play");
    let started = e.expect_line("ok play", Duration::from_secs(5)).last().unwrap().0;
    let mut msgs = e.drain(Duration::from_millis(700));
    e.send(&format!("load-audio {}", quiet.display()));
    let rest = e.expect_line("ok end", Duration::from_secs(10));
    let ended = rest.last().unwrap().0;
    msgs.extend(rest);

    let lines: Vec<&String> = msgs.iter().filter_map(|(_, m)| if let Out::Line(l) = m { Some(l) } else { None }).collect();
    assert!(lines.iter().any(|l| l.starts_with("ok loaded audio")), "{lines:?}");
    assert!(!lines.iter().any(|l| *l == "ok play"), "the reload restarted playback: {lines:?}");
    let shown = frames(&msgs);
    let numbers: Vec<i64> = shown.iter().map(|(_, f)| *f).collect();
    assert!(numbers.windows(2).all(|w| w[0] < w[1]), "frames out of order: {numbers:?}");
    assert!(numbers.len() >= 20 && *numbers.last().unwrap() >= 50, "too few frames reached the end: {numbers:?}");
    // Two seconds of sound take two seconds to play, and the picture keeps to the sound:
    // frame f is never shown before f/30 s of it has been heard.
    let took = ended - started;
    assert!(took >= Duration::from_millis(1700) && took <= Duration::from_secs(6), "two seconds played in {took:?}");
    for (at, f) in &shown {
        let due = started + Duration::from_millis((*f as u64 * 1000) / 30);
        assert!(*at + Duration::from_millis(100) >= due, "frame {f} shown {:?} before its time", due - *at);
    }

    // Pause mid-play, then seek: the seek's frame is the last one, whatever was in flight.
    e.send("play");
    e.expect_line("ok play", Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(400));
    e.send("pause");
    e.send("seek 12");
    let after: Vec<i64> = frames(&e.drain(Duration::from_millis(800))).into_iter().map(|(_, f)| f).collect();
    assert_eq!(after.last(), Some(&12), "a stale frame landed after the seek: {after:?}");
}

/// A graph as slow to open as a long timeline: hundreds of producers, each opening its file.
fn slow_graph(root: &Path) -> PathBuf {
    let media = root.join("clip.mp4");
    let producers: String = (0..300)
        .map(|i| format!(r#"<producer id="p{i}" in="0" out="29"><property name="resource">{}</property><property name="mlt_service">avformat</property></producer>"#, media.display()))
        .collect();
    let entries: String = (0..300).map(|i| format!(r#"<entry producer="p{i}" in="0" out="0"/>"#)).collect();
    let path = root.join("slow.mlt");
    std::fs::write(&path, format!(r#"<?xml version="1.0"?><mlt>{producers}<playlist id="main">{entries}</playlist></mlt>"#)).unwrap();
    path
}

#[test]
fn a_play_never_waits_behind_a_load() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = repo.join("engine/build/kuma-preview");
    if !bin.is_file() || (headless() && !preview::has_bin("xvfb-run")) || !preview::has_bin("ffmpeg") {
        eprintln!("preview engine, a display or ffmpeg missing; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (video, audio, _) = fixture(tmp.path());
    let slow = slow_graph(tmp.path());
    let mut e = Engine::start(&bin);

    // Before any timeline is in, a seek is kept and a play is held and said to be, not
    // refused: the editor asks for both as soon as it connects, while the first load runs.
    e.send("seek 30");
    e.send(&format!("load-video {}", slow.display()));
    e.send("play");
    e.expect_line("ok waiting", Duration::from_secs(5));
    e.expect_line("ok play", Duration::from_secs(30));
    let first = frames(&e.drain(Duration::from_millis(500))).first().map(|(_, f)| *f);
    assert_eq!(first, Some(30), "playback did not start where the seek asked");
    e.send("pause");

    e.send(&format!("load-video {}", video.display()));
    e.expect_line("ok loaded video", Duration::from_secs(20));
    e.send(&format!("load-audio {}", audio.display()));
    e.expect_line("ok loaded audio", Duration::from_secs(20));
    e.send("seek 0");
    e.frame(0);
    // An edit reloads a graph, which takes seconds on a long timeline. Play answers at once,
    // on the graph already held, and the new one is swapped in when it is open.
    e.send(&format!("load-audio {}", slow.display()));
    e.send("play");
    let before = e.expect_line("ok play", Duration::from_secs(5));
    assert!(!before.iter().any(|(_, m)| matches!(m, Out::Line(l) if l.starts_with("ok loaded"))), "play waited for the load");
    e.expect_line("ok loaded audio frames=300", Duration::from_secs(30));
}

#[test]
fn a_seek_overtaken_by_another_shows_nothing() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = repo.join("engine/build/kuma-preview");
    if !bin.is_file() || (headless() && !preview::has_bin("xvfb-run")) || !preview::has_bin("ffmpeg") {
        eprintln!("preview engine, a display or ffmpeg missing; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (video, _, _) = fixture(tmp.path());
    let mut e = Engine::start(&bin);
    e.send(&format!("load-video {}", video.display()));
    e.expect_line("ok loaded video", Duration::from_secs(20));
    e.frame(0);

    // A drag: forty positions at once, faster than any of them renders. Only the ones not
    // already overtaken are drawn, and the last is always where the drag stopped.
    let drag: String = (1..=40).map(|f| format!("seek {f}\n")).collect();
    e.stdin.write_all(drag.as_bytes()).unwrap();
    e.stdin.flush().unwrap();
    let shown: Vec<i64> = frames(&e.drain(Duration::from_millis(1500))).into_iter().map(|(_, f)| f).collect();
    assert_eq!(shown.last(), Some(&40), "the drag did not end where it stopped: {shown:?}");
    assert!(shown.len() < 10, "stale positions were drawn: {shown:?}");
}
