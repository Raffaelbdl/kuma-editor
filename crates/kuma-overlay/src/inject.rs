//! The kuma ⇄ overlay contract, applied to an overlay's HTML.
//!
//! Both consumers of an overlay — the bake (hyperframes) and the live preview (an iframe in
//! the editor) — must hand the overlay identical inputs, so this is the one place that
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
        // One custom property per scalar var; a point becomes `--var-<name>-x`/`-y`.
        // Values are user text: keep them from closing the declaration or the block.
        let css = |name: &str, v: &Value| {
            let s = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("--var-{name}: {};", s.replace([';', '{', '}', '<', '>', '\n'], " "))
        };
        let css_vars: String = self
            .vars
            .iter()
            .flat_map(|(k, v)| match v {
                Value::Object(o) => o.iter().map(|(sub, x)| css(&format!("{k}-{sub}"), x)).collect::<Vec<_>>(),
                Value::Array(_) => vec![],
                other => vec![css(k, other)],
            })
            .collect::<Vec<_>>()
            .join(" ");
        // `</script>` inside a string var must not end the inline script.
        let payload = json!({
            "vars": self.vars,
            "fps": { "num": self.fps.num, "den": self.fps.den },
            "width": self.width,
            "height": self.height,
            "duration": self.duration,
            "speed": self.speed,
            "bake": self.bake,
            "clip": self.clip,
        })
        .to_string()
        .replace("</", "<\\/");
        let head = format!("{}<script id=\"kuma-inject\">window.kuma = {payload};</script>\n", self.head_extra);
        // The script goes first so the overlay's own scripts see `window.kuma`; the vars
        // go last so they win over any `:root { --var-… }` fallback in the overlay's CSS.
        let style = format!("<style id=\"kuma-vars\">:root {{ {css_vars} }}</style>\n");
        // Insert after the opening `<head …>` (else `<html …>`, else the doctype) so the
        // document never drops into quirks mode, and the style before `</head>`.
        let open_end = |name: &str| tags(&html).find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| t.end);
        let at = open_end("head").or_else(|| open_end("html")).or_else(|| open_end("!doctype")).unwrap_or(0);
        let html = format!("{}\n{head}{}", &html[..at], &html[at..]);
        match html.find("</head>") {
            Some(i) => format!("{}{style}{}", &html[..i], &html[i..]),
            None => format!("{html}{style}"),
        }
    }
}

/// An opening tag (or comment/doctype) in the markup: its name and byte span.
struct Tag<'a> {
    name: &'a str,
    text: &'a str,
    start: usize,
    end: usize,
}

/// Every `<…>` in document order, skipping text and the contents of `<script>`,
/// `<style>` and comments so a selector or string mentioning `data-composition-id`
/// cannot be mistaken for the root.
fn tags(html: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        let lt = pos + html[pos..].find('<')?;
        let rest = &html[lt..];
        let (end, skip_to) = if let Some(r) = rest.strip_prefix("<!--") {
            let e = lt + 4 + r.find("-->").map(|i| i + 3).unwrap_or(r.len());
            (e, e)
        } else {
            let gt = lt + rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
            let name_end = rest[1..].find(|c: char| c.is_whitespace() || c == '>' || c == '/').map(|i| i + 1).unwrap_or(rest.len().min(gt - lt));
            let name = &rest[1..name_end];
            // Raw-text elements: jump past the closing tag.
            let lower = name.to_ascii_lowercase();
            let skip = if lower == "script" || lower == "style" {
                let close = format!("</{lower}");
                html[gt..].to_ascii_lowercase().find(&close).map(|i| gt + i).unwrap_or(html.len())
            } else {
                gt
            };
            (gt, skip)
        };
        let tag = Tag { name: html[lt + 1..end].split(|c: char| c.is_whitespace() || c == '>' || c == '/').next().unwrap_or(""), text: &html[lt..end], start: lt, end };
        pos = skip_to;
        Some(tag)
    })
}

