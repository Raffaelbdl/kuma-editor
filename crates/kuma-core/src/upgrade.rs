//! Reading a project file that an older kuma wrote.
//!
//! The document carries a `version`, and the validator refuses anything that is not
//! [`crate::PROJECT_VERSION`] — a refusal that only helps if something brings older files
//! forward first. That is this module: one step per bump, applied in order, on the JSON
//! before it is deserialized, because a struct that can no longer represent the old shape
//! cannot be the thing that reads it.
//!
//! Every project is read through [`parse_project`]. Nothing else parses one.

use serde_json::Value;

use crate::model::Project;

/// Parse a project file, upgrading it to the current format on the way in.
pub fn parse_project(text: &str) -> serde_json::Result<Project> {
    let mut v: Value = serde_json::from_str(text)?;
    upgrade(&mut v);
    // The one older shape that is refused rather than brought forward (6 → 7 below). Said
    // here, in words, because the struct cannot read it and serde's own complaint would be
    // that no variant matched.
    if let Some((id, template)) = template_overlay(&v) {
        return Err(serde::de::Error::custom(format!(
            "bin entry `{id}` names the template `{template}`, which an older kuma wrote: overlays now live in the project. \
             Copy the blueprint into `overlays/{template}/` (renaming `template.json` to `overlay.json` and dropping its `id`), \
             then set the entry's source to {{ \"overlay\": \"overlays/{template}\" }}, keeping its `vars`, and `version` to {}",
            crate::PROJECT_VERSION
        )));
    }
    serde_json::from_value(v)
}

/// Bring `v` forward to [`crate::PROJECT_VERSION`], one version at a time. A document
/// that is already current, or one whose version is unknown (newer than this binary), is
/// left exactly as it is: the validator is what says so, and it says it in words.
fn upgrade(v: &mut Value) {
    // A missing version means "the current one" to serde, so it means that here too.
    let Some(mut version) = v.get("version").and_then(Value::as_u64) else { return };
    if version == 1 {
        // 1 → 2: keyframes gained `ease`, whose default — `linear` — is exactly what a
        // version 1 keyframe always did. So nothing in the document has to change, and
        // the stamp is the whole migration.
        version = 2;
    }
    if version == 2 {
        // 2 → 3: crop, blur and color took `keyframes`, defaulting to none; and a
        // transform's or volume's keyframes were already the shape the shared type
        // reads. A stamp again.
        version = 3;
    }
    if version == 3 {
        // 3 → 4: a clip may carry a `group`, and no group is the absent field. Nothing
        // in an older document has to change, so this is a stamp too.
        version = 4;
    }
    if version == 4 {
        // 4 → 5: `volume` gained a static `gain`, defaulting to 1 — and a key without a
        // gain, which used to mean 1, now means the static, which is 1. An empty
        // `keyframes` is allowed now rather than refused. `audio_fade` is a new kind, so
        // no older document has one. A stamp again.
        version = 5;
    }
    if version == 5 {
        // 5 → 6: a bin entry may carry a `volume`, absent meaning 1. A stamp.
        version = 6;
    }
    if version == 6 && template_overlay(v).is_none() {
        // 6 → 7: an overlay entry names a folder of the project's own (`overlay`) where it
        // used to name a library template (`template`). Deliberately **not** migrated: a
        // project with no overlay entries is stamped, and one with them stays at 6, which
        // the validator refuses in words. Nothing relied on the old shape yet, and bringing
        // one forward would mean copying folders from a library this parse cannot see.
        version = 7;
    }
    v["version"] = Value::from(version);
}

/// The first bin entry that is a version 6 overlay — one naming a template rather than a
/// folder — as `(entry id, template id)`.
fn template_overlay(v: &Value) -> Option<(String, String)> {
    let bin = v.get("bin")?.as_object()?;
    bin.iter().find_map(|(id, e)| Some((id.clone(), e.pointer("/source/template")?.as_str()?.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = r#"{
      "version": 1, "name": "old", "master": "main",
      "fps": { "num": 30, "den": 1 }, "width": 1920, "height": 1080, "sample_rate": 48000,
      "bin": { "main": { "name": "Main", "source": { "tracks": [], "markers": [] } } }
    }"#;

    #[test]
    fn a_version_1_project_still_opens() {
        let p = parse_project(V1).expect("parse");
        assert_eq!(p.version, crate::PROJECT_VERSION);
        let issues = crate::validate(&p, &crate::ValidationContext { root: std::path::PathBuf::from("."), check_files: false });
        assert!(!crate::validate::has_errors(&issues), "{issues:?}");
    }

    /// Each step is a stamp so far, and each is taken in turn: a version 2 file lands on
    /// the current version too, not on 3-as-of-when-it-was-written.
    #[test]
    fn every_older_version_climbs_to_the_current_one() {
        for from in [1, 2, 3, 4, 5, 6] {
            let text = V1.replace("\"version\": 1", &format!("\"version\": {from}"));
            assert_eq!(parse_project(&text).expect("parse").version, crate::PROJECT_VERSION, "from {from}");
        }
    }

    /// A template overlay is refused, and the refusal says what to do — rather than being
    /// upgraded into an entry that points at a folder which does not exist.
    #[test]
    fn a_version_6_template_overlay_is_refused_in_words() {
        let text = V1.replace("\"version\": 1", "\"version\": 6").replace(
            r#""bin": {"#,
            r#""bin": { "tag": { "source": { "template": "location-tag" } },"#,
        );
        let mut v: Value = serde_json::from_str(&text).unwrap();
        upgrade(&mut v);
        assert_eq!(v["version"], 6, "not stamped");
        let e = parse_project(&text).expect_err("refused").to_string();
        assert!(e.contains("`tag`") && e.contains("overlays/location-tag"), "{e}");
    }

    #[test]
    fn a_version_from_the_future_is_left_for_the_validator_to_refuse() {
        let text = V1.replace("\"version\": 1", "\"version\": 99");
        let p = parse_project(&text).expect("parse");
        assert_eq!(p.version, 99, "an unknown version is reported, not silently adopted");
    }
}
