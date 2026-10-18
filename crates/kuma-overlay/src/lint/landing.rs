//! Where a var's text lands, and the `lang` there: what `cjk_lang` reads (`lint.rs`).
//!
//! Chromium picks the regional forms of han characters, and the font a missing one falls
//! back to, by the nearest `lang` — a Japanese name in a French page is `<span lang="ja">`,
//! and the page stays French. Text reaches an overlay through vars at run time, so this
//! reads the overlay's own scripts for the contract's setter, `text("id", …vars.name…)` (as
//! `text`, `tpl.text`, or the name `text: setText` gives it), whose text lands in `#id`
//! under the nearest `lang` the markup gives it. Any other write of a var's text — a DOM
//! write (`textContent`, `innerHTML`, `append`…), a template literal — lands on the page, and
//! so does a var found landing nowhere; a read that writes nothing (`if (vars.name)`, a
//! length) is no landing. What it cannot place is judged by the page's `lang`, so the check
//! errs towards warning. The one way to silence it wrongly is a var set into a `lang="ja"`
//! element and also written by a route that is not a write here (through a local first).

use std::collections::HashMap;

use crate::inject::{attr, is_void, tags};

/// A place a var's text is written: an element (`at`, `#id`) or the page (`None`), with the
/// nearest `lang` it gives (`None`: the page's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Landing {
    pub lang: Option<String>,
    pub at: Option<String>,
}

const PAGE: Landing = Landing { lang: None, at: None };

/// An overlay's markup and its own scripts, read once for every var asked about.
pub(super) struct Page<'a> {
    js: Js<'a>,
    /// Each element with an id, by id, with the nearest `lang` the markup gives it.
    langs: HashMap<String, Option<String>>,
}

impl<'a> Page<'a> {
    /// `scripts` (comments removed) run against `html`.
    pub(super) fn read(html: &str, scripts: &'a str) -> Self {
        Page { js: Js::scan(scripts), langs: element_langs(html) }
    }

    /// Every place the scripts write the text of the var `var` into; the page when none.
    pub(super) fn landings(&self, var: &str) -> Vec<Landing> {
        let mut out = Vec::new();
        for p in self.js.reads_of(var) {
            let landing = match self.js.sink(p) {
                None => continue,
                Some(Sink::Element(id)) => Landing { lang: self.langs.get(&id).cloned().flatten(), at: Some(format!("#{id}")) },
                Some(Sink::Page) => PAGE,
            };
            if !out.contains(&landing) {
                out.push(landing);
            }
        }
        if out.is_empty() {
            out.push(PAGE);
        }
        out
    }
}

/// Where one read's text goes.
enum Sink {
    Element(String),
    Page,
}

/// What each byte of a script is: code, a quoted string, or inside a template literal
/// (its `${…}` included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    Str,
    Tmpl,
}

/// A script, comments already removed, with what each byte is.
struct Js<'a> {
    src: &'a str,
    kind: Vec<Kind>,
    /// The names `kumaTemplate`'s `text` goes by besides `text`.
    setters: Vec<String>,
}

impl<'a> Js<'a> {
    fn scan(src: &'a str) -> Self {
        // A stack, since a template's `${…}` is code that may open templates of its own.
        enum Frame {
            Code { depth: usize },
            Tmpl,
        }
        let b = src.as_bytes();
        let n = b.len();
        let mut kind = vec![Kind::Code; n];
        let mut stack = vec![Frame::Code { depth: 0 }];
        let mut i = 0;
        while i < n {
            let from = i;
            let in_template = stack.len() > 1;
            match stack.last_mut().expect("the bottom frame is never popped") {
                Frame::Code { depth } => match b[i] {
                    q @ (b'"' | b'\'') => {
                        i += 1;
                        while i < n && b[i] != q && b[i] != b'\n' {
                            i += if b[i] == b'\\' { 2 } else { 1 };
                        }
                        i += 1;
                    }
                    b'`' => {
                        stack.push(Frame::Tmpl);
                        i += 1;
                    }
                    b'{' => {
                        *depth += 1;
                        i += 1;
                    }
                    b'}' if *depth > 0 => {
                        *depth -= 1;
                        i += 1;
                    }
                    b'}' if in_template => {
                        stack.pop();
                        i += 1;
                    }
                    _ => i += 1,
                },
                Frame::Tmpl => match b[i] {
                    b'\\' => i += 2,
                    b'`' => {
                        stack.pop();
                        i += 1;
                    }
                    b'$' if b.get(i + 1) == Some(&b'{') => {
                        stack.push(Frame::Code { depth: 0 });
                        i += 2;
                    }
                    _ => i += 1,
                },
            }
            i = i.min(n);
            let what = if in_template || stack.len() > 1 {
                Kind::Tmpl
            } else if matches!(b[from], b'"' | b'\'') {
                Kind::Str
            } else {
                Kind::Code
            };
            kind[from..i].fill(what);
        }
        let mut js = Js { src, kind, setters: Vec::new() };
        js.setters = js.setter_aliases();
        js
    }

