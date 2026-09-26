//! Bakes the real `location-tag` blueprint through hyperframes. Skips when the renderer or
//! a browser is unavailable.

use std::path::PathBuf;

use kuma_core::manifest::OverlayManifest;
use kuma_core::Fps;
use kuma_overlay::{BakeRequest, Baker};
use serde_json::{json, Map};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A PNG decoded to RGBA bytes, through `vf` when given.
fn rgba(png: &std::path::Path, vf: Option<&str>) -> Vec<u8> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]).arg(png);
    if let Some(vf) = vf {
        cmd.args(["-vf", vf]);
    }
    let out = cmd.args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"]).output().expect("ffmpeg on PATH");
    assert!(!out.stdout.is_empty(), "ffmpeg decoded nothing from {}: {}", png.display(), String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn rgba_at(png: &std::path::Path, x: u32, y: u32) -> [u8; 4] {
    <[u8; 4]>::try_from(rgba(png, Some(&format!("crop=1:1:{x}:{y}"))).as_slice()).unwrap()
}

#[test]
fn bakes_location_tag_with_alpha_and_caches() {
    let root = repo_root();
    let tmp = tempfile::tempdir().unwrap();
    let baker = Baker::discover(&root, root.join("library/blueprints/_vendor"), tmp.path().join("cache"));
    if baker.available().is_err() {
        eprintln!("hyperframes unavailable; skipping bake test");
        return;
    }
    if baker.browser.is_none() && std::env::var_os("KUMA_ALLOW_BROWSER_DOWNLOAD").is_none() {
        eprintln!("no headless browser found; skipping bake test");
        return;
    }
    let dir = root.join("library/blueprints/location-tag");
    let manifest = OverlayManifest::load(&dir).unwrap();
    let mut vars = Map::new();
    vars.insert("label".into(), json!("Tokyo"));
    vars.insert("accent".into(), json!("#00a0ff"));

    let req = BakeRequest {
        overlay_dir: &dir, manifest: &manifest, vars: &vars, fps: Fps::new(30, 1), width: 1920, height: 1080,
        duration: 2.0, speed: 1.0,
    };
    let r = baker.bake(&req).expect("bake");
    assert!(!r.cached);
    assert_eq!(r.frames, 60, "2 s at 30 fps");
    let f30 = r.dir.join("frames/frame_000030.png");
    assert!(f30.is_file());
    if let Some(out) = std::env::var_os("KUMA_TEST_OUT") {
        let _ = std::fs::create_dir_all(&out);
        let _ = std::fs::copy(&f30, PathBuf::from(&out).join("location-tag-f30.png"));
    }
    // Transparent corner, opaque-ish pin just inside the anchor (0.68, 0.28 → 1306, 302):
    // the pin is the first flex item, ~45 px square, so (1306+22, 302+22) is its centre.
    assert_eq!(rgba_at(&f30, 5, 5)[3], 0, "corner must be transparent");
    let pin = rgba_at(&f30, 1306 + 22, 302 + 22);
    assert!(pin[3] > 200, "pin should be opaque at frame 30, got {pin:?}");
    assert!(pin[2] > pin[0], "accent override should make the pin blue, got {pin:?}");

    // Second call is served from cache.
    let r2 = baker.bake(&req).expect("cached bake");
    assert!(r2.cached);
    assert_eq!(r2.key, r.key);

    // Speed 0.5 over the same 2 s clip still yields 60 output frames (internal time is 1 s).
    let slow = BakeRequest { speed: 0.5, ..req.clone() };
    let r3 = baker.bake(&slow).expect("slow bake");
    assert_eq!(r3.frames, 60);
    assert_ne!(r3.key, r.key);

    // A clip LONGER than the overlay's markup default (4 s) must keep animating: the bake
    // used to render only the default and pad the rest with the last frame.
    let long = BakeRequest { fps: Fps::new(4, 1), duration: 8.0, speed: 1.0, ..req.clone() };
    let r4 = baker.bake(&long).expect("long bake");
    assert_eq!(r4.frames, 32, "8 s at 4 fps");
    let f = |n: u32| std::fs::read(r4.dir.join(format!("frames/frame_{n:06}.png"))).unwrap();
    // t=4.0 (past the 4 s default) must still show the tag: frame 1 (t=0) is the empty
    // pre-entrance state, so a frozen/unmounted overlay would make them identical.
    assert_ne!(f(17), f(1), "overlay must still be mounted past the overlay's default duration");
    // The overlay anchors its out-animation to the real end (7.55–8 s): t=7.75 differs from t=4.0.
    assert_ne!(f(32), f(17), "out-animation must happen at the clip's real end, not the default's");

    // A static overlay bakes a single still frame regardless of the clip duration or speed.
    let wm_dir = root.join("library/blueprints/watermark");
    let wm = OverlayManifest::load(&wm_dir).unwrap();
    let novars = Map::new();
    let still = baker
        .bake(&BakeRequest { overlay_dir: &wm_dir, manifest: &wm, vars: &novars, fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 10.0, speed: 2.0 })
        .expect("static bake");
    assert!(still.is_still);
    assert_eq!(still.frames, 1);
    assert!(still.resource.ends_with("frame_000001.png"));
    // Bottom-right corner carries the watermark; top-left is transparent.
    assert_eq!(rgba_at(&still.resource, 10, 10)[3], 0);
    assert!(rgba_at(&still.resource, 1920 - 60, 1080 - 50)[3] > 60, "watermark chip expected at bottom-right");
}

/// The library's moving preview: animated, and still transparent. A single-frame WebP
/// would be a thumbnail with extra steps, and one without alpha would sit on the editor's
/// dark panels as a black rectangle — both have to be checked in the file, because both
/// look fine to the encoder.
#[test]
fn previews_an_overlay_as_an_animated_webp_with_alpha() {
    let root = repo_root();
    let tmp = tempfile::tempdir().unwrap();
    let baker = Baker::discover(&root, root.join("library/blueprints/_vendor"), tmp.path().join("cache"));
    if baker.available().is_err() || (baker.browser.is_none() && std::env::var_os("KUMA_ALLOW_BROWSER_DOWNLOAD").is_none()) {
        eprintln!("hyperframes or a browser is unavailable; skipping preview test");
        return;
    }
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg unavailable; skipping preview test");
        return;
    }
    let dir = root.join("library/blueprints/location-tag");
    let manifest = OverlayManifest::load(&dir).unwrap();
    let out = tmp.path().join("preview.webp");
    baker.preview(&dir, &manifest, &Map::new(), &out).expect("preview encoded");

    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
    let count = |tag: &[u8]| bytes.windows(4).filter(|w| *w == tag).count();
    // ANMF is one animation frame; ALPH is that frame's alpha.
    assert!(count(b"ANIM") >= 1, "not an animated WebP");
    assert!(count(b"ANMF") > 10, "only {} frames — the preview is barely moving", count(b"ANMF"));
    assert!(count(b"ALPH") > 10, "frames carry no alpha: the preview would be a black rectangle");
    // Nothing was left behind mid-write.
    assert!(!out.with_extension("webp.tmp").exists());
}

