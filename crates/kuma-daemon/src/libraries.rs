//! What a library holds, and the design systems the libraries offer: what an agent needs
//! to use a library it was told about — add it to the project, read what it says about
//! itself, take its design system, then place its blueprints and presets.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::state::{read, AppState};

/// Files at a library's top level written for whoever uses it, in the order to read them.
const DOCS: [&str; 4] = ["README.md", "LIBRARY.md", "CLAUDE.md", "AGENTS.md"];

/// A folder's one spelling: canonical when it is there, else with `..` taken out, so a
/// library on a drive not mounted is still the folder it names.
pub fn spelling(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.components().fold(PathBuf::new(), |mut out, c| {
        match c {
            std::path::Component::ParentDir => drop(out.pop()),
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
        out
    }))
}

/// `dir` as a project names it: relative to `root`, the project's folder, wherever it is
/// (`../house-style`), so a folder that moves together with its library keeps finding it;
/// absolute only when they share nothing (another drive). Both are absolute.
pub fn relative_to(dir: &Path, root: &Path) -> PathBuf {
    let (a, b): (Vec<_>, Vec<_>) = (dir.components().collect(), root.components().collect());
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common <= 1 {
        return dir.to_path_buf();
    }
    let rel: PathBuf = std::iter::repeat_n(std::path::Component::ParentDir, b.len() - common).chain(a[common..].iter().copied()).collect();
    if rel.as_os_str().is_empty() { PathBuf::from(".") } else { rel }
}

/// The project's libraries made `next`, as one edit. A whole-list `add`, which replaces the
/// member when there is one: no special case for a project that names none yet.
fn set_libraries(state: &AppState, origin: &str, next: impl FnOnce(&kuma_core::Project) -> anyhow::Result<Vec<String>>) -> anyhow::Result<u64> {
    let (_, rev, _) = state.apply_built(origin, None, |p| {
        let ops = serde_json::from_value(json!([{ "op": "add", "path": "/libraries", "value": next(p)? }]))?;
        Ok((ops, ()))
    })?;
    Ok(rev)
}

/// Name the library at `path` in this project, as one edit: nothing is copied, and no other
/// project sees it. Its `blueprints/` folder names it too. Answers with its folder and the
/// revision, none when the project already named it.
pub fn add(state: &AppState, path: &Path, origin: &str) -> anyhow::Result<(PathBuf, Option<u64>)> {
    let mut dir = path.canonicalize().map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?;
    if dir.file_name().is_some_and(|n| n == "blueprints") && !kuma_core::is_library(&dir) {
        dir.pop();
    }
    anyhow::ensure!(kuma_core::is_library(&dir), "{} is not a kuma library: it has none of {}/", dir.display(), kuma_core::LIBRARY_KINDS.join("/, "));
    anyhow::ensure!(spelling(&state.config.libraries.user) != dir, "{} is your own library, which every project reads", dir.display());
    let root = spelling(&state.config.project_root());
    if read(&state.project).library_dirs(&root).iter().any(|d| spelling(d) == dir) {
        return Ok((dir, None));
    }
    let name = relative_to(&dir, &root).to_string_lossy().into_owned();
    let rev = set_libraries(state, origin, |p| Ok(p.libraries.iter().cloned().chain([name]).collect()))?;
    Ok((dir, Some(rev)))
}

/// Stop naming a library in this project — by the name the project gives it, or a path to
/// the folder — as one edit. Its folder is left as it is, and so is every overlay the
/// project copied from it.
pub fn remove(state: &AppState, path: &str, origin: &str) -> anyhow::Result<u64> {
    let root = state.config.project_root();
    let wanted = spelling(&root.join(path));
    set_libraries(state, origin, |p| {
        let kept: Vec<String> = p.libraries.iter().filter(|l| *l != path && spelling(&root.join(l)) != wanted).cloned().collect();
        anyhow::ensure!(kept.len() < p.libraries.len(), "`{path}` is not one of this project's libraries: the user library, the starter set and KUMA_LIBRARY's are not a project's to remove");
        Ok(kept)
    })
}