    fn code(&self, i: usize) -> bool {
        self.kind[i] == Kind::Code
    }

    /// Each read of `vars.<var>` (or `vars["<var>"]`) outside a quoted string, by where it starts.
    fn reads_of(&self, var: &str) -> Vec<usize> {
        let b = self.src.as_bytes();
        let named = |i: usize| {
            let rest = &self.src[i + 4..];
            if let Some(r) = rest.strip_prefix('.') {
                r.strip_prefix(var).is_some_and(|after| !after.bytes().next().is_some_and(is_ident))
            } else if let Some(r) = rest.strip_prefix('[') {
                let r = r.trim_start();
                [format!("\"{var}\""), format!("'{var}'")].iter().any(|q| r.starts_with(q.as_str()))
            } else {
                false
            }
        };
        self.src
            .match_indices("vars")
            .map(|(i, _)| i)
            .filter(|&i| self.kind[i] != Kind::Str && !(i > 0 && is_ident(b[i - 1])) && named(i))
            .collect()
    }

    /// `text: <name>` in a destructuring of `kumaTemplate`'s result.
    fn setter_aliases(&self) -> Vec<String> {
        let b = self.src.as_bytes();
        self.src
            .match_indices("text")
            .map(|(i, _)| i)
            .filter(|&i| self.code(i) && !(i > 0 && (is_ident(b[i - 1]) || b[i - 1] == b'.')))
            .filter_map(|i| {
                let rest = self.src[i + 4..].trim_start().strip_prefix(':')?.trim_start();
                let name: String = rest.bytes().take_while(|&c| is_ident(c)).map(char::from).collect();
                (!name.is_empty() && rest[name.len()..].trim_start().starts_with([',', '}'])).then_some(name)
            })
            .collect()
    }

    fn is_setter(&self, callee: &str) -> bool {
        callee == "text" || callee.ends_with(".text") || self.setters.iter().any(|s| s == callee)
    }

    /// True when the `=` at `i` assigns: not `==`, `=>`, `<=`, `>=`, `!=`.
    fn is_assignment(&self, i: usize) -> bool {
        let b = self.src.as_bytes();
        let next = b.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|k| b[k]);
        !matches!(next, Some(b'=' | b'>')) && !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'))
    }

    /// The expression that ends at `end`: names, dots and bracketed groups, read backwards.
    fn expr_before(&self, end: usize) -> &'a str {
        let b = self.src.as_bytes();
        let mut j = end;
        while j > 0 && b[j - 1].is_ascii_whitespace() {
            j -= 1;
        }
        let stop = j;
        let mut depth = 0usize;
        while j > 0 {
            let k = j - 1;
            if self.code(k) {
                match b[k] {
                    b')' | b']' => depth += 1,
                    b'(' | b'[' if depth > 0 => depth -= 1,
                    c if depth == 0 && !(is_ident(c) || c == b'.') => break,
                    _ => {}
                }
            } else if depth == 0 {
                break;
            }
            j = k;
        }
        &self.src[j..stop]
    }

    /// Where the text read at `p` is written, if it is written anywhere.
    fn sink(&self, p: usize) -> Option<Sink> {
        // In a template literal: written as markup or a string somewhere this does not follow.
        if self.kind[p] == Kind::Tmpl {
            return Some(Sink::Page);
        }
        let b = self.src.as_bytes();
        let mut depth = 0usize;
        let mut j = p;
        while j > 0 {
            j -= 1;
            if !self.code(j) {
                continue;
            }
            match b[j] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
                b'{' => return None,
                b';' if depth == 0 => return None,
                b'(' => {
                    let callee = self.expr_before(j);
                    if self.is_setter(callee) {
                        let mut args = self.src[j + 1..p].splitn(2, ',');
                        let id = args.next().map(str::trim).and_then(string_literal);
                        // An id the check cannot read is a place it cannot name.
                        return args.next().map(|_| id.map_or(Sink::Page, Sink::Element));
                    }
                    match callee.rsplit('.').next().unwrap_or(callee) {
                        "if" | "while" | "for" | "switch" | "catch" => return None,
                        "append" | "prepend" | "replaceChildren" | "createTextNode" | "insertAdjacentText" | "insertAdjacentHTML" | "before" | "after" | "replaceWith" | "write" | "writeln" => {
                            return Some(Sink::Page);
                        }
                        _ => {}
                    }
                }
                b'=' if depth == 0 && self.is_assignment(j) => {
                    let lhs = self.expr_before(if j > 0 && b[j - 1] == b'+' { j - 1 } else { j });
                    let written = ["textContent", "innerText", "innerHTML", "outerHTML"].iter().any(|prop| lhs.strip_suffix(prop).is_some_and(|o| o.ends_with('.')));
                    return written.then_some(Sink::Page);
                }
                _ => {}
            }
        }
        None
    }
}

