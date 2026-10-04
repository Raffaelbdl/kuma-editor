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

/// Extensions read as fonts. A collection (`.ttc`/`.otc`) is split into these when it is
/// added (`split_collection`), and a web font unwrapped into one (`unwrap_web_font`).
pub const EXTENSIONS: &[&str] = &["ttf", "otf"];

/// Web fonts: an sfnt (or a collection) wrapped for the web — each table zlib-compressed
/// in WOFF, all of them Brotli-compressed in WOFF2, which also re-encodes `glyf`/`loca`.
/// `add_font` unwraps one into the font it holds, so a font folder only ever holds what
/// `EXTENSIONS` names and nothing downstream (the rules, the staging, the bake key) has a
/// second kind of file to know about.
pub const WEB_FONTS: &[&str] = &["woff", "woff2"];

/// Font collections: several faces sharing tables in one file, which is how CJK families
/// usually ship. `@font-face` wants one face per file.
pub const COLLECTIONS: &[&str] = &["ttc", "otc"];

/// Writing systems a face is asked about, each by one character it cannot set without:
/// what `list_fonts` reports, so text in a script a family lacks is not left to whatever
/// fallback the baking machine happens to have.
const SCRIPTS: &[(&str, char)] = &[
    ("latin", 'A'),
    ("greek", 'Ω'),
    ("cyrillic", 'Ж'),
    ("arabic", 'ع'),
    ("hebrew", 'א'),
    ("devanagari", 'क'),
    ("thai", 'ก'),
    ("han", '永'),
    ("kana", 'あ'),
    ("hangul", '한'),
];

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
    /// The writing systems it can set (`SCRIPTS`), in that order.
    pub scripts: Vec<&'static str>,
    /// The family's other names in the file, localized ones included: a `font` var
    /// written with one of them still finds the family (`FontLibrary::resolve`).
    #[serde(skip)]
    pub aliases: Vec<String>,
    /// Every character it has a glyph for: what says a text var's characters are drawn by
    /// this font rather than by whatever the baking machine falls back to.
    #[serde(skip)]
    pub coverage: Coverage,
}

impl Face {
    fn ext(&self) -> String {
        self.path.extension().and_then(|e| e.to_str()).unwrap_or("ttf").to_ascii_lowercase()
    }
}

/// The characters a font maps to glyphs, as sorted inclusive ranges of code points. Shared,
/// since a face is cloned on every lookup and a CJK face maps tens of thousands.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage(Arc<[(u32, u32)]>);

impl Coverage {
    /// From code points in any order.
    fn from_points(mut points: Vec<u32>) -> Self {
        points.sort_unstable();
        points.dedup();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for p in points {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == p => *end = p,
                _ => ranges.push((p, p)),
            }
        }
        Coverage(ranges.into())
    }

    pub fn contains(&self, c: char) -> bool {
        let c = c as u32;
        let i = self.0.partition_point(|&(_, end)| end < c);
        self.0.get(i).is_some_and(|&(start, _)| start <= c)
    }
}

/// A family and the faces it has, from the one folder that supplies it.
#[derive(Debug, Clone, Serialize)]
pub struct Family {
    pub name: String,
    pub origin: Origin,
    /// Writing systems any of its faces can set: text in another one is drawn by the
    /// overlay's fallback stack, which is the baking machine's, not the project's.
    pub scripts: Vec<&'static str>,
    /// Other names its faces carry (`Face::aliases`); `css` declares the faces under
    /// these too, since a `font` var's value is written into the CSS as it is.
    pub aliases: Vec<String>,
    pub faces: Vec<Face>,
}

impl Family {
    /// The family's folder in a font path: the name reduced to `[a-z0-9-]`.
    pub fn slug(&self) -> String {
        slug(&self.name)
    }

    /// Whether some face of the family draws `c`. Per family, not per face: which face a
    /// weight picks is the browser's business, and a family whose bold lacks a character its
    /// regular has is rare enough not to guess about.
    pub fn covers(&self, c: char) -> bool {
        self.faces.iter().any(|f| f.coverage.contains(c))
    }

