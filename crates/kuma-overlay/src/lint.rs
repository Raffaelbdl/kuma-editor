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
use serde_json::{Map, Value};

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
    /// The var it is about, when it is about one: where a project check points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
}

impl Finding {
    fn error(code: &str, message: impl Into<String>, fix: &str) -> Self {
        Finding { level: Level::Error, code: code.into(), message: message.into(), fix: Some(fix.into()), var: None }
    }
    fn warn(code: &str, message: impl Into<String>, fix: &str) -> Self {
        Finding { level: Level::Warning, code: code.into(), message: message.into(), fix: Some(fix.into()), var: None }
    }
    fn about(self, var: &str) -> Self {
        Finding { var: Some(var.into()), ..self }
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
const WALL_CLOCK: &str = "drive everything from the timeline, which the renderer seeks";
const ROUGH: &str = "Rough.js used directly draws differently on every frame and every bake";
const ROUGH_FIX: &str = "draw inside `kumaCanvas` (`_vendor/kuma-canvas.js`) with `g.rough(key)`, which seeds every stroke by its key and the frame";

/// Calls that still work but that newer helpers replace: (needle, code, why, fix).
const SUPERSEDED: &[(&str, &str, &str, &str)] = &[
    ("kumaCanvas.hash(", "correlated_random", "`kumaCanvas.hash` gives related values for keys that differ only at the end, so things placed with it fall into lines", "use `kumaCanvas.rand(key)`, which draws the same on every frame and bake"),
    ("g.rand(", "correlated_random", "`g.rand` gives related values for keys that differ only at the end, so wobble moves in step", "use `g.jitter(key)`, which holds for one drawing the same way"),
];

/// Calls an overlay's own scripts may not make: (needle, code, why, fix).
const FORBIDDEN: &[(&str, &str, &str, &str)] = &[
    ("setTimeout", "wall_clock", "`setTimeout` drives state from the wall clock", WALL_CLOCK),
    ("setInterval", "wall_clock", "`setInterval` drives state from the wall clock", WALL_CLOCK),
    ("requestAnimationFrame", "wall_clock", "`requestAnimationFrame` drives state from the wall clock", WALL_CLOCK),
    ("Date.now", "wall_clock", "`Date.now()` reads the wall clock", WALL_CLOCK),
    ("performance.now", "wall_clock", "`performance.now()` reads the wall clock", WALL_CLOCK),
    ("Math.random", "wall_clock", "`Math.random()` is different on every frame and every bake", WALL_CLOCK),
    ("rough.canvas(", "unseeded_rough", ROUGH, ROUGH_FIX),
    ("rough.svg(", "unseeded_rough", ROUGH, ROUGH_FIX),
    ("rough.generator(", "unseeded_rough", ROUGH, ROUGH_FIX),
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
        findings.extend(self.vendor_findings(req.overlay_dir, &html));
        findings.extend(self.font_findings(req.manifest, &req.manifest.resolve_vars(req.vars), &html, req.overlay_dir));

        let renderer_skipped = match hyperframes_lint(&self.hyperframes, &staged.dir) {
            Ok(mut fs) => {
                // hyperframes looks for a literal `window.__timelines[...]` assignment.
                // kuma overlays register through `timeline()` in `_vendor/kuma.js`, so
                // its miss is a false positive whenever our own check is satisfied — and
                // a report full of false positives is a report nobody reads.
                let registered = !findings.iter().any(|f| f.code == "no_timeline");
                // The same fault reported twice in different words reads as two faults.
                let wall_clock = findings.iter().any(|f| f.code == "wall_clock");
                fs.retain(|f| !(registered && f.code == "missing_timeline_registry") && !(wall_clock && f.code == "non_deterministic_code") && !not_kumas(&f.code));
                findings.append(&mut fs);
                None
            }
            Err(e) => Some(e),
        };
        findings.sort_by_key(|f| f.level == Level::Warning);
        let overlay = req.overlay_dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(LintReport { overlay, findings, renderer_skipped })
    }

    /// `_vendor/` files the overlay references that are not a library's, where a library
    /// has one of that name with other bytes: a copy of kuma's runtime beside the overlays
    /// shadows the shipped one for every overlay there, and does not get its fixes. A file
    /// of the project's own under a name of its own is what that folder is for.
    pub fn vendor_findings(&self, overlay_dir: &Path, html: &str) -> Vec<Finding> {
        let mut out = Vec::new();
        for href in local_refs(html).into_iter().collect::<std::collections::BTreeSet<_>>() {
            let Some(rel) = href.strip_prefix("_vendor/") else { continue };
            let Some(used) = self.vendor_file(overlay_dir, rel) else { continue };
            // What the libraries alone would give, which a library's own blueprint uses.
            let Some(shipped) = self.vendor_dirs.iter().map(|d| d.join(rel)).find(|p| p.is_file()) else { continue };
            if self.vendor_dirs.iter().any(|d| used.starts_with(d)) || std::fs::read(&used).ok() == std::fs::read(&shipped).ok() {
                continue;
            }
            out.push(Finding::warn(
                "vendor_shadowed",
                format!("`{href}` is `{}`, not the one kuma ships, which differs", used.display()),
                "delete the copy to use kuma's, or give a file of your own a name of its own",
            ));
        }
        out
    }

    /// The faces an overlay brings itself: each `@font-face` rule in its own markup and in
    /// the stylesheets it links (its own or `_vendor/`'s, resolved as the bake stages them),
    /// as the name the rule gives it and the face its first source is, read like any font.
    /// `None` when a rule's first source cannot be read — not there, not a font, a `data:`
    /// or remote URL, a `local()` system font — since what that face draws is unknown, and
    /// text it may well set must not be reported as text nothing can.
    pub fn bundled_faces(&self, html: &str, dir: &Path) -> Option<Vec<(String, crate::fonts::Face)>> {
        let file = |rel: &str| self.overlay_file(dir, rel);
        // kuma's own rules (the `font` vars' faces) are not the overlay's.
        let mut sheets = vec![(String::new(), without_injection(html))];
        for href in linked_stylesheets(html) {
            let Some(css) = relative_path("", &href).and_then(|rel| std::fs::read_to_string(file(&rel)?).ok()) else { continue };
            let base = href.rsplit_once('/').map(|(b, _)| format!("{b}/")).unwrap_or_default();
            sheets.push((base, css));
        }
        let mut out = Vec::new();
        for (base, css) in &sheets {
            for (family, src) in font_face_rules(css) {
                let rel = src.and_then(|s| relative_path(base, &s))?;
                let (read_as, face) = self.fonts.face(&file(&rel)?)?;
                out.push((family.unwrap_or(read_as), face));
            }
        }
        Some(out)
    }

    /// What an overlay's text will not be drawn in, with these (resolved) vars and its
    /// markup (as written, or staged): a family a `font` var names that is not installed,
    /// characters none of its fonts can set — its `font` vars' families and the faces its
    /// own `@font-face` rules bring from `dir`, its folder — and CJK text on a page whose
    /// `lang` is not a CJK one.
    /// Each is legal and renders, in whatever the baking machine falls back to — a
    /// different picture on another machine, and not what anyone picked. Part of `lint`,
    /// and what the daemon checks every overlay entry of a project for.
    pub fn font_findings(&self, manifest: &OverlayManifest, vars: &Map<String, Value>, html: &str, dir: &Path) -> Vec<Finding> {
        self.font_findings_in(manifest, vars, html, dir, &Default::default())
    }

    /// [`Self::font_findings`], the faces `dir` bundles read into `bundled` on first need:
    /// one cell per folder serves every entry using it.
    pub fn font_findings_in(&self, manifest: &OverlayManifest, vars: &Map<String, Value>, html: &str, dir: &Path, bundled: &BundledFaces) -> Vec<Finding> {
        let mut out = Vec::new();
        let fonts = self.fonts(manifest, vars);
        // A collection in a font folder is not read; saying so is the difference between
        // "not installed" and "installed the wrong way".
        let (collections, web) = if fonts.missing.is_empty() { Default::default() } else { (self.fonts.collections(), self.fonts.web_fonts()) };
        for (k, family) in &fonts.missing {
            let mut fix = String::from("add the font file to the project's `fonts/` (or a library's), or pick one that is installed");
            if let Some(c) = collections.first() {
                fix.push_str(&format!("; `{}` is a collection, which is only read once `add_font` has split it into its faces", c.display()));
            }
            if let Some(w) = web.first() {
                fix.push_str(&format!("; `{}` is a web font, which is only read once `add_font` has unwrapped it into the font it holds", w.display()));
            }
            out.push(Finding::warn("font_missing", format!("`{k}` names the font `{family}`, which is not installed"), &fix).about(k));
        }

        let text = shown_text(manifest, vars);
        // Which font draws which var is the markup's business; any family a `font` var
        // names may be the one, and so may any face the overlay bundles, so a character is
        // covered when one of them has it. With no font at all to go on (none declared,
        // none installed, none bundled) there is nothing to check against: the overlay
        // leaves its text to a system stack, or `font_missing` said why not. Nor when a
        // bundled face cannot be read, since it may be the one that sets the text.
        let bundled = if text.is_empty() { None } else { bundled.get_or_init(|| self.bundled_faces(html, dir)).as_ref() };
        if let Some(bundled) = bundled.filter(|b| !fonts.families.is_empty() || !b.is_empty()) {
            let mut names: Vec<&str> = fonts.families.iter().map(|f| f.name.as_str()).collect();
            for (name, _) in bundled {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
            let covers = |c: char| fonts.families.iter().any(|f| f.covers(c)) || bundled.iter().any(|(_, f)| f.coverage.contains(c));
            for (var, s) in &text {
                let lacking: std::collections::BTreeSet<char> = s.chars().filter(|&c| !c.is_whitespace() && !c.is_control() && !covers(c)).collect();
                if lacking.is_empty() {
                    continue;
                }
                let listed: String = lacking.iter().take(40).collect();
                let more = if lacking.len() > 40 { format!(" and {} more", lacking.len() - 40) } else { String::new() };
                out.push(
                    Finding::warn(
                        "glyphs_missing",
                        format!("`{var}` shows {listed}{more}, which none of its fonts ({}) can set: the baking machine's fallback font draws them", names.join(", ")),
                        "pick a family that covers the text (`list_fonts` gives each one's `scripts`), or add one with `add_font` and name it in a `font` var the overlay falls back to",
                    )
                    .about(var),
                );
            }
        }

        // Han characters are drawn in the regional forms the page's language picks, and a
        // fallback is chosen by it too: under `lang="en"` a Linux machine draws kanji from its
        // Simplified Chinese face. A `lang` var's value is what the contract writes there, so
        // it is the page's language whether `html` is the staged page or the one written.
        let lang = manifest.lang(vars).map(str::to_string).or_else(|| page_lang(html));
        let wanted = |s: &str| -> Option<&[&str]> {
            if s.chars().any(is_kana) {
                Some(&["ja"])
            } else if s.chars().any(is_hangul) {
                Some(&["ko"])
            } else if s.chars().any(is_han) {
                Some(&["ja", "zh"])
            } else {
                None
            }
        };
        let cjk_page = lang.as_deref().and_then(|l| l.split('-').next()).is_some_and(|p| ["ja", "zh", "ko"].iter().any(|c| p.eq_ignore_ascii_case(c)));
        if let Some((var, want)) = text.iter().find_map(|(var, s)| wanted(s).map(|w| (var, w))).filter(|_| !cjk_page) {
            let is = lang.map(|l| format!("`lang=\"{l}\"`")).unwrap_or_else(|| "no `lang`".into());
            let either = |f: &dyn Fn(&str) -> String| want.iter().map(|w| f(w)).collect::<Vec<_>>().join(" or ");
            // An overlay with a `lang` var says it per entry; one without says it in its markup.
            let fix = match manifest.lang_var() {
                Some(k) => format!("set `{k}` to {}, whichever is the language of its text", either(&|w| format!("`{w}`"))),
                None => format!(
                    "put {} on the overlay's `<html>`, whichever is the language of its text, or declare a `lang` var so each entry can say its own",
                    either(&|w| format!("`lang=\"{w}\"`"))
                ),
            };
            out.push(
                Finding::warn(
                    "cjk_lang",
                    format!("`{var}` shows CJK text and the page has {is}: which glyph forms are drawn, and which font falls back, follow the page's language"),
                    &fix,
                )
                .about(var),
            );
        }
        out
    }
}

/// The faces an overlay folder bundles ([`Baker::bundled_faces`]), once read.
pub type BundledFaces = std::cell::OnceCell<Option<Vec<(String, crate::fonts::Face)>>>;

/// What an overlay shows as text, by var: `string` and `text` vars, and the lines of a
/// `cues` list. A `string` can hold a CSS value too; any font someone picks sets those.
fn shown_text(manifest: &OverlayManifest, vars: &Map<String, Value>) -> Vec<(String, String)> {
    use kuma_core::VarType;
    manifest
        .vars
        .iter()
        .filter_map(|(k, spec)| {
            let v = vars.get(k)?;
            let s = match spec.ty {
                VarType::String | VarType::Text => v.as_str()?.to_string(),
                VarType::Cues => v.as_array()?.iter().filter_map(|c| c.get("text")?.as_str()).collect::<Vec<_>>().join("\n"),
                _ => return None,
            };
            (!s.trim().is_empty()).then(|| (k.clone(), s))
        })
        .collect()
}

fn is_kana(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F)
}

fn is_hangul(c: char) -> bool {
    matches!(c as u32, 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F)
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// The `lang` of the document's `<html>`, if it has one.
fn page_lang(html: &str) -> Option<String> {
    let tag = crate::inject::tags(html).find(|t| t.name.eq_ignore_ascii_case("html"))?;
    let (a, b) = crate::inject::attr_value_span(tag.text, "lang")?;
    // A bare value runs to the end of the tag.
    Some(tag.text[a..b].trim_end_matches(['>', '/']).trim().to_string()).filter(|v| !v.is_empty())
}

/// hyperframes rules that do not apply to an overlay kuma stages. `studio_*` are about
/// edit targets in hyperframes' Studio, which kuma never opens (its edit targets are vars).
/// `gsap_infinite_repeat` asks for a finite root `data-duration` to be kept, and the
/// contract always stamps one: the clip's length.
fn not_kumas(code: &str) -> bool {
    code.starts_with("studio_") || code == "gsap_infinite_repeat"
}

/// kuma's own contract, read off the staged document.
fn contract_findings(html: &str, manifest: &OverlayManifest, dir: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut scripts = script_text(html);
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
    for href in linked_stylesheets(html) {
        if let Ok(css) = std::fs::read_to_string(dir.join(&href)) {
            if href.starts_with("_vendor/") { vendor.push_str(&css) } else { own.push_str(&css) }
        }
    }
    // A script the overlay links to is as much its own as one written inline: an engine in
    // `engine.js` registers the timeline, reads the vars and may not read a clock either.
    // `_vendor/` is not, as above — which is where a library goes, the overlay's own
    // `_vendor/` included: one may call `Date.now` to no effect.
    for src in local_refs(html).into_iter().filter(|h| h.ends_with(".js")) {
        if let Ok(js) = std::fs::read_to_string(dir.join(&src)) {
            if src.starts_with("_vendor/") {
                vendor.push_str(&js);
            } else {
                let js = strip_comments(&js);
                own.push_str(&js);
                scripts.push_str(&js);
                scripts.push('\n');
            }
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
    for (needle, code, why, fix) in FORBIDDEN {
        if scripts.contains(needle) {
            out.push(Finding::error(code, format!("{why}; a bake renders frame by frame with no time passing"), fix));
        }
    }
    for (needle, code, why, fix) in SUPERSEDED {
        if scripts.contains(needle) {
            out.push(Finding::warn(code, *why, fix));
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
    // A `lang` var is read by the contract, which writes it onto `<html lang>`.
    for name in manifest.vars.keys().filter(|n| Some(n.as_str()) != manifest.lang_var()) {
        if !used.contains(name) && !used_by_vendor.contains(name) {
            out.push(Finding::warn("unused_var", format!("`{name}` is declared in overlay.json but never read"), "read it as `vars.<name>` or `var(--var-<name>)`, or remove it from the manifest"));
        }
    }
    // A static overlay is one frame, so it has no length to anchor to; one whose times are
    // its own on purpose (a texture that runs as long as its clip, a countdown that takes
    // as long as it counts) says so, since that cannot be told from a forgotten exit.
    if has_timeline && !manifest.is_static && !html.contains("data-fixed-timing") && !reads_duration(&scripts) {
        out.push(Finding::warn(
            "duration_not_read",
            "the animation never reads `duration`",
            "anchor the out-animation to the clip's real length (`duration` from `kumaTemplate`), or it plays at the overlay's default length whatever the clip is; if its times are its own on purpose, put `data-fixed-timing` on the composition root",
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
/// `window.__timelines` themselves, or through `kumaTemplate`'s `timeline()` — directly, or
/// by way of `onFrame`, `onFrames` or `kumaCanvas`, which call it. `gsap.timeline(…)` alone registers nothing.
fn registers_timeline(scripts: &str) -> bool {
    let own = scripts.replace("gsap.timeline(", "");
    own.contains("__timelines") || (own.contains("kumaTemplate(") && ["timeline(", "onFrame(", "onFrames(", "kumaCanvas("].iter().any(|c| own.contains(c)))
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
            if is_local_url(v) && !v.starts_with('#') {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// The stylesheets the document links to, as [`local_refs`].
fn linked_stylesheets(html: &str) -> impl Iterator<Item = String> {
    local_refs(html).into_iter().filter(|h| h.ends_with(".css"))
}

/// Whether a URL can name a file in the overlay folder: not empty, no scheme (`https:`,
/// `data:`, `mailto:`), not absolute.
fn is_local_url(url: &str) -> bool {
    let has_scheme = url.split('/').next().is_some_and(|head| head.contains(':'));
    !(url.is_empty() || has_scheme || url.starts_with('/'))
}

/// Each `@font-face` rule in a stylesheet: the family it declares, and the URL of its first
/// source — `None` when that is a `local()` font, or not one URL this can read.
fn font_face_rules(css: &str) -> Vec<(Option<String>, Option<String>)> {
    let mut out = Vec::new();
    let mut rest = css;
    while let Some(i) = rest.find("@font-face") {
        rest = &rest[i + "@font-face".len()..];
        let Some(open) = rest.find('{') else { break };
        let close = rest[open..].find('}').map_or(rest.len(), |j| open + j);
        let block = &rest[open + 1..close];
        rest = &rest[close..];
        // A `data:` URL has a `;` of its own: cut there, it has no `)` and reads as no URL.
        let decl = |name: &str| block.split(';').find_map(|d| d.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
        let family = decl("font-family").map(|f| f.trim_matches(['"', '\'']).trim().to_string()).filter(|f| !f.is_empty());
        let src = decl("src").and_then(|v| {
            let inner = v.strip_prefix("url(")?;
            Some(inner[..inner.find(')')?].trim().trim_matches(['"', '\'']).to_string())
        });
        out.push((family, src));
    }
    out
}

/// A URL a stylesheet at `base` (a folder path relative to the overlay, `""` or ending in
/// `/`) writes, as a path relative to the overlay folder; `None` for anything that is not a
/// file in it: a scheme (`data:`, `https:`), an absolute path, one that climbs out.
fn relative_path(base: &str, url: &str) -> Option<String> {
    let url = url.split(['?', '#']).next()?.trim();
    if !is_local_url(url) {
        return None;
    }
    let mut parts: Vec<String> = base.split('/').filter(|p| !p.is_empty()).map(String::from).collect();
    for p in percent_decoded(url).split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p.to_string()),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `%20` and the like, as the characters they are.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| s.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
        out.push(Finding { level, code: code.trim().to_string(), message: message.trim().to_string(), fix: None, var: None });
    }
    // The summary line is the one thing least likely to change shape. If it counts errors
    // that the lines above did not yield — the glyphs changed, say — the report must not
    // read as clean: say so once, with the raw text, rather than pass by accident.
    if out.is_empty() {
        if let Some(n) = text.lines().find_map(|l| l.trim().split_once(" error(s)")).and_then(|(head, _)| head.rsplit(' ').next()?.parse::<usize>().ok()) {
            if n > 0 {
                out.push(Finding { level: Level::Error, code: "hyperframes".into(), message: format!("hyperframes reported {n} error(s) in a form this version does not parse:\n{}", text.trim()), fix: None, var: None });
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

        let rough = r#"<div data-composition-id="main" data-no-timeline></div><script>rough.canvas(c).circle(0, 0, 9)</script>"#;
        assert!(codes(rough, &manifest(&[])).contains(&"unseeded_rough".to_string()));

        // The unmixed helpers still render, so a warning; their replacements pass.
        let related = |js: &str| codes(&format!(r#"<div data-composition-id="main" data-no-timeline></div><script>{js}</script>"#), &manifest(&[])).contains(&"correlated_random".to_string());
        assert!(related("kumaCanvas.hash(`star${i}`)"));
        assert!(related("kumaCanvas(tpl, c, (g) => g.rand(`k${i}`))"));
        assert!(!related("kumaCanvas.rand(`star${i}`); kumaCanvas(tpl, c, (g) => g.jitter(`k${i}`))"));

        let missing = r#"<div data-composition-id="main" data-no-timeline></div><img src="logo.png">"#;
        assert!(codes(missing, &manifest(&[])).contains(&"missing_asset".to_string()));
    }

    /// An overlay whose engine is a file beside it is read like one written inline: the
    /// engine registers the timeline and reads the vars, with no stub in the page to say
    /// so, and a clock read in it is found. A library in `_vendor/` is not the overlay's code.
    #[test]
    fn a_linked_script_is_read_like_an_inline_one() {
        let html = r##"<script src="_vendor/kuma.js"></script><script src="_vendor/lib.js"></script>
          <div data-composition-id="main" data-fixed-timing></div><script src="engine.js"></script>"##;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("_vendor")).unwrap();
        std::fs::write(dir.path().join("_vendor/kuma.js"), "").unwrap();
        std::fs::write(dir.path().join("_vendor/lib.js"), "var t=Date.now();vars.internal").unwrap();
        let codes = |engine: &str| -> Vec<String> {
            std::fs::write(dir.path().join("engine.js"), engine).unwrap();
            contract_findings(html, &manifest(&["spec"]), dir.path()).into_iter().map(|f| f.code).collect()
        };
        let engine = "const tpl = kumaTemplate({ spec: '' }); const words = tpl.vars.spec.split(/\\s+/); tpl.timeline();";
        assert_eq!(codes(&format!("{engine} // Date.now() would be wrong here")), Vec::<String>::new(), "a comment is not a call");
        assert_eq!(codes(&format!("{engine} const seed = Math.random();")), ["wall_clock"]);
        assert_eq!(codes("kumaTemplate({}).timeline();"), ["unused_var"]);
        assert_eq!(codes("const tpl = kumaTemplate({}); tpl.vars.spec;"), ["no_timeline"]);
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

    /// Not reading `duration` is right for an overlay that is one frame, and for one whose
    /// times are its own on purpose — a texture drawn for as long as its clip, a countdown.
    #[test]
    fn a_still_or_fixed_timing_overlay_need_not_read_duration() {
        let dir = tempfile::tempdir().unwrap();
        let texture = |root: &str| format!(r##"<div data-composition-id="main"{root}></div><script>const tpl = kumaTemplate({{}}); kumaCanvas(tpl, "#c", (g, t) => g.ctx.fillRect(0, 0, t, t));</script>"##);
        let warned = |html: &str, m: &OverlayManifest| contract_findings(html, m, dir.path()).into_iter().any(|f| f.code == "duration_not_read");
        assert!(warned(&texture(""), &manifest(&[])), "the rule itself went quiet");
        assert!(!warned(&texture(" data-fixed-timing"), &manifest(&[])));
        let mut still = manifest(&[]);
        still.is_static = true;
        assert!(!warned(&texture(""), &still));
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

    /// A baker that reads the starter fonts, and a manifest of a font var and two text vars.
    fn with_fonts() -> (Baker, OverlayManifest) {
        let tmp = std::env::temp_dir();
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/fonts");
        let baker = Baker::discover(&tmp, vec![tmp.join("no-vendor")], tmp.join("no-cache")).with_fonts(vec![(fonts, crate::fonts::Origin::Library)]);
        let manifest = serde_json::from_value(serde_json::json!({ "vars": {
            "font": { "type": "font", "default": "Inter" },
            "label": { "type": "text", "default": "" },
            "accent": { "type": "color", "default": "#ffffff" },
            "cues": { "type": "cues", "default": [] }
        } }))
        .unwrap();
        (baker, manifest)
    }

    fn vars(v: serde_json::Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    /// An overlay folder that is not there: markup with nothing of its own to read.
    fn nowhere() -> std::path::PathBuf {
        std::env::temp_dir().join("kuma-lint-no-overlay")
    }

    /// A face the overlay bundles with `@font-face` sets text as surely as one a `font` var
    /// names, so it counts — inline or in a linked stylesheet, `_vendor/` beside the
    /// overlays included, WOFF2 too. One whose source cannot be read may be the face that
    /// sets the text, so then nothing is claimed.
    #[test]
    fn a_face_the_overlay_bundles_counts_toward_its_coverage() {
        let (baker, _) = with_fonts();
        let tmp = tempfile::tempdir().unwrap();
        let overlay = tmp.path().join("overlays/label");
        std::fs::create_dir_all(overlay.join("fonts")).unwrap();
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::copy(repo.join("../../library/fonts/Inter/Inter[opsz,wght].ttf"), overlay.join("fonts/house sans.ttf")).unwrap();
        let bebas: OverlayManifest = serde_json::from_value(serde_json::json!({ "vars": {
            "font": { "type": "font", "default": "Bebas Neue" }, "label": { "type": "text", "default": "" } } }))
        .unwrap();
        // Bebas Neue has no Cyrillic and no arrows; Inter has both.
        let label = vars(serde_json::json!({ "font": "Bebas Neue", "label": "Жук →" }));
        let page = |style: &str| format!("<html lang=\"ru\"><head><style>{style}</style></head><body></body></html>");
        let codes = |html: &str| baker.font_findings(&bebas, &label, html, &overlay).into_iter().map(|f| f.code).collect::<Vec<_>>();
        assert_eq!(codes(&page("")), ["glyphs_missing"]);
        assert!(codes(&page(r#"@font-face { font-family: "House"; src: url("fonts/house%20sans.ttf?v=2") format("truetype"); }"#)).is_empty());
        // Nothing to read, so nothing to say: a file that is not there, a system font.
        assert!(codes(&page(r#"@font-face { font-family: House; src: url(fonts/gone.woff2); }"#)).is_empty());
        assert!(codes(&page(r#"@font-face { font-family: House; src: local("House"), url(fonts/house%20sans.ttf); }"#)).is_empty());
        // kuma's own rules are not the overlay's: the injected `font` var faces are skipped.
        let staged = r#"<html><head><style id="kuma-vars">@font-face { font-family: "Inter"; src: url("_kuma/fonts/inter/0.ttf"); }:root { }</style></head></html>"#;
        assert_eq!(codes(staged), ["glyphs_missing"]);

        // No `font` var at all: the bundled face is what there is to go on. A shared stylesheet
        // beside the overlays, its URL relative to itself, the face a WOFF2.
        let beside = tmp.path().join("overlays/_vendor");
        std::fs::create_dir_all(beside.join("fonts")).unwrap();
        std::fs::copy(repo.join("tests/fixtures/fonts/BebasNeue-Regular.woff2"), beside.join("fonts/house.woff2")).unwrap();
        std::fs::write(beside.join("house.css"), "@font-face {\n  font-family: 'House Display';\n  src: url(fonts/house.woff2) format('woff2');\n}\n").unwrap();
        let bare: OverlayManifest = serde_json::from_value(serde_json::json!({ "vars": { "label": { "type": "text", "default": "" } } })).unwrap();
        let linked = r#"<html lang="en"><head><link rel="stylesheet" href="_vendor/house.css"></head></html>"#;
        let found = baker.font_findings(&bare, &vars(serde_json::json!({ "label": "HELLO Ж" })), linked, &overlay);
        assert_eq!(found.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(), ["glyphs_missing"], "{found:?}");
        assert!(found[0].message.contains("shows Ж,") && found[0].message.contains("(House Display)"), "{}", found[0].message);
        assert!(baker.font_findings(&bare, &vars(serde_json::json!({ "label": "HELLO" })), linked, &overlay).is_empty());
    }

    #[test]
    fn a_font_face_source_is_read_as_a_path_in_the_folder() {
        assert_eq!(relative_path("", "fonts/a%20b.woff2#iefix").as_deref(), Some("fonts/a b.woff2"));
        assert_eq!(relative_path("css/", "../fonts/./a.ttf").as_deref(), Some("fonts/a.ttf"));
        assert_eq!(relative_path("_vendor/", "a.woff2").as_deref(), Some("_vendor/a.woff2"));
        for outside in ["../a.ttf", "/fonts/a.ttf", "data:font/woff2;base64,AAAA", "https://example.com/a.woff2", ""] {
            assert_eq!(relative_path("", outside), None, "{outside}");
        }
        let rules = font_face_rules("p{} @font-face{font-family:\"A\";src:url('a.woff2')} @font-face { src: local(B) } @font-face { src: url(data:font/woff2;base64,AAAA) }");
        assert_eq!(rules, [(Some("A".to_string()), Some("a.woff2".to_string())), (None, None), (None, None)]);
    }

    /// Kanji in a Latin font's label are drawn by the machine's CJK fallback, in forms the
    /// page's language picks: both are said, with the characters, and nothing is said about
    /// text the font sets or vars that are not text.
    #[test]
    fn text_its_fonts_cannot_set_is_named() {
        let (baker, m) = with_fonts();
        let en = r#"<!doctype html><html lang="en"><body></body></html>"#;
        let found = baker.font_findings(&m, &vars(serde_json::json!({ "font": "Inter", "label": "承ける take it up", "accent": "#fff", "cues": [{ "start": 0, "end": 1, "text": "Café" }] })), en, &nowhere());
        let codes: Vec<&str> = found.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["glyphs_missing", "cjk_lang"], "{found:?}");
        assert!(found[0].message.contains("ける承") && found[0].message.contains("Inter"), "{}", found[0].message);
        assert_eq!(found[0].var.as_deref(), Some("label"));
        assert!(found[1].fix.as_deref().unwrap().contains(r#"lang="ja""#), "kana say Japanese: {:?}", found[1].fix);

        // Set in a font that has the characters, on a Japanese page: nothing to say.
        let ja = r#"<html lang="ja-JP"><body></body></html>"#;
        assert!(baker.font_findings(&m, &vars(serde_json::json!({ "font": "Inter", "label": "take it up" })), ja, &nowhere()).is_empty());
        let found = baker.font_findings(&m, &vars(serde_json::json!({ "font": "Inter", "label": "承" })), ja, &nowhere());
        assert_eq!(found.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(), ["glyphs_missing"], "{found:?}");
    }

    /// A `lang` var is the page's language, whether the page is the staged one carrying it
    /// or the one as written; the fix for CJK text on a page in another language is then to
    /// set the var, and the var is not unused for not being read by the markup.
    #[test]
    fn a_lang_var_decides_the_language_the_check_reads() {
        let (baker, _) = with_fonts();
        let m: OverlayManifest = serde_json::from_value(serde_json::json!({ "vars": {
            "font": { "type": "font", "default": "Inter" }, "label": { "type": "text", "default": "" }, "lang": { "type": "lang", "default": "" } } }))
        .unwrap();
        let en = r#"<html lang="en"><body></body></html>"#;
        let codes = |v: serde_json::Value| baker.font_findings(&m, &vars(v), en, &nowhere()).into_iter().map(|f| (f.code, f.fix.unwrap_or_default())).collect::<Vec<_>>();
        let found = codes(serde_json::json!({ "font": "Inter", "label": "ありがとう", "lang": "" }));
        assert_eq!(found.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["glyphs_missing", "cjk_lang"]);
        assert!(found[1].1.starts_with("set `lang` to `ja`"), "{}", found[1].1);
        let found = codes(serde_json::json!({ "font": "Inter", "label": "ありがとう", "lang": "ja" }));
        assert_eq!(found.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["glyphs_missing"]);

        let dir = tempfile::tempdir().unwrap();
        let html = r#"<div data-composition-id="main" data-no-timeline style="font-family: var(--var-font)"></div><script>vars.label</script>"#;
        assert!(contract_findings(html, &m, dir.path()).is_empty(), "{:?}", contract_findings(html, &m, dir.path()));
    }

    /// A family that is not installed is `font_missing`, and there is then nothing to check
    /// coverage against; an overlay with no `font` var names its own fonts.
    #[test]
    fn coverage_is_checked_only_against_fonts_that_are_there() {
        let (baker, m) = with_fonts();
        let page = "<html><body></body></html>";
        let found = baker.font_findings(&m, &vars(serde_json::json!({ "font": "Nope Sans", "label": "é" })), page, &nowhere());
        assert_eq!(found.iter().map(|f| (f.code.as_str(), f.var.as_deref())).collect::<Vec<_>>(), [("font_missing", Some("font"))], "{found:?}");
        let bare: OverlayManifest = serde_json::from_value(serde_json::json!({ "vars": { "label": { "type": "string", "default": "" } } })).unwrap();
        assert!(baker.font_findings(&bare, &vars(serde_json::json!({ "label": "ÿ ☃" })), page, &nowhere()).is_empty());
    }

    #[test]
    fn the_page_language_is_read_from_the_html_tag() {
        assert_eq!(page_lang(r#"<!doctype html><HTML class="x" lang='ja'><head lang="en">"#).as_deref(), Some("ja"));
        assert_eq!(page_lang(r#"<html xml:lang="ja"><body>"#), None);
        assert_eq!(page_lang("<htmlx lang=ja><html lang=ko>").as_deref(), Some("ko"));
        assert_eq!(page_lang("<div>no page</div>"), None);
    }

    /// A file of the project's own beside its overlays is what that folder is for; a copy of
    /// kuma's runtime there that is not the shipped one is said, since it shadows the shipped
    /// one for every overlay beside it. A copy identical to kuma's is nothing to say.
    #[test]
    fn a_copy_of_kumas_runtime_beside_the_overlays_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("library/_vendor");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("kuma.js"), "shipped").unwrap();
        let overlay = tmp.path().join("overlays/title");
        let beside = tmp.path().join("overlays/_vendor");
        std::fs::create_dir_all(&overlay).unwrap();
        std::fs::create_dir_all(&beside).unwrap();
        std::fs::write(beside.join("house.css"), "mine").unwrap();
        std::fs::write(beside.join("kuma.js"), "shipped").unwrap();
        let baker = Baker::discover(tmp.path(), vec![library], tmp.path().join("cache"));
        let html = r#"<link rel="stylesheet" href="_vendor/house.css"><script src="_vendor/kuma.js"></script><script src="_vendor/kuma.js"></script>"#;
        assert!(baker.vendor_findings(&overlay, html).is_empty());
        std::fs::write(beside.join("kuma.js"), "an older one").unwrap();
        let found = baker.vendor_findings(&overlay, html);
        assert_eq!(found.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(), ["vendor_shadowed"], "{found:?}");
        assert!(found[0].message.contains("overlays/_vendor/kuma.js"), "{}", found[0].message);
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
