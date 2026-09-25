//! The starter library is what everyone copies, and the scaffold is what agents start
//! from — so both have to pass the check that the contract describes. Needs no renderer:
//! hyperframes' own linter is added when it is installed and skipped when it is not, and
//! kuma's rules read the staged markup either way.

use std::path::{Path, PathBuf};

use kuma_core::manifest::OverlayManifest;
use kuma_core::Fps;
use kuma_overlay::{BakeRequest, Baker, LintReport};
use serde_json::Map;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn lint(baker: &Baker, dir: &Path) -> LintReport {
    let manifest = OverlayManifest::load(dir).unwrap_or_else(|e| panic!("{}: {e:?}", dir.display()));
    let req = BakeRequest {
        overlay_dir: dir,
        manifest: &manifest,
        vars: &Map::new(),
        fps: Fps::new(30, 1),
        width: 1920,
        height: 1080,
        duration: manifest.duration_default,
        speed: 1.0,
    };
    baker.lint(&req).expect("lint ran")
}

fn baker(tmp: &tempfile::TempDir) -> (Baker, PathBuf) {
    let root = repo_root();
    let vendor = root.join("library/blueprints/_vendor");
    // The starter fonts, as `kuma library` installs them: the titles name them.
    let fonts = vec![(root.join("library/fonts"), kuma_overlay::Origin::Library)];
    (Baker::discover(&root, vendor, tmp.path().join("cache")).with_fonts(fonts), root)
}

#[test]
fn every_shipped_blueprint_passes_its_own_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let (baker, root) = baker(&tmp);
    let mut checked = 0;
    for entry in std::fs::read_dir(root.join("library/blueprints")).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.join(kuma_core::manifest::MANIFEST).is_file() {
            continue; // `_vendor/` and anything else that is not a blueprint
        }
        // Clean, not merely error-free: a blueprint is read as an example of the contract,
        // like the scaffold, so a warning in it teaches the wrong thing — or is a rule
        // that is wrong, which is worth finding here rather than in someone's project.
        let report = lint(&baker, &dir);
        assert!(report.findings.is_empty(), "{}:\n  {}", dir.display(), report.lines().join("\n  "));
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} blueprints were checked — did the library move?");
}

/// The skeleton an agent edits must itself be beyond reproach: it is read as an example of
/// the contract, so a warning in it teaches the wrong thing.
#[test]
fn the_scaffold_lints_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let (baker, _) = baker(&tmp);
    let overlays = tmp.path().join("overlays");
    std::fs::create_dir_all(&overlays).unwrap();
    let dir = kuma_overlay::new_overlay(&overlays, "fresh", "Fresh", "").expect("scaffolded");

    let report = lint(&baker, &dir);
    assert!(report.findings.is_empty(), "the scaffold is not clean:\n  {}", report.lines().join("\n  "));

    // And it is refused rather than overwritten the second time.
    assert!(kuma_overlay::new_overlay(&overlays, "fresh", "Fresh", "").is_err());
    // An id that could not be a folder or a URL segment is refused before anything is written.
    assert!(kuma_overlay::new_overlay(&overlays, "2 bad/id", "", "").is_err());
    assert!(!overlays.join("2 bad").exists());
}

/// The check has to fail on an overlay that would fail to bake, or it is decoration. The
/// fault here is the one that costs 45 seconds and a cryptic timeout in real life.
#[test]
fn an_overlay_that_would_not_bake_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (baker, _) = baker(&tmp);
    let dir = tmp.path().join("overlays/broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(kuma_core::manifest::MANIFEST), r#"{ "name": "Broken", "vars": {} }"#).unwrap();
    std::fs::write(
        dir.join("index.html"),
        r#"<html><body><div id="root" data-composition-id="main" data-start="0" data-duration="4">x</div>
           <script>setInterval(() => {}, 100);</script></body></html>"#,
    )
    .unwrap();

    let report = lint(&baker, &dir);
    let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
    assert!(!report.ok(), "an overlay with no timeline and a wall clock passed: {codes:?}");
    assert!(codes.contains(&"no_timeline"), "{codes:?}");
    assert!(codes.contains(&"wall_clock"), "{codes:?}");
}