/// What each library gives a project, by folder: its blueprints (those another library does
/// not shadow), presets, design systems and font families, and the documents it carries about
/// itself. What every library shares (design systems, font families) is read once.
pub fn contents(state: &AppState, dirs: &[PathBuf]) -> Vec<Value> {
    let loaded = read(&state.blueprints);
    let systems = state.config.installed_design_systems();
    let families = state.baker.font_library().families();
    dirs.iter()
        .map(|dir| {
            let blueprints: Vec<&String> = loaded.loaded.iter().filter(|(_, (d, _))| crate::state::library_of(d) == *dir).map(|(id, _)| id).collect();
            let presets: Vec<String> = preset_files(&dir.join("presets")).into_iter().map(|(id, _)| id).collect();
            let design_systems: Vec<&String> = systems.iter().filter(|(_, (d, _))| d.parent() == Some(&dir.join("design-systems"))).map(|(id, _)| id).collect();
            let fonts_dir = dir.join("fonts");
            let fonts: Vec<&String> = families.values().filter(|f| f.faces.iter().any(|x| x.path.starts_with(&fonts_dir))).map(|f| &f.name).collect();
            let docs: Vec<PathBuf> = DOCS.iter().map(|d| dir.join(d)).filter(|p| p.is_file()).collect();
            json!({ "blueprints": blueprints, "presets": presets, "design_systems": design_systems, "fonts": fonts, "docs": docs })
        })
        .collect()
}

/// The effect presets in `dir` (`*.json` with an `id`), by id.
pub fn preset_files(dir: &Path) -> Vec<(String, Value)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, Value)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()))
        .filter_map(|v| Some((v.get("id")?.as_str()?.to_string(), v)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Every design system the project's libraries offer — id, title, the library's folder, its
/// `DESIGN.md` — and which one the project carries, if any.
pub fn design_systems(state: &AppState) -> Value {
    let carried = state.config.project_design_system().and_then(|p| std::fs::read_to_string(p).ok());
    let installed: Vec<Value> = state
        .config
        .installed_design_systems()
        .into_iter()
        .map(|(id, (dir, title))| {
            let this = carried.is_some() && std::fs::read_to_string(dir.join("DESIGN.md")).ok() == carried;
            json!({ "id": id, "title": title, "library": dir.parent().and_then(Path::parent), "design": dir.join("DESIGN.md"), "this_project": this })
        })
        .collect();
    json!({ "design_systems": installed, "project": state.config.project_design_system() })
}

/// Copy design system `id` into the project's `design-system/`, the one place an agent looks
/// for it; refused over one already there unless `force`. Not an edit of the document: what
/// applying it changes is the overlays and vars an agent then edits, each an edit of its own.
pub fn apply_design_system(state: &AppState, id: &str, force: bool) -> anyhow::Result<Value> {
    let (src, title) = state.config.installed_design_systems().remove(id).ok_or_else(|| anyhow::anyhow!("no design system `{id}` in this project's libraries (`list_design_systems`, `kuma design-system`)"))?;
    let dst = state.config.project_root().join("design-system");
    if dst.exists() {
        anyhow::ensure!(force, "{} already exists: pass `force` to replace it", dst.display());
        std::fs::remove_dir_all(&dst)?;
    }
    crate::state::copy_tree(&src, &dst)?;
    Ok(json!({ "id": id, "title": title, "path": dst, "read": dst.join("DESIGN.md"), "tokens": dst.join("tokens.css") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project names a library from its own folder, so the two can move together.
    #[test]
    fn a_library_is_named_relative_to_the_project() {
        let rel = |dir: &str, root: &str| relative_to(Path::new(dir), Path::new(root));
        assert_eq!(rel("/media/T7/yt/overlays-mat", "/media/T7/yt/16bit-akiba"), Path::new("../overlays-mat"));
        assert_eq!(rel("/media/T7/yt/shorts/lib", "/media/T7/yt/16bit-akiba"), Path::new("../shorts/lib"));
        assert_eq!(rel("/media/T7/yt/16bit-akiba/lib", "/media/T7/yt/16bit-akiba"), Path::new("lib"));
        assert_eq!(rel("/opt/lib", "/home/me/video"), Path::new("/opt/lib"), "nothing shared but the root");
        assert_eq!(spelling(Path::new("/nowhere/a/../b")), Path::new("/nowhere/b"), "a folder not there keeps one spelling too");
    }
}