/// Each element of `html` with an id, by id, with the nearest `lang` on it or an ancestor
/// below `<html>` (whose `lang` is the page's, which the caller has).
fn element_langs(html: &str) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    // Open elements, each with its name and the nearest `lang` from it up.
    let mut open: Vec<(String, Option<String>)> = Vec::new();
    for t in tags(html) {
        if let Some(name) = t.text.strip_prefix("</") {
            let name = name.trim_end_matches('>').trim().to_ascii_lowercase();
            // A stray one closes nothing; one closing an outer element closes what is inside.
            if let Some(k) = open.iter().rposition(|e| e.0 == name) {
                open.truncate(k);
            }
            continue;
        }
        let name = t.name.to_ascii_lowercase();
        if name.starts_with('!') || name.is_empty() || name == "html" {
            continue;
        }
        let lang = attr(t.text, "lang").map(str::to_string).or_else(|| open.last().and_then(|e| e.1.clone()));
        if let Some(id) = attr(t.text, "id") {
            out.entry(id.to_string()).or_insert_with(|| lang.clone());
        }
        if !is_void(&name, t.text) {
            open.push((name, lang));
        }
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// A quoted string's contents, when the whole expression is one (a template without `${`
/// counts).
fn string_literal(expr: &str) -> Option<String> {
    let q = expr.chars().next().filter(|q| matches!(q, '"' | '\'' | '`'))?;
    let inner = expr.strip_prefix(q)?.strip_suffix(q)?;
    (!inner.contains(q) && !inner.contains('\\') && !(q == '`' && inner.contains("${"))).then(|| inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Seen = (Option<String>, Option<String>);

    fn at(html: &str, js: &str, var: &str) -> Vec<Seen> {
        Page::read(html, js).landings(var).into_iter().map(|l| (l.lang, l.at)).collect()
    }

    fn el(lang: Option<&str>, id: &str) -> Seen {
        (lang.map(String::from), Some(format!("#{id}")))
    }

    const ON_PAGE: Seen = (None, None);

    #[test]
    fn the_setter_lands_in_its_element_under_its_nearest_lang() {
        let html = r#"<html lang="fr"><body><div id="root"><p lang="ja"><b><span id="name"></span></b></p><span id="sub"></span><img src="x.png"><span id="x" lang="en"></span></div></body></html>"#;
        assert_eq!(at(html, r#"text("name", vars.name);"#, "name"), [el(Some("ja"), "name")]);
        assert_eq!(at(html, r#"const { text: setText } = kumaTemplate({}); setText("sub", vars.name)"#, "name"), [el(None, "sub")]);
        assert_eq!(at(html, r#"tpl.text("x", String(tpl.vars["name"]).trim())"#, "name"), [el(Some("en"), "x")]);
        // A read that writes no text is no landing, and a var with none is on the page.
        assert_eq!(at(html, r##"if (vars.name) { gsap.set("#x", {}); } const n = vars.name.length;"##, "name"), [ON_PAGE]);
        assert_eq!(at(html, r#"text("name", vars.other); text("name", myvars.name)"#, "name"), [ON_PAGE]);
    }

    /// Any other write lands on the page, so a var set into a `lang="ja"` element and also
    /// written elsewhere is seen landing twice.
    #[test]
    fn other_writes_land_on_the_page() {
        let html = r#"<div id="a" lang="ja"></div>"#;
        for js in [
            r#"document.getElementById("a").textContent = vars.v;"#,
            r#"el.innerHTML += vars.v"#,
            r#"root.appendChild(document.createTextNode(vars.v))"#,
            r#"el.innerHTML = `<p lang="ja">${vars.v}</p>`;"#,
        ] {
            assert_eq!(at(html, js, "v"), [ON_PAGE], "{js}");
        }
        assert_eq!(at(html, r#"text("a", vars.v); other.textContent = vars.v;"#, "v"), [el(Some("ja"), "a"), ON_PAGE]);
        // A template or a string before the read is one value, whatever its inside holds.
        assert_eq!(at(html, r#"const a = `${f(1)}; {`, b = "(;"; text("a", vars.v)"#, "v"), [el(Some("ja"), "a")]);
    }

    #[test]
    fn closing_tags_end_the_elements_they_close() {
        let html = r#"<div lang="ja"><span>a</span></div><p><span id="after"></span></p><ul lang="ja"><li>x<li id="li"></ul>"#;
        assert_eq!(at(html, r#"text("after", vars.v)"#, "v"), [el(None, "after")]);
        assert_eq!(at(html, r#"text("li", vars.v)"#, "v"), [el(Some("ja"), "li")]);
    }
}
