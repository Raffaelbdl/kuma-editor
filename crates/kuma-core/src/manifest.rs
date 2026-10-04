//! Overlay manifests (`overlay.json`, beside the overlay's `index.html`).
//!
//! The same shape for a project's overlays and the library's blueprints. The UI builds a
//! properties panel from `vars`, in the order they are written, and an entry's `vars` are
//! validated against it.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Default clip duration when dropped on the timeline, seconds.
    #[serde(default = "default_duration")]
    pub duration_default: f64,
    /// If true the overlay does not animate: it bakes to a single still frame.
    #[serde(default, rename = "static")]
    pub is_static: bool,
    /// Typed variables exposed in the properties panel, in the order the panel shows them.
    #[serde(default)]
    pub vars: IndexMap<String, VarSpec>,
    /// Free-form tags for the blueprint browser.
    #[serde(default)]
    pub tags: Vec<String>,
    /// A blueprint offered in the bin's **New** menu, under this label ("Title"). Creating
    /// one is the same copy as adding the blueprint; this only says it is a starting point
    /// someone reaches for often enough to want it one click from the bin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
}

fn default_duration() -> f64 {
    4.0
}

/// A pixel rectangle in composition space (frame sampling crops to one).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VarType {
    String,
    Text,
    Number,
    Color,
    Boolean,
    Enum,
    Point,
    /// A list of timed lines, `[{ "start", "end", "text" }, …]` in seconds relative to
    /// the clip start. What a subtitles overlay takes; the inspector edits it as rows.
    Cues,
    /// A font family by name (`"Inter"`), from the project's `fonts/` or a library's. kuma
    /// supplies the `@font-face` rules; the overlay writes `font-family: var(--var-<name>)`.
    Font,
    /// The language of the overlay's text, a BCP 47 tag (`ja`, `zh-Hant`, `en`), or empty
    /// for the page's own. kuma writes it onto the page's `<html lang>`, which decides the
    /// glyph forms of han characters and the fallback font, so one folder serves entries
    /// in several languages. The overlay need not read it. At most one per manifest.
    Lang,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VarSpec {
    #[serde(rename = "type")]
    pub ty: VarType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Allowed values for `enum`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The inspector section the var is listed under ("Text", "Box", "Animation"). Sections
    /// appear in the order their first var does; a var without one is listed first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Show the control only while other vars have these values: `{ "box": true }`, or a
    /// list of allowed values, `{ "enter": ["rise", "drop"] }`. Presentation only — a hidden
    /// var still has its value and still reaches the overlay.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub when: IndexMap<String, Value>,
}

/// The manifest's file name inside an overlay folder.
pub const MANIFEST: &str = "overlay.json";