    /// Each face under the one name it is staged and served as, `<i>.<ext>`.
    pub fn files(&self) -> impl Iterator<Item = (String, &Face)> {
        self.faces.iter().enumerate().map(|(i, f)| (format!("{i}.{}", f.ext()), f))
    }

    /// `@font-face` rules for every face, under the family's name and each alias, with
    /// sources at `<prefix><slug>/<file>`.
    pub fn css(&self, prefix: &str) -> String {
        let slug = self.slug();
        let mut css = String::new();
        for family in std::iter::once(&self.name).chain(&self.aliases).map(|n| css_string(n)) {
            for (file, f) in self.files() {
                let weight = if f.weight[0] == f.weight[1] { f.weight[0].to_string() } else { format!("{} {}", f.weight[0], f.weight[1]) };
                let format = if f.ext() == "otf" { "opentype" } else { "truetype" };
                css.push_str(&format!(
                    "@font-face {{ font-family: {family}; src: url(\"{prefix}{slug}/{file}\") format(\"{format}\"); font-weight: {weight}; font-style: {}; font-display: block; }}\n",
                    if f.italic { "italic" } else { "normal" }
                ));
            }
        }
        css
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

/// The font a WOFF or WOFF2 file wraps, as the sfnt it was made from (a collection, when
/// that is what was wrapped); `None` when `data` is not a web font, which is the file's to
/// say rather than its extension's. An `Err` is a web font that does not decode.
pub fn unwrap_web_font(data: &[u8]) -> Option<Result<Vec<u8>, String>> {
    let decoded = match data.get(..4)? {
        b"wOFF" => wuff::decompress_woff1(data),
        b"wOF2" => wuff::decompress_woff2(data),
        _ => return None,
    };
    // The decoder's error says only that it failed.
    Some(decoded.map_err(|_| "it is damaged, or not a WOFF/WOFF2 file after all".to_string()))
}

/// The extension an sfnt's outlines call for: `otf` for CFF, else `ttf`.
pub fn sfnt_ext(data: &[u8]) -> &'static str {
    if data.starts_with(b"OTTO") { "otf" } else { "ttf" }
}

/// What a font file says about itself, or `None` when it is not one kuma can read. A web
/// font is read as the font it wraps.
pub fn read_face(path: &Path) -> Option<(String, Face)> {
    let data = std::fs::read(path).ok()?;
    let info = match unwrap_web_font(&data) {
        Some(sfnt) => parse_face(&sfnt.ok()?)?,
        None => parse_face(&data)?,
    };
    let digest = hex::encode(Sha256::digest(&data));
    let face = Face { path: path.to_path_buf(), weight: info.weight, italic: info.italic, digest, scripts: info.scripts, aliases: info.aliases, coverage: info.coverage };
    Some((info.family, face))
}

/// What a font's bytes say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceInfo {
    pub family: String,
    pub weight: [u16; 2],
    pub italic: bool,
    pub scripts: Vec<&'static str>,
    pub aliases: Vec<String>,
    pub coverage: Coverage,
}

