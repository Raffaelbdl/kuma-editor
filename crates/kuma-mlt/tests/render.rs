//! End-to-end: generate synthetic media with ffmpeg, compile the reference project, render
//! with melt, and check the output. Skips (with a message) when ffmpeg/melt are missing.

use std::path::Path;
use std::process::Command;

use kuma_mlt::melt::snapshot;
use kuma_mlt::{compile, presets, render, CompileOptions, OverlayBake, RenderJob, Streams};

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

    let p = kuma_core::parse_project(include_str!("fixtures/reference.kuma.json")).unwrap();
    let mut opts = CompileOptions::new(&root);
    opts.bakes.insert(
        "o_c1".into(),
        OverlayBake { resource: root.join("bake/frame_%06d.png"), is_still: false },
    );
    let xml = compile(&p, &opts).unwrap();
    let mlt_file = root.join("project.mlt");
    std::fs::write(&mlt_file, &xml).unwrap();

    let out = root.join("out.mp4");
    let job = RenderJob { mlt_file: mlt_file.clone(), output: out.clone(), preset: presets::find("draft").unwrap(), range: None, extra: vec![] };
    let mut last = 0;
    render(&job, &std::sync::atomic::AtomicBool::new(false), |p| last = p.percent).expect("melt render");
    assert!(out.is_file());
    // `-silent` once swallowed these, and a render sat at 50% until it finished.
    assert!(last > 0, "melt reported no progress");

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
    snapshot(&mlt_file, 105, &with).expect("snapshot");
    let bare_mlt = root.join("bare.mlt");
    std::fs::write(&bare_mlt, compile(&p, &CompileOptions::new(&root)).unwrap()).unwrap();
    let without = root.join("f105-bare.png");
    snapshot(&bare_mlt, 105, &without).expect("snapshot without overlay");
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

    // A trimmed clip must move: two frames of `c1` (source in = 3 s, a moving test
    // pattern) must differ. (`in` on the producer element used to freeze the cut.)
    let (a, b) = (root.join("f120.png"), root.join("f150.png"));
    snapshot(&bare_mlt, 120, &a).unwrap();
    snapshot(&bare_mlt, 150, &b).unwrap();
    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "trimmed clip renders as a still");

    // A nested sequence must render as itself. `s1` places the whole `intro` sequence on
    // the master's top track at t=9, so the master at 9+x has to show what `intro` shows
    // at x — through the transparent base, with the inner sequence's own track stack
    // composited, and with the master's other tracks hidden underneath rather than
    // bleeding through. Compiling `intro` as its own root is the same thing the preview
    // does when you open a sub-sequence.
    let mut inner_opts = CompileOptions::new(&root);
    inner_opts.sequence = Some("intro".into());
    let inner_mlt = root.join("intro.mlt");
    std::fs::write(&inner_mlt, compile(&p, &inner_opts).unwrap()).unwrap();
    // intro t=0.5 s is bare colour; t=2.0 s also has the inner sequence's own logo track.
    for (offset, probes) in [(15i64, vec![(200u32, 950u32)]), (60, vec![(200, 950), (960, 340)])] {
        let outer = root.join(format!("outer{offset}.png"));
        let inner = root.join(format!("inner{offset}.png"));
        snapshot(&bare_mlt, 9 * 30 + offset, &outer).unwrap();
        snapshot(&inner_mlt, offset, &inner).unwrap();
        for (x, y) in probes {
            let (o, i) = (pixel(&outer, x, y), pixel(&inner, x, y));
            assert_eq!(o.len(), 3);
            for c in 0..3 {
                assert!((o[c] as i32 - i[c] as i32).abs() <= 2, "nested sequence differs at ({x},{y}) frame {offset}: {o:?} vs {i:?}");
            }
        }
    }
}

