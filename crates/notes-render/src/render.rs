//! Decoded note (text + attribute runs) → paragraphs → sanitized HTML.
//!
//! Nothing from the note is ever passed through as markup: every tag comes from this file,
//! and all note text / attribute values go through `push_text` / `attr`.

use notes_proto::proto::{AttributeRun, Color};

use crate::{AttachmentResolver, AttachmentView};

/// Deepest list / indent level rendered; deeper levels are clamped.
const MAX_DEPTH: usize = 8;
/// Tables nested inside table cells are not expanded beyond this depth.
const MAX_TABLE_DEPTH: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListKind {
    Bullet,
    Dash,
    Numbered,
    Check,
}

impl ListKind {
    fn of(a: &ParaAttrs) -> Option<Self> {
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
struct ParaAttrs {
    style: i32,
    indent: i32,
    align: i32,
    quote: bool,
    /// Some(done) for checklist items.
    check: Option<bool>,
}

impl ParaAttrs {
    fn body() -> Self {
        Self { style: -1, indent: 0, align: 0, quote: false, check: None }
    }
    fn of(run: &AttributeRun) -> Self {
        let ps = run.paragraph_style.as_ref();
        Self {
            style: ps.and_then(|p| p.style_type).unwrap_or(-1),
            indent: ps.and_then(|p| p.indent_amount).unwrap_or(0),
            align: ps.and_then(|p| p.alignment).unwrap_or(0),
            quote: ps.and_then(|p| p.block_quote).unwrap_or(0) != 0,
            check: ps.and_then(|p| p.checklist.as_ref()).map(|c| c.done != 0),
        }
    }
}

/// Part of one run that falls inside one paragraph.
struct Seg<'a> {
    text: String,
    run: &'a AttributeRun,
}

struct Para<'a> {
    attrs: ParaAttrs,
    segs: Vec<Seg<'a>>,
}

/// Cut `text` into one string per run. Run lengths are UTF-16 units; a cut inside a surrogate pair
/// yields U+FFFD instead of panicking.
fn spans<'a>(text: &str, runs: &'a [AttributeRun]) -> Vec<Seg<'a>> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let len = run.length.max(0) as usize;
        let end = pos.saturating_add(len).min(units.len());
        let slice = String::from_utf16_lossy(&units[pos.min(end)..end]);
        out.push(Seg { text: slice, run });
        pos = pos.saturating_add(len);
    }
    out
}

/// Split runs at `\n` into paragraphs.
fn paragraphs<'a>(text: &str, runs: &'a [AttributeRun]) -> Vec<Para<'a>> {
    let mut out = Vec::new();
    let mut segs = Vec::new();
    let mut last_attrs: Option<ParaAttrs> = None;
    for s in spans(text, runs) {
        let a = ParaAttrs::of(s.run);
        let mut rest = s.text.as_str();
        loop {
            match rest.find('\n') {
                Some(n) => {
                    if n > 0 {
                        segs.push(Seg { text: rest[..n].to_string(), run: s.run });
                    }
                    out.push(Para { attrs: a.clone(), segs: std::mem::take(&mut segs) });
                    last_attrs = None;
                    rest = &rest[n + 1..];
                }
                None => {
                    if !rest.is_empty() {
                        segs.push(Seg { text: rest.to_string(), run: s.run });
                        last_attrs = Some(a.clone());
                    }
                    break;
                }
            }
        }
    }
    if !segs.is_empty() {
        // Last paragraph has no `\n`: use its last run's attributes.
        out.push(Para { attrs: last_attrs.unwrap_or_else(ParaAttrs::body), segs });
    }
    out
}

// ---------- sanitizing ----------

/// Escape note text for an HTML text node, dropping characters that can mislead or break layout.
fn push_text(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' => out.push('\t'),
            '\u{2028}' | '\u{2029}' => out.push_str("<br>"),
            // Bidi controls (spoofing) and stray object-replacement characters.
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FFFC}' => {}
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
}