/// A drawn overlay (`kumaCanvas` + Rough.js) is a pure function of time: two bakes into
/// separate caches give the same pixels in every frame (the PNG bytes may differ). `paint-wipe`
/// also covers the whole frame at its midpoint, the cut.
#[test]
fn a_drawn_overlay_bakes_the_same_pixels_twice() {
    let root = repo_root();
    let tmp = tempfile::tempdir().unwrap();
    let baker = |cache: &str| Baker::discover(&root, root.join("library/blueprints/_vendor"), tmp.path().join(cache));
    let (a, b) = (baker("a"), baker("b"));
    if a.available().is_err() || (a.browser.is_none() && std::env::var_os("KUMA_ALLOW_BROWSER_DOWNLOAD").is_none()) {
        eprintln!("hyperframes or a browser is unavailable; skipping drawn-overlay bake test");
        return;
    }
    let dir = root.join("library/blueprints/paint-wipe");
    let manifest = OverlayManifest::load(&dir).unwrap();
    let vars = Map::new();
    let (w, h) = (480, 270);
    let req = BakeRequest { overlay_dir: &dir, manifest: &manifest, vars: &vars, fps: Fps::new(12, 1), width: w, height: h, duration: 1.0, speed: 1.0 };
    let (ra, rb) = std::thread::scope(|s| {
        let (ja, jb) = (s.spawn(|| a.bake(&req).expect("bake a")), s.spawn(|| b.bake(&req).expect("bake b")));
        (ja.join().unwrap(), jb.join().unwrap())
    });
    assert_eq!(ra.frames, 12);
    let frame = |r: &kuma_overlay::BakeResult, n: i64| rgba(&r.dir.join(format!("frames/frame_{n:06}.png")), None);
    let frames: Vec<Vec<u8>> = (1..=ra.frames).map(|n| frame(&ra, n)).collect();
    for (i, pixels) in frames.iter().enumerate() {
        assert!(*pixels == frame(&rb, i as i64 + 1), "frame {} differs between two bakes", i + 1);
    }
    let alpha = |pixels: &[u8], x: u32, y: u32| pixels[((y * w + x) * 4 + 3) as usize];
    for (x, y) in [(2, 2), (w - 3, 2), (w / 2, h / 2), (2, h - 3), (w - 3, h - 3)] {
        assert_eq!(alpha(&frames[6], x, y), 255, "the midpoint (t = 0.5) must cover the frame at ({x}, {y})");
    }
    assert_eq!(alpha(&frames[0], w / 2, h / 2), 0, "nothing is drawn at t = 0");
}

