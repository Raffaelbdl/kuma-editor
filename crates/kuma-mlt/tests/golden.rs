//! Golden-file test: the compiler's output for a reference project must not drift
//! unintentionally. Regenerate with `UPDATE_GOLDEN=1 cargo test -p kuma-mlt`.

use std::path::PathBuf;

use kuma_core::Project;
use kuma_mlt::{compile, CompileOptions, OverlayBake};

fn reference_project() -> Project {
    serde_json::from_str(include_str!("fixtures/reference.kuma.json")).unwrap()
}

#[test]
fn reference_project_matches_golden() {
    let p = reference_project();
    let mut opts = CompileOptions::new("/proj");
    opts.cache_dir = std::env::temp_dir().join("kuma-golden-cache");
    opts.bakes.insert(
        "o_c1".into(),
        OverlayBake { resource: PathBuf::from("/proj/.kuma/bakes/abc/frame_%06d.png"), region: None, is_still: false },
    );
    let xml = compile(&p, &opts).unwrap();

    let golden_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.mlt");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden_path, &xml).unwrap();
    }
    let golden = std::fs::read_to_string(&golden_path).expect("golden file exists (run with UPDATE_GOLDEN=1)");
    assert_eq!(xml, golden, "compiler output drifted from tests/fixtures/reference.mlt");
}

#[test]
fn golden_validates_against_dtd() {
    // The DTD shipped with MLT 7.22 has a syntax error in `properties`; we keep a fixed copy.
    let dtd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mlt-xml.dtd");
    let mlt = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.mlt");
    if !mlt.is_file() {
        return;
    }
    let ok = std::process::Command::new("xmllint")
        .args(["--noout", "--dtdvalid"])
        .arg(&dtd)
        .arg(&mlt)
        .status()
        .map(|s| s.success());
    match ok {
        Ok(true) => {}
        Ok(false) => panic!("reference.mlt does not validate against the MLT DTD"),
        Err(_) => eprintln!("xmllint not installed; skipping DTD validation"),
    }
}