/// Escape a value for a double-quoted attribute. Control characters are dropped.
fn attr(s: &str) -> String {
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
fn link_scheme(raw: &str) -> String {
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

/// Content-free bucket, used as a title on links that are not rendered.
fn link_bucket(raw: &str) -> &'static str {
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
fn safe_href(raw: &str) -> Option<String> {
    let scheme = link_scheme(raw);
    if !matches!(scheme.as_str(), "http" | "https" | "mailto") || raw.chars().any(char::is_control) {
        return None;
    }
    Some(raw.trim().to_string())
}

/// Colours that would be (nearly) invisible against the page background in one of the two themes are
/// dropped: grey-ish and either very dark or very light. The theme's own text colour is used instead.
fn css_color(c: &Color) -> Option<String> {
    let ch = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let (r, g, b) = (ch(c.red), ch(c.green), ch(c.blue));
    let chroma = r.max(g).max(b) - r.min(g).min(b);
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if chroma < 0.12 && !(0.3..=0.8).contains(&lum) {
        return None;
    }
    // Floor alpha so a colour can't make text invisible.
    let a = if c.alpha.is_finite() { c.alpha.clamp(0.3, 1.0) } else { 1.0 };
    let byte = |v: f32| (v * 255.0).round() as u8;
    Some(format!("rgba({},{},{},{:.2})", byte(r), byte(g), byte(b), a))
}

fn ok_token(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

fn attachment_kind(uti: &str) -> &'static str {
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

// ---------- attachments ----------

fn push_placeholder(out: &mut String, kind: &str) {
    // Kind is a short label; keep only a safe token for the attribute.
    let kind = if ok_token(kind, "-", 32) { kind } else { "attachment" };
    out.push_str("<span class=\"att\" data-kind=\"");
    out.push_str(kind);
    out.push_str("\">[");
    out.push_str(kind);
    out.push_str("]</span>");
}

/// Emit one attachment. Returns true when block-level markup (a table) was written.
fn push_attachment(out: &mut String, run: &AttributeRun, r: &dyn AttachmentResolver, depth: usize) -> bool {
    let info = run.attachment_info.as_ref();
    let uti = info.and_then(|a| a.type_uti.as_deref()).unwrap_or("");
    let id = info.and_then(|a| a.attachment_identifier.as_deref()).unwrap_or("");
    let kind = attachment_kind(uti);
    // Ids and UTIs come from the note: they only reach the resolver if they look like ids.
    if !ok_token(id, "-", 64) {
        push_placeholder(out, kind);
        return false;
    }
    let uti = if ok_token(uti, ".-", 128) { uti } else { "" };
    match r.resolve(id, uti) {
        AttachmentView::Image { src, w, h, alt } => {
            if !src.starts_with("notes-asset://localhost/") {
                push_placeholder(out, "image");
                return false;
            }
            out.push_str("<img loading=\"lazy\" src=\"");
            out.push_str(&attr(&src));
            out.push_str("\" alt=\"");
            out.push_str(&attr(&alt));
            out.push('"');
            if let Some(w) = w {
                out.push_str(&format!(" width=\"{w}\""));
            }
            if let Some(h) = h {
                out.push_str(&format!(" height=\"{h}\""));
            }
            out.push('>');
            false
        }
        AttachmentView::Table(t) => {
            if depth >= MAX_TABLE_DEPTH {
                push_placeholder(out, "table");
                return false;
            }
            out.push_str("<table>");
            for row in &t.rows {
                out.push_str("<tr>");
                for cell in row {
                    out.push_str("<td>");
                    if let Some(n) = cell {
                        push_cell(out, n, r, depth + 1);
                    }
                    out.push_str("</td>");
                }
                out.push_str("</tr>");
            }
            out.push_str("</table>");
            true
        }
        AttachmentView::Text(s) => {
            out.push_str("<span class=\"inl\">");
            push_text(out, &s);
            out.push_str("</span>");
            false
        }
        AttachmentView::Placeholder { kind } => {
            push_placeholder(out, &kind);
            false
        }
    }
}

/// A table cell: the inline renderer on each paragraph, paragraphs separated by `<br>`.
fn push_cell(out: &mut String, note: &notes_proto::proto::Note, r: &dyn AttachmentResolver, depth: usize) {
    let paras = paragraphs(&note.note_text, &note.attribute_run);
    for (i, p) in paras.iter().enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        let mut inner = String::new();
        push_inline(&mut inner, p, r, depth);
        // An empty paragraph produces a bare <br>, which the separator already covers.
        if inner != "<br>" {
            out.push_str(&inner);
        }
    }
}

// ---------- HTML ----------

fn push_seg(out: &mut String, seg: &Seg, r: &dyn AttachmentResolver, depth: usize) -> bool {
    let run = seg.run;
    if run.attachment_info.is_some() {
        let block = push_attachment(out, run, r, depth);
        if seg.text != "\u{FFFC}" {
            push_text(out, &seg.text);
        }
        return block;
    }
    let mut close: Vec<&str> = Vec::new();
    let mut open = |out: &mut String, o: &str, cl: &'static str| {
        out.push_str(o);
        close.push(cl);
    };
    match run.font_weight.unwrap_or(0) {
        1 => open(out, "<b>", "</b>"),
        2 => open(out, "<i>", "</i>"),
        3 => {
            open(out, "<b>", "</b>");
            open(out, "<i>", "</i>");
        }
        _ => {}
    }
    if run.underlined.unwrap_or(0) != 0 {
        open(out, "<u>", "</u>");
    }
    if run.strikethrough.unwrap_or(0) != 0 {
        open(out, "<s>", "</s>");
    }
    match run.superscript.unwrap_or(0) {
        s if s > 0 => open(out, "<sup>", "</sup>"),
        s if s < 0 => open(out, "<sub>", "</sub>"),
        _ => {}
    }
    if let Some(e) = run.emphasis_style {
        open(out, &format!("<mark class=\"em{}\">", e.clamp(0, 99)), "</mark>");
    }
    if let Some(col) = run.color.as_ref().and_then(css_color) {
        open(out, &format!("<span style=\"color:{col}\">"), "</span>");
    }
    push_text(out, &seg.text);
    for cl in close.iter().rev() {
        out.push_str(cl);
    }
    false
}

/// Inline content of one paragraph; consecutive segments with the same link share one `<a>`.
/// Returns true if block-level markup (a table) was emitted.
fn push_inline(out: &mut String, p: &Para, r: &dyn AttachmentResolver, depth: usize) -> bool {
    if p.segs.is_empty() {
        out.push_str("<br>");
        return false;
    }
    let mut block = false;
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
                        let class = if b == "applenotes" || b == "mobilenotes" { "notelink" } else { "link-off" };
                        out.push_str(&format!("<span class=\"{class}\" title=\"link not opened ({})\">", attr(b)));
                        close_a = "</span>";
                    }
                }
            }
            cur = link;
        }
        block |= push_seg(out, seg, r, depth);
    }
    out.push_str(close_a);
    block
}

