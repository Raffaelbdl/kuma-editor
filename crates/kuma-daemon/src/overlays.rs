//! Overlays and blueprints: the two things an HTML overlay can be, and the moves between
//! them.
//!
//! An **overlay** is a folder the project owns (`overlays/<name>/`: `overlay.json` and
//! `index.html`) that a bin entry points at. A **blueprint** is the same kind of folder in
//! the user's library, reusable across projects and never read through an entry. So:
//!
//! * a blueprint reaches a project by being **copied** into it ([`copy_blueprint`]) — later
//!   edits to the blueprint never re-style a finished video;
//! * an overlay folder reaches the bin by being **imported** ([`import_overlay`]), the way a
//!   media file is added — so an agent writes the folder, checks it, and only then makes it
//!   an entry;
//! * entries may share a folder (two wordings of one design); [`make_unique`] gives one its
//!   own copy so the two can diverge;
//! * an overlay worth reusing goes back to the library with [`save_blueprint`].
//!
//! Every copy here is a filesystem write outside the document, so undoing the edit that
//! followed it leaves the folder behind. That is deliberate: a folder nothing points at
//! costs nothing, and deleting one could lose somebody's HTML.

use std::path::{Path, PathBuf};

use kuma_core::manifest::{check_overlay_path, OverlayManifest, MANIFEST};
use kuma_core::{BinItem, BinSource, Project};
use kuma_overlay::copy_dir;
use serde_json::{Map, Value};

use crate::jobs::{bin_op, bin_replace_op};
use crate::state::{read, AppState};

/// An overlay folder, resolved: where it is on disk and what its manifest says.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// As the entry writes it: relative to the project, `/`-separated.
    pub rel: String,
    pub dir: PathBuf,
    pub manifest: OverlayManifest,
}

impl Resolved {
    /// The folder's own name: what a copy, an entry id and a fallback title are named after.
    fn folder_name(&self) -> String {
        self.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "overlay".into())
    }
}

/// Where an entry's `overlay` folder is, without reading it: enough to serve a file from.
pub fn folder(state: &AppState, rel: &str) -> anyhow::Result<PathBuf> {
    if let Some(problem) = check_overlay_path(rel) {
        anyhow::bail!(problem);
    }
    Ok(state.config.project_root().join(rel))
}

/// Resolve an entry's `overlay` folder against the project and load its manifest.
pub fn resolve(state: &AppState, rel: &str) -> anyhow::Result<Resolved> {
    let dir = folder(state, rel)?;
    let manifest = OverlayManifest::load(&dir).map_err(|e| anyhow::anyhow!("overlay `{rel}`: {}", e.reason))?;
    Ok(Resolved { rel: rel.to_string(), dir, manifest })
}

/// The overlay folder a caller named, as the project-relative path an entry stores. Takes
/// the folder or its `overlay.json`, relative to the project or absolute inside it — what
/// an agent that just wrote the file is likely to have in hand.
pub fn folder_in_project(state: &AppState, given: &str) -> anyhow::Result<String> {
    let mut p = PathBuf::from(given);
    if p.file_name().is_some_and(|f| f == MANIFEST) {
        p.pop();
    }
    let abs = crate::jobs::resolve_path(state, &p);
    let (abs, root) = (abs.canonicalize().map_err(|e| anyhow::anyhow!("{}: {e}", abs.display()))?, state.config.project_root().canonicalize()?);
    let rel = abs.strip_prefix(&root).map_err(|_| {
        anyhow::anyhow!("{} is outside the project: an overlay belongs to its project — write it under {}", abs.display(), root.join("overlays").display())
    })?;
    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    anyhow::ensure!(!rel.is_empty(), "the project directory itself is not an overlay folder");
    Ok(rel)
}

/// An overlay entry's folder and vars, or why `bin` is not one.
fn overlay_entry(p: &Project, bin: &str) -> anyhow::Result<(String, Map<String, Value>)> {
    match p.bin.get(bin).map(|i| &i.source) {
        None => anyhow::bail!("no bin entry `{bin}`"),
        Some(BinSource::Overlay { overlay, vars }) => Ok((overlay.clone(), vars.clone())),
        Some(_) => anyhow::bail!("`{bin}` is not an overlay entry"),
    }
}

/// The first of `<base>`, `<base>-2`, `<base>-3`… that is not already a folder in `dir`.
fn free_folder(dir: &Path, base: &str) -> String {
    std::iter::once(base.to_string()).chain((2..).map(|n| format!("{base}-{n}"))).find(|n| !dir.join(n).exists()).unwrap_or_default()
}

