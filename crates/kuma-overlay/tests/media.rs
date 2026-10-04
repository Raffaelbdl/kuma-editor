//! Probes real files made by ffmpeg. Skips when ffmpeg is unavailable.

use kuma_core::Fps;
use kuma_overlay::media;

fn make(dir: &std::path::Path, name: &str, args: &[&str]) -> std::path::PathBuf {
    let out = dir.join(name);
    let mut a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    a.push(out.to_string_lossy().into_owned());
    media::ffmpeg(&a).unwrap();
    out
}

#[test]
fn a_variable_rate_file_probes_as_its_average() {
    if !media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping probe test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Frames 0, 3 and 5 of every six at 30 fps: 100/67/33 ms apart, 15 fps on average,
    // and a timestamp grid of 30.
    let vfr = make(tmp.path(), "vfr.mp4", &[
        "-f", "lavfi", "-i", "testsrc=rate=30:duration=2:size=64x48",
        "-vf", "select='not(mod(n,6))+eq(mod(n,6),3)+eq(mod(n,6),5)'", "-fps_mode", "passthrough",
    ]);
    let meta = media::probe(&vfr).unwrap().meta;
    assert_eq!((meta.fps, meta.variable_fps), (Some(Fps::new(15, 1)), true));

    let ntsc = make(tmp.path(), "ntsc.mp4", &["-f", "lavfi", "-i", "testsrc=rate=30000/1001:duration=1:size=64x48"]);
    let meta = media::probe(&ntsc).unwrap().meta;
    assert_eq!((meta.fps, meta.variable_fps), (Some(Fps::new(30000, 1001)), false));
}

/// A source is metered and its pauses found in one pass, in the file's own seconds even
/// when only part of it is read: a compressed file, so the seek before decoding is real.
#[test]
fn a_recording_is_metered_and_its_pauses_found_in_its_own_seconds() {
    if !media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping loudness test");
        return;
    }
    use kuma_overlay::loudness::{measure, silences, Quiet};
    let tmp = tempfile::tempdir().unwrap();
    // A tone with two pauses, 2–2.5 s and 5–5.2 s, 6 dB quieter after the first; 1 ms
    // frames so the volume steps where it is told to.
    let tone = "sine=frequency=440:duration=8:sample_rate=48000:samples_per_frame=48,volume='if(between(t,2,2.5)+between(t,5,5.2),0,if(gt(t,2.5),0.5,1))':eval=frame";
    let file = make(tmp.path(), "talk.m4a", &["-f", "lavfi", "-i", tone, "-ac", "2", "-c:a", "aac", "-b:a", "192k"]);
    let no = std::sync::atomic::AtomicBool::new(false);
    let near = |a: f64, b: f64| (a - b).abs() < 0.03;
    let pauses = |s: &[(f64, f64)], want: &[(f64, f64)]| s.len() == want.len() && s.iter().zip(want).all(|(a, b)| near(a.0, b.0) && near(a.1, b.1));

    let heard = std::sync::Mutex::new(0.0);
    let m = measure(&file, None, Quiet::default(), &no, &|t| *heard.lock().unwrap() = t).unwrap();
    assert!(pauses(&m.silences, &[(2.0, 2.5), (5.0, 5.2)]), "{:?}", m.silences);
    let (loud, quiet) = (m.scan.integrated_over(0.0, 2.0).unwrap(), m.scan.integrated_over(2.5, 5.0).unwrap());
    assert!((loud - quiet - 6.0).abs() < 0.3, "{loud} and {quiet}");
    assert!(*heard.lock().unwrap() > 7.5, "progress reached the end");
    // A pause shorter than asked for is not one.
    let long = measure(&file, None, Quiet { db: -35.0, min: 0.3 }, &no, &|_| {}).unwrap();
    assert!(pauses(&long.silences, &[(2.0, 2.5)]), "{:?}", long.silences);

    // Part of it: the times are still the file's, and so is what the curve starts from.
    let part = measure(&file, Some((3.0, 7.0)), Quiet::default(), &no, &|_| {}).unwrap();
    assert!(pauses(&part.silences, &[(5.0, 5.2)]), "{:?}", part.silences);
    assert!(near(part.scan.steps[0].t, 3.1) && near(part.scan.steps.last().unwrap().t, 7.0), "{:?}", part.scan.steps.last());
    assert!((part.scan.integrated.unwrap() - quiet).abs() < 0.3);

    assert!(pauses(&silences(&file, Quiet::default()).unwrap(), &[(2.0, 2.5), (5.0, 5.2)]));
    let stop = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(measure(&file, None, Quiet::default(), &stop, &|_| {}).unwrap_err(), "cancelled");
}
