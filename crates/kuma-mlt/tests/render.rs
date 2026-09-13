//! End-to-end: generate synthetic media with ffmpeg, compile the reference project, render
//! with melt, and check the output. Skips (with a message) when ffmpeg/melt are missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use kuma_core::Project;
use kuma_mlt::melt::snapshot;
use kuma_mlt::{compile, presets, render, CompileOptions, OverlayBake, RenderJob};

fn have(bin: &str) -> bool {
    Command::new(bin).arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

fn make_media(root: &Path) {
    std::fs::create_dir_all(root.join("media")).unwrap();
    let m = |n: &str| root.join("media").join(n).to_string_lossy().into_owned();
    ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30:duration=14", "-f", "lavfi", "-i", "sine=frequency=440:duration=14",
        "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", &m("screen.mp4")]);
    ffmpeg(&["-f", "lavfi", "-i", "smptebars=size=1920x1080:rate=30:duration=6", "-f", "lavfi", "-i", "sine=frequency=220:duration=6",
        "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", &m("cam.mp4")]);
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red@0.8:size=640x360:rate=1,format=rgba", "-frames:v", "1", &m("logo.png")]);
    // A fake overlay bake: 90 RGBA frames of a semi-transparent blue box on a transparent frame.
    std::fs::create_dir_all(root.join("bake")).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", "color=c=black@0.0:size=1920x1080:rate=30:duration=3,format=rgba,drawbox=x=600:y=400:w=500:h=300:color=blue@0.5:t=fill",
        "-pix_fmt", "rgba", &root.join("bake/frame_%06d.png").to_string_lossy()]);
}

#[test]
fn renders_reference_project_with_melt() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping render test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    make_media(&root);

    let p: Project = serde_json::from_str(include_str!("fixtures/reference.kuma.json")).unwrap();
    let mut opts = CompileOptions::new(&root);
    opts.bakes.insert(
        "o_c1".into(),
        OverlayBake { resource: root.join("bake/frame_%06d.png"), region: None, is_still: false },
    );
    let xml = compile(&p, &opts).unwrap();
    let mlt_file = root.join("project.mlt");
    std::fs::write(&mlt_file, &xml).unwrap();

    let out = root.join("out.mp4");
    let job = RenderJob { mlt_file: mlt_file.clone(), output: out.clone(), preset: presets::find("draft").unwrap(), range: None, extra: vec![] };
    let mut last = 0;
    render(&job, |p| last = p.percent).expect("melt render");
    assert!(out.is_file());

    // Duration is 15 s (450 frames) and the file has audio.
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type:format=duration", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    assert!(text.contains("video") && text.contains("audio"), "{text}");
    let dur: f64 = text.lines().last().unwrap().trim().parse().unwrap();
    assert!((dur - 15.0).abs() < 0.2, "duration {dur}");

    // A frame at t=3.5 s must contain the blue overlay box over the zoomed test pattern:
    // check that the pixel at the box centre is bluish.
    let png = root.join("f105.png");
    snapshot(&mlt_file, 105, &png, None).expect("snapshot");
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&png)
        .args(["-vf", "crop=1:1:850:550", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(raw.len(), 3, "one rgb pixel");
    assert!(raw[2] as i32 > raw[0] as i32 + 40, "expected bluish pixel, got {raw:?}");
    let _ = PathBuf::from(&png);
}