impl OverlayManifest {
    /// Load `dir/overlay.json`. The folder name is the identity; the manifest has no id.
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join(MANIFEST);
        let err = |reason: String| ManifestError { path: path.clone(), reason };
        let text = std::fs::read_to_string(&path).map_err(|e| err(format!("cannot read: {e}")))?;
        let m: OverlayManifest = serde_json::from_str(&text).map_err(|e| err(format!("cannot parse: {e}")))?;
        if let Some(problem) = m.check() {
            return Err(err(problem));
        }
        // The renderer and the live preview both open `index.html` unconditionally, so a
        // folder without one resolves fine and then 404s in preview and fails the bake.
        // Refuse it here instead, the way a bin entry with a missing file is refused.
        if !dir.join("index.html").is_file() {
            return Err(err("index.html is missing from the overlay folder".into()));
        }
        Ok(m)
    }

    /// The manifest's own consistency: defaults obey their specs, enums have options,
    /// the default duration is positive.
    pub fn check(&self) -> Option<String> {
        if !(self.duration_default > 0.0) {
            return Some("duration_default must be > 0".into());
        }
        for (k, spec) in &self.vars {
            if spec.ty == VarType::Enum && spec.options.is_empty() {
                return Some(format!("var `{k}`: enum needs options"));
            }
            if let Some(p) = spec.check(&spec.default) {
                return Some(format!("var `{k}`: default {p}"));
            }
            if spec.ty == VarType::Lang && self.vars.iter().take_while(|(other, _)| *other != k).any(|(_, s)| s.ty == VarType::Lang) {
                return Some(format!("var `{k}`: a page has one language, and another `lang` var already sets it"));
            }
            for (other, allowed) in &spec.when {
                let Some(target) = self.vars.get(other).filter(|_| other != k) else {
                    return Some(format!("var `{k}`: `when` names `{other}`, which is not another var"));
                };
                let values = allowed.as_array().cloned().unwrap_or_else(|| vec![allowed.clone()]);
                if let Some(p) = values.iter().find_map(|v| target.check(v)) {
                    return Some(format!("var `{k}`: `when` gives `{other}` a value it cannot take: {p}"));
                }
            }
        }
        if let Some(label) = &self.create {
            if label.trim().is_empty() {
                return Some("`create` must be a label, e.g. \"Title\"".into());
            }
        }
        None
    }

    /// Load every `<root>/*/overlay.json`, keyed by folder name. Failures are reported, not
    /// skipped, so a typo does not make a blueprint vanish silently.
    pub fn load_all(root: &Path) -> (BTreeMap<String, (PathBuf, OverlayManifest)>, Vec<ManifestError>) {
        let mut out = BTreeMap::new();
        let mut errors = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else { return (out, errors) };
        for entry in rd.flatten() {
            let dir = entry.path();
            let Some(name) = dir.file_name().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            if dir.join(MANIFEST).is_file() {
                match OverlayManifest::load(&dir) {
                    Ok(m) => {
                        out.insert(name, (dir, m));
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        (out, errors)
    }

    /// Effective variable values: defaults overlaid with `overrides`.
    pub fn resolve_vars(&self, overrides: &Map<String, Value>) -> Map<String, Value> {
        let mut out = Map::new();
        for (k, spec) in &self.vars {
            out.insert(k.clone(), overrides.get(k).cloned().unwrap_or(spec.default.clone()));
        }
        out
    }

    /// The page language these (resolved) vars set: the `lang` var's value, unless it is
    /// empty or there is none, when the page keeps the `lang` it is written with.
    pub fn lang<'v>(&self, vars: &'v Map<String, Value>) -> Option<&'v str> {
        let (k, _) = self.vars.iter().find(|(_, s)| s.ty == VarType::Lang)?;
        vars.get(k)?.as_str().map(str::trim).filter(|l| !l.is_empty())
    }

    /// Validate overrides against the manifest. Returns human-readable problems.
    pub fn check_vars(&self, overrides: &Map<String, Value>) -> Vec<String> {
        let mut problems = Vec::new();
        for (k, v) in overrides {
            let Some(spec) = self.vars.get(k) else {
                problems.push(format!("unknown var `{k}`: the overlay's manifest does not declare it"));
                continue;
            };
            if let Some(p) = spec.check(v) {
                problems.push(format!("var `{k}`: {p}"));
            }
        }
        problems
    }
}

impl VarSpec {
    pub fn check(&self, v: &Value) -> Option<String> {
        match self.ty {
            VarType::String | VarType::Text => {
                if !v.is_string() {
                    return Some("expected a string".into());
                }
            }
            VarType::Font => {
                if !v.as_str().is_some_and(|s| !s.trim().is_empty()) {
                    return Some("expected a font family name".into());
                }
            }
            VarType::Lang => {
                let Some(s) = v.as_str() else { return Some("expected a language tag such as `ja`, or empty".into()) };
                // BCP 47's shape, not its registry: letters first, then dash-separated parts.
                let parts: Vec<&str> = s.trim().split('-').collect();
                let shaped = parts[0].len() >= 2 && parts[0].len() <= 8 && parts[0].chars().all(|c| c.is_ascii_alphabetic()) && parts[1..].iter().all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
                if !s.trim().is_empty() && !shaped {
                    return Some(format!("`{s}` is not a language tag (`ja`, `zh-Hant`, `en-GB`), nor empty for the page's own"));
                }
            }
            VarType::Color => {
                let Some(s) = v.as_str() else { return Some("expected a colour string".into()) };
                if !is_css_color(s) {
                    return Some(format!("`{s}` is not a #rgb/#rrggbb/#rrggbbaa colour"));
                }
            }
            VarType::Number => {
                let Some(n) = v.as_f64() else { return Some("expected a number".into()) };
                if let Some(min) = self.min {
                    if n < min {
                        return Some(format!("{n} < min {min}"));
                    }
                }
                if let Some(max) = self.max {
                    if n > max {
                        return Some(format!("{n} > max {max}"));
                    }
                }
            }
            VarType::Boolean => {
                if !v.is_boolean() {
                    return Some("expected true/false".into());
                }
            }
            VarType::Enum => {
                let Some(s) = v.as_str() else { return Some("expected a string".into()) };
                if !self.options.iter().any(|o| o == s) {
                    return Some(format!("`{s}` not in {:?}", self.options));
                }
            }
            VarType::Point => {
                let ok = v.get("x").and_then(Value::as_f64).is_some()
                    && v.get("y").and_then(Value::as_f64).is_some();
                if !ok {
                    return Some("expected {x, y}".into());
                }
            }
            VarType::Cues => {
                let Some(list) = v.as_array() else { return Some("expected a list of {start, end, text}".into()) };
                for (i, cue) in list.iter().enumerate() {
                    let time = |k: &str| cue.get(k).and_then(Value::as_f64).filter(|t| t.is_finite() && *t >= 0.0);
                    match (time("start"), time("end"), cue.get("text").and_then(Value::as_str)) {
                        (Some(start), Some(end), Some(_)) if start < end => {}
                        (Some(_), Some(_), Some(_)) => return Some(format!("cue {i}: end must be after start")),
                        _ => return Some(format!("cue {i}: expected {{start, end, text}} with times ≥ 0")),
                    }
                }
            }
        }
        None
    }
}

/// A bin entry's `overlay` folder must be relative and inside the project: overlays are the
/// project's own copies (media, by contrast, may live anywhere).
pub fn check_overlay_path(rel: &str) -> Option<String> {
    let p = Path::new(rel);
    if rel.is_empty() {
        return Some("the overlay folder is empty".into());
    }
    if p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
        return Some(format!("`{rel}` must be a folder inside the project, relative to it (e.g. `overlays/{}`)", p.file_name().and_then(|s| s.to_str()).unwrap_or("title")));
    }
    None
}

