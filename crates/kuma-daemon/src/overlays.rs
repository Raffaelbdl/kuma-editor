//! Overlays (folders a project owns, which bin entries name) and blueprints (the same
//! folders in the user's library). Every move between them is a copy, so editing a
//! blueprint never re-styles a finished video. Undo leaves copied folders on disk.

use std::path::{Path, PathBuf};

use kuma_core::manifest::{check_overlay_path, OverlayManifest, MANIFEST};
use kuma_core::{BinItem, BinSource, Project};
use kuma_overlay::copy_dir;
use serde_json::{Map, Value};

use crate::jobs::{bin_op, bin_replace_op};
use crate::state::{read, AppState};

/// An overlay folder with its manifest loaded.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub dir: PathBuf,
    pub manifest: OverlayManifest,
}

/// An entry's overlay folder on disk, without reading it.
pub fn folder(state: &AppState, rel: &str) -> anyhow::Result<PathBuf> {
    if let Some(problem) = check_overlay_path(rel) {
        anyhow::bail!(problem);
    }
    Ok(state.config.project_root().join(rel))
}

pub fn resolve(state: &AppState, rel: &str) -> anyhow::Result<Resolved> {
    let dir = folder(state, rel)?;
    let manifest = OverlayManifest::load(&dir).map_err(|e| anyhow::anyhow!("overlay `{rel}`: {}", e.reason))?;
    Ok(Resolved { dir, manifest })
}

/// A folder (or its `overlay.json`), relative or absolute: its canonical path, and the path
/// an entry would store when it is inside the project.
fn locate(state: &AppState, given: &str) -> anyhow::Result<(PathBuf, Option<String>)> {
    let mut p = PathBuf::from(given);
    if p.file_name().is_some_and(|f| f == MANIFEST) {
        p.pop();
    }
    let abs = crate::jobs::resolve_path(state, &p);
    let (abs, root) = (abs.canonicalize().map_err(|e| anyhow::anyhow!("{}: {e}", abs.display()))?, state.config.project_root().canonicalize()?);
    let rel = abs.strip_prefix(&root).ok().map(|r| r.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
    anyhow::ensure!(rel.as_deref() != Some(""), "the project directory itself is not an overlay folder");
    Ok((abs, rel))
}

/// A folder inside the project, as the path an entry stores; refused outside it.
pub fn folder_in_project(state: &AppState, given: &str) -> anyhow::Result<String> {
    let (abs, rel) = locate(state, given)?;
    rel.ok_or_else(|| anyhow::anyhow!("{} is outside the project", abs.display()))
}

fn overlay_entry(p: &Project, bin: &str) -> anyhow::Result<(String, Map<String, Value>)> {
    match p.bin.get(bin).map(|i| &i.source) {
        None => anyhow::bail!("no bin entry `{bin}`"),
        Some(BinSource::Overlay { overlay, vars }) => Ok((overlay.clone(), vars.clone())),
        Some(_) => anyhow::bail!("`{bin}` is not an overlay entry"),
    }
}

/// `base`, else `base-2`, `base-3`…: the first name not taken in `dir`.
fn free_folder(dir: &Path, base: &str) -> String {
    std::iter::once(base.to_string()).chain((2..).map(|n| format!("{base}-{n}"))).find(|n| !dir.join(n).exists()).unwrap_or_default()
}

/// Copy a blueprint into `overlays/<name>` (the blueprint id by default, suffixed when
/// taken) and return that folder.
pub fn copy_blueprint(state: &AppState, blueprint: &str, name: Option<&str>) -> anyhow::Result<String> {
    let src = state.blueprint_dir(blueprint)?;
    let base = name.unwrap_or(blueprint);
    anyhow::ensure!(kuma_core::is_valid_id(base), "`{base}` must match [A-Za-z_][A-Za-z0-9_-]*");
    let overlays = state.config.overlays_dir();
    let folder = free_folder(&overlays, base);
    copy_dir(&src, &overlays.join(&folder))?;
    Ok(format!("overlays/{folder}"))
}

/// How an imported overlay enters the bin; all optional.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Import {
    /// Bin id; minted from the folder name when absent.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name; the manifest's when absent.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vars: Map<String, Value>,
    /// The bin entry (a folder) to list it under.
    #[serde(default)]
    pub parent: Option<String>,
}