/// A var holding `Let's` opened a CSS string that ran over the custom properties after it,
/// to the end of the stylesheet or the next var's apostrophe, so the overlay drew its
/// fallbacks with no error. The colour var sits between two such vars: a red frame is the
/// colour read, blue its fallback.
#[test]
fn an_apostrophe_in_a_var_leaves_the_vars_after_it_alone() {
    let root = repo_root();
    let tmp = tempfile::tempdir().unwrap();
    let baker = Baker::discover(&root, root.join("library/blueprints/_vendor"), tmp.path().join("cache"));
    if baker.available().is_err() || (baker.browser.is_none() && std::env::var_os("KUMA_ALLOW_BROWSER_DOWNLOAD").is_none()) {
        eprintln!("hyperframes or a browser is unavailable; skipping");
        return;
    }
    let dir = tmp.path().join("swatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("overlay.json"), r##"{ "name": "Swatch", "static": true, "vars": {
        "a_title": { "type": "string", "default": "" }, "b_color": { "type": "color", "default": "#00ff00" },
        "c_note": { "type": "string", "default": "" } } }"##).unwrap();
    std::fs::write(dir.join("index.html"), r#"<!doctype html><html><head><style>
        html, body { margin: 0; background: transparent; }
        #fill { position: absolute; inset: 0; background: var(--var-b_color, #0000ff); }
        #fill::after { content: var(--var-a_title) var(--var-c_note); }
      </style></head><body>
      <div id="root" data-composition-id="main" data-no-timeline data-start="0" data-duration="1" data-width="64" data-height="36"><div id="fill"></div></div>
      </body></html>"#).unwrap();
    let manifest = OverlayManifest::load(&dir).unwrap();
    let vars: Map<String, serde_json::Value> = json!({ "a_title": "Let's go", "b_color": "#ff0000", "c_note": "it's red" }).as_object().unwrap().clone();
    let req = BakeRequest { overlay_dir: &dir, manifest: &manifest, vars: &vars, fps: Fps::new(30, 1), width: 64, height: 36, duration: 1.0, speed: 1.0 };
    let r = baker.bake(&req).expect("bake");
    let px = rgba_at(&r.resource, 32, 18);
    assert!(px[0] > 200 && px[2] < 50, "the colour after the apostrophe was not read: {px:?}");
}

/// A baker whose renderer is a stand-in that takes a second, writes six empty frames, and
/// logs `start` and `end` for each run, in the order they happen; and a probe overlay. So
/// these need no hyperframes and no browser.
fn stand_in(tmp: &std::path::Path) -> (Baker, PathBuf, OverlayManifest, PathBuf) {
    let log = tmp.join("log");
    let fake = tmp.join("hyperframes");
    // `render <src> --format png-sequence -o <dir> …`, so the output is `$6`.
    let script = format!("#!/bin/sh\necho start >> '{0}'\nsleep 1\nmkdir -p \"$6\"\nfor i in 1 2 3 4 5 6; do : > \"$6/f$i.png\"; done\necho end >> '{0}'\n", log.display());
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let overlay = tmp.join("probe");
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(overlay.join("overlay.json"), r#"{ "name": "Probe" }"#).unwrap();
    std::fs::write(overlay.join("index.html"), "<html><body data-composition-id=\"root\"></body></html>").unwrap();
    let manifest = OverlayManifest::load(&overlay).unwrap();
    let mut baker = Baker::discover(tmp, tmp.join("vendor"), tmp.join("cache"));
    baker.hyperframes = fake;
    baker.version = Some("stand-in".into());
    baker.browser = None;
    (baker, overlay, manifest, log)
}

/// Two bakes of one key at once — an agent retrying a sample while the first is still
/// baking — render once: the second waits and reads the first's result.
#[test]
fn concurrent_bakes_of_one_key_render_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (baker, overlay, manifest, log) = stand_in(tmp.path());
    let vars = Map::new();
    let req = BakeRequest { overlay_dir: &overlay, manifest: &manifest, vars: &vars, fps: Fps::new(6, 1), width: 64, height: 36, duration: 1.0, speed: 1.0 };
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| baker.bake(&req).expect("first bake"));
        let b = s.spawn(|| baker.bake(&req).expect("second bake"));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(std::fs::read_to_string(&log).unwrap().matches("start").count(), 1, "the renderer ran for each bake");
    assert_eq!(a.key, b.key);
    assert!(a.cached != b.cached, "one bake renders, the other reads it");
}

/// Bakes of different keys run side by side up to `concurrency` and no further, however
/// many callers ask at once: the limit is the baker's, not the loop's that asks.
#[test]
fn renders_run_side_by_side_up_to_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut baker, overlay, manifest, log) = stand_in(tmp.path());
    baker.concurrency = 2;

    // Four keys: the width is in the key, and the stand-in does not read it.
    let vars = Map::new();
    let reqs: Vec<BakeRequest> = (0..4).map(|i| BakeRequest { overlay_dir: &overlay, manifest: &manifest, vars: &vars, fps: Fps::new(6, 1), width: 64 + 2 * i, height: 36, duration: 1.0, speed: 1.0 }).collect();
    std::thread::scope(|s| {
        for req in &reqs {
            let baker = &baker;
            s.spawn(move || baker.bake(req).expect("bake"));
        }
    });
    // The most running at once, counted down the log.
    let text = std::fs::read_to_string(&log).unwrap();
    let most = text.lines().scan(0, |now, l| {
        *now += if l == "start" { 1 } else { -1 };
        Some(*now)
    });
    assert_eq!(text.lines().count(), 8, "four renders, each a start and an end");
    assert_eq!(most.max(), Some(2), "two at a time, never more");
}