/// Copy a blueprint into the project's `overlays/` and return the folder an entry should
/// name. The folder is `name` (the blueprint's id when absent), suffixed when taken: a copy
/// never lands on a folder that is already there, which may be somebody's edited overlay.
pub fn copy_blueprint(state: &AppState, blueprint: &str, name: Option<&str>) -> anyhow::Result<String> {
    let src = state.blueprint_dir(blueprint)?;
    let base = name.unwrap_or(blueprint);
    anyhow::ensure!(kuma_core::is_valid_id(base), "`{base}` must match [A-Za-z_][A-Za-z0-9_-]*: it is a folder name");
    let overlays = state.config.overlays_dir();
    let folder = free_folder(&overlays, base);
    copy_dir(&src, &overlays.join(&folder))?;
    Ok(format!("overlays/{folder}"))
}

/// A bin id for an overlay folder: `id` when given (refused if taken — JSON Patch `add`
/// would silently replace the entry and rebind every clip), else one minted from the
/// folder name.
fn bin_id(p: &Project, resolved: &Resolved, id: Option<String>) -> anyhow::Result<String> {
    match id {
        Some(id) => {
            anyhow::ensure!(!p.bin.contains_key(&id), "bin id `{id}` already exists (pass a different id)");
            Ok(id)
        }
        None => {
            let base: String = resolved.folder_name().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            let base = if kuma_core::is_valid_id(&base) { base } else { format!("o_{base}") };
            Ok(p.free_bin_id(&base))
        }
    }
}

/// Add an overlay folder of this project to the bin: the one way an overlay becomes an
/// entry. Its manifest is loaded first, so a folder that would not validate is refused
/// with the manifest's own complaint rather than a patch error.
pub fn import_overlay(
    state: &AppState,
    path: &str,
    id: Option<String>,
    name: Option<String>,
    vars: Map<String, Value>,
    base_revision: Option<u64>,
    origin: &str,
) -> anyhow::Result<(String, u64, Vec<kuma_core::Issue>)> {
    let resolved = resolve(state, &folder_in_project(state, path)?)?;
    let problems = resolved.manifest.check_vars(&vars);
    anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
    let title = if resolved.manifest.name.is_empty() { resolved.folder_name() } else { resolved.manifest.name.clone() };
    let item = BinItem {
        name: name.filter(|n| !n.is_empty()).unwrap_or(title),
        source: BinSource::Overlay { overlay: resolved.rel.clone(), vars },
        meta: None,
        parent: None,
        order: None,
        volume: 1.0,
    };
    let (id, rev, warnings) = state.apply_built(origin, base_revision, |p| {
        let id = bin_id(p, &resolved, id)?;
        Ok((vec![bin_op(&id, "", serde_json::to_value(&item)?)?], id))
    })?;
    Ok((id, rev, warnings))
}

/// Copy a blueprint into the project and import the copy: the agent's and the CLI's one
/// call. Returns the folder with what `import_overlay` returns.
pub fn add_blueprint(
    state: &AppState,
    blueprint: &str,
    id: Option<String>,
    name: Option<String>,
    vars: Map<String, Value>,
    origin: &str,
) -> anyhow::Result<(String, String, u64, Vec<kuma_core::Issue>)> {
    let overlay = copy_blueprint(state, blueprint, None)?;
    let (id, rev, warnings) = import_overlay(state, &overlay, id, name, vars, None, origin)?;
    Ok((overlay, id, rev, warnings))
}

/// Give an entry a folder of its own: copy the one it shares and point it at the copy.
/// The other entries on the old folder keep it, unchanged.
pub fn make_unique(state: &AppState, bin: &str, origin: &str) -> anyhow::Result<(String, u64)> {
    let (rel, _) = overlay_entry(&read(&state.project), bin)?;
    let src = resolve(state, &rel)?;
    // Beside the original, named after the entry: that is what now owns it.
    let parent = src.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.config.overlays_dir());
    let folder = free_folder(&parent, bin);
    copy_dir(&src.dir, &parent.join(&folder))?;
    let new_rel = match rel.rsplit_once('/') {
        Some((head, _)) => format!("{head}/{folder}"),
        None => folder,
    };
    let (_, rev, _) = state.apply_built(origin, None, |p| {
        // Re-checked under the lock: the entry may have been repointed while we copied.
        anyhow::ensure!(overlay_entry(p, bin)?.0 == rel, "`{bin}` changed while its folder was being copied; try again");
        Ok((vec![bin_replace_op(bin, "/source/overlay", Value::String(new_rel.clone()))?], ()))
    })?;
    Ok((new_rel, rev))
}