fn block_attrs(p: &Para, list: bool) -> String {
    let a = &p.attrs;
    let mut class = Vec::new();
    if !list && a.indent > 0 {
        class.push(format!("ind{}", (a.indent as usize).min(MAX_DEPTH)));
    }
    match a.align {
        // NSTextAlignment on iOS: 0 left, 1 center, 2 right, 3 justified, 4 natural.
        1 => class.push("center".to_string()),
        2 => class.push("right".to_string()),
        3 => class.push("justify".to_string()),
        _ => {}
    }
    if a.check == Some(true) {
        class.push("done".to_string());
    }
    if class.is_empty() { String::new() } else { format!(" class=\"{}\"", class.join(" ")) }
}

struct W<'r> {
    out: String,
    /// Open lists, innermost last: (kind, an <li> is open).
    lists: Vec<(ListKind, bool)>,
    quote: bool,
    pre: bool,
    resolver: &'r dyn AttachmentResolver,
}

impl W<'_> {
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
    fn list_item(&mut self, k: ListKind, p: &Para) {
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
        self.out.push_str(&format!("<li{}>", block_attrs(p, true)));
        if let Some(done) = p.attrs.check {
            self.out.push_str(if done { "<span class=\"box\">\u{2611}</span> " } else { "<span class=\"box\">\u{2610}</span> " });
        }
        push_inline(&mut self.out, p, self.resolver, 0);
    }
}

