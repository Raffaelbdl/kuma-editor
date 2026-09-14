//! Bakes the real `location-tag` template through hyperframes. Skips when the renderer or
//! a browser is unavailable.

use std::path::PathBuf;

use kuma_core::template::TemplateManifest;
use kuma_core::Fps;
use kuma_overlay::{BakeRequest, Baker, Format};
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
    let baker = Baker::discover(&root, tmp.path().join("cache"));
    if baker.available().is_err() {
        eprintln!("hyperframes unavailable; skipping bake test");
        return;
    }
    if baker.browser.is_none() && std::env::var_os("KUMA_ALLOW_BROWSER_DOWNLOAD").is_none() {
        eprintln!("no headless browser found; skipping bake test");
        return;
    }
    let dir = root.join("overlays/location-tag");
    let manifest = TemplateManifest::load(&dir).unwrap();
    let mut vars = Map::new();
    vars.insert("label".into(), json!("Tokyo"));
    vars.insert("accent".into(), json!("#00a0ff"));

    let req = BakeRequest {
        template_dir: &dir, manifest: &manifest, vars: &vars, fps: Fps::new(30, 1), width: 1920, height: 1080,
        duration: 2.0, speed: 1.0, format: Format::PngSequence, supersample: 1,
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

    // A clip LONGER than the template's markup default (4 s) must keep animating: the bake
    // used to render only the default and pad the rest with the last frame.
    let long = BakeRequest { fps: Fps::new(4, 1), duration: 8.0, speed: 1.0, ..req.clone() };
    let r4 = baker.bake(&long).expect("long bake");
    assert_eq!(r4.frames, 32, "8 s at 4 fps");
    let f = |n: u32| std::fs::read(r4.dir.join(format!("frames/frame_{n:06}.png"))).unwrap();
    // t=4.0 (past the 4 s default) must still show the tag: frame 1 (t=0) is the empty
    // pre-entrance state, so a frozen/unmounted overlay would make them identical.
    assert_ne!(f(17), f(1), "overlay must still be mounted past the template's default duration");
    // The template anchors its out-animation to the real end (7.55–8 s): t=7.75 differs from t=4.0.
    assert_ne!(f(32), f(17), "out-animation must happen at the clip's real end, not the default's");

    // A static template bakes a single still frame regardless of the clip duration.
    let wm_dir = root.join("overlays/watermark");
    let wm = TemplateManifest::load(&wm_dir).unwrap();
    let novars = Map::new();
    let still = baker
        .bake(&BakeRequest { template_dir: &wm_dir, manifest: &wm, vars: &novars, fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 10.0, speed: 1.0, format: Format::PngSequence, supersample: 1 })
        .expect("static bake");
    assert!(still.is_still);
    assert_eq!(still.frames, 1);
    assert!(still.resource.ends_with("frame_000001.png"));
    // Bottom-right corner carries the watermark; top-left is transparent.
    assert_eq!(rgba_at(&still.resource, 10, 10)[3], 0);
    assert!(rgba_at(&still.resource, 1920 - 60, 1080 - 50)[3] > 60, "watermark chip expected at bottom-right");
}