/// Family, weight range, slant and scripts from a font's bytes.
pub fn parse_face(data: &[u8]) -> Option<FaceInfo> {
    use ttf_parser::{name_id, Face as Ttf, Tag};
    let face = Ttf::parse(data, 0).ok()?;
    // Every name for an id, English first: a CJK font often lists its localized names
    // first, and "WenQuanYi Zen Hei" is what a person types, not "文泉驛正黑". The rest
    // stay aliases, so a var written when the first record won still resolves.
    let names = |id: u16| {
        let records: Vec<_> = face.names().into_iter().filter(|n| n.name_id == id).collect();
        let english = records.iter().filter(|n| n.language() == ttf_parser::Language::English_UnitedStates);
        let mut out: Vec<String> = Vec::new();
        for s in english.chain(records.iter()).filter_map(|n| n.to_string()).map(|s| s.trim().to_string()) {
            if !s.is_empty() && !out.contains(&s) {
                out.push(s);
            }
        }
        out
    };
    // The typographic family groups "Oswald Light" and "Oswald Bold" under "Oswald"; older
    // fonts only have the legacy family, which is the whole name for a static weight.
    let mut family = Some(names(name_id::TYPOGRAPHIC_FAMILY)).filter(|n| !n.is_empty()).unwrap_or_else(|| names(name_id::FAMILY));
    let aliases = family.split_off(1.min(family.len()));
    let family = family.pop()?;
    let fixed = face.weight().to_number();
    let weight = face
        .variation_axes()
        .into_iter()
        .find(|a| a.tag == Tag::from_bytes(b"wght"))
        .map(|a| [a.min_value.round() as u16, a.max_value.round() as u16])
        .unwrap_or([fixed, fixed]);
    let scripts = SCRIPTS.iter().filter(|(_, c)| face.glyph_index(*c).is_some()).map(|(s, _)| *s).collect();
    // Every Unicode subtable's mapped code points; a code point mapped to glyph 0 is
    // `.notdef`, the missing-glyph box, and does not count.
    let mut points = Vec::new();
    for sub in face.tables().cmap.into_iter().flat_map(|c| c.subtables).filter(|s| s.is_unicode()) {
        sub.codepoints(|p| {
            if sub.glyph_index(p).is_some_and(|g| g.0 != 0) {
                points.push(p);
            }
        });
    }
    Some(FaceInfo { family, weight, italic: face.is_italic() || face.is_oblique(), scripts, aliases, coverage: Coverage::from_points(points) })
}

/// The faces of a font collection as standalone fonts, each with the extension its
/// outlines call for (`otf` for CFF, else `ttf`); `None` when `data` is not a collection.
/// A face is its own table directory pointing into the shared tables, so it is rewritten
/// with its tables copied after it; nothing inside a table changes. Shared tables are
/// copied into every face, which is the price of `@font-face` taking one face per file.
pub fn split_collection(data: &[u8]) -> Option<Vec<(Vec<u8>, &'static str)>> {
    use ttf_parser::{fonts_in_collection, RawFace, Tag};
    (0..fonts_in_collection(data)?)
        .map(|i| {
            let records: Vec<_> = RawFace::parse(data, i).ok()?.table_records.into_iter().collect();
            let cff = records.iter().any(|r| r.tag == Tag::from_bytes(b"CFF ") || r.tag == Tag::from_bytes(b"CFF2"));
            // The offset table: version, then numTables and the binary-search fields
            // derived from it.
            let n = records.len();
            let log = n.checked_ilog2()? as usize;
            let mut out = if cff { b"OTTO".to_vec() } else { vec![0, 1, 0, 0] };
            for v in [n, 16 << log, log, n * 16 - (16 << log)] {
                out.extend_from_slice(&(v as u16).to_be_bytes());
            }
            let head = out.len() + 16 * n;
            let mut body = Vec::new();
            for r in &records {
                out.extend_from_slice(&r.tag.to_bytes());
                out.extend_from_slice(&r.check_sum.to_be_bytes());
                out.extend_from_slice(&((head + body.len()) as u32).to_be_bytes());
                out.extend_from_slice(&r.length.to_be_bytes());
                body.extend_from_slice(data.get(r.offset as usize..)?.get(..r.length as usize)?);
                body.resize(body.len().next_multiple_of(4), 0);
            }
            out.extend(body);
            Some((out, if cff { "otf" } else { "ttf" }))
        })
        .collect()
}

