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
