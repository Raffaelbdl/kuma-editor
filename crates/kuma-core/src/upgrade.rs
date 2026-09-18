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
    serde_json::from_value(v)
}

/// Bring `v` forward to [`crate::PROJECT_VERSION`], one version at a time. A document
/// that is already current, or one whose version is unknown (newer than this binary), is
/// left exactly as it is: the validator is what says so, and it says it in words.
fn upgrade(v: &mut Value) {
    // A missing version means "the current one" to serde, so it means that here too.
    let Some(version) = v.get("version").and_then(Value::as_u64) else { return };
    if version == 1 {
        // 1 → 2: keyframes gained `ease`, whose default — `linear` — is exactly what a
        // version 1 keyframe always did. So nothing in the document has to change, and
        // the stamp is the whole migration.
        v["version"] = Value::from(2);
    }
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
        let issues = crate::validate(&p, &crate::ValidationContext { root: std::path::PathBuf::from("."), templates: None, check_files: false });
        assert!(!crate::validate::has_errors(&issues), "{issues:?}");
    }

    #[test]
    fn a_version_from_the_future_is_left_for_the_validator_to_refuse() {
        let text = V1.replace("\"version\": 1", "\"version\": 99");
        let p = parse_project(&text).expect("parse");
        assert_eq!(p.version, 99, "an unknown version is reported, not silently adopted");
    }
}