fn render_paras(paras: &[Para], resolver: &dyn AttachmentResolver) -> String {
    let mut w = W { out: String::new(), lists: Vec::new(), quote: false, pre: false, resolver };
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
            w.list_item(k, p);
            continue;
        }
        w.close_lists_to(0);
        if mono {
            if w.pre {
                w.out.push('\n');
            } else {
                w.out.push_str(&format!("<pre{}>", block_attrs(p, false)));
                w.pre = true;
            }
            // <br> for an empty line would double up inside <pre>.
            if !p.segs.is_empty() {
                push_inline(&mut w.out, p, resolver, 0);
            }
            continue;
        }
        let tag = match a.style {
            0 => "h1",
            1 => "h2",
            2 => "h3",
            _ => "p",
        };
        let mut inner = String::new();
        let has_table = push_inline(&mut inner, p, resolver, 0);
        // A table cannot live inside <p>/<h*>: the parser would close the paragraph around it.
        let tag = if has_table { "div" } else { tag };
        w.out.push_str(&format!("<{tag}{}>{inner}</{tag}>\n", block_attrs(p, false)));
    }
    w.close_pre();
    w.close_lists_to(0);
    if w.quote {
        w.out.push_str("</blockquote>\n");
    }
    w.out
}

pub fn render_note(note: &notes_proto::proto::Note, resolver: &dyn AttachmentResolver) -> String {
    render_paras(&paragraphs(&note.note_text, &note.attribute_run), resolver)
}

// ---------- page ----------

const CSS: &str = r#"
:root { --fg:#1d1d1f; --bg:#fff; --muted:#86868b; --att:#f2f2f7; --mark:#fff3a3; --line:#d2d2d7; --link:#0a64d0; }
:root.dark { --fg:#f5f5f7; --bg:#1c1c1e; --muted:#98989d; --att:#2c2c2e; --mark:#6b5d00; --line:#48484a; --link:#6cb4ff; }
:root { color-scheme: light; } :root.dark { color-scheme: dark; }
body { font: 16px/1.5 -apple-system, "Segoe UI", system-ui, sans-serif; color: var(--fg); background: var(--bg);
       max-width: 46rem; margin: 2rem auto; padding: 0 1rem; overflow-wrap: anywhere; }
a { color: var(--link); }
h1 { font-size: 1.75em; margin: .2em 0 .4em; } h2 { font-size: 1.35em; margin: .8em 0 .3em; }
h3 { font-size: 1.1em; margin: .7em 0 .2em; } p, div { margin: 0; min-height: 1.5em; }
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
img { max-width: 100%; height: auto; border-radius: 4px; vertical-align: middle; }
table { border-collapse: collapse; margin: .5em 0; max-width: 100%; }
td { border: 1px solid var(--line); padding: .3em .6em; vertical-align: top; min-width: 3em; }
"#;

/// A standalone page. The CSP blocks scripts, fonts and network; images only from our asset scheme.
pub fn page(fragment: &str, dark: bool) -> String {
    let mut s = String::from(if dark { "<!doctype html>\n<html class=\"dark\"><head><meta charset=\"utf-8\">\n" } else { "<!doctype html>\n<html><head><meta charset=\"utf-8\">\n" });
    s.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src notes-asset: http://notes-asset.localhost\">\n");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<base target=\"_blank\">\n<style>");
    s.push_str(CSS);
    s.push_str("</style></head>\n<body>\n");
    s.push_str(fragment);
    s.push_str("</body></html>\n");
    s
}

#[cfg(test)]
mod tests;
