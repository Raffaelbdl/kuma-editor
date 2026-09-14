//! The kuma ⇄ template contract, applied to a template's HTML.
//!
//! Both consumers of an overlay — the bake (hyperframes) and the live preview (an iframe in
//! the editor) — must hand the template identical inputs, so this is the one place that
//! builds them:
//!
//! * `window.kuma = { vars, fps, width, height, duration, speed, bake, clip }` in `<head>`;
//! * `--var-<name>` CSS custom properties on `:root`;
//! * the root composition's `data-duration`/`data-width`/`data-height` **rewritten in the
//!   markup** (hyperframes sizes the render from the attribute as written, before any
//!   script runs, and mounts children by their own `data-duration`).

use kuma_core::Fps;
use serde_json::{json, Map, Value};

pub struct Contract<'a> {
    pub vars: &'a Map<String, Value>,
    pub fps: Fps,
    pub width: u32,
    pub height: u32,
    /// Internal duration (clip length × |speed|), seconds.
    pub duration: f64,
    pub speed: f64,
    /// True when rendering for export, false in the live preview.
    pub bake: bool,
    pub clip: Option<&'a str>,
    /// Extra markup inserted at the top of `<head>` (the preview adds `<base>` and the
    /// hyperframes runtime here).
    pub head_extra: &'a str,
}

impl Contract<'_> {
    pub fn apply(&self, html: &str) -> String {
        let html = stamp_root(html, self.duration, self.width, self.height);
        let css_vars: String = self
            .vars
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("--var-{}: {};", k, s.replace(';', ""))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let payload = json!({
            "vars": self.vars,
            "fps": { "num": self.fps.num, "den": self.fps.den },
            "width": self.width,
            "height": self.height,
            "duration": self.duration,
            "speed": self.speed,
            "bake": self.bake,
            "clip": self.clip,
        });
        let head = format!(
            "{}<script id=\"kuma-inject\">window.kuma = {payload};</script>\n<style id=\"kuma-vars\">:root {{ {css_vars} }}</style>\n",
            self.head_extra
        );
        match html.find("<head>") {
            Some(i) => {
                let (a, b) = html.split_at(i + "<head>".len());
                format!("{a}\n{head}{b}")
            }
            None => format!("{head}{html}"),
        }
    }
}

/// Rewrite the root composition's timing/size attributes, and stretch every child whose
/// end coincided with the old root default (i.e. was meant to last "until the end") to the
/// new end. Children that end earlier keep their timing; nested compositions are untouched.
pub fn stamp_root(html: &str, duration: f64, width: u32, height: u32) -> String {
    let Some(marker) = html.find("data-composition-id") else { return html.to_string() };
    let Some(root_start) = html[..marker].rfind('<') else { return html.to_string() };
    let Some(end_rel) = html[marker..].find('>') else { return html.to_string() };
    let old_default = attr_num(&html[root_start..marker + end_rel], "data-duration");

    let mut out = String::with_capacity(html.len() + 64);
    let mut pos = 0;
    while let Some(lt_rel) = html[pos..].find('<') {
        let lt = pos + lt_rel;
        let Some(gt_rel) = html[lt..].find('>') else { break };
        let gt = lt + gt_rel;
        let tag = &html[lt..gt];
        out.push_str(&html[pos..lt]);
        let stamped = if lt == root_start {
            let t = set_attr(tag, "data-duration", &fmt_secs(duration));
            let t = set_attr(&t, "data-width", &width.to_string());
            set_attr(&t, "data-height", &height.to_string())
        } else if let (Some(d0), Some(child_dur)) = (old_default, attr_num(tag, "data-duration")) {
            let start = attr_num(tag, "data-start").unwrap_or(0.0);
            let ends_at_root = (start + child_dur - d0).abs() < 1e-6 && !tag.contains("data-composition-src");
            if ends_at_root {
                set_attr(tag, "data-duration", &fmt_secs((duration - start).max(0.0)))
            } else {
                tag.to_string()
            }
        } else {
            tag.to_string()
        };
        out.push_str(&stamped);
        pos = gt;
    }
    out.push_str(&html[pos..]);
    out
}

/// Seconds as a compact decimal (`32.633`, `4`), never exponent notation.
pub fn fmt_secs(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.to_string() }
}

