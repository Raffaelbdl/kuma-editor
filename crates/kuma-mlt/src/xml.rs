//! A deliberately tiny XML writer. MLT XML is flat and regular; a dependency would be
//! more code than this.

pub struct XmlWriter {
    buf: String,
    depth: usize,
}

impl Default for XmlWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl XmlWriter {
    pub fn new() -> Self {
        XmlWriter { buf: String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"), depth: 0 }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.buf.push_str("  ");
        }
    }

    /// `<tag a="b">` and increase depth.
    pub fn open(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.indent();
        self.buf.push('<');
        self.buf.push_str(tag);
        for (k, v) in attrs {
            self.buf.push(' ');
            self.buf.push_str(k);
            self.buf.push_str("=\"");
            self.buf.push_str(&escape(v));
            self.buf.push('"');
        }
        self.buf.push_str(">\n");
        self.depth += 1;
    }

    /// `<tag a="b"/>`.
    pub fn empty(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.indent();
        self.buf.push('<');
        self.buf.push_str(tag);
        for (k, v) in attrs {
            self.buf.push(' ');
            self.buf.push_str(k);
            self.buf.push_str("=\"");
            self.buf.push_str(&escape(v));
            self.buf.push('"');
        }
        self.buf.push_str("/>\n");
    }

    pub fn close(&mut self, tag: &str) {
        self.depth -= 1;
        self.indent();
        self.buf.push_str("</");
        self.buf.push_str(tag);
        self.buf.push_str(">\n");
    }

    /// `<property name="k">v</property>`.
    pub fn property(&mut self, name: &str, value: impl AsRef<str>) {
        self.indent();
        self.buf.push_str("<property name=\"");
        self.buf.push_str(&escape(name));
        self.buf.push_str("\">");
        self.buf.push_str(&escape(value.as_ref()));
        self.buf.push_str("</property>\n");
    }

    pub fn finish(self) -> String {
        self.buf
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Format a float the way MLT expects (`LC_NUMERIC="C"`, no exponent, trimmed).
pub fn num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e12 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0');
    s.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_formats() {
        assert_eq!(escape("a<b&\"c\""), "a&lt;b&amp;&quot;c&quot;");
        assert_eq!(num(1.0), "1");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(-384.0), "-384");
        assert_eq!(num(1.23456789), "1.234568");
    }
}
