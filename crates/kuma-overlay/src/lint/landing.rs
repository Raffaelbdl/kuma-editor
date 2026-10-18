//! Where a var's text lands, and what language it is drawn in there.
//!
//! The `cjk_lang` check is about the language an element's text is drawn in. Chromium picks
//! the regional forms of han characters, and the font a missing character falls back to, by
//! the element's own language: its nearest `lang`, which is `<html lang>` only when nothing
//! nearer says one. A Japanese name in a French page is `<span lang="ja">`, and the page's
//! `lang` is then right to stay French.
//!
//! Text reaches an overlay through vars, at run time, so the markup alone does not say where
//! a var's text goes; the overlay's own scripts do. This reads them for the writes the
//! contract teaches and a few DOM ones, and nothing cleverer:
//!
//! - `text("id", …vars.name…)` — `kumaTemplate`'s setter, as `text`, `tpl.text` or the
//!   name a destructuring gives it (`text: setText`) — lands in `#id`;
//! - `el.textContent = …`, `innerText`, `innerHTML`, `outerHTML` (and `+=`), and
//!   `el.append(…)`, `prepend`, `replaceChildren`, land in `el`, when `el` is
//!   `getElementById("id")`, `querySelector("#id")`, a setter call, or a name declared as
//!   one of those;
//! - a `${…}` inside a template literal lands inside the template's own markup first: a
//!   `lang` on an element of the template around it decides, and without one the template
//!   lands wherever it is written;
//! - anything else that writes text (`createTextNode`, `insertAdjacentHTML`, a write to an
//!   element it cannot name) lands somewhere unknown, which counts as the page.
//!
//! An element's language is its own `lang` or its nearest ancestor's below `<html>`, from the
//! markup as written or set by a script (`el.lang = …`, `el.setAttribute("lang", …)`) to a
//! string or a var's value. A read of a var that writes no text (`if (vars.name)`, a length)
//! is no landing at all. A var with no landing found is on the page, as before.
//!
//! The limits are those of reading code without running it: text that travels through a
//! local first (`const words = vars.title.split(…)`, then `span.textContent = w`) is not
//! followed, so such a var is judged by its other landings, or by the page when it has none —
//! conservative, since that is the warning that was always given. What is not conservative
//! is a var written both into a `lang="ja"` element and, by a route not followed, into
//! another: only the first is seen.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::inject::{attr_value_span, tags};

/// The language a landing's text is drawn in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Lang {
    /// Nothing between it and `<html>` says one: the page's.
    Page,
    /// A `lang` says this.
    Is(String),
    /// A script sets one to something this cannot read.
    Unread,
}

/// A place a var's text is written into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Landing {
    pub lang: Lang,
    /// Where, for a message: `#id`, or `None` for markup a script writes or a place unknown.
    pub at: Option<String>,
}

/// Every place the overlay's `scripts` write the text of the var `var` into, with `html` the
/// markup they run against and `vars` the resolved vars a `lang` may be set from.
pub(super) fn landings(html: &str, scripts: &str, vars: &Map<String, Value>, var: &str) -> Vec<Landing> {
    let js = Js::scan(scripts);
    let reads = js.reads_of(var);
    if reads.is_empty() {
        return Vec::new();
    }
    let elements = Elements::read(html);
    let dynamic = js.dynamic_langs(vars);
    let mut out = Vec::new();
    for p in reads {
        let landing = match js.sink(p, vars) {
            None => continue,
            Some(Sink::Lang(lang)) => Landing { lang, at: None },
            Some(Sink::Unplaced) => Landing { lang: Lang::Page, at: None },
            Some(Sink::Element(id)) => Landing { lang: elements.lang_of(&id, &dynamic), at: Some(format!("#{id}")) },
        };
        if !out.contains(&landing) {
            out.push(landing);
        }
    }
    out
}

