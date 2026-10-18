//! What a library holds, and the design systems the libraries offer: what an agent needs
//! to use a library it was told about — add it to the project, read what it says about
//! itself, take its design system, then place its blueprints and presets.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::state::{read, AppState};

/// Files at a library's top level written for whoever uses it, in the order to read them.
const DOCS: [&str; 4] = ["README.md", "LIBRARY.md", "CLAUDE.md", "AGENTS.md"];

/// What the library at `dir` gives a project: its blueprints (those another library does not
/// shadow), presets, design systems and font families, and the documents it carries about
/// itself, by path.
pub fn contents(state: &AppState, dir: &Path) -> Value {
    let blueprints: Vec<String> = read(&state.blueprints).loaded.iter().filter(|(_, (d, _))| crate::state::library_of(d) == dir).map(|(id, _)| id.clone()).collect();
    let presets: Vec<String> = preset_files(&dir.join("presets")).into_iter().map(|(id, _)| id).collect();
    let design_systems: Vec<String> = state.config.installed_design_systems().into_iter().filter(|(_, (d, _))| d.parent() == Some(&dir.join("design-systems"))).map(|(id, _)| id).collect();
    let fonts_dir = dir.join("fonts");
    let fonts: Vec<String> = state.baker.font_library().families().into_values().filter(|f| f.faces.iter().any(|x| x.path.starts_with(&fonts_dir))).map(|f| f.name).collect();
    let docs: Vec<PathBuf> = DOCS.iter().map(|d| dir.join(d)).filter(|p| p.is_file()).collect();
    json!({ "blueprints": blueprints, "presets": presets, "design_systems": design_systems, "fonts": fonts, "docs": docs })
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
