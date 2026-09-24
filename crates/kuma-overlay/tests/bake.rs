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

fn rgba_at(png: &std::path::Path, x: u32, y: u32) -> [u8; 4] {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(png)
        .args(["-vf", &format!("crop=1:1:{x}:{y}"), "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .output()
        .expect("ffmpeg on PATH");
    <[u8; 4]>::try_from(out.stdout.as_slice()).unwrap_or_else(|_| panic!("ffmpeg gave {} bytes for {}: {}", out.stdout.len(), png.display(), String::from_utf8_lossy(&out.stderr)))
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
/// separate caches give the same pixels in every frame. The PNG bytes may differ; the
/// image may not. `paint-wipe` also covers the whole frame at its midpoint, the cut.
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
    let req = BakeRequest { overlay_dir: &dir, manifest: &manifest, vars: &vars, fps: Fps::new(12, 1), width: 480, height: 270, duration: 1.0, speed: 1.0 };
    let (ra, rb) = (a.bake(&req).expect("bake a"), b.bake(&req).expect("bake b"));
    assert_eq!(ra.frames, 12);
    let pixels = |dir: &std::path::Path, n: i64| {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(dir.join(format!("frames/frame_{n:06}.png")))
            .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
            .output()
            .expect("ffmpeg on PATH");
        out.stdout
    };
    for n in 1..=ra.frames {
        assert!(pixels(&ra.dir, n) == pixels(&rb.dir, n), "frame {n} differs between two bakes");
    }
    let mid = ra.dir.join("frames/frame_000007.png"); // t = 0.5, the midpoint
    for (x, y) in [(2, 2), (477, 2), (240, 135), (2, 267), (477, 267)] {
        assert_eq!(rgba_at(&mid, x, y)[3], 255, "the midpoint must cover the frame at ({x}, {y})");
    }
    assert_eq!(rgba_at(&ra.dir.join("frames/frame_000001.png"), 240, 135)[3], 0, "nothing is drawn at t = 0");
}
