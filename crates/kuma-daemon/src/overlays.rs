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
use serde_json::{Map, Value};

use crate::state::{read, AppState};

/// An overlay folder, resolved: where it is on disk and what its manifest says.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// As the entry writes it: relative to the project, `/`-separated.
    pub rel: String,
    pub dir: PathBuf,
    pub manifest: OverlayManifest,
}

/// Resolve an entry's `overlay` folder against the project.
pub fn resolve(state: &AppState, rel: &str) -> anyhow::Result<Resolved> {
    if let Some(problem) = check_overlay_path(rel) {
        anyhow::bail!(problem);
    }
    let dir = state.config.project_root().join(rel);
    let manifest = OverlayManifest::load(&dir).map_err(|e| anyhow::anyhow!("overlay `{rel}`: {}", e.reason))?;
    Ok(Resolved { rel: rel.to_string(), dir, manifest })
}

/// The overlay folder a caller named, as the project-relative path an entry stores. Takes
/// the folder or its `overlay.json`, relative to the project or absolute inside it — what
/// an agent that just wrote the file is likely to have in hand.
pub fn project_relative(state: &AppState, given: &str) -> anyhow::Result<String> {
    let root = state.config.project_root();
    let mut p = PathBuf::from(given);
    if p.file_name().is_some_and(|f| f == MANIFEST) {
        p.pop();
    }
    let abs = if p.is_absolute() { p.clone() } else { root.join(&p) };
    let (abs, root) = (abs.canonicalize().map_err(|e| anyhow::anyhow!("{}: {e}", abs.display()))?, root.canonicalize()?);
    let rel = abs.strip_prefix(&root).map_err(|_| {
        anyhow::anyhow!("{} is outside the project: an overlay belongs to its project — write it under {}", abs.display(), root.join("overlays").display())
    })?;
    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    anyhow::ensure!(!rel.is_empty(), "the project directory itself is not an overlay folder");
    Ok(rel)
}

