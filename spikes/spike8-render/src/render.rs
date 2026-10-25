//! Decoded note (text + attribute runs) -> paragraphs -> sanitized HTML.
//!
//! Nothing from the note is ever passed through as markup: every tag comes from this file,
//! and all note text / attribute values go through `push_text` / `attr`.

use std::collections::BTreeMap;

use crate::decode::proto::{AttachmentInfo, AttributeRun, Color};
use crate::decode::{describe, spans, style_name};

/// Deepest list / indent level rendered; deeper levels are clamped.
pub const MAX_DEPTH: usize = 8;

/// Content-free counters filled while rendering (what the sanitizer had to do).
pub type Counters = BTreeMap<String, usize>;

pub fn bump(c: &mut Counters, k: &str) {
    *c.entry(k.to_string()).or_default() += 1;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListKind {
    Bullet,
    Dash,
    Numbered,
    Check,
}

impl ListKind {
    pub fn of(a: &ParaAttrs) -> Option<Self> {
        if a.check.is_some() {
            return Some(Self::Check);
        }
        match a.style {
            100 => Some(Self::Bullet),
            101 => Some(Self::Dash),
            102 => Some(Self::Numbered),
            103 => Some(Self::Check),
            _ => None,
        }
    }
    fn open(self) -> &'static str {
        match self {
            Self::Bullet => "<ul>",
            Self::Dash => "<ul class=\"dash\">",
            Self::Numbered => "<ol>",
            Self::Check => "<ul class=\"check\">",
        }
    }
    fn close(self) -> &'static str {
        if self == Self::Numbered { "</ol>" } else { "</ul>" }
    }
}

/// Paragraph-level attributes, taken from the run that holds the paragraph's `\n`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParaAttrs {
    pub style: i32,
    pub indent: i32,
    pub align: i32,
    pub quote: bool,
    /// Some(done) for checklist items.
    pub check: Option<bool>,
    /// ParagraphStyle.7, not in the schema (read from the raw bytes).
    pub ps7: Option<u64>,
}

impl ParaAttrs {
    pub fn body() -> Self {
        Self { style: -1, indent: 0, align: 0, quote: false, check: None, ps7: None }
    }
    pub fn of(run: &AttributeRun, ps7: Option<u64>) -> Self {
        let ps = run.paragraph_style.as_ref();
        Self {
            style: ps.and_then(|p| p.style_type).unwrap_or(-1),
            indent: ps.and_then(|p| p.indent_amount).unwrap_or(0),
            align: ps.and_then(|p| p.alignment).unwrap_or(0),
            quote: ps.and_then(|p| p.block_quote).unwrap_or(0) != 0,
            check: ps.and_then(|p| p.checklist.as_ref()).map(|c| c.done != 0),
            ps7,
        }
    }
    /// The attributes that change how a paragraph renders (ps7 excluded: meaning unknown).
    fn key(&self) -> (i32, i32, i32, bool, Option<bool>) {
        (self.style, self.indent, self.align, self.quote, self.check)
    }
}

/// Part of one run that falls inside one paragraph.
pub struct Seg<'a> {
    pub text: String,
    pub run: &'a AttributeRun,
}

pub struct Para<'a> {
    pub attrs: ParaAttrs,
    pub segs: Vec<Seg<'a>>,
    /// Runs inside this paragraph disagreed on paragraph attributes.
    pub mixed: bool,
}

