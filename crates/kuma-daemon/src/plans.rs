//! A plan: a whole project document that a script wrote, made the project in one edit.
//!
//! This is the loop agents built for themselves out of `kuma diff` and `apply_edits`
//! (`docs/decisions.md`, *Field report: the edit loop agents built around MCP*), as one
//! call: diff against the live document under the edit lock, validate, apply, and keep the
//! plan as the base of the next diff, so a rebuild leaves what a person changed in the
//! editor since. Nothing here is a second way to edit: the patch is applied, validated,
//! recorded and undone like any other.

use std::path::{Path, PathBuf};

use kuma_core::Issue;
use serde::{Deserialize, Serialize};

use crate::{jobs, AppState};

/// What to apply, and how.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanRequest {
    /// The plan, a project document in a file inside the project (relative to it, or
    /// absolute under it), e.g. `build/plan.json`.
    pub path: String,
    /// Leave bin entries the plan does not have, rather than remove them.
    #[serde(default)]
    pub keep: bool,
    /// Make the project exactly the plan: a two-way diff, ignoring the plan last applied
    /// from this path, so what was changed in the editor since goes back too.
    #[serde(default)]
    pub exact: bool,
    /// Work out what would change and change nothing; the patch is in the answer.
    #[serde(default)]
    pub dry_run: bool,
    /// Who asks, for the history (`mcp: …`, `cli: apply`).
    #[serde(default)]
    pub origin: Option<String>,
    /// Ids of the markers this edit answers, as `apply_edits` takes them.
    #[serde(default)]
    pub answers: Vec<String>,
}

/// What a plan did.
#[derive(Debug, Serialize)]
pub struct Applied {
    /// The project's revision afterwards: unchanged when there was nothing to do, or on a
    /// dry run.
    pub revision: u64,
    pub ops: usize,
    /// Whether a revision was made.
    pub applied: bool,
    /// What the diff was against: `last-applied` (the plan last applied from this path, so
    /// the editor's changes since stay) or `none` (two-way: the project becomes the plan).
    pub base: &'static str,
    /// Places both the plan and someone in the editor changed since the base, left as the
    /// editor has them.
    pub kept: Vec<String>,
    /// Bin entries the patch removes because the plan lacks them (`keep` leaves them).
    pub removed: Vec<String>,
    /// Markers a two-way diff removes because the plan lacks them.
    pub lost_markers: usize,
    pub warnings: Vec<Issue>,
    /// The patch, on a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<serde_json::Value>,
}

/// Where the plan last applied from `rel` (a path relative to the project) is kept: under
/// `.kuma/plans/`, by project file then by the plan's path, never pruned — it is what tells
/// the script's changes from a person's, and nothing makes it again.
pub fn base_file(state: &AppState, rel: &str) -> PathBuf {
    let name = rel.replace(['/', '\\'], "__");
    state.config.cache_dir().join("plans").join(state.config.project_file.file_name().unwrap_or_default()).join(name)
}

/// Apply the plan `req` names. A refused edit comes back as the [`kuma_core::EditError`]
/// inside the error (stale, invalid), for a caller to downcast; anything before it (no such
/// file, not a project document) as itself.
pub fn apply(state: &AppState, req: &PlanRequest) -> anyhow::Result<Applied> {
    use anyhow::Context;
    let file = jobs::resolve_in_project(state, &req.path)?;
    let rel = jobs::project_relative(state, &file);
    let wanted = std::fs::read_to_string(&file).with_context(|| format!("cannot read `{}`", req.path))?;
    let base_path = base_file(state, &rel);
    let base = if req.exact { None } else { read_base(&base_path)? };
    let base_name = if base.is_some() { "last-applied" } else { "none" };
    let diff = |p: &kuma_core::Project| kuma_core::diff(p, base.as_deref(), &wanted, req.keep).with_context(|| format!("`{}` is not a project document", req.path));

    // Looked at first without the edit lock: a dry run, and a plan with nothing to do, make
    // no revision. One that changes something is applied under the lock, diffed again only
    // if an edit landed in between, so that edit is merged, never overwritten.
    let (first, revision) = {
        let live = crate::state::read(&state.project);
        (diff(&live)?, live.revision)
    };
    let mut out = applied(&first, revision, false, base_name);
    if req.dry_run {
        out.patch = Some(serde_json::to_value(&first.patch).context("serialising the patch")?);
        return Ok(out);
    }
    let next_base = if first.patch.0.is_empty() {
        first.next_base
    } else {
        let origin = req.origin.clone().unwrap_or_else(|| format!("other: apply {rel}"));
        let mut first = Some(first);
        let (d, rev, warnings) = state.apply_built_answering(&origin, None, &req.answers, |p| {
            let d = match first.take() {
                Some(d) if p.revision == revision => d,
                _ => diff(p)?,
            };
            Ok((d.patch.0.clone(), d))
        })?;
        out = Applied { warnings, ..applied(&d, rev, true, base_name) };
        d.next_base
    };
    // Only once it is the project (or already was): a plan refused leaves the base as it was.
    // The base is the plan except where `kept` held the editor's value, which keeps the old
    // base's, so that conflict is reported again rather than read as agreement.
    let next_base = serde_json::to_string(&next_base).context("serialising the base")?;
    if base.as_deref() != Some(next_base.as_str()) {
        jobs::write_atomic(&base_path, next_base.as_bytes())?;
    }
    Ok(out)
}

fn applied(d: &kuma_core::edit::Diff, revision: u64, applied: bool, base: &'static str) -> Applied {
    Applied { revision, ops: d.patch.0.len(), applied, base, kept: d.kept.clone(), removed: d.removed.clone(), lost_markers: d.lost_markers, warnings: Vec::new(), patch: None }
}

fn read_base(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("cannot read the last applied plan {}: {e}", path.display())),
    }
}