pub(crate) fn is_css_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else { return false };
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// A manifest that could not be loaded: which file, and why.
#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[error("{path}: {reason}")]
pub struct ManifestError {
    pub path: PathBuf,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> OverlayManifest {
        serde_json::from_value(json!({
            "name": "Location tag",
            "vars": {
                "label": { "type": "string", "default": "Paris" },
                "accent": { "type": "color", "default": "#ff4f00" },
                "x": { "type": "number", "default": 0.7, "min": 0, "max": 1 },
                "style": { "type": "enum", "options": ["pin", "chip"], "default": "pin" }
            }
        }))
        .unwrap()
    }

    #[test]
    fn resolves_defaults_and_overrides() {
        let m = manifest();
        let mut o = Map::new();
        o.insert("label".into(), json!("Tokyo"));
        let r = m.resolve_vars(&o);
        assert_eq!(r["label"], json!("Tokyo"));
        assert_eq!(r["accent"], json!("#ff4f00"));
    }

    #[test]
    fn rejects_bad_values() {
        let m = manifest();
        let mut o = Map::new();
        o.insert("x".into(), json!(2.0));
        o.insert("style".into(), json!("blob"));
        o.insert("accent".into(), json!("red"));
        o.insert("nope".into(), json!(1));
        let p = m.check_vars(&o);
        assert_eq!(p.len(), 4, "{p:?}");
    }

    #[test]
    fn manifest_checks_itself() {
        let bad = |v: serde_json::Value| serde_json::from_value::<OverlayManifest>(v).unwrap().check().expect("should be rejected");
        assert!(bad(json!({ "duration_default": 0 })).contains("duration"));
        assert!(bad(json!({ "vars": { "s": { "type": "enum", "default": "a", "options": [] } } })).contains("options"));
        assert!(bad(json!({ "vars": { "n": { "type": "number", "default": "big" } } })).contains("default"));
        assert!(bad(json!({ "vars": { "n": { "type": "number", "default": 5, "max": 1 } } })).contains("max"));
        assert!(manifest().check().is_none());
    }