/// Add an overlay folder of this project to the bin. Returns the bin id.
pub fn import_overlay(state: &AppState, path: &str, how: Import, base_revision: Option<u64>, origin: &str) -> anyhow::Result<(String, u64, Vec<kuma_core::Issue>)> {
    let Import { id, name, vars, parent } = how;
    let rel = match locate(state, path)? {
        (_, Some(rel)) => rel,
        // A folder from elsewhere is copied into `overlays/` first, so the project owns it.
        (abs, None) => {
            OverlayManifest::load(&abs).map_err(|e| anyhow::anyhow!("{e}"))?;
            let base = abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "overlay".into());
            let folder = free_folder(&state.config.overlays_dir(), &base);
            copy_dir(&abs, &state.config.overlays_dir().join(&folder))?;
            format!("overlays/{folder}")
        }
    };
    let r = resolve(state, &rel)?;
    let problems = r.manifest.check_vars(&vars);
    anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
    let folder_name = r.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = if r.manifest.name.is_empty() { folder_name.clone() } else { r.manifest.name.clone() };
    let item = BinItem {
        name: name.filter(|n| !n.is_empty()).unwrap_or(title),
        source: BinSource::Overlay { overlay: rel, vars },
        meta: None,
        parent,
        order: None,
        volume: 1.0,
    };
    let (id, rev, warnings) = state.apply_built(origin, base_revision, |p| {
        let id = match id {
            Some(id) => {
                // `add` would silently replace an entry and rebind its clips.
                anyhow::ensure!(!p.bin.contains_key(&id), "bin id `{id}` already exists");
                id
            }
            None => {
                let base: String = folder_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
                p.free_bin_id(if kuma_core::is_valid_id(&base) { &base } else { "overlay" })
            }
        };
        Ok((vec![bin_op(&id, "", serde_json::to_value(&item)?)?], id))
    })?;
    Ok((id, rev, warnings))
}

/// Give an entry its own copy of the folder it shares, beside the original and named
/// after the entry. Returns the new folder.
pub fn make_unique(state: &AppState, bin: &str, origin: &str) -> anyhow::Result<(String, u64)> {
    let (rel, _) = overlay_entry(&read(&state.project), bin)?;
    let src = folder(state, &rel)?;
    let parent = src.parent().map(Path::to_path_buf).unwrap_or_else(|| state.config.overlays_dir());
    let name = free_folder(&parent, bin);
    copy_dir(&src, &parent.join(&name))?;
    let new_rel = rel.rsplit_once('/').map_or(name.clone(), |(head, _)| format!("{head}/{name}"));
    let (_, rev, _) = state.apply_built(origin, None, |p| {
        anyhow::ensure!(overlay_entry(p, bin)?.0 == rel, "`{bin}` changed while its folder was copied; try again");
        Ok((vec![bin_replace_op(bin, "/source/overlay", Value::String(new_rel.clone()))?], ()))
    })?;
    Ok((new_rel, rev))
}

/// Save an overlay entry into the user library as blueprint `id`: its folder, with the
/// entry's vars as the defaults. Refuses to replace a blueprint unless `force`.
pub fn save_blueprint(state: &AppState, bin: &str, id: &str, force: bool) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(kuma_core::is_valid_id(id), "`{id}` must match [A-Za-z_][A-Za-z0-9_-]*");
    let (rel, vars) = overlay_entry(&read(&state.project), bin)?;
    let src = folder(state, &rel)?;
    let dst = state.config.user_blueprint_dir().join(id);
    if dst.exists() {
        anyhow::ensure!(force, "blueprint `{id}` already exists at {}", dst.display());
        std::fs::remove_dir_all(&dst)?;
    }
    copy_dir(&src, &dst)?;
    if !vars.is_empty() {
        // Edited as JSON so nothing else in the manifest changes.
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

/// Lint an overlay folder at the project's size and rate. Blocking.
pub fn lint(state: &AppState, dir: &Path) -> anyhow::Result<kuma_overlay::LintReport> {
    let (fps, width, height) = {
        let p = read(&state.project);
        (p.fps, p.width, p.height)
    };
    let manifest = OverlayManifest::load(dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let req = kuma_overlay::BakeRequest { overlay_dir: dir, manifest: &manifest, vars: &Map::new(), fps, width, height, duration: manifest.duration_default, speed: 1.0 };
    Ok(state.baker.lint(&req)?)
}

pub fn lint_overlay(state: &AppState, path: &str) -> anyhow::Result<kuma_overlay::LintReport> {
    lint(state, &state.config.project_root().join(folder_in_project(state, path)?))
}

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
}
