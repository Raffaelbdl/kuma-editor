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
    #[error("patch failed: {0}")]
    Patch(String),
    #[error("resulting document is not a valid project: {0}")]
    Shape(String),
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
    if let Some(base) = base_revision {
        if base != current.revision {
            return Err(EditError::RevisionMismatch { current: current.revision, base });
        }
    }
    let mut doc: Value = serde_json::to_value(current).map_err(|e| EditError::Shape(e.to_string()))?;
    json_patch::patch(&mut doc, patch).map_err(|e| EditError::Patch(e.to_string()))?;
    finish(current, doc, ctx)
}

/// Replace the whole document (used by `set_project` and by undo/redo).
pub fn replace(
    current: &Project,
    base_revision: Option<u64>,
    doc: Value,
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    if let Some(base) = base_revision {
        if base != current.revision {
            return Err(EditError::RevisionMismatch { current: current.revision, base });
        }
    }
    finish(current, doc, ctx)
}

fn finish(current: &Project, mut doc: Value, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    // The revision is owned by the editor, never by the patch.
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("revision".into(), Value::from(current.revision + 1));
    }
    let project: Project = serde_json::from_value(doc).map_err(|e| EditError::Shape(e.to_string()))?;
    let issues = validate(&project, ctx);
    if has_errors(&issues) {
        return Err(EditError::Invalid(issues));
    }
    Ok(EditOutcome { project, warnings: issues })
}

/// Compute the patch that turns `a` into `b` (used to describe undo entries).
pub fn diff(a: &Project, b: &Project) -> json_patch::Patch {
    let va = serde_json::to_value(a).unwrap_or(Value::Null);
    let vb = serde_json::to_value(b).unwrap_or(Value::Null);
    json_patch::diff(&va, &vb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_bumps_revision_and_validates() {
        let p: Project = serde_json::from_value(json!({
            "assets": { "a": { "path": "a.mp4" } },
            "tracks": [{ "id": "v1", "clips": [] }]
        }))
        .unwrap();
        let patch: json_patch::Patch = serde_json::from_value(json!([
            { "op": "add", "path": "/tracks/0/clips/-", "value": { "id": "c1", "start": 0, "end": 2, "src": { "asset": "a" } } }
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