/// Split runs at `\n` into paragraphs. `ps7[i]` belongs to `runs[i]`.
pub fn paragraphs<'a>(text: &str, runs: &'a [AttributeRun], ps7: &[Option<u64>]) -> Vec<Para<'a>> {
    let (sp, _) = spans(text, runs);
    let mut out = Vec::new();
    let mut segs = Vec::new();
    let mut seen: Vec<ParaAttrs> = Vec::new();
    for (i, s) in sp.iter().enumerate() {
        let a = ParaAttrs::of(s.run, ps7.get(i).copied().flatten());
        let mut rest = s.text.as_str();
        loop {
            match rest.find('\n') {
                Some(n) => {
                    if n > 0 {
                        segs.push(Seg { text: rest[..n].to_string(), run: s.run });
                    }
                    let mixed = seen.iter().any(|x| x.key() != a.key());
                    out.push(Para { attrs: a.clone(), segs: std::mem::take(&mut segs), mixed });
                    seen.clear();
                    rest = &rest[n + 1..];
                }
                None => {
                    if !rest.is_empty() {
                        segs.push(Seg { text: rest.to_string(), run: s.run });
                        seen.push(a.clone());
                    }
                    break;
                }
            }
        }
    }
    if !segs.is_empty() {
        // Last paragraph has no `\n`: use its last run's attributes.
        let a = seen.last().cloned().unwrap_or_else(ParaAttrs::body);
        let mixed = seen.iter().any(|x| x.key() != a.key());
        out.push(Para { attrs: a, segs, mixed });
    }
    out
}

// ---------- sanitizing ----------

/// Escape note text for an HTML text node, dropping characters that can mislead or break layout.
pub fn push_text(out: &mut String, s: &str, c: &mut Counters) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' => out.push('\t'),
            '\u{2028}' | '\u{2029}' => {
                bump(c, "sanitize: U+2028/2029 -> <br>");
                out.push_str("<br>");
            }
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => bump(c, "sanitize: bidi control stripped"),
            '\u{FFFC}' => bump(c, "sanitize: stray U+FFFC stripped"),
            '\u{FFFD}' => {
                bump(c, "sanitize: U+FFFD in text");
                out.push(ch);
            }
            ch if ch.is_control() => bump(c, "sanitize: control char stripped"),
            ch => out.push(ch),
        }
    }
}

/// Escape a value for a double-quoted attribute. Control characters are dropped.
pub fn attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

