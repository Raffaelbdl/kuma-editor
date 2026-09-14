//! End-to-end: generate synthetic media with ffmpeg, compile the reference project, render
//! with melt, and check the output. Skips (with a message) when ffmpeg/melt are missing.

use std::path::Path;
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
    // A fake overlay bake: 90 RGBA frames of a half-transparent blue box (600,400 500×300)
    // padded onto a fully transparent frame. (`drawbox` would leave alpha at 0.)
    std::fs::create_dir_all(root.join("bake")).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", "color=c=blue@0.5:size=500x300:rate=30:duration=3,format=rgba,pad=1920:1080:600:400:color=black@0.0",
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

    // A frame at t=3.5 s must contain the semi-transparent blue overlay box over the test
    // pattern: the same frame compiled without the bake must differ inside the box and
    // match outside it, which proves the overlay is composited (with alpha) where it belongs.
    let pixel = |png: &Path, x: u32, y: u32| -> Vec<u8> {
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(png)
            .args(["-vf", &format!("crop=1:1:{x}:{y}"), "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
            .output()
            .unwrap()
            .stdout
    };
    let with = root.join("f105.png");
    snapshot(&mlt_file, 105, &with, None).expect("snapshot");
    let bare_mlt = root.join("bare.mlt");
    std::fs::write(&bare_mlt, compile(&p, &CompileOptions::new(&root)).unwrap()).unwrap();
    let without = root.join("f105-bare.png");
    snapshot(&bare_mlt, 105, &without, None).expect("snapshot without overlay");
    if let Some(out) = std::env::var_os("KUMA_TEST_OUT") {
        let _ = std::fs::create_dir_all(&out);
        for f in ["f105.png", "f105-bare.png", "project.mlt"] {
            let _ = std::fs::copy(root.join(f), Path::new(&out).join(f));
        }
    }
    let inside = (pixel(&with, 850, 550), pixel(&without, 850, 550));
    let outside = (pixel(&with, 200, 950), pixel(&without, 200, 950));
    assert_eq!(inside.0.len(), 3);
    assert_ne!(inside.0, inside.1, "overlay must change the pixels under it");
    // Half-transparent blue over the footage: blue goes up, the others halve.
    let (r, g, b) = (inside.1[0] as i32, inside.1[1] as i32, inside.1[2] as i32);
    let expect = [r / 2, g / 2, (b + 255) / 2];
    for (i, e) in expect.iter().enumerate() {
        assert!((inside.0[i] as i32 - e).abs() <= 8, "channel {i}: expected ≈{e}, got {:?} over {:?}", inside.0, inside.1);
    }
    assert_eq!(outside.0, outside.1, "overlay must not change pixels outside its box");
}