/// The preview's two half graphs must each play exactly what the full graph plays of
/// their stream: every video frame identical, every audio frame identical. That is what
/// lets the split be a preview optimisation rather than a second renderer. The reference
/// project gains an audio track with a volume ramp, so a track that only sounds is covered.
#[test]
fn each_half_graph_matches_the_full_graph() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping stream split test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    make_media(&root);

    let mut doc: serde_json::Value = serde_json::from_str(include_str!("fixtures/reference.kuma.json")).unwrap();
    doc["bin"]["main"]["source"]["tracks"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "a1", "kind": "audio", "name": "Music",
        "clips": [ { "id": "m1", "start": 1.0, "end": 12.0, "bin": "screen", "in": 2.0,
            "effects": [ { "type": "volume", "keyframes": [ { "t": 0, "gain": 0 }, { "t": 3, "gain": 0.8 } ] } ] } ]
    }));
    let p = kuma_core::parse_project(&doc.to_string()).unwrap();

    // Preview scale, as the engine would get it; framemd5 keeps the outputs small.
    let graph = |name: &str, streams: Streams| {
        let mut opts = CompileOptions::new(&root);
        opts.size = Some((480, 270));
        opts.streams = streams;
        opts.bakes.insert("o_c1".into(), OverlayBake { resource: root.join("bake/frame_%06d.png"), is_still: false });
        let file = root.join(format!("{name}.mlt"));
        std::fs::write(&file, compile(&p, &opts).unwrap()).unwrap();
        file
    };
    let md5s = |mlt: &Path, name: &str, props: &[(&str, &str)]| -> Vec<String> {
        let out = root.join(format!("{name}.framemd5"));
        let mut extra: Vec<(String, String)> = vec![("f".into(), "framemd5".into()), ("real_time".into(), "-1".into())];
        extra.extend(props.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let job = RenderJob { mlt_file: mlt.to_path_buf(), output: out.clone(), preset: presets::find("draft").unwrap(), range: None, extra };
        render(&job, &std::sync::atomic::AtomicBool::new(false), |_| {}).expect("melt render");
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<String> = text.lines().filter(|l| !l.starts_with('#')).map(str::to_string).collect();
        assert!(!lines.is_empty(), "{name}: no frames");
        lines
    };
    let as_video = [("vcodec", "rawvideo"), ("pix_fmt", "rgb24"), ("an", "1")];
    let as_audio = [("acodec", "pcm_s16le"), ("ar", "48000"), ("ac", "2"), ("vn", "1")];

    let full = graph("full", Streams::Both);
    let video = graph("video", Streams::Video);
    let audio = graph("audio", Streams::Audio);
    assert_eq!(md5s(&video, "video", &as_video), md5s(&full, "full-video", &as_video), "the video graph's picture differs");
    let sound = md5s(&full, "full-audio", &as_audio);
    assert_eq!(md5s(&audio, "audio", &as_audio), sound, "the audio graph's sound differs");
    // Not silence: the comparison would pass vacuously if both sides were empty.
    assert!(sound.iter().collect::<std::collections::HashSet<_>>().len() > 10, "the reference sound is silent");
}

/// A bake is one PNG per timeline frame, and an image sequence has no rate of its own:
/// ffmpeg reads one at 25 fps unless told otherwise, which at 30 fps played every overlay
/// at five sixths speed, so subtitles fell further behind the longer the clip ran. Frame k
/// of this bake is grey level 4k, so the frame the timeline shows says which one it read.
#[test]
fn a_baked_overlay_plays_at_the_project_rate() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("bake")).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", "color=c=black:size=320x180:rate=30:duration=2,format=gray,geq=lum='N*4',format=rgba",
        "-pix_fmt", "rgba", &root.join("bake/frame_%06d.png").to_string_lossy()]);
    let p = kuma_core::parse_project(&serde_json::json!({
        "version": 4, "name": "rate", "master": "main",
        "fps": { "num": 30, "den": 1 }, "width": 320, "height": 180, "sample_rate": 48000,
        "bin": {
            "tag": { "name": "Tag", "source": { "template": "location-tag" } },
            "main": { "name": "Main", "source": { "tracks": [
                { "id": "o1", "kind": "overlay", "clips": [ { "id": "o", "start": 0, "end": 2, "bin": "tag" } ] }
            ], "markers": [] } }
        }
    }).to_string()).unwrap();
    let mut opts = CompileOptions::new(root);
    opts.bakes.insert("o".into(), OverlayBake { resource: root.join("bake/frame_%06d.png"), is_still: false });
    let mlt = root.join("rate.mlt");
    std::fs::write(&mlt, compile(&p, &opts).unwrap()).unwrap();

    for frame in [15i64, 45] {
        let png = root.join(format!("f{frame}.png"));
        snapshot(&mlt, frame, &png).expect("snapshot");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&png)
            .args(["-vf", "crop=1:1:160:90,format=gray", "-f", "rawvideo", "-"])
            .output()
            .unwrap();
        let grey = out.stdout[0] as i64;
        assert!((grey - 4 * frame).abs() <= 3, "timeline frame {frame} shows bake frame ≈{}", grey / 4);
    }
}
