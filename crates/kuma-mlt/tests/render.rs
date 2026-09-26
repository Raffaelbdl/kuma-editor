//! End-to-end: generate synthetic media with ffmpeg, compile the reference project, render
//! with melt, and check the output. Skips (with a message) when ffmpeg/melt are missing.

use std::path::Path;
use std::process::Command;

use kuma_mlt::melt::snapshot;
use kuma_mlt::{compile, presets, render, CompileOptions, OverlayBake, RenderJob, Streams, Tuning};

fn have(bin: &str) -> bool {
    Command::new(bin).arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

/// The RGB of one pixel of a PNG.
fn pixel(png: &Path, x: u32, y: u32) -> Vec<u8> {
    Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(png)
        .args(["-vf", &format!("crop=1:1:{x}:{y}"), "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .unwrap()
        .stdout
}

/// The packet lines of a `framemd5` render of `mlt` with the `draft` preset and `tuning`,
/// each ending in the MD5 of one packet; `extra` says which streams and how.
fn framemd5(mlt: &Path, out: &Path, tuning: Tuning, extra: &[(&str, &str)]) -> Vec<String> {
    let mut props = vec![("f".to_string(), "framemd5".to_string())];
    props.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let job = RenderJob { mlt_file: mlt.to_path_buf(), output: out.to_path_buf(), preset: presets::find("draft").unwrap(), range: None, tuning, extra: props };
    render(&job, &std::sync::atomic::AtomicBool::new(false), |_| {}).expect("melt render");
    std::fs::read_to_string(out).unwrap().lines().filter(|l| !l.starts_with('#')).map(str::to_string).collect()
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
    let job = RenderJob { mlt_file: mlt_file.clone(), output: out.clone(), preset: presets::find("draft").unwrap(), range: None, tuning: Tuning::default(), extra: vec![] };
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
        let lines = framemd5(mlt, &root.join(format!("{name}.framemd5")), Tuning { threads: Some(1), ..Tuning::default() }, props);
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

/// A render builds several frames at once (`real_time` below -1), which is only an
/// optimisation if it plays exactly what one frame at a time does: every video frame and
/// every audio frame of the reference timeline, which covers every service the compiler
/// emits (timewarp, luma dissolves, blur, colour, fades, crop, blend modes, stills, nested
/// sequences, a moving overlay). Not every MLT service is thread-safe, so a new effect or
/// an MLT upgrade that breaks this is caught here.
#[test]
fn parallel_frames_render_what_serial_frames_do() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping parallel frames test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    make_media(&root);
    // An overlay that moves, so frames delivered out of order would show; and a still.
    std::fs::create_dir_all(root.join("moving")).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=500x300:rate=30:duration=3,format=rgba,colorchannelmixer=aa=0.6,pad=1920:1080:600:400:color=black@0.0",
        "-pix_fmt", "rgba", &root.join("moving/frame_%06d.png").to_string_lossy()]);

    let p = kuma_core::parse_project(include_str!("fixtures/reference.kuma.json")).unwrap();
    let mut opts = CompileOptions::new(&root);
    opts.size = Some((480, 270));
    opts.bakes.insert("o_c1".into(), OverlayBake { resource: root.join("moving/frame_%06d.png"), is_still: false });
    opts.bakes.insert("o_c2".into(), OverlayBake { resource: root.join("media/logo.png"), is_still: true });
    let mlt = root.join("reference.mlt");
    std::fs::write(&mlt, compile(&p, &opts).unwrap()).unwrap();

    // Picture and sound in one pass, as a render has them; each line is one packet of
    // stream 0 (video) or 1 (audio), ending in its MD5.
    let md5s = |threads: usize| -> [Vec<String>; 2] {
        let props = [("vcodec", "rawvideo"), ("pix_fmt", "rgb24"), ("acodec", "pcm_s16le"), ("ar", "48000"), ("ac", "2")];
        let lines = framemd5(&mlt, &root.join(format!("threads{threads}.framemd5")), Tuning { threads: Some(threads), ..Tuning::default() }, &props);
        let stream = |i: &str| lines.iter().filter(|l| l.split(',').next().map(str::trim) == Some(i)).cloned().collect();
        [stream("0"), stream("1")]
    };

    let [picture, sound] = md5s(1);
    // Not vacuous: a timeline that barely changes would pass whatever the order.
    let distinct = |v: &[String]| v.iter().map(|l| l.rsplit(',').next().unwrap()).collect::<std::collections::HashSet<_>>().len();
    assert!(picture.len() == 450 && distinct(&picture) > 200, "{} frames, {} distinct", picture.len(), distinct(&picture));
    assert!(distinct(&sound) > 10, "the reference sound is silent");
    let [parallel_picture, parallel_sound] = md5s(4);
    assert_eq!(parallel_picture, picture, "four frames at once: the picture differs");
    assert_eq!(parallel_sound, sound, "four frames at once: the sound differs");
}

/// The GPU preset renders where there is a GPU: an H.264 file with every frame, and its
/// quality reaches NVENC (a CQ of 45 makes a smaller file than the preset's 21). Elsewhere
/// it says why it is not available, and this skips.
#[test]
fn the_nvenc_preset_renders_where_there_is_a_gpu() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping");
        return;
    }
    let preset = presets::find("youtube-nvenc").unwrap();
    if let Err(why) = kuma_mlt::presets::available(preset) {
        eprintln!("skipping: youtube-nvenc is not available here: {why}");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    make_media(&root);
    let p = kuma_core::parse_project(include_str!("fixtures/reference.kuma.json")).unwrap();
    let mut opts = CompileOptions::new(&root);
    opts.size = Some((640, 360));
    let mlt = root.join("reference.mlt");
    std::fs::write(&mlt, compile(&p, &opts).unwrap()).unwrap();
    let encode = |name: &str, tuning: Tuning| {
        let out = root.join(name);
        let job = RenderJob { mlt_file: mlt.clone(), output: out.clone(), preset, range: Some((0, 89)), tuning, extra: vec![] };
        render(&job, &std::sync::atomic::AtomicBool::new(false), |_| {}).expect("nvenc render");
        out
    };
    let own = encode("own.mp4", Tuning::default());
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,nb_read_frames", "-of", "csv=p=0"])
        .arg(&own)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "h264,90");
    let coarse = encode("coarse.mp4", Tuning { quality: Some(45), ..Tuning::default() });
    let size = |f: &Path| std::fs::metadata(f).unwrap().len();
    assert!(size(&coarse) < size(&own), "CQ 45 ({}) should be smaller than CQ 21 ({})", size(&coarse), size(&own));
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
        "version": 7, "name": "rate", "master": "main",
        "fps": { "num": 30, "den": 1 }, "width": 320, "height": 180, "sample_rate": 48000,
        "bin": {
            "tag": { "name": "Tag", "source": { "overlay": "overlays/location-tag" } },
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

/// A blend mode has to reach the pixels, not just the XML: qtblend reads `compositing` as
/// a plain integer, and when it was written as discrete keyframes (`0|=14`) every mode
/// rendered as normal while the compiler test on the string passed. Two opaque colours,
/// the top one screened: screen inside a blended span, the top colour outside it.
#[test]
fn a_blend_mode_changes_the_pixels() {
    if !have("ffmpeg") || !have("melt") {
        eprintln!("ffmpeg/melt not available; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let screen = serde_json::json!([ { "type": "blend", "mode": "screen" } ]);
    // screen(a, b) = 255 − (255 − a)(255 − b)/255: (160, 160, 160) under (64, 96, 192) → (184, 196, 232).
    let (top, screened) = ([64, 96, 192], [184, 196, 232]);
    // Screened for one second of three (a transition per span), then for the whole track
    // (one transition throughout): the compiler writes the two differently.
    let layouts = [
        (serde_json::json!([
            { "id": "b", "start": 0, "end": 1, "bin": "blue" },
            { "id": "c", "start": 1, "end": 2, "bin": "blue", "effects": screen },
            { "id": "d", "start": 2, "end": 3, "bin": "blue" }
        ]), [top, screened, top]),
        (serde_json::json!([ { "id": "c", "start": 0, "end": 3, "bin": "blue", "effects": screen } ]), [screened; 3]),
    ];
    let near = |got: &[u8], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| (*g as i32 - w as i32).abs() <= 3);
    for (n, (clips, wants)) in layouts.into_iter().enumerate() {
        let p = kuma_core::parse_project(&serde_json::json!({
            "version": 7, "name": "blend", "master": "main",
            "fps": { "num": 30, "den": 1 }, "width": 64, "height": 36, "sample_rate": 48000,
            "bin": {
                "grey": { "name": "Grey", "source": { "color": "#a0a0a0" } },
                "blue": { "name": "Blue", "source": { "color": "#4060c0" } },
                "main": { "name": "Main", "source": { "tracks": [
                    { "id": "v1", "kind": "video", "clips": [ { "id": "a", "start": 0, "end": 3, "bin": "grey" } ] },
                    { "id": "v2", "kind": "video", "clips": clips }
                ], "markers": [] } }
            }
        }).to_string()).unwrap();
        let mlt = root.join(format!("blend{n}.mlt"));
        std::fs::write(&mlt, compile(&p, &CompileOptions::new(root)).unwrap()).unwrap();
        for (frame, want) in [15, 45, 75].into_iter().zip(wants) {
            let png = root.join(format!("blend{n}-{frame}.png"));
            snapshot(&mlt, frame, &png).expect("snapshot");
            let got = pixel(&png, 32, 18);
            assert!(near(&got, want), "layout {n}, frame {frame}: {got:?}, expected ≈{want:?}");
        }
    }
}
