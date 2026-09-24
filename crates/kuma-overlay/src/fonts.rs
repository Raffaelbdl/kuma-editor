//! Fonts an overlay can name: `font` vars resolve against folders of font files.
//!
//! A family is whatever the files say it is (the `name` table), not what the file or its
//! folder is called, so a font dropped in under any name is found under the name a person
//! would type. Weight and italic come from the files too — a variable font's `wght` axis
//! becomes a weight range — which is what lets kuma write the `@font-face` rules itself and
//! an overlay only say `font-family: var(--var-font)`.
//!
//! Folders are searched lowest precedence first; a family found in a later folder replaces
//! the earlier one whole, so a project's own copy of a family is the one that renders.
//! Each face has one name inside its family, `<i>.<ext>` (`Family::files`): the bake
//! stages the file under it and the preview serves it under it, so both read the same bytes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Extensions read as fonts. WOFF/WOFF2 are compressed and would need decoding to read
/// their names; a font that ships only as one is rare enough to convert.
pub const EXTENSIONS: &[&str] = &["ttf", "otf"];

/// Where a font folder is: what the picker groups families by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Library,
    Project,
}

/// One font file: which weights and slant it draws.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Face {
    #[serde(skip)]
    pub path: PathBuf,
    /// `[min, max]`; equal for a static font, the `wght` axis for a variable one.
    pub weight: [u16; 2],
    pub italic: bool,
    /// SHA-256 of the file, taken when it was read: what a bake key hashes, so a cached
    /// bake is found without reading the font again.
    #[serde(skip)]
    pub digest: String,
}

impl Face {
    fn ext(&self) -> String {
        self.path.extension().and_then(|e| e.to_str()).unwrap_or("ttf").to_ascii_lowercase()
    }
}

/// A family and the faces it has, from the one folder that supplies it.
#[derive(Debug, Clone, Serialize)]
pub struct Family {
    pub name: String,
    pub origin: Origin,
    pub faces: Vec<Face>,
}

impl Family {
    /// The family's folder in a font path: the name reduced to `[a-z0-9-]`.
    pub fn slug(&self) -> String {
        slug(&self.name)
    }

    /// Each face under the one name it is staged and served as, `<i>.<ext>`.
    pub fn files(&self) -> impl Iterator<Item = (String, &Face)> {
        self.faces.iter().enumerate().map(|(i, f)| (format!("{i}.{}", f.ext()), f))
    }

    /// `@font-face` rules for every face, with sources at `<prefix><slug>/<file>`.
    pub fn css(&self, prefix: &str) -> String {
        let slug = self.slug();
        let family = css_string(&self.name);
        self.files()
            .map(|(file, f)| {
                let weight = if f.weight[0] == f.weight[1] { f.weight[0].to_string() } else { format!("{} {}", f.weight[0], f.weight[1]) };
                let format = if f.ext() == "otf" { "opentype" } else { "truetype" };
                format!(
                    "@font-face {{ font-family: {family}; src: url(\"{prefix}{slug}/{file}\") format(\"{format}\"); font-weight: {weight}; font-style: {}; font-display: block; }}\n",
                    if f.italic { "italic" } else { "normal" }
                )
            })
            .collect()
    }

    /// The file behind a name `files` gave.
    pub fn file(&self, name: &str) -> Option<&Path> {
        self.files().find(|(n, _)| n == name).map(|(_, f)| f.path.as_path())
    }
}

/// What an overlay's `font` vars come to: the families found, the vars that are fonts, and
/// each `(var, family)` that names a family nobody installed.
#[derive(Debug, Default, Clone)]
pub struct Resolved {
    pub families: Vec<Family>,
    pub vars: Vec<String>,
    pub missing: Vec<(String, String)>,
}

impl Resolved {
    /// Every found family's `@font-face` rules, sources under `prefix`.
    pub fn css(&self, prefix: &str) -> String {
        self.families.iter().map(|f| f.css(prefix)).collect()
    }
}

