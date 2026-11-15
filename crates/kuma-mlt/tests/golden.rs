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

/// What the reference project is compiled with: an animated bake (`o_c1`) and a still.
fn reference_options() -> CompileOptions {
    let mut opts = CompileOptions::new("/proj");
    opts.cache_dir = std::env::temp_dir().join("kuma-golden-cache");
    opts.bakes.insert("o_c1".into(), bake("/proj/.kuma/bakes/abc/frame_%06d.png", false));
    opts.bakes.insert("o_c2".into(), bake("/proj/.kuma/bakes/def/frame_000001.png", true));
    opts
}

#[test]
fn reference_project_matches_golden_and_the_dtd() {
    let p = reference_project();
    let opts = reference_options();
    // A grade's LUT is written under the cache, which is wherever the machine keeps its
    // temporary files; the golden file says where relative to it.
    let xml = compile(&p, &opts).unwrap().replace(&*opts.cache_dir.to_string_lossy(), "$CACHE");

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

/// A track that gives the graph nothing is left out: a hidden overlay track is neither
/// composited nor baked, so it compiles with no bake even when every overlay must have one.
/// A hidden track with sound keeps its sound (`hide="video"`), and so does what it plays.
/// The daemon's bake set follows the same rule (`Project::track_streams`): it once skipped
/// hidden tracks the compiler still emitted, and every render of such a project failed.
#[test]
fn a_hidden_track_gives_only_its_sound() {
    let p: Project = serde_json::from_value(serde_json::json!({
        "fps": 30,
        "bin": {
            "tag": { "source": { "overlay": "overlays/location-tag" } },
            "main": { "source": { "tracks": [{ "id": "o1", "kind": "overlay", "hidden": true, "clips": [
                { "id": "oh", "start": 0, "end": 3, "bin": "tag" }
            ]}]}}
        }
    }))
    .unwrap();
    let mut opts = CompileOptions::new("/proj");
    opts.unbaked = UnbakedPolicy::Error;
    let xml = compile(&p, &opts).expect("no bake is needed for an overlay nothing shows");
    assert!(!xml.contains("clip_oh"), "{xml}");

    // The reference's `mus` track plays `screen`'s sound: hidden, it is still heard.
    let mut p = reference_project();
    let Some(kuma_core::BinSource::Sequence(main)) = p.bin.get_mut("main").map(|i| &mut i.source) else { panic!("no master") };
    for t in &mut main.tracks {
        t.hidden = t.id == "mus" || t.id == "o1";
    }
    let xml = compile(&p, &CompileOptions::new("/proj")).unwrap();
    assert!(xml.contains(r#"hide="video""#) && !xml.contains(r#"hide="both""#), "{xml}");
    assert!(!xml.contains("clip_o_c1"), "the hidden overlay track is gone");
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

/// A scrubbed graph — the preview's — reads the animated bake with `qimage` and changes
/// nothing else; the golden file is the render's (*The preview reads animated bakes with
/// `qimage`* in docs/decisions.md).
#[test]
fn a_scrubbed_graph_reads_animated_bakes_with_qimage() {
    let p = reference_project();
    let mut opts = reference_options();
    let rendered = compile(&p, &opts).unwrap();
    opts.scrubbed = true;
    let scrubbed = compile(&p, &opts).unwrap();

    let producer = |xml: &str| {
        let at = xml.find(r#"<producer id="clip_o_c1""#).expect("the animated bake is in the graph");
        xml[at..at + xml[at..].find("</producer>").unwrap()].to_string()
    };
    let (before, after) = (producer(&rendered), producer(&scrubbed));
    assert_eq!(
        after,
        r#"<producer id="clip_o_c1" in="0" out="89">
    <property name="mlt_service">qimage</property>
    <property name="ttl">1</property>
    <property name="begin">1</property>
    <property name="resource">/proj/.kuma/bakes/abc/frame_%06d.png</property>
    <property name="length">90</property>
    <property name="mlt_image_format">rgba</property>
  "#
    );
    assert_eq!(rendered.replace(&before, ""), scrubbed.replace(&after, ""), "only that producer changes");
}
