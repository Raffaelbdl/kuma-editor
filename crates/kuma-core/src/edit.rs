//! Editing the project as a document: RFC 6902 JSON Patch with revision checks.
//!
//! Every mutation — from the UI or from the agent — goes through [`apply_patch`]. The
//! result is re-parsed against the typed model (so unknown fields and bad shapes are
//! rejected) and revalidated; only then is the revision bumped.

use serde_json::Value;

use crate::model::Project;
use crate::validate::{has_errors, validate, Issue, ValidationContext};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("revision mismatch: project is at {current}, edit was based on {base}")]
    RevisionMismatch { current: u64, base: u64 },
    /// The patch could not be applied or the result is not a project document.
    #[error("patch failed: {0}")]
    Patch(String),
    #[error("validation failed")]
    Invalid(Vec<Issue>),
}

/// Outcome of a successful edit.
#[derive(Debug, Clone)]
pub struct EditOutcome {
    pub project: Project,
    /// Warnings that did not block the edit.
    pub warnings: Vec<Issue>,
}

/// Apply a JSON Patch to `current`. `base_revision`, when given, must equal the current
/// revision. On success the revision is incremented.
pub fn apply_patch(
    current: &Project,
    base_revision: Option<u64>,
    patch: &[json_patch::PatchOperation],
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    let mut doc: Value = serde_json::to_value(current).map_err(|e| EditError::Patch(e.to_string()))?;
    json_patch::patch(&mut doc, patch).map_err(|e| EditError::Patch(e.to_string()))?;
    finish(current, doc, ctx)
}

fn check_base(current: &Project, base_revision: Option<u64>) -> Result<(), EditError> {
    match base_revision {
        Some(base) if base != current.revision => Err(EditError::RevisionMismatch { current: current.revision, base }),
        _ => Ok(()),
    }
}

/// Replace the whole document (undo/redo).
pub fn replace(
    current: &Project,
    base_revision: Option<u64>,
    doc: Value,
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    finish(current, doc, ctx)
}

fn finish(current: &Project, mut doc: Value, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    // The revision is owned by the editor, never by the patch.
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("revision".into(), Value::from(current.revision + 1));
    }
    let project: Project = serde_json::from_value(doc).map_err(|e| EditError::Patch(format!("result is not a valid project: {e}")))?;
    let issues = validate(&project, ctx);
    if has_errors(&issues) {
        return Err(EditError::Invalid(issues));
    }
    Ok(EditOutcome { project, warnings: issues })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_bumps_revision_and_validates() {
        let p: Project = serde_json::from_value(json!({
            "bin": { "a": { "source": { "path": "a.mp4" } } },
            "tracks": [{ "id": "v1", "clips": [] }]
        }))
        .unwrap();
        let patch: json_patch::Patch = serde_json::from_value(json!([
            { "op": "add", "path": "/tracks/0/clips/-", "value": { "id": "c1", "start": 0, "end": 2, "bin": "a" } }
        ]))
        .unwrap();
        let out = apply_patch(&p, Some(0), &patch.0, &ValidationContext::default()).unwrap();
        assert_eq!(out.project.revision, 1);
        assert_eq!(out.project.tracks[0].clips.len(), 1);

        // Stale base revision is rejected.
        let err = apply_patch(&out.project, Some(0), &patch.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::RevisionMismatch { .. }));

        // A patch that produces an invalid project is rejected atomically.
        let bad: json_patch::Patch = serde_json::from_value(json!([
            { "op": "replace", "path": "/tracks/0/clips/0/end", "value": -1 }
        ]))
        .unwrap();
        let err = apply_patch(&out.project, None, &bad.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::Invalid(_)));
    }
}
