//! Adding fonts: a file goes into the project's `fonts/` or, shared, the user library's.
//!
//! Like an overlay folder, a font is a file beside the project rather than part of the
//! document, so adding one is not an edit and is never undone. What `font` vars resolve
//! against is `Config::font_dirs`; this only puts a file where that looks.
//!
//! By path, as media and overlays are added: the daemon reads the file where it is. A
//! daemon on another machine than the editor would need the bytes sent instead — see
//! *Fonts are added by path* in `docs/decisions.md`.

use std::path::Path;

use serde_json::{json, Value};

use crate::state::AppState;

/// Copy the font at `path` (relative to the project, or absolute) under
/// `<fonts>/<Family>/`. The same bytes already there are not written twice; a different
/// file of that name is kept and this one is suffixed. A collection (`.ttc`/`.otc`) is
/// split into one file per face, each under its own family, since `@font-face` takes one
/// face per file. A web font (`.woff`/`.woff2`) is unwrapped into the font it holds and
/// written as that (`.ttf`/`.otf`), so a font folder holds one kind of file whatever was
/// added. Answers with the first face's family and path, and every face.
pub fn add_font(state: &AppState, path: &Path, shared: bool) -> anyhow::Result<Value> {
    use kuma_overlay::fonts::{parse_face, sfnt_ext, split_collection, unwrap_web_font, COLLECTIONS, EXTENSIONS, WEB_FONTS};
    let src = crate::jobs::resolve_path(state, path);
    let mut file = src.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    anyhow::ensure!(
        [COLLECTIONS, EXTENSIONS, WEB_FONTS].iter().any(|exts| exts.contains(&ext.as_str())),
        "`{file}` is not a .ttf, .otf, .woff, .woff2, .ttc or .otc file"
    );
    let mut bytes = std::fs::read(&src).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", src.display()))?;
    // Whether it is a web font is the file's to say too. What it wraps is what is kept: the
    // same bytes the font was made from, under the extension they call for.
    let unwrapped = match unwrap_web_font(&bytes) {
        Some(sfnt) => {
            bytes = sfnt.map_err(|e| anyhow::anyhow!("`{file}` does not decode: {e}"))?;
            true
        }
        None => false,
    };
    // A font named `.woff2` that is not one is kept too, under a name a font folder reads.
    if unwrapped || WEB_FONTS.contains(&ext.as_str()) {
        let stem = file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s).to_string();
        file = format!("{stem}.{}", sfnt_ext(&bytes));
    }
    let (stem, _) = file.rsplit_once('.').unwrap_or((&file, ""));
    // Whether it is a collection is the file's to say, not its extension's.
    let faces: Vec<(Vec<u8>, String)> = match split_collection(&bytes) {
        Some(faces) => faces.into_iter().enumerate().map(|(i, (b, e))| (b, format!("{stem}-{i}.{e}"))).collect(),
        None => vec![(bytes, file.clone())],
    };
    anyhow::ensure!(!faces.is_empty(), "`{file}` is a collection with no faces kuma can read");
    let root = if shared { state.config.user_font_dir() } else { state.config.project_font_dir() };
    let mut placed = Vec::new();
    for (bytes, name) in faces {
        let info = parse_face(&bytes).ok_or_else(|| anyhow::anyhow!("`{name}` is not a font kuma can read"))?;
        let target = place(&root, &info.family, &name, &bytes)?;
        placed.push((info, target));
    }
    // The family each face is found under, which is not always the one it reads as: a
    // legacy per-weight name joins a family the folder already has (`FontLibrary::families`).
    let families = state.baker.font_library().families();
    let mut added = Vec::new();
    for (info, target) in placed {
        let family = families.values().find(|f| f.faces.iter().any(|x| x.path == target)).map_or(info.family, |f| f.name.clone());
        added.push(json!({ "family": family, "path": target, "scripts": info.scripts }));
    }
    let origin = if shared { kuma_overlay::Origin::Library } else { kuma_overlay::Origin::Project };
    Ok(json!({ "family": added[0]["family"], "origin": origin, "path": added[0]["path"], "faces": added }))
}

/// What the overlay entries `ids` will not be drawn in (`Baker::font_findings`), as project
/// warnings pointing at the var: a family that is not installed, characters none of the
/// entry's fonts can set, CJK text landing in an element (or a page) in another language.
/// All of it is legal and renders in the baking machine's fallbacks, which is why a render
/// says so rather than falling back quietly, and `validate_project` checks the whole bin.
/// An entry whose folder does not load is validation's to report, not this.
pub fn check(state: &AppState, project: &kuma_core::Project, ids: &[&str]) -> Vec<kuma_core::Issue> {
    use kuma_core::{BinSource, Issue, Severity};
    let mut folders: std::collections::HashMap<&str, Option<(crate::overlays::Resolved, String, kuma_overlay::lint::BundledFaces)>> = Default::default();
    let mut out = Vec::new();
    for &id in ids {
        let Some(BinSource::Overlay { overlay, vars }) = project.bin.get(id).map(|i| &i.source) else { continue };
        let folder = folders.entry(overlay.as_str()).or_insert_with(|| {
            let r = crate::overlays::resolve(state, overlay).ok()?;
            let html = std::fs::read_to_string(r.dir.join("index.html")).ok()?;
            Some((r, html, Default::default()))
        });
        let Some((r, html, bundled)) = folder else { continue };
        for f in state.baker.font_findings_in(&r.manifest, &r.manifest.resolve_vars(vars), html, &r.dir, bundled) {
            let path = match &f.var {
                Some(var) => format!("/bin/{id}/source/vars/{var}"),
                None => format!("/bin/{id}"),
            };
            let fix = f.fix.map(|x| format!("; {x}")).unwrap_or_default();
            out.push(Issue { severity: Severity::Warning, path, message: format!("{}{fix}", f.message) });
        }
    }
    out
}

/// Write `bytes` as `<root>/<family>/<name>`, or find them already there.
fn place(root: &Path, family: &str, name: &str, bytes: &[u8]) -> anyhow::Result<std::path::PathBuf> {
    // The folder is named after the family for a person browsing it; kuma reads the name
    // from the file, so nothing depends on it.
    let folder: String = family.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '-' }).collect();
    let dir = root.join(folder.trim());
    std::fs::create_dir_all(&dir)?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let mut target = dir.join(name);
    let mut n = 2;
    while target.exists() {
        if std::fs::read(&target)? == bytes {
            return Ok(target);
        }
        target = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    std::fs::write(&target, bytes)?;
    Ok(target)
}