/// Every file under `dir` with one of `exts`, at any depth, sorted.
fn font_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            if p.is_dir() {
                walk(&p, exts, out);
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str())) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, exts, &mut files);
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

    /// A font file anywhere, web fonts included, read through the same cache: what an
    /// overlay's own `@font-face` rules point at. `None` when it is not a font kuma can read.
    pub fn read(&self, path: &Path) -> Option<(String, Face)> {
        self.face(path)
    }

    /// Every family, by name; a later folder's family replaces an earlier one's whole.
    /// Within a family, upright before italic and light before heavy: a stable order,
    /// since `Family::files` numbers them.
    pub fn families(&self) -> BTreeMap<String, Family> {
        let mut out = BTreeMap::new();
        for (dir, origin) in &self.dirs {
            let mut found: BTreeMap<String, Vec<Face>> = BTreeMap::new();
            for (family, face) in font_files(dir, EXTENSIONS).iter().filter_map(|p| self.face(p)) {
                found.entry(family).or_default().push(face);
            }
            for (name, mut faces) in found {
                faces.sort_by(|a, b| (a.italic, a.weight, &a.path).cmp(&(b.italic, b.weight, &b.path)));
                let scripts = SCRIPTS.iter().map(|(s, _)| *s).filter(|s| faces.iter().any(|f| f.scripts.contains(s))).collect();
                let mut aliases: Vec<String> = faces.iter().flat_map(|f| f.aliases.iter().cloned()).collect();
                aliases.sort();
                aliases.dedup();
                out.insert(name.clone(), Family { name, origin: *origin, scripts, aliases, faces });
            }
        }
        out
    }

    /// Collections left in a font folder as they are: not read, since a face has to be its
    /// own file (`add_font` splits one). Named where a missing family is reported.
    pub fn collections(&self) -> Vec<PathBuf> {
        self.dirs.iter().flat_map(|(dir, _)| font_files(dir, COLLECTIONS)).collect()
    }

    /// Web fonts left in a font folder as they are, copied in by hand: not read, since
    /// `add_font` unwraps one into the font it holds. Named where a missing family is.
    pub fn web_fonts(&self) -> Vec<PathBuf> {
        self.dirs.iter().flat_map(|(dir, _)| font_files(dir, WEB_FONTS)).collect()
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
            match all.get(&name).or_else(|| all.values().find(|f| f.aliases.contains(&name))) {
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

    /// A collection built from standalone fonts: each appended whole, its table offsets
    /// moved by where it landed, and the header pointing at its table directory.
    fn collection(fonts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"ttcf\0\x01\0\0".to_vec();
        out.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
        out.resize(out.len() + 4 * fonts.len(), 0);
        for (i, f) in fonts.iter().enumerate() {
            let base = out.len();
            out[12 + 4 * i..16 + 4 * i].copy_from_slice(&(base as u32).to_be_bytes());
            out.extend_from_slice(f);
            for t in 0..u16::from_be_bytes([f[4], f[5]]) as usize {
                let at = base + 12 + 16 * t + 8;
                let offset = u32::from_be_bytes(out[at..at + 4].try_into().unwrap()) + base as u32;
                out[at..at + 4].copy_from_slice(&offset.to_be_bytes());
            }
            out.resize(out.len().next_multiple_of(4), 0);
        }
        out
    }

    /// Each face of a collection comes out a font of its own that reads as the face it was.
    #[test]
    fn a_collection_splits_into_the_fonts_it_holds() {
        let read = |p: &str| std::fs::read(repo_fonts().join(p)).unwrap();
        let fonts = [read("Anton/Anton-Regular.ttf"), read("Inter/Inter-Italic[opsz,wght].ttf")];
        let faces = split_collection(&collection(&fonts)).expect("a collection");
        assert_eq!(faces.len(), 2);
        for ((bytes, ext), original) in faces.iter().zip(&fonts) {
            assert_eq!(*ext, "ttf");
            assert_eq!(parse_face(bytes), parse_face(original), "a face reads as it did before it was collected");
        }
        assert_eq!(parse_face(&faces[1].0).unwrap().family, "Inter");
        assert!(split_collection(&fonts[0]).is_none(), "a single font is not a collection");
        assert!(split_collection(&collection(&fonts)[..40]).is_none(), "a truncated file is refused, not read past");
    }

    /// A web font unwraps into a font that reads exactly as the one it was made from: the
    /// same family, weight, slant and characters. The fixtures are the starter set's Bebas
    /// Neue as fontTools writes WOFF and WOFF2 (the latter with `glyf` transformed).
    #[test]
    fn a_web_font_unwraps_into_the_font_it_was_made_from() {
        let original = parse_face(&std::fs::read(repo_fonts().join("Bebas Neue/BebasNeue-Regular.ttf")).unwrap()).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts");
        for file in ["BebasNeue-Regular.woff", "BebasNeue-Regular.woff2"] {
            let sfnt = unwrap_web_font(&std::fs::read(fixtures.join(file)).unwrap()).expect("a web font").unwrap();
            assert_eq!(sfnt_ext(&sfnt), "ttf");
            assert_eq!(parse_face(&sfnt), Some(original.clone()), "{file}");
            let (family, face) = read_face(&fixtures.join(file)).unwrap();
            assert_eq!((family.as_str(), face.coverage), ("Bebas Neue", original.coverage.clone()), "{file} is read as what it wraps");
        }
        assert!(unwrap_web_font(&std::fs::read(repo_fonts().join("Anton/Anton-Regular.ttf")).unwrap()).is_none(), "a font that is not a web font");
        assert!(unwrap_web_font(b"wOF2 and then nothing").unwrap().is_err(), "a damaged one is refused, not read past");
    }

    /// A family read under its English name still answers to the localized one a var may
    /// have been written with, and declares its faces under both, since a var's value goes
    /// into the CSS as it is. Needs a font with localized names; the starter set has none.
    #[test]
    fn a_family_answers_to_its_localized_names() {
        let Ok(ttc) = std::fs::read("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc") else {
            eprintln!("no wqy-zenhei.ttc here; skipping");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let (face, ext) = split_collection(&ttc).unwrap().remove(0);
        std::fs::write(tmp.path().join(format!("zenhei.{ext}")), face).unwrap();
        let lib = FontLibrary::new(vec![(tmp.path().to_path_buf(), Origin::Project)]);
        let family = &lib.families()["WenQuanYi Zen Hei"];
        assert!(family.aliases.contains(&"文泉驛正黑".to_string()), "{:?}", family.aliases);
        let r = lib.resolve(vec![("font".into(), "文泉驛正黑".into())]);
        assert!(r.missing.is_empty() && r.families[0].name == "WenQuanYi Zen Hei");
        let css = family.css("_fonts/");
        assert!(css.contains("font-family: \"WenQuanYi Zen Hei\"") && css.contains("font-family: \"文泉驛正黑\""), "{css}");
    }

    /// Which writing systems a font sets is read from the font, not from its name.
    #[test]
    fn a_face_says_which_scripts_it_sets() {
        let inter = parse_face(&std::fs::read(repo_fonts().join("Inter/Inter[opsz,wght].ttf")).unwrap()).unwrap();
        assert!(inter.scripts.contains(&"latin") && inter.scripts.contains(&"cyrillic"), "{:?}", inter.scripts);
        assert!(!inter.scripts.contains(&"han") && !inter.scripts.contains(&"kana"), "{:?}", inter.scripts);
        let lib = FontLibrary::new(vec![(repo_fonts(), Origin::Library)]);
        assert!(lib.families()["Inter"].scripts.contains(&"latin"));
    }

    /// Coverage is the font's own cmap: every character it maps, and nothing else.
    #[test]
    fn a_face_knows_every_character_it_draws() {
        let c = Coverage::from_points(vec![0x41, 0x43, 0x42, 0x61, 0x41]);
        assert_eq!(&*c.0, [(0x41, 0x43), (0x61, 0x61)]);
        assert!(c.contains('A') && c.contains('C') && c.contains('a'));
        assert!(!c.contains('@') && !c.contains('D') && !c.contains('b') && !Coverage::default().contains('A'));

        let inter = parse_face(&std::fs::read(repo_fonts().join("Inter/Inter[opsz,wght].ttf")).unwrap()).unwrap();
        for ch in ['A', 'é', 'Ж', '→'] {
            assert!(inter.coverage.contains(ch), "Inter draws {ch}");
        }
        assert!(!inter.coverage.contains('承') && !inter.coverage.contains('あ'));
        let lib = FontLibrary::new(vec![(repo_fonts(), Origin::Library)]);
        assert!(lib.families()["Inter"].covers('é'));
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