    #[test]
    fn vars_keep_the_order_they_are_written_in() {
        let m: OverlayManifest = serde_json::from_str(r##"{ "vars": {
            "text": { "type": "string", "default": "a" },
            "font": { "type": "font", "default": "Inter" },
            "color": { "type": "color", "default": "#fff" } } }"##).unwrap();
        assert_eq!(m.vars.keys().collect::<Vec<_>>(), ["text", "font", "color"]);
        assert_eq!(m.resolve_vars(&Map::new()).keys().collect::<Vec<_>>(), ["text", "font", "color"]);
        let back = serde_json::to_string(&m).unwrap();
        assert!(back.find("\"text\"").unwrap() < back.find("\"font\"").unwrap() && back.find("\"font\"").unwrap() < back.find("\"color\"").unwrap(), "{back}");
    }

    #[test]
    fn a_font_is_a_family_name() {
        let spec: VarSpec = serde_json::from_value(json!({ "type": "font", "default": "Inter" })).unwrap();
        assert!(spec.check(&json!("Playfair Display")).is_none());
        assert!(spec.check(&json!("")).is_some() && spec.check(&json!(3)).is_some());
    }

    /// A `lang` var is a language tag or empty, the page has one, and empty leaves the
    /// page's own `lang` in place.
    #[test]
    fn a_lang_is_a_language_tag_or_nothing() {
        let spec: VarSpec = serde_json::from_value(json!({ "type": "lang", "default": "" })).unwrap();
        for ok in ["", "ja", "zh-Hant", "en-GB", "ja-JP-u-ca-japanese"] {
            assert!(spec.check(&json!(ok)).is_none(), "{ok}");
        }
        for bad in [json!("j"), json!("ja_JP"), json!("日本語"), json!("ja-"), json!(3)] {
            assert!(spec.check(&bad).is_some(), "{bad}");
        }
        let m: OverlayManifest = serde_json::from_value(json!({ "vars": { "label": { "type": "string", "default": "x" }, "lang": { "type": "lang", "default": "" } } })).unwrap();
        assert!(m.check().is_none());
        assert_eq!(m.lang(&m.resolve_vars(&Map::new())), None);
        let ja = json!({ "lang": "ja" });
        assert_eq!(m.lang(&m.resolve_vars(ja.as_object().unwrap())), Some("ja"));
        let two: OverlayManifest = serde_json::from_value(json!({ "vars": { "a": { "type": "lang", "default": "" }, "b": { "type": "lang", "default": "ja" } } })).unwrap();
        assert!(two.check().unwrap().contains("var `b`"), "{:?}", two.check());
    }

    #[test]
    fn when_names_another_var_and_values_it_can_take() {
        let with = |when: serde_json::Value| {
            serde_json::from_value::<OverlayManifest>(json!({ "vars": {
                "box": { "type": "boolean", "default": false },
                "enter": { "type": "enum", "options": ["fade", "rise"], "default": "fade" },
                "pad": { "type": "number", "default": 1, "when": when } } }))
            .unwrap()
            .check()
        };
        assert!(with(json!({ "box": true })).is_none());
        assert!(with(json!({ "enter": ["fade", "rise"], "box": true })).is_none());
        assert!(with(json!({ "nope": true })).unwrap().contains("not another var"));
        assert!(with(json!({ "pad": 1 })).unwrap().contains("not another var"));
        assert!(with(json!({ "enter": ["fly"] })).unwrap().contains("cannot take"));
        assert!(with(json!({ "box": "yes" })).unwrap().contains("cannot take"));
    }

    #[test]
    fn an_overlay_folder_stays_inside_the_project() {
        assert!(check_overlay_path("overlays/title").is_none());
        assert!(check_overlay_path("./overlays/title").is_none());
        for bad in ["", "/abs/title", "../other/overlays/title", "overlays/../../x"] {
            assert!(check_overlay_path(bad).is_some(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn cues_are_a_list_of_timed_lines() {
        let spec: VarSpec = serde_json::from_value(json!({ "type": "cues", "default": [] })).unwrap();
        assert!(spec.check(&json!([{ "start": 0, "end": 1.5, "text": "hi" }, { "start": 1.5, "end": 2, "text": "" }])).is_none());
        let bad = |v: Value| spec.check(&v).expect("should be rejected");
        assert!(bad(json!("no")).contains("list"));
        assert!(bad(json!([{ "start": 2, "end": 1, "text": "x" }])).contains("cue 0: end"));
        assert!(bad(json!([{ "start": 0, "end": 1, "text": "x" }, { "start": -1, "end": 1, "text": "x" }])).contains("cue 1"));
        assert!(bad(json!([{ "start": 0, "end": 1 }])).contains("text"));
    }

    /// A folder carrying a manifest but no markup used to load, resolve and validate
    /// cleanly, then 404 in the live preview and fail the bake.
    #[test]
    fn an_overlay_folder_needs_its_index_html() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("location-tag");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), r#"{ "name": "Location tag" }"#).unwrap();

        let e = OverlayManifest::load(&dir).expect_err("no index.html yet");
        assert!(e.reason.contains("index.html"), "{e:?}");
        // load_all reports it rather than skipping the folder silently.
        let (loaded, errors) = OverlayManifest::load_all(tmp.path());
        assert!(loaded.is_empty() && errors.len() == 1, "{loaded:?} {errors:?}");

        std::fs::write(dir.join("index.html"), "<div data-composition-id=\"main\"></div>").unwrap();
        assert_eq!(OverlayManifest::load(&dir).unwrap().name, "Location tag");
        // Keyed by folder, since the manifest carries no id of its own.
        assert!(OverlayManifest::load_all(tmp.path()).0.contains_key("location-tag"));
    }
}
