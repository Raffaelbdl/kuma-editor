//! Checking an overlay against the contract, before a bake proves it the slow way.
//!
//! An overlay that breaks the contract fails far from its cause: a missing
//! `data-no-timeline` waits 45 seconds for a timeline that will never register, a
//! `Date.now()` renders a frozen or wrong frame with no error at all, and an undeclared
//! var reaches the document as `undefined`. All of it is visible in the markup, so it is
//! read here — where the answer costs a second and names the fix.
//!
//! Two sources, one report. hyperframes ships its own linter and knows its own runtime;
//! kuma knows its contract (`library/blueprints/CLAUDE.md`), which is stricter on the two
//! things that make a bake deterministic: no wall clock, and nothing fetched over the
//! network. What is checked is the **staged** overlay — the injection applied, `_vendor/`
//! in place — so a rule never fires on something the renderer would not have seen.

use std::path::Path;
use std::process::Command;

use kuma_core::manifest::OverlayManifest;
use serde::{Deserialize, Serialize};

use crate::bake::{BakeError, BakeRequest, Baker};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The bake fails, or the frames come out wrong. Fix before rendering.
    Error,
    /// Legal, and usually a mistake.
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub level: Level,
    /// A short stable slug, so a report can be diffed and a rule can be searched for.
    pub code: String,
    pub message: String,
    /// What to do about it, in one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Finding {
    fn error(code: &str, message: impl Into<String>, fix: &str) -> Self {
        Finding { level: Level::Error, code: code.into(), message: message.into(), fix: Some(fix.into()) }
    }
    fn warn(code: &str, message: impl Into<String>, fix: &str) -> Self {
        Finding { level: Level::Warning, code: code.into(), message: message.into(), fix: Some(fix.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintReport {
    /// The overlay folder's name.
    pub overlay: String,
    pub findings: Vec<Finding>,
    /// Why hyperframes' own linter did not run, when it did not. Its absence is not a
    /// pass and not a failure — the same stance the render tests take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_skipped: Option<String>,
}

impl LintReport {
    pub fn errors(&self) -> usize {
        self.findings.iter().filter(|f| f.level == Level::Error).count()
    }
    pub fn warnings(&self) -> usize {
        self.findings.iter().filter(|f| f.level == Level::Warning).count()
    }
    /// True when nothing would stop this overlay rendering correctly.
    pub fn ok(&self) -> bool {
        self.errors() == 0
    }
    /// One line per finding, for a terminal or a toast.
    pub fn lines(&self) -> Vec<String> {
        self.findings
            .iter()
            .map(|f| format!("{} {}: {}{}", if f.level == Level::Error { "✗" } else { "⚠" }, f.code, f.message, f.fix.as_deref().map(|x| format!(" — {x}")).unwrap_or_default()))
            .collect()
    }
}

/// Wall-clock and non-determinism: the contract forbids these outright, because the bake
/// renders frame by frame with no real time passing and the preview scrubs.
const FORBIDDEN: &[(&str, &str)] = &[
    ("setTimeout", "`setTimeout` drives state from the wall clock"),
    ("setInterval", "`setInterval` drives state from the wall clock"),
    ("requestAnimationFrame", "`requestAnimationFrame` drives state from the wall clock"),
    ("Date.now", "`Date.now()` reads the wall clock"),
    ("performance.now", "`performance.now()` reads the wall clock"),
    ("Math.random", "`Math.random()` is different on every frame and every bake"),
];

impl Baker {
    /// Lint an overlay: kuma's contract over the staged document, plus hyperframes' own
    /// linter when it is installed. `req` decides the size and length the document is
    /// staged at, exactly as a bake would.
    pub fn lint(&self, req: &BakeRequest) -> Result<LintReport, BakeError> {
        let work = tempfile::Builder::new().prefix("kuma-lint-").tempdir()?;
        let staged = self.stage(req, &work.path().join("src"))?;
        let html = std::fs::read_to_string(&staged.entry)?;
        let mut findings = contract_findings(&html, req.manifest, &staged.dir);
        // A family that is not installed renders in the overlay's fallback stack: legal,
        // and not what anyone picked.
        for (k, family) in self.fonts(req.manifest, &req.manifest.resolve_vars(req.vars)).missing {
            findings.push(Finding::warn(
                "font_missing",
                format!("`{k}` names the font `{family}`, which is not installed"),
                "add the font file to the project's `fonts/` (or a library's), or pick one that is installed",
            ));
        }

        let renderer_skipped = match hyperframes_lint(&self.hyperframes, &staged.dir) {
            Ok(mut fs) => {
                // hyperframes looks for a literal `window.__timelines[...]` assignment.
                // kuma overlays register through `timeline()` in `_vendor/kuma.js`, so
                // its miss is a false positive whenever our own check is satisfied — and
                // a report full of false positives is a report nobody reads.
                let registered = !findings.iter().any(|f| f.code == "no_timeline");
                // The same fault reported twice in different words reads as two faults.
                let wall_clock = findings.iter().any(|f| f.code == "wall_clock");
                fs.retain(|f| !(registered && f.code == "missing_timeline_registry") && !(wall_clock && f.code == "non_deterministic_code"));
                findings.append(&mut fs);
                None
            }
            Err(e) => Some(e),
        };
        findings.sort_by_key(|f| f.level == Level::Warning);
        let overlay = req.overlay_dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(LintReport { overlay, findings, renderer_skipped })
    }
}

/// kuma's own contract, read off the staged document.
fn contract_findings(html: &str, manifest: &OverlayManifest, dir: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let scripts = script_text(html);
    // The staged document carries kuma's own injection: `window.kuma = …` and a
    // `:root { --var-<name>: … }` for every var in the manifest. The var rules read the
    // overlay's markup, not that — or every declared var would look read. Its own
    // linked stylesheets are part of what it reads, though: `_vendor/kuma.css` is what
    // turns `size` into `--u`, so an overlay that only sizes in `--u` still uses `size`.
    // What `_vendor/` reads, though, is kuma's and not the overlay's: it counts toward a var
    // being used, never toward one being unknown (`kuma.css` reads `size` with a fallback,
    // and a title that has no `size` has nothing wrong with it).
    let mut own = without_injection(html);
    let mut vendor = String::new();
    for href in local_refs(html).into_iter().filter(|h| h.ends_with(".css")) {
        if let Ok(css) = std::fs::read_to_string(dir.join(&href)) {
            if href.starts_with("_vendor/") { vendor.push_str(&css) } else { own.push_str(&css) }
        }
    }

    if !html.contains("data-composition-id") {
        out.push(Finding::error(
            "no_composition_root",
            "no element carries `data-composition-id`",
            "give the root `data-composition-id=\"main\"` with `data-start`, `data-duration`, `data-width` and `data-height`",
        ));
    }
    // A timeline is *registered* either on `window.__timelines` or through the
    // `timeline()` that `kumaTemplate` hands out (which does that registration). A bare
    // `gsap.timeline()` is neither: the renderer waits 45 s for a registration that never
    // comes, which is the failure this rule exists to name.
    let has_timeline = registers_timeline(&scripts);
    if !has_timeline && !html.contains("data-no-timeline") {
        out.push(Finding::error(
            "no_timeline",
            "no GSAP timeline is registered and the root does not carry `data-no-timeline`",
            "add `data-no-timeline` to the composition root, or register one with `timeline()` from `_vendor/kuma.js`",
        ));
    }
    for (needle, why) in FORBIDDEN {
        if scripts.contains(needle) {
            out.push(Finding::error("wall_clock", format!("{why}; a bake renders frame by frame with no time passing"), "drive everything from the timeline, which the renderer seeks"));
        }
    }
    if html.contains("fonts.googleapis.com") || html.contains("fonts.gstatic.com") {
        out.push(Finding::error("remote_font", "a Google Fonts URL is referenced", "bundle the font in the overlay folder and reference it with a relative `@font-face` src"));
    }
    if html.contains("<video") && html.contains("autoplay") {
        out.push(Finding::error("autoplay_video", "an autoplaying `<video>` cannot be seeked deterministically", "drive the video from the timeline, or bake it as media and place it on a video track"));
    }

    // Vars: the manifest is what the inspector shows and what the injection resolves, so
    // a var used but not declared arrives as `undefined` and one declared but unused is a
    // control that does nothing.
    let used = used_vars(&own);
    for name in used.iter().filter(|n| !manifest.vars.contains_key(*n)) {
        out.push(Finding::error("unknown_var", format!("`{name}` is read but not declared in overlay.json"), "declare it under `vars` with a type and a default, or stop reading it"));
    }
    let used_by_vendor = used_vars(&vendor);
    for name in manifest.vars.keys() {
        if !used.contains(name) && !used_by_vendor.contains(name) {
            out.push(Finding::warn("unused_var", format!("`{name}` is declared in overlay.json but never read"), "read it as `vars.<name>` or `var(--var-<name>)`, or remove it from the manifest"));
        }
    }
    if has_timeline && !reads_duration(&scripts) {
        out.push(Finding::warn(
            "duration_not_read",
            "the animation never reads `duration`",
            "anchor the out-animation to the clip's real length (`duration` from `kumaTemplate`), or it plays at the overlay's default length whatever the clip is",
        ));
    }

    // Local assets, resolved the way the renderer resolves them: after staging, so a
    // `_vendor/` reference that only the shared library satisfies is not a miss.
    for href in local_refs(html) {
        if !dir.join(&href).is_file() {
            out.push(Finding::error("missing_asset", format!("`{href}` is referenced but not in the overlay folder"), "add the file, or fix the path (it resolves against the overlay folder)"));
        }
    }
    out
}

/// The overlay's own markup: the staged document with kuma's injected script and var
/// declarations cut out again.
fn without_injection(html: &str) -> String {
    let mut out = html.to_string();
    for (open, close) in [("<script id=\"kuma-inject\">", "</script>"), ("<style id=\"kuma-vars\">", "</style>")] {
        if let Some(a) = out.find(open) {
            if let Some(b) = out[a..].find(close) {
                out.replace_range(a..a + b + close.len(), "");
            }
        }
    }
    out
}

/// True when the scripts register a timeline the renderer can find: on
/// `window.__timelines` themselves, or through `kumaTemplate`'s `timeline()`, which does
/// it for them. `gsap.timeline(…)` alone registers nothing.
fn registers_timeline(scripts: &str) -> bool {
    let own = scripts.replace("gsap.timeline(", "");
    own.contains("__timelines") || (own.contains("kumaTemplate(") && own.contains("timeline("))
}

/// True when the scripts read the clip's real length: `duration` as a value from
/// `kumaTemplate` or `window.kuma`, not the `duration:` every tween carries.
fn reads_duration(scripts: &str) -> bool {
    // Drop the object-key spelling, then look for the word.
    let mut own = scripts.to_string();
    while let Some(i) = own.find("duration") {
        let after = own[i + 8..].trim_start();
        if after.starts_with(':') {
            own.replace_range(i..i + 8, " ");
        } else {
            return true;
        }
    }
    false
}

/// Everything inside `<script>` tags, comments removed — where the rules about behaviour
/// apply. kuma's own injected script is skipped: it is not the overlay's to answer for.
///
/// Comments come out because an overlay that *warns* about `Date.now()` is not using it,
/// and a linter that cannot tell the two apart is one people learn to ignore. The
/// scaffold this crate writes says exactly that in a comment, and used to fail its own
/// check.
fn script_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else { break };
        let Some(close) = after.find("</script") else { break };
        let tag = &after[..open_end];
        let body = &after[open_end + 1..close];
        if !tag.contains("kuma-inject") {
            out.push_str(&strip_comments(body));
            out.push('\n');
        }
        rest = &after[close + 8..];
    }
    out
}

/// Drop `//` and `/* */` comments, leaving string literals alone so a `https://` inside
/// one is not read as the start of a comment. Regex literals are not tracked: a `//`
/// inside one would take the rest of its line, which costs a missed check, never a false
/// one.
fn strip_comments(js: &str) -> String {
    let b: Vec<char> = js.chars().collect();
    let mut out = String::with_capacity(js.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '"' | '\'' | '`' => {
                // A string literal, copied through with its escapes.
                let quote = c;
                out.push(c);
                i += 1;
                while i < b.len() {
                    out.push(b[i]);
                    if b[i] == '\\' {
                        if i + 1 < b.len() {
                            out.push(b[i + 1]);
                        }
                        i += 2;
                        continue;
                    }
                    if b[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '/' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                // Keep the newline count roughly honest for anything reading lines later.
                out.push(' ');
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Var names the document reads, either as `vars.<name>` in script or `--var-<name>` in
/// CSS. Both are how a value reaches an overlay, so both count as using it.
fn used_vars(html: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let ident = |s: &str| -> String { s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect() };
    for (marker, dashes) in [("vars.", false), ("--var-", true)] {
        let mut rest = html;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let name = if dashes {
                // `--var-anchor-x` is the `point` var `anchor`; the axis is the injection's.
                let raw: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
                raw.strip_suffix("-x").or_else(|| raw.strip_suffix("-y")).unwrap_or(&raw).to_string()
            } else {
                ident(rest)
            };
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
    out
}

/// Local files the document references: `src="…"` and `href="…"` that are not URLs.
fn local_refs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in ["src=\"", "href=\""] {
        let mut rest = html;
        while let Some(i) = rest.find(attr) {
            rest = &rest[i + attr.len()..];
            let Some(end) = rest.find('"') else { break };
            let v = &rest[..end];
            // Anything with a scheme (`https:`, `data:`, `mailto:`) is not a file here.
            let has_scheme = v.split('/').next().is_some_and(|head| head.contains(':'));
            let skip = v.is_empty() || has_scheme || v.starts_with("//") || v.starts_with('#') || v.starts_with('/');
            if !skip {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// hyperframes' own linter over the staged folder. `Err` is why it could not run.
fn hyperframes_lint(bin: &Path, dir: &Path) -> Result<Vec<Finding>, String> {
    let out = Command::new(bin).arg("lint").arg(dir).output().map_err(|e| format!("{}: {e}", bin.display()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let findings = parse_hyperframes(&text);
    // It exits non-zero when it finds errors, which is fine; non-zero with nothing found
    // is a linter that did not run (no such subcommand, a crash), and that must land in
    // `renderer_skipped` rather than read as a pass.
    if !out.status.success() && findings.is_empty() {
        return Err(format!("hyperframes lint failed: {}", text.trim()));
    }
    Ok(findings)
}

/// Its output is a human report: `✗ code: message` / `⚠ code: message`, each optionally
/// followed by an indented `Fix: …`. Parsed rather than asked for as JSON because the
/// command has no JSON mode.
fn parse_hyperframes(text: &str) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let (level, rest) = if let Some(r) = t.strip_prefix('✗') {
            (Level::Error, r)
        } else if let Some(r) = t.strip_prefix('⚠') {
            (Level::Warning, r)
        } else {
            if let Some(fix) = t.strip_prefix("Fix:") {
                if let Some(last) = out.last_mut() {
                    last.fix = Some(fix.trim().to_string());
                }
            }
            continue;
        };
        let rest = rest.trim();
        let (code, message) = rest.split_once(':').unwrap_or(("hyperframes", rest));
        out.push(Finding { level, code: code.trim().to_string(), message: message.trim().to_string(), fix: None });
    }
    // The summary line is the one thing least likely to change shape. If it counts errors
    // that the lines above did not yield — the glyphs changed, say — the report must not
    // read as clean: say so once, with the raw text, rather than pass by accident.
    if out.is_empty() {
        if let Some(n) = text.lines().find_map(|l| l.trim().split_once(" error(s)")).and_then(|(head, _)| head.rsplit(' ').next()?.parse::<usize>().ok()) {
            if n > 0 {
                out.push(Finding { level: Level::Error, code: "hyperframes".into(), message: format!("hyperframes reported {n} error(s) in a form this version does not parse:\n{}", text.trim()), fix: None });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(vars: &[&str]) -> OverlayManifest {
        let vars: serde_json::Map<String, serde_json::Value> =
            vars.iter().map(|v| ((*v).to_string(), serde_json::json!({ "type": "string", "default": "x" }))).collect();
        serde_json::from_value(serde_json::json!({ "vars": vars })).unwrap()
    }

    /// The overlay that follows the contract: nothing to report.
    #[test]
    fn a_contract_abiding_overlay_is_clean() {
        let html = r##"<html><head></head><body>
          <script src="_vendor/kuma.js"></script>
          <div id="root" data-composition-id="main" data-start="0" data-duration="4"><span id="label"></span></div>
          <script>const { vars, duration, text, timeline } = kumaTemplate({ label: "x" });
          text("label", vars.label);
          const tl = timeline(); tl.to("#label", { opacity: 1 }, duration - 0.45);</script>
        </body></html>"##;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("_vendor")).unwrap();
        std::fs::write(dir.path().join("_vendor/kuma.js"), "").unwrap();
        let out = contract_findings(html, &manifest(&["label"]), dir.path());
        assert!(out.is_empty(), "{out:?}");
    }

    /// Each rule fires on its own fault, so a report names one thing to fix.
    #[test]
    fn each_rule_catches_its_own_fault() {
        let dir = tempfile::tempdir().unwrap();
        let codes = |html: &str, m: &OverlayManifest| -> Vec<String> { contract_findings(html, m, dir.path()).into_iter().map(|f| f.code).collect() };

        let no_root = "<div><script>const tl = timeline();</script></div>";
        assert!(codes(no_root, &manifest(&[])).contains(&"no_composition_root".to_string()));

        // A static overlay says so; one that neither animates nor says so is the 45-second
        // bake failure this whole file exists to prevent.
        let silent = r#"<div data-composition-id="main"></div>"#;
        assert!(codes(silent, &manifest(&[])).contains(&"no_timeline".to_string()));
        let declared = r#"<div data-composition-id="main" data-no-timeline></div>"#;
        assert!(!codes(declared, &manifest(&[])).contains(&"no_timeline".to_string()));

        let clock = r#"<div data-composition-id="main" data-no-timeline></div><script>setTimeout(f, 10)</script>"#;
        assert!(codes(clock, &manifest(&[])).contains(&"wall_clock".to_string()));
        // The same call in a comment-free string is still a call as far as this is
        // concerned; what matters is that nothing in the overlay's own scripts reads a
        // clock. kuma's injected script is not the overlay's to answer for.
        let injected = r#"<div data-composition-id="main" data-no-timeline></div><script id="kuma-inject">Date.now()</script>"#;
        assert!(!codes(injected, &manifest(&[])).contains(&"wall_clock".to_string()));

        let remote = r#"<link href="https://fonts.googleapis.com/css2?family=X"><div data-composition-id="main" data-no-timeline></div>"#;
        assert!(codes(remote, &manifest(&[])).contains(&"remote_font".to_string()));

        let unknown = r#"<div data-composition-id="main" data-no-timeline></div><script>vars.nope</script>"#;
        assert!(codes(unknown, &manifest(&[])).contains(&"unknown_var".to_string()));
        assert!(codes(unknown, &manifest(&["label"])).contains(&"unused_var".to_string()));

        let missing = r#"<div data-composition-id="main" data-no-timeline></div><img src="logo.png">"#;
        assert!(codes(missing, &manifest(&[])).contains(&"missing_asset".to_string()));
    }

    /// `gsap.timeline()` on its own registers nothing; the 45-second bake timeout is exactly
    /// this overlay. And `duration: 0.45` on a tween is not a read of the clip's length.
    #[test]
    fn a_bare_gsap_timeline_and_a_tween_duration_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let bare = r##"<div data-composition-id="main"></div><script>const tl = gsap.timeline({ paused: true }); tl.to("#x", { opacity: 0, duration: 0.3 }, 3.65);</script>"##;
        let codes: Vec<String> = contract_findings(bare, &manifest(&[]), dir.path()).into_iter().map(|f| f.code).collect();
        assert!(codes.contains(&"no_timeline".to_string()), "{codes:?}");

        let registered = r##"<div data-composition-id="main"></div><script>const { timeline } = kumaTemplate({}); const tl = timeline(); tl.to("#x", { opacity: 0, duration: 0.3 }, 3.65);</script>"##;
        let codes: Vec<String> = contract_findings(registered, &manifest(&[]), dir.path()).into_iter().map(|f| f.code).collect();
        assert!(!codes.contains(&"no_timeline".to_string()), "{codes:?}");
        assert!(codes.contains(&"duration_not_read".to_string()), "a hard-coded exit passed: {codes:?}");

        let reads = r##"<div data-composition-id="main"></div><script>const { duration, timeline } = kumaTemplate({}); const tl = timeline(); tl.to("#x", { opacity: 0, duration: 0.3 }, duration - 0.3);</script>"##;
        let codes: Vec<String> = contract_findings(reads, &manifest(&[]), dir.path()).into_iter().map(|f| f.code).collect();
        assert!(!codes.contains(&"duration_not_read".to_string()), "{codes:?}");
    }

    /// The staged document declares every var in `:root`; that is kuma's doing, not the
    /// overlay's, and must not make an unused var look used.
    #[test]
    fn injected_declarations_do_not_count_as_reads() {
        let dir = tempfile::tempdir().unwrap();
        let staged = r#"<html><head><script id="kuma-inject">window.kuma = { vars: { size: 1 } };</script>
          <style id="kuma-vars">:root { --var-size: 1; }</style></head>
          <body><div data-composition-id="main" data-no-timeline></div></body></html>"#;
        let codes: Vec<String> = contract_findings(staged, &manifest(&["size"]), dir.path()).into_iter().map(|f| f.code).collect();
        assert!(codes.contains(&"unused_var".to_string()), "{codes:?}");
    }

    /// A comment is not code. The scaffold warns about `Date.now()` in a comment, and a
    /// linter that flagged that would be one nobody trusts.
    #[test]
    fn a_comment_is_not_a_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let html = r#"<div data-composition-id="main" data-no-timeline></div>
          <script>// never use Date.now() or setTimeout here
          /* nor Math.random() */
          const url = "https://example.com/x"; // a protocol is not a comment
          </script>"#;
        let found = contract_findings(html, &manifest(&[]), dir.path());
        assert!(found.is_empty(), "{found:?}");
        // …and the real thing still is.
        let real = r#"<div data-composition-id="main" data-no-timeline></div><script>const t = Date.now();</script>"#;
        assert!(contract_findings(real, &manifest(&[]), dir.path()).iter().any(|f| f.code == "wall_clock"));
    }

    /// A `point` var reaches CSS as two properties; both name the one var.
    #[test]
    fn a_point_var_counts_as_used() {
        let html = r#"<style>.p { left: calc(var(--var-anchor-x) * 100%); top: calc(var(--var-anchor-y) * 100%); }</style>
          <div data-composition-id="main" data-no-timeline></div>"#;
        let dir = tempfile::tempdir().unwrap();
        let codes: Vec<String> = contract_findings(html, &manifest(&["anchor"]), dir.path()).into_iter().map(|f| f.code).collect();
        assert!(!codes.contains(&"unused_var".to_string()), "{codes:?}");
    }

    /// A format change must not turn into a clean report.
    #[test]
    fn an_unparsed_hyperframes_error_still_counts() {
        let f = parse_hyperframes("Linting x\n  !! something new: bad\n  2 error(s), 0 warning(s)\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Error);
        assert!(f[0].message.contains("2 error(s)"));
        assert!(parse_hyperframes("  0 errors, 0 warnings\n").is_empty());
    }

    #[test]
    fn hyperframes_output_is_read_as_findings() {
        let text = "◆  Linting src/index.html\n\n  ✗ missing_timeline_registry: Missing `window.__timelines` registration.\n    Fix: Register each composition timeline.\n  ⚠ gsap_infinite_repeat: GSAP tween uses `repeat: -1`.\n\n◇  1 error(s), 1 warning(s)\n";
        let f = parse_hyperframes(text);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].code, "missing_timeline_registry");
        assert_eq!(f[0].level, Level::Error);
        assert_eq!(f[0].fix.as_deref(), Some("Register each composition timeline."));
        assert_eq!(f[1].level, Level::Warning);
    }
}
