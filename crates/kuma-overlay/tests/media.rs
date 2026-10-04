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

/// A transparent picture says so, which is what tells an agent why it shows black: where
/// it is clear the tracks below show, and the master's background where there are none.
#[test]
fn a_picture_with_an_alpha_channel_says_so_and_an_opaque_one_does_not() {
    if !media::ffprobe_available() {
        eprintln!("ffmpeg unavailable; skipping probe test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let clear = make(tmp.path(), "clear.png", &["-f", "lavfi", "-i", "color=c=red@0.5:size=64x48,format=rgba", "-frames:v", "1"]);
    let opaque = make(tmp.path(), "opaque.png", &["-f", "lavfi", "-i", "color=c=red:size=64x48,format=rgb24", "-frames:v", "1"]);
    let jpeg = make(tmp.path(), "photo.jpg", &["-f", "lavfi", "-i", "color=c=red:size=64x48", "-frames:v", "1"]);
    let clip = make(tmp.path(), "clip.mov", &["-f", "lavfi", "-i", "testsrc=duration=1:size=64x48,format=rgba", "-c:v", "qtrle"]);
    let alpha = |p: &std::path::Path| media::probe(p).unwrap().meta.alpha;
    assert!(alpha(&clear) && alpha(&clip));
    assert!(!alpha(&opaque) && !alpha(&jpeg));
}

#[test]
fn a_folder_is_scanned_for_media_by_name() {
    assert!(media::is_media_name(std::path::Path::new("a/Take 1.MOV")) && media::is_media_name(std::path::Path::new("x.wav")));
    assert!(!media::is_media_name(std::path::Path::new("notes.txt")) && !media::is_media_name(std::path::Path::new("logo.svg")) && !media::is_media_name(std::path::Path::new("README")));
}