/// `name` reduced to lowercase ASCII letters, digits and single dashes.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// A CSS string literal: quotes, and nothing that could close the declaration.
pub fn css_string(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !matches!(c, '"' | '\\' | ';' | '{' | '}' | '<' | '>' | '\n' | '\r')).collect();
    format!("\"{clean}\"")
}

/// What a font file says about itself, or `None` when it is not one kuma can read.
pub fn read_face(path: &Path) -> Option<(String, Face)> {
    let data = std::fs::read(path).ok()?;
    let (family, weight, italic) = parse_face(&data)?;
    let digest = hex::encode(Sha256::digest(&data));
    Some((family, Face { path: path.to_path_buf(), weight, italic, digest }))
}

/// Family, weight range and slant from a font's bytes.
pub fn parse_face(data: &[u8]) -> Option<(String, [u16; 2], bool)> {
    use ttf_parser::{name_id, Face as Ttf, Tag};
    let face = Ttf::parse(data, 0).ok()?;
    let name = |id: u16| face.names().into_iter().filter(|n| n.name_id == id).find_map(|n| n.to_string()).filter(|s| !s.trim().is_empty());
    // The typographic family groups "Oswald Light" and "Oswald Bold" under "Oswald"; older
    // fonts only have the legacy family, which is the whole name for a static weight.
    let family = name(name_id::TYPOGRAPHIC_FAMILY).or_else(|| name(name_id::FAMILY))?.trim().to_string();
    let fixed = face.weight().to_number();
    let weight = face
        .variation_axes()
        .into_iter()
        .find(|a| a.tag == Tag::from_bytes(b"wght"))
        .map(|a| [a.min_value.round() as u16, a.max_value.round() as u16])
        .unwrap_or([fixed, fixed]);
    Some((family, weight, face.is_italic() || face.is_oblique()))
}

/// Every font file under `dir`, at any depth, sorted.
fn font_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.sort();
    files
}

/// A parsed file, valid while its mtime and size hold; `None` when it is not a font.
type Cached = ((SystemTime, u64), Option<(String, Face)>);

/// Folders of fonts, lowest precedence first, each with where it is.
#[derive(Debug, Default, Clone)]
pub struct FontLibrary {
    dirs: Vec<(PathBuf, Origin)>,
    /// Parsed files by path: a bake, a preview and every font request resolve fonts, and
    /// reading each file's tables every time is the slow part. Shared by clones.
    cache: Arc<Mutex<BTreeMap<PathBuf, Cached>>>,
}

impl FontLibrary {
    pub fn new(dirs: Vec<(PathBuf, Origin)>) -> Self {
        FontLibrary { dirs, cache: Arc::default() }
    }