/// URL scheme as a browser would see it (leading C0/space trimmed, tab/CR/LF removed), lowercased.
pub fn link_scheme(raw: &str) -> String {
    let cleaned: String = raw
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    match cleaned.find(':') {
        Some(i)
            if cleaned.as_bytes()[0].is_ascii_alphabetic()
                && cleaned[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            cleaned[..i].to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

/// Content-free bucket for stats.
pub fn link_bucket(raw: &str) -> &'static str {
    match link_scheme(raw).as_str() {
        "http" => "http",
        "https" => "https",
        "mailto" => "mailto",
        "tel" => "tel",
        "applenotes" => "applenotes",
        "mobilenotes" => "mobilenotes",
        "" => "(no scheme)",
        _ => "other",
    }
}

/// An href that is safe to put in the page, or None.
pub fn safe_href(raw: &str) -> Option<String> {
    let scheme = link_scheme(raw);
    if !matches!(scheme.as_str(), "http" | "https" | "mailto") || raw.chars().any(char::is_control) {
        return None;
    }
    Some(raw.trim().to_string())
}

fn css_color(c: &Color) -> String {
    let ch = |v: f32| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
    // Floor alpha so a colour can't make text invisible.
    let a = if c.alpha.is_finite() { c.alpha.clamp(0.3, 1.0) } else { 1.0 };
    format!("rgba({},{},{},{:.2})", ch(c.red), ch(c.green), ch(c.blue), a)
}

fn ok_token(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

pub fn attachment_kind(uti: &str) -> &'static str {
    match uti {
        "com.apple.notes.table" => "table",
        "com.apple.paper" => "sketch",
        u if u.starts_with("com.apple.drawing") => "drawing",
        u if u.starts_with("com.apple.notes.inlinetextattachment.calculateresult") => "calculation",
        u if u.starts_with("com.apple.notes.inlinetextattachment") => "inline",
        "public.url" => "link-card",
        "public.png" | "public.jpeg" | "public.heic" | "public.tiff" | "public.gif" | "org.webmproject.webp"
        | "public.image" => "image",
        u if u.starts_with("public.") && (u.contains("movie") || u.contains("mpeg-4") || u.contains("video")) => "video",
        u if u.contains("audio") => "audio",
        "com.adobe.pdf" => "pdf",
        _ => "attachment",
    }
}

fn push_attachment(out: &mut String, a: &AttachmentInfo, c: &mut Counters) {
    let uti = a.type_uti.as_deref().unwrap_or("");
    let kind = attachment_kind(uti);
    out.push_str("<span class=\"att\" data-kind=\"");
    out.push_str(kind);
    out.push('"');
    if ok_token(uti, ".-", 128) {
        out.push_str(" data-uti=\"");
        out.push_str(uti);
        out.push('"');
    } else if !uti.is_empty() {
        bump(c, "sanitize: attachment UTI dropped");
    }
    match a.attachment_identifier.as_deref() {
        Some(id) if ok_token(id, "-", 64) => {
            out.push_str(" data-id=\"");
            out.push_str(id);
            out.push('"');
        }
        Some(_) => bump(c, "sanitize: attachment id dropped"),
        None => bump(c, "attachment run without id"),
    }
    out.push_str(">[");
    out.push_str(kind);
    out.push_str("]</span>");
}

// ---------- HTML ----------

#[derive(Default, Clone, Copy)]
pub struct Opts {
    /// Add content-free run/paragraph descriptions as attributes (for checking by eye).
    pub debug: bool,
}

fn push_seg(out: &mut String, seg: &Seg, opts: Opts, c: &mut Counters) {
    let r = seg.run;
    if let Some(a) = &r.attachment_info {
        push_attachment(out, a, c);
        if seg.text != "\u{FFFC}" {
            bump(c, "attachment run with other text");
            push_text(out, &seg.text, c);
        }
        return;
    }
    let mut close: Vec<&str> = Vec::new();
    let mut open = |out: &mut String, o: &str, cl: &'static str| {
        out.push_str(o);
        close.push(cl);
    };
    if opts.debug {
        open(out, &format!("<span class=\"dbg\" title=\"{}\">", attr(&describe(r))), "</span>");
    }
    match r.font_weight.unwrap_or(0) {
        1 => open(out, "<b>", "</b>"),
        2 => open(out, "<i>", "</i>"),
        3 => {
            open(out, "<b>", "</b>");
            open(out, "<i>", "</i>");
        }
        _ => {}
    }
    if r.underlined.unwrap_or(0) != 0 {
        open(out, "<u>", "</u>");
    }
    if r.strikethrough.unwrap_or(0) != 0 {
        open(out, "<s>", "</s>");
    }
    match r.superscript.unwrap_or(0) {
        s if s > 0 => open(out, "<sup>", "</sup>"),
        s if s < 0 => open(out, "<sub>", "</sub>"),
        _ => {}
    }
    if let Some(e) = r.emphasis_style {
        open(out, &format!("<mark class=\"em{}\">", e.clamp(0, 99)), "</mark>");
    }
    if let Some(col) = &r.color {
        open(out, &format!("<span style=\"color:{}\">", css_color(col)), "</span>");
    }
    push_text(out, &seg.text, c);
    for cl in close.iter().rev() {
        out.push_str(cl);
    }
}

/// Inline content of one paragraph; consecutive segments with the same link share one `<a>`.
fn push_inline(out: &mut String, p: &Para, opts: Opts, c: &mut Counters) {
    if p.segs.is_empty() {
        out.push_str("<br>");
        return;
    }
    let mut cur: Option<&str> = None;
    let mut close_a = "";
    for seg in &p.segs {
        let link = seg.run.link.as_deref();
        if link != cur {
            out.push_str(close_a);
            close_a = "";
            if let Some(l) = link {
                match safe_href(l) {
                    Some(h) => {
                        out.push_str(&format!("<a href=\"{}\" rel=\"noopener noreferrer\">", attr(&h)));
                        close_a = "</a>";
                    }
                    None => {
                        let b = link_bucket(l);
                        bump(c, &format!("sanitize: link not rendered ({b})"));
                        let class = if b == "applenotes" || b == "mobilenotes" { "notelink" } else { "link-off" };
                        out.push_str(&format!("<span class=\"{class}\" title=\"link not opened ({})\">", attr(b)));
                        close_a = "</span>";
                    }
                }
            }
            cur = link;
        }
        push_seg(out, seg, opts, c);
    }
    out.push_str(close_a);
}

fn block_attrs(p: &Para, opts: Opts, list: bool) -> String {
    let a = &p.attrs;
    let mut class = Vec::new();
    if !list && a.indent > 0 {
        class.push(format!("ind{}", (a.indent as usize).min(MAX_DEPTH)));
    }
    match a.align {
        // NSTextAlignment on iOS: 0 left, 1 center, 2 right, 3 justified, 4 natural.
        1 => class.push("center".into()),
        2 => class.push("right".into()),
        3 => class.push("justify".into()),
        _ => {}
    }
    if a.check == Some(true) {
        class.push("done".into());
    }
    let mut s = String::new();
    if !class.is_empty() {
        s.push_str(&format!(" class=\"{}\"", class.join(" ")));
    }
    if opts.debug {
        let mut d = format!("{} indent={} align={}", style_name(a.style), a.indent, a.align);
        if let Some(v) = a.ps7 {
            d.push_str(&format!(" ps7={v}"));
        }
        if p.mixed {
            d.push_str(" MIXED");
        }
        s.push_str(&format!(" data-dbg=\"{}\"", attr(&d)));
    }
    s
}

struct W {
    out: String,
    /// Open lists, innermost last: (kind, an <li> is open).
    lists: Vec<(ListKind, bool)>,
    quote: bool,
    pre: bool,
}

impl W {
    fn close_pre(&mut self) {
        if self.pre {
            self.out.push_str("</pre>\n");
            self.pre = false;
        }
    }
    fn close_lists_to(&mut self, n: usize) {
        while self.lists.len() > n {
            let (k, li) = self.lists.pop().unwrap();
            if li {
                self.out.push_str("</li>");
            }
            self.out.push_str(k.close());
            self.out.push('\n');
        }
    }
    fn list_item(&mut self, k: ListKind, p: &Para, opts: Opts, c: &mut Counters) {
        let level = (p.attrs.indent.max(0) as usize).min(MAX_DEPTH - 1);
        self.close_lists_to(level + 1);
        if self.lists.len() == level + 1 && self.lists[level].0 != k {
            self.close_lists_to(level);
        }
        while self.lists.len() < level + 1 {
            // A nested list must sit inside an <li>; open an empty one if the parent has none.
            if let Some(top) = self.lists.last_mut()
                && !top.1
            {
                self.out.push_str("<li class=\"nest\">");
                top.1 = true;
            }
            self.out.push_str(k.open());
            self.lists.push((k, false));
        }
        let top = self.lists.last_mut().unwrap();
        if top.1 {
            self.out.push_str("</li>\n");
        }
        top.1 = true;
        self.out.push_str(&format!("<li{}>", block_attrs(p, opts, true)));
        if let Some(done) = p.attrs.check {
            self.out.push_str(if done { "<span class=\"box\">\u{2611}</span> " } else { "<span class=\"box\">\u{2610}</span> " });
        }
        push_inline(&mut self.out, p, opts, c);
    }
}

/// Render paragraphs to an HTML fragment.
pub fn render_body(paras: &[Para], opts: Opts, c: &mut Counters) -> String {
    let mut w = W { out: String::new(), lists: Vec::new(), quote: false, pre: false };
    for p in paras {
        let a = &p.attrs;
        let kind = ListKind::of(a);
        let mono = a.style == 4 && kind.is_none();
        if !mono {
            w.close_pre();
        }
        if a.quote != w.quote {
            w.close_pre();
            w.close_lists_to(0);
            w.out.push_str(if a.quote { "<blockquote>\n" } else { "</blockquote>\n" });
            w.quote = a.quote;
        }
        if let Some(k) = kind {
            w.list_item(k, p, opts, c);
            continue;
        }
        w.close_lists_to(0);
        if mono {
            if w.pre {
                w.out.push('\n');
            } else {
                w.out.push_str(&format!("<pre{}>", block_attrs(p, opts, false)));
                w.pre = true;
            }
            // <br> for an empty line would double up inside <pre>.
            if !p.segs.is_empty() {
                push_inline(&mut w.out, p, opts, c);
            }
            continue;
        }
        let tag = match a.style {
            0 => "h1",
            1 => "h2",
            2 => "h3",
            _ => "p",
        };
        w.out.push_str(&format!("<{tag}{}>", block_attrs(p, opts, false)));
        push_inline(&mut w.out, p, opts, c);
        w.out.push_str(&format!("</{tag}>\n"));
    }
    w.close_pre();
    w.close_lists_to(0);
    if w.quote {
        w.out.push_str("</blockquote>\n");
    }
    w.out
}

const CSS: &str = r#"
:root { color-scheme: light dark; --fg:#1d1d1f; --bg:#fff; --muted:#86868b; --att:#f2f2f7; --mark:#fff3a3; }
@media (prefers-color-scheme: dark) { :root { --fg:#f5f5f7; --bg:#1c1c1e; --muted:#98989d; --att:#2c2c2e; --mark:#6b5d00; } }
body { font: 16px/1.45 -apple-system, "Segoe UI", system-ui, sans-serif; color: var(--fg); background: var(--bg);
       max-width: 46rem; margin: 2rem auto; padding: 0 1rem; overflow-wrap: anywhere; }
h1 { font-size: 1.75em; margin: .2em 0 .4em; } h2 { font-size: 1.35em; margin: .8em 0 .3em; }
h3 { font-size: 1.1em; margin: .7em 0 .2em; } p { margin: 0; min-height: 1.45em; }
pre { font: 14px/1.4 ui-monospace, Consolas, monospace; background: var(--att); padding: .5em .7em; border-radius: 6px;
      white-space: pre-wrap; margin: .3em 0; }
ul, ol { margin: 0; padding-left: 1.6em; } ul.dash { list-style: none; } ul.dash > li::before { content: "\2013\00a0"; margin-left: -1em; }
ul.check { list-style: none; padding-left: 1.2em; } li.nest { list-style: none; } li .box { color: var(--muted); }
blockquote { margin: .3em 0; padding-left: .8em; border-left: 3px solid var(--muted); }
.ind1{margin-left:1.6em}.ind2{margin-left:3.2em}.ind3{margin-left:4.8em}.ind4{margin-left:6.4em}
.ind5{margin-left:8em}.ind6{margin-left:9.6em}.ind7{margin-left:11.2em}.ind8{margin-left:12.8em}
.center{text-align:center}.right{text-align:right}.justify{text-align:justify}
mark { background: var(--mark); color: inherit; }
.att { display: inline-block; background: var(--att); color: var(--muted); border-radius: 4px; padding: 0 .4em; font-size: .85em; }
.link-off, .notelink { text-decoration: underline dotted; }
.dbg:hover, [data-dbg]:hover { outline: 1px dashed #f80; }
"#;

/// A standalone page. The CSP blocks scripts, images, fonts and network entirely.
pub fn document(body: &str, title: &str, opts: Opts) -> String {
    let mut s = String::from("<!doctype html>\n<html><head><meta charset=\"utf-8\">\n");
    s.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\n");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>");
    s.push_str(&attr(title));
    s.push_str("</title>\n<style>");
    s.push_str(CSS);
    if opts.debug {
        s.push_str("[data-dbg]::after { content: \"  \u{27e8}\" attr(data-dbg) \"\u{27e9}\"; color: #f80; font: 11px monospace; }\n");
    }
    s.push_str("</style></head>\n<body>\n");
    s.push_str(body);
    s.push_str("</body></html>\n");
    s
}

/// Check that our own output nests correctly (text is escaped, so every `<` starts a tag).
pub fn balanced(html: &str) -> Result<(), String> {
    const VOID: &[&str] = &["br", "meta", "!doctype"];
    let mut stack: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>').ok_or("unclosed <")?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            match stack.pop() {
                Some(open) if open == name => {}
                other => return Err(format!("</{name}> closes {other:?}")),
            }
        } else {
            let name = tag.split([' ', '\n']).next().unwrap_or("").to_ascii_lowercase();
            if !VOID.contains(&name.as_str()) {
                stack.push(name);
            }
        }
    }
    if stack.is_empty() { Ok(()) } else { Err(format!("left open: {stack:?}")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::proto::*;

    fn run(text: &str) -> AttributeRun {
        AttributeRun { length: text.encode_utf16().count() as i32, ..Default::default() }
    }
    fn para_run(text: &str, style: i32, indent: i32) -> AttributeRun {
        AttributeRun {
            paragraph_style: Some(ParagraphStyle {
                style_type: Some(style),
                indent_amount: Some(indent),
                ..Default::default()
            }),
            ..run(text)
        }
    }
    fn html(parts: &[(&str, AttributeRun)]) -> (String, Counters) {
        let text: String = parts.iter().map(|p| p.0).collect();
        let runs: Vec<AttributeRun> = parts.iter().map(|p| p.1.clone()).collect();
        let mut c = Counters::new();
        let h = render_body(&paragraphs(&text, &runs, &[]), Opts::default(), &mut c);
        balanced(&h).unwrap_or_else(|e| panic!("{e}\n{h}"));
        (h, c)
    }

    #[test]
    fn text_is_escaped() {
        let t = "<script>alert(\"x\")</script> & 'q'\n";
        let (h, _) = html(&[(t, run(t))]);
        assert_eq!(h, "<p>&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;q&#39;</p>\n");
    }

    #[test]
    fn links_are_filtered() {
        for bad in ["javascript:alert(1)", " JaVaScRiPt:alert(1)", "java\tscript:alert(1)", "data:text/html,x", "/relative", "file:///c:/x"] {
            assert_eq!(safe_href(bad), None, "{bad:?}");
        }
        assert_eq!(safe_href("https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
        let l = AttributeRun { link: Some("https://e.com/\"onmouseover=\"x".into()), ..run("hi") };
        let (h, _) = html(&[("hi", l), ("\n", run("\n"))]);
        assert_eq!(h, "<p><a href=\"https://e.com/&quot;onmouseover=&quot;x\" rel=\"noopener noreferrer\">hi</a></p>\n");
        let js = AttributeRun { link: Some("javascript:alert(1)".into()), ..run("x") };
        let (h, c) = html(&[("x", js)]);
        assert!(!h.contains("href") && h.contains("link-off"), "{h}");
        assert_eq!(c["sanitize: link not rendered (other)"], 1);
    }

    #[test]
    fn one_anchor_across_runs_with_same_link() {
        let a = AttributeRun { link: Some("https://e.com".into()), ..run("a") };
        let b = AttributeRun { link: Some("https://e.com".into()), font_weight: Some(1), ..run("b") };
        let (h, _) = html(&[("a", a), ("b", b)]);
        assert_eq!(h, "<p><a href=\"https://e.com\" rel=\"noopener noreferrer\">a<b>b</b></a></p>\n");
    }

    #[test]
    fn nested_lists() {
        let (h, _) = html(&[
            ("a\n", para_run("a\n", 100, 0)),
            ("b\n", para_run("b\n", 100, 1)),
            ("c\n", para_run("c\n", 100, 1)),
            ("d\n", para_run("d\n", 100, 0)),
            ("e\n", para_run("e\n", 102, 0)),
            ("f\n", para_run("f\n", -1, 0)),
        ]);
        assert_eq!(
            h,
            "<ul><li>a<ul><li>b</li>\n<li>c</li></ul>\n</li>\n<li>d</li></ul>\n<ol><li>e</li></ol>\n<p>f</p>\n"
        );
    }

    #[test]
    fn list_starting_deep_gets_wrapper_li() {
        let (h, _) = html(&[("a\n", para_run("a\n", 101, 2))]);
        assert_eq!(h, "<ul class=\"dash\"><li class=\"nest\"><ul class=\"dash\"><li class=\"nest\"><ul class=\"dash\"><li>a</li></ul>\n</li></ul>\n</li></ul>\n");
    }

    #[test]
    fn checklists() {
        let mut done = para_run("x\n", 103, 0);
        done.paragraph_style.as_mut().unwrap().checklist = Some(Checklist { uuid: vec![1; 16], done: 1 });
        let mut todo = para_run("y\n", 103, 0);
        todo.paragraph_style.as_mut().unwrap().checklist = Some(Checklist { uuid: vec![2; 16], done: 0 });
        let (h, _) = html(&[("x\n", done), ("y\n", todo)]);
        assert_eq!(
            h,
            "<ul class=\"check\"><li class=\"done\"><span class=\"box\">\u{2611}</span> x</li>\n<li><span class=\"box\">\u{2610}</span> y</li></ul>\n"
        );
    }

    #[test]
    fn headings_mono_and_quote() {
        let mut q = run("q\n");
        q.paragraph_style = Some(ParagraphStyle { block_quote: Some(1), ..Default::default() });
        let (h, _) = html(&[
            ("T\n", para_run("T\n", 0, 0)),
            ("H\n", para_run("H\n", 1, 0)),
            ("m1\n", para_run("m1\n", 4, 0)),
            ("\n", para_run("\n", 4, 0)),
            ("m2\n", para_run("m2\n", 4, 0)),
            ("q\n", q),
            ("\n", run("\n")),
        ]);
        assert_eq!(h, "<h1>T</h1>\n<h2>H</h2>\n<pre>m1\n\nm2</pre>\n<blockquote>\n<p>q</p>\n</blockquote>\n<p><br></p>\n");
    }

    #[test]
    fn utf16_boundaries() {
        // Runs split around a non-BMP emoji (2 UTF-16 units).
        let (h, _) = html(&[
            ("a", AttributeRun { font_weight: Some(1), ..run("a") }),
            ("\u{1F95A}", AttributeRun { font_weight: Some(2), ..run("\u{1F95A}") }),
            ("b\n", run("b\n")),
        ]);
        assert_eq!(h, "<p><b>a</b><i>\u{1F95A}</i>b</p>\n");
        // A run ending mid-surrogate: still balanced, flagged.
        let text = "a\u{1F95A}b\n";
        let runs = vec![AttributeRun { length: 2, ..Default::default() }, AttributeRun { length: 3, ..Default::default() }];
        let mut c = Counters::new();
        let h = render_body(&paragraphs(text, &runs, &[]), Opts::default(), &mut c);
        balanced(&h).unwrap();
        assert_eq!(c["sanitize: U+FFFD in text"], 2);
    }

    #[test]
    fn attachments_and_bad_ids() {
        let att = |id: &str, uti: &str| AttributeRun {
            attachment_info: Some(AttachmentInfo {
                attachment_identifier: Some(id.into()),
                type_uti: Some(uti.into()),
            }),
            ..run("\u{FFFC}")
        };
        let (h, c) = html(&[
            ("\u{FFFC}", att("ABCD-1234", "com.apple.notes.table")),
            ("\u{FFFC}", att("\"><script>", "public.png\"x")),
        ]);
        assert_eq!(
            h,
            "<p><span class=\"att\" data-kind=\"table\" data-uti=\"com.apple.notes.table\" data-id=\"ABCD-1234\">[table]</span>\
             <span class=\"att\" data-kind=\"attachment\">[attachment]</span></p>\n"
        );
        assert_eq!(c["sanitize: attachment id dropped"], 1);
        assert_eq!(c["sanitize: attachment UTI dropped"], 1);
    }

    #[test]
    fn bidi_and_controls_stripped() {
        let t = "a\u{202E}b\u{0007}c\r\n";
        let (h, c) = html(&[(t, run(t))]);
        assert_eq!(h, "<p>abc</p>\n");
        assert_eq!(c["sanitize: bidi control stripped"], 1);
        assert_eq!(c["sanitize: control char stripped"], 2);
    }

    #[test]
    fn colour_is_clamped() {
        let r = AttributeRun {
            color: Some(Color { red: 2.0, green: f32::NAN, blue: 0.5, alpha: -1.0 }),
            ..run("x")
        };
        let (h, _) = html(&[("x", r)]);
        assert_eq!(h, "<p><span style=\"color:rgba(255,0,128,0.30)\">x</span></p>\n");
    }

    #[test]
    fn last_paragraph_without_newline_and_mixed() {
        let text = "ab";
        let runs = vec![para_run("a", 1, 0), para_run("b", -1, 0)];
        let p = paragraphs(text, &runs, &[]);
        assert_eq!(p.len(), 1);
        assert!(p[0].mixed);
        assert_eq!(p[0].attrs.style, -1);
    }
}