/// Copy an overlay folder, leaving out what is generated from it (thumbnails, previews):
/// they describe the source's configuration and would be stale in the copy.
pub fn copy_folder(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        if kuma_overlay::GENERATED.iter().any(|g| e.file_name() == *g) {
            continue;
        }
        let target = dst.join(e.file_name());
        if e.path().is_dir() {
            crate::copy_tree(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// The first of `<base>`, `<base>-2`, `<base>-3`… that is not already a folder in `dir`.
fn free_folder(dir: &Path, base: &str) -> String {
    std::iter::once(base.to_string()).chain((2..).map(|n| format!("{base}-{n}"))).find(|n| !dir.join(n).exists()).unwrap_or_default()
}

/// Copy a blueprint into the project's `overlays/` and return the folder an entry should
/// name. The folder is `name` (the blueprint's id when absent), suffixed when taken: a copy
/// never lands on a folder that is already there, which may be somebody's edited overlay.
pub fn copy_blueprint(state: &AppState, blueprint: &str, name: Option<&str>) -> anyhow::Result<String> {
    let src = read(&state.blueprints).loaded.get(blueprint).map(|(dir, _)| dir.clone());
    let src = src.ok_or_else(|| anyhow::anyhow!("no blueprint `{blueprint}` in the library; `kuma library` installs the starter set"))?;
    let base = name.unwrap_or(blueprint);
    anyhow::ensure!(kuma_core::is_valid_id(base), "`{base}` must match [A-Za-z_][A-Za-z0-9_-]*: it is a folder name");
    let overlays = state.config.overlays_dir();
    let folder = free_folder(&overlays, base);
    copy_folder(&src, &overlays.join(&folder))?;
    Ok(format!("overlays/{folder}"))
}

/// The bin entry for an overlay folder, as [`import_overlay`] would add it.
pub fn entry(resolved: &Resolved, name: Option<String>, vars: Map<String, Value>, parent: Option<String>) -> BinItem {
    let folder = resolved.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    BinItem {
        name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| if resolved.manifest.name.is_empty() { folder } else { resolved.manifest.name.clone() }),
        source: BinSource::Overlay { overlay: resolved.rel.clone(), vars },
        meta: None,
        parent,
        order: None,
        volume: 1.0,
    }
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
            let folder = resolved.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "overlay".into());
            let base: String = folder.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
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
    let rel = project_relative(state, path)?;
    let resolved = resolve(state, &rel)?;
    let problems = resolved.manifest.check_vars(&vars);
    anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
    let item = entry(&resolved, name, vars, None);
    let (id, rev, warnings) = state.apply_built(origin, |p| {
        if let Some(base) = base_revision {
            anyhow::ensure!(p.revision == base, "stale revision: the project is at {}, not {base}", p.revision);
        }
        let id = bin_id(p, &resolved, id)?;
        Ok((vec![add_op(&id, serde_json::to_value(&item)?)?], id))
    })?;
    Ok((id, rev, warnings))
}

/// Give an entry a folder of its own: copy the one it shares and point it at the copy.
/// The other entries on the old folder keep it, unchanged.
pub fn make_unique(state: &AppState, bin: &str, origin: &str) -> anyhow::Result<(String, u64)> {
    let rel = {
        let p = read(&state.project);
        let item = p.bin.get(bin).ok_or_else(|| anyhow::anyhow!("no bin entry `{bin}`"))?;
        let BinSource::Overlay { overlay, .. } = &item.source else { anyhow::bail!("`{bin}` is not an overlay entry") };
        overlay.clone()
    };
    let src = resolve(state, &rel)?;
    // Beside the original, named after the entry: that is what now owns it.
    let parent = src.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.config.overlays_dir());
    let folder = free_folder(&parent, bin);
    copy_folder(&src.dir, &parent.join(&folder))?;
    let new_rel = match rel.rsplit_once('/') {
        Some((head, _)) => format!("{head}/{folder}"),
        None => folder,
    };
    let (_, rev, _) = state.apply_built(origin, |p| {
        // Re-checked under the lock: the entry may have been repointed while we copied.
        let still = matches!(p.bin.get(bin).map(|i| &i.source), Some(BinSource::Overlay { overlay, .. }) if *overlay == rel);
        anyhow::ensure!(still, "`{bin}` changed while its folder was being copied; try again");
        Ok((vec![replace_op(&format!("/bin/{bin}/source/overlay"), Value::String(new_rel.clone()))?], ()))
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
        SaveFrom::Folder(path) => (project_relative(state, path)?, Map::new()),
        SaveFrom::Entry(bin) => {
            let p = read(&state.project);
            let item = p.bin.get(bin).ok_or_else(|| anyhow::anyhow!("no bin entry `{bin}`"))?;
            let BinSource::Overlay { overlay, vars } = &item.source else { anyhow::bail!("`{bin}` is not an overlay entry") };
            (overlay.clone(), vars.clone())
        }
    };
    let src = resolve(state, &rel)?;
    let dst = state.config.user_blueprint_dir().join(id);
    if dst.exists() {
        anyhow::ensure!(force, "a blueprint `{id}` already exists at {}; pick another name or overwrite it", dst.display());
        std::fs::remove_dir_all(&dst)?;
    }
    copy_folder(&src.dir, &dst)?;
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
    let rel = project_relative(state, path)?;
    lint(state, &state.config.project_root().join(rel))
}

/// Lint an installed blueprint, by id.
pub fn lint_blueprint(state: &AppState, id: &str) -> anyhow::Result<kuma_overlay::LintReport> {
    let dir = read(&state.blueprints).loaded.get(id).map(|(d, _)| d.clone()).ok_or_else(|| anyhow::anyhow!("no blueprint `{id}`"))?;
    lint(state, &dir)
}

fn add_op(id: &str, value: Value) -> anyhow::Result<json_patch::PatchOperation> {
    Ok(serde_json::from_value(serde_json::json!({ "op": "add", "path": format!("/bin/{id}"), "value": value }))?)
}

fn replace_op(path: &str, value: Value) -> anyhow::Result<json_patch::PatchOperation> {
    Ok(serde_json::from_value(serde_json::json!({ "op": "replace", "path": path, "value": value }))?)
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
        for f in ["overlay.json", "index.html", "thumb.png", "preview.webp", "fonts/a.woff2"] {
            std::fs::write(src.join(f), b"x").unwrap();
        }
        let dst = tmp.path().join("dst");
        copy_folder(&src, &dst).unwrap();
        assert!(dst.join("overlay.json").is_file() && dst.join("index.html").is_file() && dst.join("fonts/a.woff2").is_file());
        assert!(!dst.join("thumb.png").exists() && !dst.join("preview.webp").exists());
    }
}