/// Numeric value of `name="…"` / `name='…'` in an opening tag, if present.
fn attr_num(tag: &str, name: &str) -> Option<f64> {
    let (vs, ve) = attr_value_span(tag, name)?;
    tag[vs..ve].trim().parse().ok()
}

/// Byte span of the value of attribute `name` inside an opening tag (quoted or bare).
fn attr_value_span(tag: &str, name: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(rel) = tag[from..].find(name) {
        let i = from + rel;
        let boundary = i == 0 || !(tag.as_bytes()[i - 1].is_ascii_alphanumeric() || tag.as_bytes()[i - 1] == b'-');
        let after = &tag[i + name.len()..];
        let trimmed = after.trim_start();
        if boundary && trimmed.starts_with('=') {
            let eq = i + name.len() + (after.len() - trimmed.len());
            let rest = &tag[eq + 1..];
            let vs0 = eq + 1 + (rest.len() - rest.trim_start().len());
            return Some(match tag[vs0..].chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let vs = vs0 + 1;
                    (vs, tag[vs..].find(q).map(|x| vs + x).unwrap_or(tag.len()))
                }
                _ => (vs0, tag[vs0..].find(char::is_whitespace).map(|x| vs0 + x).unwrap_or(tag.len())),
            });
        }
        from = i + name.len();
    }
    None
}

/// Replace (or append) an attribute value in an opening tag.
fn set_attr(tag: &str, name: &str, value: &str) -> String {
    match attr_value_span(tag, name) {
        Some((vs, ve)) => format!("{}{}{}", &tag[..vs], value, &tag[ve..]),
        None => format!("{tag} {name}=\"{value}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_root_and_end_anchored_children_only() {
        let html = "<html><body><div id=\"root\" data-composition-id=\"main\" data-start=\"0\" data-duration=\"4\" data-width=\"1920\" data-height=\"1080\"><div class=\"clip\" data-start=\"0\" data-duration=\"4\"></div><div data-start=\"1\" data-duration=\"1\"></div><div data-start=\"1\" data-duration=\"3\"></div></div></body></html>";
        let out = stamp_root(html, 32.5, 1080, 1920);
        assert!(out.contains("data-composition-id=\"main\" data-start=\"0\" data-duration=\"32.5\" data-width=\"1080\" data-height=\"1920\""), "{out}");
        assert!(out.contains("<div class=\"clip\" data-start=\"0\" data-duration=\"32.5\">"), "whole-clip child stretched: {out}");
        assert!(out.contains("data-start=\"1\" data-duration=\"1\""), "shorter child untouched: {out}");
        assert!(out.contains("data-start=\"1\" data-duration=\"31.5\""), "end-anchored child stretched from its start: {out}");
    }

    #[test]
    fn handles_quotes_and_missing_attributes() {
        let out = stamp_root("<div data-composition-id='main' data-duration='5'>", 8.0, 1920, 1080);
        assert!(out.contains("data-duration='8'"), "{out}");
        let out = stamp_root("<div data-composition-id=\"main\">", 2.25, 1920, 1080);
        assert_eq!(out, "<div data-composition-id=\"main\" data-duration=\"2.25\" data-width=\"1920\" data-height=\"1080\">");
        assert_eq!(fmt_secs(32.633), "32.633");
        assert_eq!(fmt_secs(4.0), "4");
    }

    #[test]
    fn contract_injects_payload_and_css_vars() {
        let mut vars = Map::new();
        vars.insert("label".into(), json!("hi"));
        let c = Contract { vars: &vars, fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 2.0, speed: 1.0, bake: true, clip: None, head_extra: "<base href=\"/x/\">\n" };
        let html = c.apply("<html><head><title>x</title></head><body><div data-composition-id=\"main\" data-duration=\"4\"></div></body></html>");
        assert!(html.find("<base").unwrap() < html.find("kuma-inject").unwrap());
        assert!(html.find("kuma-inject").unwrap() < html.find("<title>").unwrap());
        assert!(html.contains("window.kuma = {"));
        assert!(html.contains("--var-label: hi;"));
        assert!(html.contains("data-composition-id=\"main\" data-duration=\"2\""), "{html}");
    }
}