/// What "Save as blueprint" saves: a folder as it is, or a bin entry — its folder, with
/// the entry's vars written in as the blueprint's defaults, so the blueprint looks like
/// the thing that was saved rather than like the folder before anyone configured it.
pub enum SaveFrom<'a> {
    Folder(&'a str),
    Entry(&'a str),
}

/// Copy an overlay of this project into the user library as a blueprint. Refuses to
/// overwrite one unless `force`: the library is shared by every project.
pub fn save_blueprint(state: &AppState, from: SaveFrom, id: &str, force: bool) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(kuma_core::is_valid_id(id), "`{id}` must match [A-Za-z_][A-Za-z0-9_-]*: it is the blueprint's folder name");
    let (rel, vars) = match from {
        SaveFrom::Folder(path) => (folder_in_project(state, path)?, Map::new()),
        SaveFrom::Entry(bin) => overlay_entry(&read(&state.project), bin)?,
    };
    let src = resolve(state, &rel)?;
    let dst = state.config.user_blueprint_dir().join(id);
    if dst.exists() {
        anyhow::ensure!(force, "a blueprint `{id}` already exists at {}; pick another name or overwrite it", dst.display());
        std::fs::remove_dir_all(&dst)?;
    }
    copy_dir(&src.dir, &dst)?;
    if !vars.is_empty() {
        // Edited as JSON rather than through the struct, so nothing the manifest carries
        // is lost to a round trip; only the defaults of vars the entry set change.
        let path = dst.join(MANIFEST);
        let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        for (k, v) in vars {
            if let Some(spec) = manifest.pointer_mut(&format!("/vars/{k}")).and_then(Value::as_object_mut) {
                spec.insert("default".into(), v);
            }
        }
        std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&manifest)?))?;
    }
    state.reload_blueprints();
    Ok(dst)
}

/// Lint an overlay folder at the project's frame and rate, so a rule that depends on either
/// judges the size the video is actually in. Blocking: it stages a copy and runs a
/// subprocess, so callers on the runtime go through `spawn_blocking`.
pub fn lint(state: &AppState, dir: &Path) -> anyhow::Result<kuma_overlay::LintReport> {
    let (fps, width, height) = {
        let p = read(&state.project);
        (p.fps, p.width, p.height)
    };
    let manifest = OverlayManifest::load(dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let req = kuma_overlay::BakeRequest { overlay_dir: dir, manifest: &manifest, vars: &Map::new(), fps, width, height, duration: manifest.duration_default, speed: 1.0 };
    Ok(state.baker.lint(&req)?)
}

/// Lint a project overlay, by the path an entry would name.
pub fn lint_overlay(state: &AppState, path: &str) -> anyhow::Result<kuma_overlay::LintReport> {
    let rel = folder_in_project(state, path)?;
    lint(state, &state.config.project_root().join(rel))
}

/// Lint an installed blueprint, by id.
pub fn lint_blueprint(state: &AppState, id: &str) -> anyhow::Result<kuma_overlay::LintReport> {
    lint(state, &state.blueprint_dir(id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_never_lands_on_an_existing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(free_folder(tmp.path(), "title"), "title");
        std::fs::create_dir_all(tmp.path().join("title")).unwrap();
        std::fs::create_dir_all(tmp.path().join("title-2")).unwrap();
        assert_eq!(free_folder(tmp.path(), "title"), "title-3");
    }

    #[test]
    fn a_copied_folder_leaves_its_generated_previews_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("fonts")).unwrap();
        for f in ["overlay.json", "index.html", "thumb.png", "preview.webp", "fonts/a.woff2", ".DS_Store"] {
            std::fs::write(src.join(f), b"x").unwrap();
        }
        let dst = tmp.path().join("dst");
        copy_dir(&src, &dst).unwrap();
        assert!(dst.join("overlay.json").is_file() && dst.join("index.html").is_file() && dst.join("fonts/a.woff2").is_file());
        assert!(!dst.join("thumb.png").exists() && !dst.join("preview.webp").exists() && !dst.join(".DS_Store").exists());
    }
}