    /// A font file, from its stamp and the cache or, when it changed, read afresh. The
    /// lock is held for the lookup and the insert, never for reading a file.
    fn face(&self, path: &Path) -> Option<(String, Face)> {
        let meta = std::fs::metadata(path).ok()?;
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        let lock = || self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, v)) = lock().get(path) {
            if *s == stamp {
                return v.clone();
            }
        }
        let v = read_face(path);
        lock().insert(path.to_path_buf(), (stamp, v.clone()));
        v
    }

    /// Every family, by name; a later folder's family replaces an earlier one's whole.
    /// Within a family, upright before italic and light before heavy: a stable order,
    /// since `Family::files` numbers them.
    pub fn families(&self) -> BTreeMap<String, Family> {
        let mut out = BTreeMap::new();
        for (dir, origin) in &self.dirs {
            let mut found: BTreeMap<String, Vec<Face>> = BTreeMap::new();
            for (family, face) in font_files(dir).iter().filter_map(|p| self.face(p)) {
                found.entry(family).or_default().push(face);
            }
            for (name, mut faces) in found {
                faces.sort_by(|a, b| (a.italic, a.weight, &a.path).cmp(&(b.italic, b.weight, &b.path)));
                out.insert(name.clone(), Family { name, origin: *origin, faces });
            }
        }
        out
    }

    /// The file a font path names: `<slug>/<file>` as `Family::css` wrote it.
    pub fn file(&self, slug: &str, file: &str) -> Option<PathBuf> {
        self.families().into_values().find(|f| f.slug() == slug)?.file(file).map(Path::to_path_buf)
    }

    /// Resolve `(var, family)` pairs: each family found once, in the order given, and the
    /// pairs whose family is not installed.
    pub fn resolve(&self, wanted: Vec<(String, String)>) -> Resolved {
        let all = self.families();
        let mut out = Resolved::default();
        for (var, name) in wanted {
            match all.get(&name) {
                Some(f) if !out.families.iter().any(|o| o.name == f.name) => out.families.push(f.clone()),
                Some(_) => {}
                None => out.missing.push((var.clone(), name)),
            }
            out.vars.push(var);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_fonts() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/fonts")
    }

    #[test]
    fn slugs_are_path_safe() {
        assert_eq!(slug("Playfair Display"), "playfair-display");
        assert_eq!(slug("  Söhne / Mono  "), "s-hne-mono");
        assert_eq!(css_string("A\"; } body {"), "\"A  body \"");
    }

    /// The starter set reads as the families its folders are named after, which is the
    /// name a person picks and a manifest writes.
    #[test]
    fn the_starter_fonts_read_as_their_families() {
        let lib = FontLibrary::new(vec![(repo_fonts(), Origin::Library)]);
        let all = lib.families();
        for name in ["Inter", "Ubuntu", "Bebas Neue", "Playfair Display", "JetBrains Mono", "Permanent Marker"] {
            assert!(all.contains_key(name), "{name} missing from {:?}", all.keys().collect::<Vec<_>>());
        }
        let inter = &all["Inter"];
        assert_eq!(inter.faces.len(), 2, "{inter:?}");
        assert!(!inter.faces[0].italic && inter.faces[1].italic);
        assert!(inter.faces[0].weight[0] <= 100 && inter.faces[0].weight[1] >= 900, "a variable font is a weight range: {:?}", inter.faces[0]);
        let css = inter.css("_fonts/");
        assert!(css.contains("font-family: \"Inter\"; src: url(\"_fonts/inter/0.ttf\")"), "{css}");
        assert!(css.contains("font-style: italic"), "{css}");
        assert_eq!(inter.file("1.ttf"), Some(inter.faces[1].path.as_path()));
        assert_eq!(inter.file("1.otf"), None);
        assert_eq!(inter.file("9.ttf"), None);
        // A static family is one file per weight and slant, gathered under one name.
        let ubuntu = &all["Ubuntu"];
        let upright: Vec<u16> = ubuntu.faces.iter().filter(|f| !f.italic).map(|f| f.weight[0]).collect();
        assert_eq!(upright, [300, 400, 500, 700], "{ubuntu:?}");
        assert_eq!(ubuntu.faces.iter().filter(|f| f.italic).count(), 4);
    }

    #[test]
    fn a_later_folder_replaces_a_family_whole() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::copy(repo_fonts().join("Anton/Anton-Regular.ttf"), tmp.path().join("mine.ttf")).unwrap();
        std::fs::write(tmp.path().join("notes.ttf"), b"not a font").unwrap();
        let lib = FontLibrary::new(vec![(repo_fonts(), Origin::Library), (tmp.path().to_path_buf(), Origin::Project)]);
        let anton = lib.families().remove("Anton").unwrap();
        assert_eq!(anton.origin, Origin::Project);
        assert_eq!(anton.faces.len(), 1);
        assert_eq!(lib.file("anton", "0.ttf"), Some(tmp.path().join("mine.ttf")));
        let pair = |v: &str, f: &str| (v.to_string(), f.to_string());
        let r = lib.resolve(vec![pair("a", "Anton"), pair("b", "Nope"), pair("c", "Anton")]);
        assert_eq!(r.families.len(), 1);
        assert_eq!(r.vars, ["a", "b", "c"]);
        assert_eq!(r.missing, [pair("b", "Nope")]);
    }
}