/// Where one read's text goes.
#[derive(Debug, PartialEq)]
enum Sink {
    Element(String),
    /// Inside markup a script writes, under a `lang` of that markup.
    Lang(Lang),
    /// Written somewhere this cannot name.
    Unplaced,
}

/// What each byte of a script is: code, a quoted string, or a template literal's text.
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
    /// For a byte inside a template literal (its interpolations included), where the
    /// outermost template around it opens.
    owner: Vec<Option<usize>>,
    /// Each `${…}`: where its template opens, where its code starts, where it ends.
    interps: Vec<(usize, usize, usize)>,
    /// The names `kumaTemplate`'s `text` goes by besides `text`.
    setters: Vec<String>,
    /// Names declared as an element: `const card = document.getElementById("card")`.
    aliases: HashMap<String, String>,
}

impl<'a> Js<'a> {
    fn scan(src: &'a str) -> Self {
        enum Frame {
            Code { depth: usize, interp: Option<usize> },
            Tmpl { start: usize },
        }
        let b = src.as_bytes();
        let n = b.len();
        let mut kind = vec![Kind::Code; n];
        let mut owner = vec![None; n];
        let mut interps: Vec<(usize, usize, usize)> = Vec::new();
        let mut stack = vec![Frame::Code { depth: 0, interp: None }];
        let mut outer: Option<usize> = None;
        let mut templates = 0usize;
        let mut i = 0;
        while i < n {
            let from = i;
            let inside = templates > 0;
            match stack.last_mut().expect("the bottom frame is never popped") {
                Frame::Code { depth, interp } => match b[i] {
                    q @ (b'"' | b'\'') => {
                        i += 1;
                        while i < n && b[i] != q && b[i] != b'\n' {
                            i += if b[i] == b'\\' { 2 } else { 1 };
                        }
                        i = (i + 1).min(n);
                        kind[from..i].fill(Kind::Str);
                    }
                    b'`' => {
                        kind[i] = Kind::Tmpl;
                        if templates == 0 {
                            outer = Some(i);
                        }
                        templates += 1;
                        stack.push(Frame::Tmpl { start: i });
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
                    b'}' => {
                        if let Some(k) = *interp {
                            interps[k].2 = i;
                            kind[i] = Kind::Tmpl;
                            stack.pop();
                        }
                        i += 1;
                    }
                    _ => i += 1,
                },
                Frame::Tmpl { start } => {
                    let start = *start;
                    kind[i] = Kind::Tmpl;
                    match b[i] {
                        b'\\' => {
                            i = (i + 2).min(n);
                            kind[from..i].fill(Kind::Tmpl);
                        }
                        b'`' => {
                            stack.pop();
                            templates -= 1;
                            i += 1;
                        }
                        b'$' if b.get(i + 1) == Some(&b'{') => {
                            kind[i + 1] = Kind::Tmpl;
                            interps.push((start, i + 2, n));
                            stack.push(Frame::Code { depth: 0, interp: Some(interps.len() - 1) });
                            i += 2;
                        }
                        _ => i += 1,
                    }
                }
            }
            if inside || templates > 0 {
                owner[from..i].fill(outer);
            }
            if templates == 0 {
                outer = None;
            }
        }
        let mut js = Js { src, kind, owner, interps, setters: Vec::new(), aliases: HashMap::new() };
        js.setters = js.setter_aliases();
        js.aliases = js.element_aliases();
        js
    }

    fn code(&self, i: usize) -> bool {
        self.kind[i] == Kind::Code
    }

    /// Each read of `vars.<var>` (or `vars["<var>"]`) in code, by where it starts.
    fn reads_of(&self, var: &str) -> Vec<usize> {
        let b = self.src.as_bytes();
        let mut out = Vec::new();
        for (i, _) in self.src.match_indices("vars") {
            if !self.code(i) || (i > 0 && is_ident(b[i - 1])) {
                continue;
            }
            let rest = &self.src[i + 4..];
            let named = if let Some(r) = rest.strip_prefix('.') {
                r.strip_prefix(var).is_some_and(|after| !after.bytes().next().is_some_and(is_ident))
            } else if let Some(r) = rest.strip_prefix('[') {
                let r = r.trim_start();
                [format!("\"{var}\""), format!("'{var}'")].iter().any(|q| r.starts_with(q.as_str()))
            } else {
                false
            };
            if named {
                out.push(i);
            }
        }
        out
    }

    /// `text: <name>` in a destructuring of `kumaTemplate`'s result.
    fn setter_aliases(&self) -> Vec<String> {
        let b = self.src.as_bytes();
        let mut out = Vec::new();
        for (i, _) in self.src.match_indices("text") {
            if !self.code(i) || (i > 0 && (is_ident(b[i - 1]) || b[i - 1] == b'.')) {
                continue;
            }
            let Some(rest) = self.src[i + 4..].trim_start().strip_prefix(':') else { continue };
            let rest = rest.trim_start();
            let name: String = rest.bytes().take_while(|&c| is_ident(c)).map(char::from).collect();
            if !name.is_empty() && rest[name.len()..].trim_start().starts_with([',', '}']) {
                out.push(name);
            }
        }
        out
    }

    fn is_setter(&self, callee: &str) -> bool {
        callee == "text" || callee.ends_with(".text") || self.setters.iter().any(|s| s == callee)
    }

    /// `<name> = <element>` anywhere: the element a name holds.
    fn element_aliases(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (i, _) in self.src.match_indices('=') {
            if !self.code(i) || !self.is_assignment(i) {
                continue;
            }
            let name = self.src[..i].trim_end();
            let name = &name[name.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')).map_or(0, |k| k + 1)..];
            if name.is_empty() || name.bytes().next().is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Some(id) = self.element_of(self.expr_after(i + 1)) {
                out.insert(name.to_string(), id);
            }
        }
        out
    }

    /// True when the `=` at `i` assigns: not `==`, `=>`, `<=`, `>=`, `!=`.
    fn is_assignment(&self, i: usize) -> bool {
        let b = self.src.as_bytes();
        let next = b.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|k| b[k]);
        !matches!(next, Some(b'=' | b'>')) && !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'))
    }

    /// The element an expression names, if it names one by id.
    fn element_of(&self, expr: &str) -> Option<String> {
        let expr = expr.trim();
        if let Some(id) = self.aliases.get(expr) {
            return Some(id.clone());
        }
        let (callee, args) = split_call(expr)?;
        let first = args.split(',').next().map(str::trim).and_then(string_literal)?;
        if callee.ends_with("getElementById") || self.is_setter(callee) {
            return Some(first);
        }
        if callee.ends_with("querySelector") {
            let id = first.strip_prefix('#')?;
            return id.bytes().all(|c| is_ident(c) || c == b'-').then(|| id.to_string());
        }
        None
    }

    /// The expression that ends at `end`: names, dots and bracketed groups, read backwards.
    fn expr_before(&self, end: usize) -> &'a str {
        let b = self.src.as_bytes();
        let mut j = end;
        while j > 0 && b[j - 1].is_ascii_whitespace() {
            j -= 1;
        }
        let stop = j;
        while j > 0 {
            let c = b[j - 1];
            if self.code(j - 1) && (is_ident(c) || c == b'.') {
                j -= 1;
            } else if self.code(j - 1) && matches!(c, b')' | b']') {
                let mut depth = 0usize;
                let mut k = j;
                let mut matched = false;
                while k > 0 {
                    k -= 1;
                    if !self.code(k) {
                        continue;
                    }
                    match b[k] {
                        b')' | b']' => depth += 1,
                        b'(' | b'[' => {
                            depth -= 1;
                            if depth == 0 {
                                matched = true;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if !matched {
                    break;
                }
                j = k;
            } else {
                break;
            }
        }
        &self.src[j..stop]
    }

    /// The expression that starts at `start`: names, dots, strings and bracketed groups.
    fn expr_after(&self, start: usize) -> &'a str {
        let b = self.src.as_bytes();
        let mut i = start;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let from = i;
        let mut depth = 0usize;
        while i < b.len() {
            if !self.code(i) {
                i += 1;
                continue;
            }
            match b[i] {
                b'(' | b'[' => depth += 1,
                b')' | b']' if depth > 0 => depth -= 1,
                c if depth == 0 && !(is_ident(c) || c == b'.') => break,
                _ => {}
            }
            i += 1;
        }
        &self.src[from..i]
    }

    /// The language an expression gives: a string, or a var's value. `None` for anything else.
    fn eval(expr: &str, vars: &Map<String, Value>) -> Option<String> {
        let expr = expr.trim();
        if let Some(s) = string_literal(expr) {
            return Some(s);
        }
        let at = expr.rfind("vars")?;
        if at > 0 && !expr[..at].ends_with('.') {
            return None;
        }
        let rest = &expr[at + 4..];
        let name = match rest.strip_prefix('.') {
            Some(n) => n.to_string(),
            None => string_literal(rest.strip_prefix('[')?.strip_suffix(']')?.trim())?,
        };
        if !name.bytes().all(is_ident) || !expr[..at].bytes().all(|c| is_ident(c) || c == b'.') {
            return None;
        }
        vars.get(&name)?.as_str().map(|s| s.trim().to_string())
    }

    fn lang_of(expr: &str, vars: &Map<String, Value>) -> Lang {
        Self::eval(expr, vars).map_or(Lang::Unread, Lang::Is)
    }

    /// `lang`s scripts set: `el.lang = …` and `el.setAttribute("lang", …)`, by element id.
    fn dynamic_langs(&self, vars: &Map<String, Value>) -> HashMap<String, Lang> {
        let b = self.src.as_bytes();
        let mut out = HashMap::new();
        for (i, _) in self.src.match_indices(".lang") {
            if !self.code(i) || b.get(i + 5).is_some_and(|&c| is_ident(c)) {
                continue;
            }
            let after = self.src[i + 5..].trim_start();
            let eq = self.src.len() - after.len();
            if !after.starts_with('=') || !self.is_assignment(eq) {
                continue;
            }
            if let Some(id) = self.element_of(self.expr_before(i)) {
                out.insert(id, Self::lang_of(self.expr_after(eq + 1), vars));
            }
        }
        for (i, _) in self.src.match_indices(".setAttribute(") {
            if !self.code(i) {
                continue;
            }
            let call = self.expr_after(i + 1);
            let Some((_, args)) = split_call(call) else { continue };
            let Some((name, value)) = args.split_once(',') else { continue };
            if string_literal(name.trim()).as_deref() != Some("lang") {
                continue;
            }
            if let Some(id) = self.element_of(self.expr_before(i)) {
                out.insert(id, Self::lang_of(value, vars));
            }
        }
        out
    }

    /// The innermost `${…}` around `p`.
    fn interp_around(&self, p: usize) -> Option<(usize, usize)> {
        self.interps.iter().filter(|&&(_, from, to)| from <= p && p < to).max_by_key(|&&(_, from, _)| from).map(|&(t, from, _)| (t, from))
    }

    /// Where the text read at `p` is written, if it is written anywhere.
    fn sink(&self, mut p: usize, vars: &Map<String, Value>) -> Option<Sink> {
        // Inside a template: its own markup first, then wherever the template goes.
        while let Some((open, from)) = self.interp_around(p) {
            let markup = &self.src[open + 1..from - 2];
            if let Some(lang) = template_lang(markup, vars) {
                return Some(Sink::Lang(lang));
            }
            p = open;
        }
        let b = self.src.as_bytes();
        let mut depth = 0usize;
        let mut j = p;
        while j > 0 {
            j -= 1;
            if let Some(open) = self.owner[j] {
                // A whole template before `p`: one value, stepped over.
                j = open;
                continue;
            }
            if !self.code(j) {
                continue;
            }
            match b[j] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
                b'{' => return None,
                b';' if depth == 0 => return None,
                b'[' => {}
                b'(' => {
                    let callee = self.expr_before(j);
                    if self.is_setter(callee) {
                        let args = &self.src[j + 1..p];
                        let mut parts = args.splitn(2, ',');
                        let id = parts.next().map(str::trim).and_then(string_literal);
                        // An id the check cannot read is a place it cannot name.
                        return match (id, parts.next()) {
                            (Some(id), Some(_)) => Some(Sink::Element(id)),
                            (None, Some(_)) => Some(Sink::Unplaced),
                            (_, None) => None,
                        };
                    }
                    let method = callee.rsplit('.').next().unwrap_or(callee);
                    let object = callee.strip_suffix(method).and_then(|o| o.strip_suffix('.'));
                    match method {
                        "if" | "while" | "for" | "switch" | "catch" => return None,
                        "append" | "prepend" | "replaceChildren" => {
                            return Some(object.and_then(|o| self.element_of(o)).map_or(Sink::Unplaced, Sink::Element));
                        }
                        "createTextNode" | "insertAdjacentText" | "insertAdjacentHTML" | "before" | "after" | "replaceWith" | "write" | "writeln" => {
                            return Some(Sink::Unplaced);
                        }
                        _ => {}
                    }
                }
                b'=' if depth == 0 && self.is_assignment(j) => {
                    let lhs_end = if j > 0 && b[j - 1] == b'+' { j - 1 } else { j };
                    let lhs = self.expr_before(lhs_end);
                    let target = ["textContent", "innerText", "innerHTML", "outerHTML"].iter().find_map(|prop| lhs.strip_suffix(prop)?.strip_suffix('.'));
                    return target.map(|el| self.element_of(el).map_or(Sink::Unplaced, Sink::Element));
                }
                _ => {}
            }
        }
        None
    }
}

/// The `lang` of the innermost element still open at the end of a template's `markup`, if
/// one of them has one.
fn template_lang(markup: &str, vars: &Map<String, Value>) -> Option<Lang> {
    let mut open: Vec<(String, Option<&str>)> = Vec::new();
    for t in tags(markup) {
        if let Some(name) = t.text.strip_prefix("</") {
            close(&mut open, name, |e| &e.0);
            continue;
        }
        if t.name.starts_with('!') || t.name.is_empty() || is_void(t.name, t.text) {
            continue;
        }
        open.push((t.name.to_ascii_lowercase(), attr(t.text, "lang")));
    }
    let lang = open.iter().rev().find_map(|(_, l)| *l)?;
    Some(match lang.strip_prefix("${").and_then(|l| l.strip_suffix('}')) {
        Some(expr) => Js::lang_of(expr, vars),
        None if lang.contains("${") => Lang::Unread,
        None => Lang::Is(lang.to_string()),
    })
}

/// The markup's elements with an id, each with the `lang`s of it and its ancestors below
/// `<html>` (nearest last), by id.
struct Elements {
    chains: HashMap<String, Vec<Link>>,
}

/// One element of a chain: its id and its `lang`, either of which it may lack.
type Link = (Option<String>, Option<String>);

impl Elements {
    fn read(html: &str) -> Self {
        let mut chains = HashMap::new();
        let mut open: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        for t in tags(html) {
            if let Some(name) = t.text.strip_prefix("</") {
                close(&mut open, name, |e| &e.0);
                continue;
            }
            let name = t.name.to_ascii_lowercase();
            // `<html lang>` is the page's language, which the caller has (a `lang` var may
            // replace it).
            if name.starts_with('!') || name.is_empty() || name == "html" {
                continue;
            }
            let id = attr(t.text, "id").map(str::to_string);
            let lang = attr(t.text, "lang").map(str::to_string);
            let element = (name.clone(), id.clone(), lang);
            if let Some(id) = id {
                let chain = open.iter().chain([&element]).map(|(_, id, lang)| (id.clone(), lang.clone())).collect();
                chains.entry(id).or_insert(chain);
            }
            if !is_void(&name, t.text) {
                open.push(element);
            }
        }
        Elements { chains }
    }

    /// The language `#id`'s text is drawn in: the nearest `lang`, a script's first.
    fn lang_of(&self, id: &str, dynamic: &HashMap<String, Lang>) -> Lang {
        if let Some(l) = dynamic.get(id) {
            return l.clone();
        }
        for (eid, lang) in self.chains.get(id).into_iter().flatten().rev() {
            if let Some(l) = eid.as_ref().and_then(|e| dynamic.get(e)) {
                return l.clone();
            }
            if let Some(l) = lang {
                return Lang::Is(l.clone());
            }
        }
        Lang::Page
    }
}

/// Close the element a `</name>` ends, and any left open inside it; a stray one closes nothing.
fn close<T>(open: &mut Vec<T>, name: &str, tag: impl Fn(&T) -> &String) {
    let name = name.trim_end_matches('>').trim().to_ascii_lowercase();
    if let Some(k) = open.iter().rposition(|e| *tag(e) == name) {
        open.truncate(k);
    }
}

fn is_void(name: &str, text: &str) -> bool {
    text.ends_with("/>")
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta" | "param" | "source" | "track" | "wbr"
        )
}

/// An attribute's value in an opening tag; a bare one runs to the end of the tag.
fn attr<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let (a, b) = attr_value_span(tag, name)?;
    Some(tag[a..b].trim_end_matches(['>', '/']).trim())
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

/// `callee(args)` split, when the expression is exactly one call.
fn split_call(expr: &str) -> Option<(&str, &str)> {
    let open = expr.find('(')?;
    let inner = expr[open + 1..].strip_suffix(')')?;
    Some((expr[..open].trim(), inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(html: &str, js: &str, var: &str) -> Vec<(Lang, Option<String>)> {
        at_with(html, js, var, serde_json::json!({}))
    }

    fn at_with(html: &str, js: &str, var: &str, vars: Value) -> Vec<(Lang, Option<String>)> {
        landings(html, js, vars.as_object().unwrap(), var).into_iter().map(|l| (l.lang, l.at)).collect()
    }

    fn is(l: &str) -> Lang {
        Lang::Is(l.into())
    }

    #[test]
    fn the_setter_lands_in_its_element_under_its_nearest_lang() {
        let html = r#"<html lang="fr"><body><div id="root"><p lang="ja"><b><span id="name"></span></b></p><span id="sub"></span><img src="x.png"><span id="x" lang="en"></span></div></body></html>"#;
        assert_eq!(at(html, r#"text("name", vars.name);"#, "name"), [(is("ja"), Some("#name".into()))]);
        assert_eq!(at(html, r#"const { text: setText } = kumaTemplate({}); setText("sub", vars.name)"#, "name"), [(Lang::Page, Some("#sub".into()))]);
        assert_eq!(at(html, r#"tpl.text("x", String(tpl.vars["name"]).trim())"#, "name"), [(is("en"), Some("#x".into()))]);
        // A read that writes no text is no landing; a var not read at all has none.
        assert_eq!(at(html, r##"if (vars.name) { gsap.set("#x", {}); } const n = vars.name.length;"##, "name"), []);
        assert_eq!(at(html, r#"text("name", vars.other)"#, "name"), []);
        assert_eq!(at(html, r#"text("name", myvars.name)"#, "name"), []);
    }

    #[test]
    fn dom_writes_land_where_they_name() {
        let html = r#"<div id="a" lang="ja-JP"></div><div id="b"></div>"#;
        assert_eq!(at(html, r#"document.getElementById("a").textContent = vars.v;"#, "v"), [(is("ja-JP"), Some("#a".into()))]);
        assert_eq!(at(html, r#"const el = document.querySelector('#a'); el.innerHTML += vars.v"#, "v"), [(is("ja-JP"), Some("#a".into()))]);
        assert_eq!(at(html, r#"document.getElementById("b").append(vars.v)"#, "v"), [(Lang::Page, Some("#b".into()))]);
        // A write the check cannot follow to an element is on the page.
        assert_eq!(at(html, r#"for (const e of els) e.textContent = vars.v;"#, "v"), [(Lang::Page, None)]);
        assert_eq!(at(html, r#"root.appendChild(document.createTextNode(vars.v))"#, "v"), [(Lang::Page, None)]);
    }

    #[test]
    fn a_template_lands_under_its_own_lang_first() {
        let html = r#"<div id="host" lang="ko"></div><div id="plain"></div>"#;
        let js = r#"document.getElementById("plain").innerHTML = `<p class="x"><span lang="ja">${vars.name}</span> ${vars.place}</p>`;"#;
        assert_eq!(at(html, js, "name"), [(is("ja"), None)]);
        assert_eq!(at(html, js, "place"), [(Lang::Page, Some("#plain".into()))]);
        let js = r#"const host = document.getElementById("host"); host.innerHTML = `<b>${vars.name.toUpperCase()}</b>`;"#;
        assert_eq!(at(html, js, "name"), [(is("ko"), Some("#host".into()))]);
        // A `lang` from a var is that var's value.
        let js = r#"el.innerHTML = `<span lang="${vars.name_lang}">${vars.name}</span>`;"#;
        assert_eq!(at_with(html, js, "name", serde_json::json!({ "name_lang": "zh-Hant" })), [(is("zh-Hant"), None)]);
        let js = r#"el.innerHTML = `<span lang="${pick(vars.name_lang)}">${vars.name}</span>`;"#;
        assert_eq!(at(html, js, "name"), [(Lang::Unread, None)]);
        // A template before the read is one value, whatever its inside holds.
        let js = r#"const a = `${f(1)}; {`; text("plain", vars.name)"#;
        assert_eq!(at(html, js, "name"), [(Lang::Page, Some("#plain".into()))]);
    }

    #[test]
    fn a_script_sets_a_lang_from_a_string_or_a_var() {
        let html = r#"<div id="card"><span id="name"></span></div><span id="sub" lang="ja"></span>"#;
        let vars = serde_json::json!({ "name_lang": "ja" });
        let js = r#"const card = document.getElementById("card"); card.lang = vars.name_lang; text("name", vars.name);"#;
        assert_eq!(at_with(html, js, "name", vars.clone()), [(is("ja"), Some("#name".into()))]);
        let js = r#"const s = text("name", vars.name); s.setAttribute("lang", "zh");"#;
        assert_eq!(at(html, js, "name"), [(is("zh"), Some("#name".into()))]);
        let js = r#"document.getElementById("sub").lang = vars.missing; text("sub", vars.name);"#;
        assert_eq!(at(html, js, "name"), [(Lang::Unread, Some("#sub".into()))]);
        // A comparison is not a setting.
        let js = r#"if (document.getElementById("sub").lang == "en") {} text("sub", vars.name);"#;
        assert_eq!(at(html, js, "name"), [(is("ja"), Some("#sub".into()))]);
    }

    #[test]
    fn closing_tags_end_the_elements_they_close() {
        let html = r#"<div lang="ja"><span>a</span></div><p><span id="after"></span></p><ul lang="ja"><li>x<li id="li"></ul>"#;
        assert_eq!(at(html, r#"text("after", vars.v)"#, "v"), [(Lang::Page, Some("#after".into()))]);
        assert_eq!(at(html, r#"text("li", vars.v)"#, "v"), [(is("ja"), Some("#li".into()))]);
    }
}
