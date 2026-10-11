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

/// [`replace`] with a document already typed — one this editor kept, so there is no JSON
/// to parse it from. The revision is still the editor's, and it is validated all the same.
pub fn replace_project(
    current: &Project,
    base_revision: Option<u64>,
    mut project: Project,
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    project.revision = current.revision + 1;
    checked(project, ctx)
}

fn finish(current: &Project, mut doc: Value, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    // The revision is owned by the editor, never by the patch.
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("revision".into(), Value::from(current.revision + 1));
    }
    let project: Project = serde_json::from_value(doc).map_err(|e| EditError::Patch(format!("result is not a valid project: {e}")))?;
    checked(project, ctx)
}

fn checked(project: Project, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    let issues = validate(&project, ctx);
    if has_errors(&issues) {
        return Err(EditError::Invalid(issues));
    }
    Ok(EditOutcome { project, warnings: issues })
}

/// The JSON Patch that makes `live` the project `wanted` describes: what a script that
/// builds a whole project sends, instead of working out the difference itself. Both are
/// compared as kuma stores them, so a field the script left at its default, or wrote as
/// kuma would not, is no difference. Two things a script does not know are `live`'s to
/// keep: the revision, and the probed `meta` of an entry the script gives none for. With
/// `keep`, bin entries `wanted` does not have stay too (someone else's, or not yet placed);
/// without it they are removed. Apply the patch at `live`'s revision; made again
/// afterwards, it is empty.
pub fn diff(live: &Project, wanted: &str, keep: bool) -> serde_json::Result<json_patch::Patch> {
    let mut wanted = crate::parse_project(wanted)?;
    wanted.revision = live.revision;
    for (id, theirs) in &live.bin {
        match wanted.bin.get_mut(id) {
            Some(mine) if mine.meta.is_none() => mine.meta = theirs.meta.clone(),
            None if keep => drop(wanted.bin.insert(id.clone(), theirs.clone())),
            _ => {}
        }
    }
    Ok(json_patch::diff(&serde_json::to_value(live)?, &serde_json::to_value(&wanted)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A script's document, written its own way, against the stored one: only what really
    /// differs is sent, and sending it leaves nothing to send.
    #[test]
    fn a_diff_sends_what_differs_and_then_nothing() {
        let live: Project = serde_json::from_value(json!({
            "revision": 7,
            "bin": {
                "a": { "source": { "path": "a.mp4" }, "meta": { "duration": 9.5, "has_audio": true } },
                "theirs": { "source": { "color": "#ffffff" } },
                "main": { "source": { "tracks": [{ "id": "v1", "clips": [{ "id": "c1", "start": 0, "end": 2, "bin": "a" }] }] } }
            }
        }))
        .unwrap();
        // No revision, no meta, defaults spelled out or left out as the script pleases.
        let wanted = |end: f64| json!({ "bin": {
            "a": { "source": { "path": "a.mp4" } },
            "main": { "source": { "tracks": [{ "id": "v1", "muted": false, "clips": [{ "id": "c1", "start": 0.0, "end": end, "bin": "a", "speed": 1 }] }] } }
        } }).to_string();
        assert_eq!(diff(&live, &wanted(2.0), true).unwrap().0, [], "the same project, said differently");

        let ops = |keep| serde_json::to_value(diff(&live, &wanted(3.0), keep).unwrap()).unwrap();
        let retime = json!({ "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/end", "value": 3.0 });
        assert_eq!(ops(true), json!([retime]));
        assert_eq!(ops(false), json!([retime, { "op": "remove", "path": "/bin/theirs" }]), "an entry the script does not make goes, unless kept");

        let after = apply_patch(&live, Some(7), &diff(&live, &wanted(3.0), false).unwrap().0, &ValidationContext::default()).unwrap().project;
        assert_eq!(after.bin["a"].meta, live.bin["a"].meta, "what was probed is kept");
        assert_eq!(diff(&after, &wanted(3.0), false).unwrap().0, [], "applied, there is nothing left to send");
    }

    #[test]
    fn patch_bumps_revision_and_validates() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "a": { "source": { "path": "a.mp4" } },
                "main": { "source": { "tracks": [{ "id": "v1", "clips": [] }] } }
            }
        }))
        .unwrap();
        let clips = "/bin/main/source/tracks/0/clips";
        let patch: json_patch::Patch = serde_json::from_value(json!([
            { "op": "add", "path": format!("{clips}/-"), "value": { "id": "c1", "start": 0, "end": 2, "bin": "a" } }
        ]))
        .unwrap();
        let out = apply_patch(&p, Some(0), &patch.0, &ValidationContext::default()).unwrap();
        assert_eq!(out.project.revision, 1);
        assert_eq!(out.project.master_seq().unwrap().tracks[0].clips.len(), 1);

        // Stale base revision is rejected.
        let err = apply_patch(&out.project, Some(0), &patch.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::RevisionMismatch { .. }));

        // A patch that produces an invalid project is rejected atomically.
        let bad: json_patch::Patch = serde_json::from_value(json!([
            { "op": "replace", "path": format!("{clips}/0/end"), "value": -1 }
        ]))
        .unwrap();
        let err = apply_patch(&out.project, None, &bad.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::Invalid(_)));

        // A typed replace takes the revision past the current one, whatever the document
        // says, and is validated like a patch.
        let back = replace_project(&out.project, Some(1), Project { revision: 0, ..p.clone() }, &ValidationContext::default()).unwrap();
        assert_eq!((back.project.revision, back.project.master_seq().unwrap().tracks[0].clips.len()), (2, 0));
        let mut broken = out.project.clone();
        broken.master = "nowhere".into();
        assert!(matches!(replace_project(&out.project, None, broken, &ValidationContext::default()), Err(EditError::Invalid(_))));
        assert!(matches!(replace_project(&out.project, Some(0), p, &ValidationContext::default()), Err(EditError::RevisionMismatch { .. })));
    }
}