/// Rewrite the root composition's timing/size attributes, and stretch every direct child
/// whose end coincided with the old root default (i.e. was meant to last "until the end")
/// to the new end. Children that end earlier keep their timing; nested compositions and
/// everything inside them are untouched.
pub fn stamp_root(html: &str, duration: f64, width: u32, height: u32) -> String {
    let Some(root) = tags(html).find(|t| !t.name.starts_with('!') && attr_value_span(t.text, "data-composition-id").is_some()) else {
        return html.to_string();
    };
    let old_default = attr_num(root.text, "data-duration");
    let root_start = root.start;
    let is_composition = |text: &str| attr_value_span(text, "data-composition-id").is_some() || text.contains("data-composition-src");
    let mut out = String::with_capacity(html.len() + 64);
    let mut pos = 0;
    let mut nested = 0usize; // element depth inside a nested composition, which is opaque
    for t in tags(html) {
        out.push_str(&html[pos..t.start]);
        pos = t.end;
        let text = t.text;
        let is_close = text.starts_with("</");
        let is_void = text.ends_with("/>") || t.name.starts_with('!') || matches!(t.name.to_ascii_lowercase().as_str(), "br" | "img" | "input" | "meta" | "link" | "hr" | "source");
        let mut stamped = text.to_string();
        if nested > 0 {
            if is_close {
                nested -= 1;
            } else if !is_void {
                nested += 1;
            }
        } else if t.start == root_start {
            stamped = set_attr(text, "data-duration", &fmt_secs(duration));
            stamped = set_attr(&stamped, "data-width", &width.to_string());
            stamped = set_attr(&stamped, "data-height", &height.to_string());
        } else if !is_close && is_composition(text) {
            if !is_void {
                nested = 1;
            }
        } else if !is_close {
            if let (Some(d0), Some(child_dur)) = (old_default, attr_num(text, "data-duration")) {
                let start = attr_num(text, "data-start").unwrap_or(0.0);
                if (start + child_dur - d0).abs() < 1e-6 {
                    stamped = set_attr(text, "data-duration", &fmt_secs((duration - start).max(0.0)));
                }
            }
        }
        out.push_str(&stamped);
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

/// Replace (or append) an attribute value in an opening tag (with or without its `>`).
fn set_attr(tag: &str, name: &str, value: &str) -> String {
    match attr_value_span(tag, name) {
        Some((vs, ve)) => format!("{}{}{}", &tag[..vs], value, &tag[ve..]),
        None => {
            let close = if tag.ends_with("/>") { 2 } else if tag.ends_with('>') { 1 } else { 0 };
            let body = tag[..tag.len() - close].trim_end();
            format!("{body} {name}=\"{value}\"{}", &tag[tag.len() - close..])
        }
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
        vars.insert("at".into(), json!({ "x": 0.5, "y": 0.25 }));
        let c = Contract { vars: &vars, fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 2.0, speed: 1.0, bake: true, clip: None, head_extra: "<base href=\"/x/\">\n" };
        let html = c.apply("<html><head><title>x</title></head><body><div data-composition-id=\"main\" data-duration=\"4\"></div></body></html>");
        assert!(html.find("<base").unwrap() < html.find("kuma-inject").unwrap());
        assert!(html.find("kuma-inject").unwrap() < html.find("<title>").unwrap());
        // The vars style comes after the overlay's own <head> content so it wins the cascade.
        assert!(html.find("<title>").unwrap() < html.find("kuma-vars").unwrap());
        assert!(html.find("kuma-vars").unwrap() < html.find("</head>").unwrap());
        assert!(html.contains("window.kuma = {"));
        assert!(html.contains("--var-label: hi;"));
        assert!(html.contains("--var-at-x: 0.5; --var-at-y: 0.25;"), "{html}");
        assert!(html.contains("data-composition-id=\"main\" data-duration=\"2\""), "{html}");
    }

    #[test]
    fn root_is_found_by_tag_not_by_text_and_nested_compositions_are_opaque() {
        let html = "<!doctype html><html><head><style>[data-composition-id]{x:1}</style><script>const S=\"data-composition-id\";</script></head><body><!-- <div data-composition-id=\"m\" data-duration=\"9\"> -->\
<div title=\"a < b\" data-composition-id=\"main\" data-duration=\"4\"><div class=\"clip\" data-start=\"0\" data-duration=\"4\"></div>\
<div data-composition-id=\"sub\" data-start=\"0\" data-duration=\"4\"><div data-start=\"0\" data-duration=\"4\"></div></div><div data-start=\"1\" data-duration=\"3\"></div></div></body></html>";
        let out = stamp_root(html, 8.0, 1080, 1920);
        assert!(out.contains("<style>[data-composition-id]{x:1}</style>"), "{out}");
        assert!(out.contains("<!-- <div data-composition-id=\"m\" data-duration=\"9\"> -->"), "{out}");
        assert!(out.contains("data-composition-id=\"main\" data-duration=\"8\" data-width=\"1080\" data-height=\"1920\""), "{out}");
        assert!(out.contains("<div class=\"clip\" data-start=\"0\" data-duration=\"8\">"), "{out}");
        assert!(out.contains("<div data-composition-id=\"sub\" data-start=\"0\" data-duration=\"4\"><div data-start=\"0\" data-duration=\"4\"></div></div>"), "nested untouched: {out}");
        assert!(out.contains("<div data-start=\"1\" data-duration=\"7\">"), "{out}");
    }

    #[test]
    fn injection_lands_inside_head_or_after_the_doctype() {
        let c = Contract { vars: &Map::new(), fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 2.0, speed: 1.0, bake: true, clip: None, head_extra: "" };
        let a = c.apply("<!DOCTYPE html><html lang=\"en\"><head lang=\"en\"><title>x</title></head><body></body></html>");
        assert!(a.find("<head lang=\"en\">").unwrap() < a.find("kuma-inject").unwrap() && a.find("kuma-inject").unwrap() < a.find("<title>").unwrap(), "{a}");
        let b = c.apply("<!DOCTYPE html><div data-composition-id=\"main\"></div>");
        assert!(b.starts_with("<!DOCTYPE html>"), "{b}");
        assert!(b.find("kuma-inject").unwrap() < b.find("data-composition-id").unwrap(), "{b}");
    }

    #[test]
    fn user_text_cannot_break_out_of_script_or_style() {
        let mut vars = Map::new();
        vars.insert("label".into(), json!("</script><script>alert(1)</script>"));
        vars.insert("sub".into(), json!("x; } body { display:none"));
        let c = Contract { vars: &vars, fps: Fps::new(30, 1), width: 1920, height: 1080, duration: 2.0, speed: 1.0, bake: false, clip: Some("c1"), head_extra: "" };
        let html = c.apply("<html><head></head><body><div data-composition-id=\"main\"></div></body></html>");
        assert!(!html.contains("</script><script>alert"), "{html}");
        assert!(html.contains("<\\/script>"), "{html}");
        let style = &html[html.find("<style id=\"kuma-vars\">").unwrap() + 22..];
        let style = &style[..style.find("</style>").unwrap()];
        assert!(!style.contains('<') && style.contains("--var-sub: x    body   display:none;"), "{style}");
    }
}
