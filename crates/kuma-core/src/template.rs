//! Overlay template manifests (`overlays/<id>/template.json`).
//!
//! The manifest is what makes agent-free editing possible: the UI builds a properties
//! panel from `vars`, and clip `vars` are validated against it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateManifest {
    /// Directory name under `overlays/`. Must match the folder.
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Default clip duration when dropped on the timeline, seconds.
    #[serde(default = "default_duration")]
    pub duration_default: f64,
    /// If true the template does not animate: it bakes to a single still frame.
    #[serde(default, rename = "static")]
    pub is_static: bool,
    /// Typed variables exposed in the properties panel.
    #[serde(default)]
    pub vars: BTreeMap<String, VarSpec>,
    /// Free-form tags for the library browser.
    #[serde(default)]
    pub tags: Vec<String>,
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
}

impl TemplateManifest {
    pub fn load(dir: &Path) -> Result<Self, TemplateError> {
        let path = dir.join("template.json");
        let err = |reason: String| TemplateError { path: path.clone(), reason };
        let text = std::fs::read_to_string(&path).map_err(|e| err(format!("cannot read: {e}")))?;
        let m: TemplateManifest = serde_json::from_str(&text).map_err(|e| err(format!("cannot parse: {e}")))?;
        let folder = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if m.id != folder {
            return Err(err(format!("manifest id `{}` does not match folder `{folder}`", m.id)));
        }
        if let Some(problem) = m.check() {
            return Err(err(problem));
        }
        // The renderer and the live preview both open `index.html` unconditionally, so a
        // folder without one resolves fine and then 404s in preview and fails the bake.
        // Refuse it here instead, the way a bin entry with a missing file is refused.
        if !dir.join("index.html").is_file() {
            return Err(err("index.html is missing from the template folder".into()));
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
        }
        None
    }

    /// Load every `<root>/*/template.json`. A folder whose manifest fails to load is
    /// reported, not silently skipped: a typo in one manifest would otherwise turn every
    /// clip using it into an "unknown template" error.
    pub fn load_all(root: &Path) -> (BTreeMap<String, (PathBuf, TemplateManifest)>, Vec<TemplateError>) {
        let mut out = BTreeMap::new();
        let mut errors = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else { return (out, errors) };
        for entry in rd.flatten() {
            let dir = entry.path();
            if dir.join("template.json").is_file() {
                match TemplateManifest::load(&dir) {
                    Ok(m) => {
                        out.insert(m.id.clone(), (dir, m));
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

    /// Validate overrides against the manifest. Returns human-readable problems.
    pub fn check_vars(&self, overrides: &Map<String, Value>) -> Vec<String> {
        let mut problems = Vec::new();
        for (k, v) in overrides {
            let Some(spec) = self.vars.get(k) else {
                problems.push(format!("unknown var `{k}` (template `{}`)", self.id));
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
        }
        None
    }
}

pub(crate) fn is_css_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else { return false };
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// A manifest that could not be loaded: which file, and why.
#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[error("{path}: {reason}")]
pub struct TemplateError {
    pub path: PathBuf,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> TemplateManifest {
        serde_json::from_value(json!({
            "id": "location-tag", "name": "Location tag",
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
        let bad = |v: serde_json::Value| serde_json::from_value::<TemplateManifest>(v).unwrap().check().expect("should be rejected");
        assert!(bad(json!({ "id": "t", "duration_default": 0 })).contains("duration"));
        assert!(bad(json!({ "id": "t", "vars": { "s": { "type": "enum", "default": "a", "options": [] } } })).contains("options"));
        assert!(bad(json!({ "id": "t", "vars": { "n": { "type": "number", "default": "big" } } })).contains("default"));
        assert!(bad(json!({ "id": "t", "vars": { "n": { "type": "number", "default": 5, "max": 1 } } })).contains("max"));
        assert!(manifest().check().is_none());
    }

    /// A folder carrying a manifest but no markup used to load, resolve and validate
    /// cleanly, then 404 in the live preview and fail the bake.
    #[test]
    fn a_template_folder_needs_its_index_html() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("location-tag");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("template.json"), r#"{ "id": "location-tag" }"#).unwrap();

        let e = TemplateManifest::load(&dir).expect_err("no index.html yet");
        assert!(e.reason.contains("index.html"), "{e:?}");
        // load_all reports it rather than skipping the folder silently.
        let (loaded, errors) = TemplateManifest::load_all(tmp.path());
        assert!(loaded.is_empty() && errors.len() == 1, "{loaded:?} {errors:?}");

        std::fs::write(dir.join("index.html"), "<div data-composition-id=\"main\"></div>").unwrap();
        assert_eq!(TemplateManifest::load(&dir).unwrap().id, "location-tag");
    }
}
