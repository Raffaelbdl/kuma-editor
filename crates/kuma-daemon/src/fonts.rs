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
/// file of that name is kept and this one is suffixed.
pub fn add_font(state: &AppState, path: &Path, shared: bool) -> anyhow::Result<Value> {
    let src = crate::jobs::resolve_path(state, path);
    let file = src.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    anyhow::ensure!(
        kuma_overlay::fonts::EXTENSIONS.contains(&ext.as_str()),
        "`{file}` is not a .ttf or .otf file (a .woff/.woff2 needs converting first)"
    );
    let bytes = std::fs::read(&src).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", src.display()))?;
    let (family, _, _) = kuma_overlay::fonts::parse_face(&bytes).ok_or_else(|| anyhow::anyhow!("`{file}` is not a font kuma can read"))?;
    let root = if shared { state.config.user_font_dir() } else { state.config.project_font_dir() };
    // The folder is named after the family for a person browsing it; kuma reads the name
    // from the file, so nothing depends on it.
    let folder: String = family.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '-' }).collect();
    let dir = root.join(folder.trim());
    std::fs::create_dir_all(&dir)?;
    let (stem, _) = file.rsplit_once('.').unwrap_or((&file, ""));
    let mut target = dir.join(&file);
    let mut n = 2;
    while target.exists() {
        if std::fs::read(&target)? == bytes {
            break;
        }
        target = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    if !target.exists() {
        std::fs::write(&target, &bytes)?;
    }
    let origin = if shared { kuma_overlay::Origin::Library } else { kuma_overlay::Origin::Project };
    Ok(json!({ "family": family, "origin": origin, "path": target }))
}
