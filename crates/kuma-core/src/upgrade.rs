//! Reading a project file that an older kuma wrote.
//!
//! The document carries a `version`, and the validator refuses anything that is not
//! [`crate::PROJECT_VERSION`] — a refusal that only helps if something brings older files
//! forward first. That is this module: one step per bump, applied in order, on the JSON
//! before it is deserialized, because a struct that can no longer represent the old shape
//! cannot be the thing that reads it.
//!
//! Every project is read through [`parse_project`]. Nothing else parses one; and every
//! project file is written as [`project_text`] makes it.

use serde_json::Value;

use crate::model::Project;

/// Parse a project file, upgrading it to the current format on the way in.
pub fn parse_project(text: &str) -> serde_json::Result<Project> {
    let mut v: Value = serde_json::from_str(text)?;
    upgrade(&mut v).map_err(serde::de::Error::custom)?;
    serde_json::from_value(v)
}

/// The text of a project file, as every writer of one writes it. Fields at their defaults
/// are left out (a keyframe's `ease: "linear"`, an empty `group`), so a document a script
/// built and the file kuma stored differ where they mean the same: compare [`canonical`]
/// forms, never a hand-built document against a stored one.
pub fn project_text(p: &Project) -> serde_json::Result<String> {
    serde_json::to_string_pretty(p)
}

/// `text`, a project document of any version, in the form kuma stores it: parsed, brought
/// forward and written back (`kuma canonical`, `POST /api/project/canonical`).
pub fn canonical(text: &str) -> serde_json::Result<String> {
    project_text(&parse_project(text)?)
}

/// Bring `v` forward to [`crate::PROJECT_VERSION`], one version at a time. A document
/// that is already current, or one whose version is unknown (newer than this binary), is
/// left exactly as it is: the validator is what says so, and it says it in words. A step
/// that cannot bring a document forward refuses it, in words of its own.
fn upgrade(v: &mut Value) -> Result<(), String> {
    // A missing version means "the current one" to serde, so it means that here too.
    let Some(mut version) = v.get("version").and_then(Value::as_u64) else { return Ok(()) };
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
    if version == 6 {
        // 6 → 7: overlay entries name a project folder (`overlay`), not a library template
        // (`template`). Not migrated: one with template entries is refused, saying how to
        // convert it, since this parse cannot see the library to copy from.
        if let Some((id, template)) = template_overlay(v) {
            return Err(format!("bin entry `{id}` names template `{template}` (before v7): copy the blueprint to `overlays/{template}/` and set its source to {{ \"overlay\": \"overlays/{template}\" }}"));
        }
        version = 7;
    }
    if version == 7 {
        // 7 → 8: a sequence may carry `effects`, defaulting to none, and `limiter`,
        // `auto_level` and `freeze` are new kinds, so no older document has one. A stamp.
        version = 8;
    }
    if version == 8 {
        // 8 → 9: a marker may carry an `origin`, who left it; absent reads as a person's,
        // which every older marker was. A stamp — the bump is only so that an older kuma
        // refuses a v9 file rather than load it and drop the origins when it saves.
        version = 9;
    }
    if version == 9 {
        // 9 → 10: a project may name the `libraries` it uses, absent meaning none. A stamp,
        // so that an older kuma refuses a v10 file rather than drop its libraries on save.
        version = 10;
    }
    if version == 10 {
        // 10 → 11: `grade` is a new effect kind, so no older document has one. A stamp, so
        // that an older kuma refuses a v11 file rather than fail on its first grade.
        version = 11;
    }
    v["version"] = Value::from(version);
    Ok(())
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
        let issues = crate::validate(&p, &crate::ValidationContext { root: std::path::PathBuf::from("."), check_files: false, ..Default::default() });
        assert!(!crate::validate::has_errors(&issues), "{issues:?}");
    }

    /// Each step is a stamp so far, and each is taken in turn: a version 2 file lands on
    /// the current version too, not on 3-as-of-when-it-was-written.
    #[test]
    fn every_older_version_climbs_to_the_current_one() {
        for from in [1, 2, 3, 4, 5, 6, 7, 8] {
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
        assert!(upgrade(&mut v).is_err());
        assert_eq!(v["version"], 6, "not stamped");
        let e = parse_project(&text).expect_err("refused").to_string();
        assert!(e.contains("`tag`") && e.contains("overlays/location-tag"), "{e}");
    }

    /// What a script writes and what kuma stores agree once both are canonical: defaults
    /// spelled out are dropped, the version is brought forward, and a canonical document
    /// is its own canonical form.
    #[test]
    fn a_canonical_document_is_the_one_kuma_stores() {
        let text = V1.replace(
            r#""tracks": []"#,
            r#""tracks": [{ "id": "v1", "kind": "video", "clips": [{ "id": "c1", "bin": "main", "start": 0, "end": 2, "effects": [
                { "type": "transform", "keyframes": [{ "t": 0, "x": 0, "ease": "linear" }, { "t": 1, "x": 10, "ease": "hold" }] }] }] }]"#,
        );
        let once = canonical(&text).expect("canonical");
        let v: Value = serde_json::from_str(&once).unwrap();
        let keys = v.pointer("/bin/main/source/tracks/0/clips/0/effects/0/keyframes").unwrap();
        assert!(keys[0].get("ease").is_none(), "linear is the default, so it is not stored: {keys}");
        assert_eq!(keys[1]["ease"], "hold");
        assert_eq!(v["version"], crate::PROJECT_VERSION);
        assert_eq!(canonical(&once).unwrap(), once, "canonical twice is canonical once");
        assert_eq!(once, project_text(&parse_project(&text).unwrap()).unwrap(), "what a project file holds");
    }

    #[test]
    fn a_version_from_the_future_is_left_for_the_validator_to_refuse() {
        let text = V1.replace("\"version\": 1", "\"version\": 99");
        let p = parse_project(&text).expect("parse");
        assert_eq!(p.version, 99, "an unknown version is reported, not silently adopted");
    }
}
