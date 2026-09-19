//! Golden-file test: the compiler's output for a reference project must not drift
//! unintentionally. Regenerate with `UPDATE_GOLDEN=1 cargo test -p kuma-mlt`.

use std::path::PathBuf;

use kuma_core::Project;
use kuma_mlt::{compile, CompileError, CompileOptions, OverlayBake, UnbakedPolicy};

/// Read through `parse_project`, as the daemon does — so the fixture stays the version 1
/// document it was written as, and the golden file being unchanged by the upgrade is the
/// proof that the upgrade changes nothing about what is rendered.
fn reference_project() -> Project {
    kuma_core::parse_project(include_str!("fixtures/reference.kuma.json")).unwrap()
}

fn bake(path: &str, is_still: bool) -> OverlayBake {
    OverlayBake { resource: PathBuf::from(path), is_still }
}

#[test]
fn reference_project_matches_golden_and_the_dtd() {
    let p = reference_project();
    let mut opts = CompileOptions::new("/proj");
    opts.cache_dir = std::env::temp_dir().join("kuma-golden-cache");
    opts.bakes.insert("o_c1".into(), bake("/proj/.kuma/bakes/abc/frame_%06d.png", false));
    opts.bakes.insert("o_c2".into(), bake("/proj/.kuma/bakes/def/frame_000001.png", true));
    let xml = compile(&p, &opts).unwrap();

    let golden_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.mlt");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden_path, &xml).unwrap();
    }
    let golden = std::fs::read_to_string(&golden_path).expect("golden file exists (run with UPDATE_GOLDEN=1)");
    assert_eq!(xml, golden, "compiler output drifted from tests/fixtures/reference.mlt");

    // Validate what was just compiled, not what is on disk. The DTD shipped with MLT 7.22
    // has a syntax error in `properties`; we keep a fixed copy.
    let dtd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mlt-xml.dtd");
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &xml).unwrap();
    match std::process::Command::new("xmllint").args(["--noout", "--dtdvalid"]).arg(&dtd).arg(tmp.path()).status().map(|s| s.success()) {
        Ok(true) => {}
        Ok(false) => panic!("compiled XML does not validate against the MLT DTD"),
        Err(_) => eprintln!("xmllint not installed; skipping DTD validation"),
    }
}

/// Hiding is per track, applied on the tractor, so a hidden track's clips are still
/// compiled and a hidden overlay still needs its bake. The daemon picks the bake set, and
/// skipping hidden tracks there made every render of such a project fail.
#[test]
fn a_hidden_track_still_needs_its_overlays_baked() {
    let p: Project = serde_json::from_value(serde_json::json!({
        "fps": 30,
        "bin": {
            "tag": { "source": { "template": "location-tag" } },
            "main": { "source": { "tracks": [{ "id": "o1", "kind": "overlay", "hidden": true, "clips": [
                { "id": "oh", "start": 0, "end": 3, "bin": "tag" }
            ]}]}}
        }
    }))
    .unwrap();
    let mut opts = CompileOptions::new("/proj");
    opts.unbaked = UnbakedPolicy::Error;
    assert!(matches!(compile(&p, &opts), Err(CompileError::Unbaked(id)) if id == "oh"));
    opts.bakes.insert("oh".into(), bake("/proj/.kuma/bakes/x/frame_%06d.png", false));
    assert!(compile(&p, &opts).unwrap().contains(r#"<producer id="clip_oh""#));
}

#[test]
fn unbaked_overlays_skip_or_fail_by_policy() {
    let p = reference_project();
    let mut opts = CompileOptions::new("/proj");
    let xml = compile(&p, &opts).unwrap();
    assert!(!xml.contains("clip_o_c1"), "unbaked overlays are left out under Skip");
    opts.unbaked = UnbakedPolicy::Error;
    assert!(matches!(compile(&p, &opts), Err(CompileError::Unbaked(id)) if id == "o_c1"));
}
