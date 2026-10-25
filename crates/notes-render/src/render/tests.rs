use std::cell::RefCell;

use notes_proto::Table;
use notes_proto::proto::*;

use super::*;

struct Fake {
    seen: RefCell<Vec<(String, String)>>,
    view: AttachmentView,
}

impl AttachmentResolver for Fake {
    fn resolve(&self, id: &str, uti: &str) -> AttachmentView {
        self.seen.borrow_mut().push((id.into(), uti.into()));
        self.view.clone()
    }
}

fn fake(view: AttachmentView) -> Fake {
    Fake { seen: RefCell::new(Vec::new()), view }
}

fn run(text: &str) -> AttributeRun {
    AttributeRun { length: text.encode_utf16().count() as i32, ..Default::default() }
}
fn para_run(text: &str, style: i32, indent: i32) -> AttributeRun {
    AttributeRun {
        paragraph_style: Some(ParagraphStyle { style_type: Some(style), indent_amount: Some(indent), ..Default::default() }),
        ..run(text)
    }
}
fn note(parts: &[(&str, AttributeRun)]) -> Note {
    Note {
        note_text: parts.iter().map(|p| p.0).collect(),
        attribute_run: parts.iter().map(|p| p.1.clone()).collect(),
    }
}
fn html_with(parts: &[(&str, AttributeRun)], r: &dyn AttachmentResolver) -> String {
    let h = render_note(&note(parts), r);
    balanced(&h).unwrap_or_else(|e| panic!("{e}\n{h}"));
    h
}
fn html(parts: &[(&str, AttributeRun)]) -> String {
    html_with(parts, &fake(AttachmentView::Placeholder { kind: "x".into() }))
}
fn att(id: &str, uti: &str) -> AttributeRun {
    AttributeRun {
        attachment_info: Some(AttachmentInfo { attachment_identifier: Some(id.into()), type_uti: Some(uti.into()) }),
        ..run("\u{FFFC}")
    }
}

/// Check that our own output nests correctly (text is escaped, so every `<` starts a tag).
fn balanced(html: &str) -> Result<(), String> {
    const VOID: &[&str] = &["br", "meta", "img", "base", "!doctype"];
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

#[test]
fn text_is_escaped() {
    let t = "<script>alert(\"x\")</script> & 'q'\n";
    assert_eq!(html(&[(t, run(t))]), "<p>&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;q&#39;</p>\n");
}

#[test]
fn links_are_filtered() {
    for bad in ["javascript:alert(1)", " JaVaScRiPt:alert(1)", "java\tscript:alert(1)", "data:text/html,x", "/relative", "file:///c:/x"] {
        assert_eq!(safe_href(bad), None, "{bad:?}");
    }
    assert_eq!(safe_href("https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
    let l = AttributeRun { link: Some("https://e.com/\"onmouseover=\"x".into()), ..run("hi") };
    assert_eq!(
        html(&[("hi", l), ("\n", run("\n"))]),
        "<p><a href=\"https://e.com/&quot;onmouseover=&quot;x\" rel=\"noopener noreferrer\">hi</a></p>\n"
    );
    let js = AttributeRun { link: Some("javascript:alert(1)".into()), ..run("x") };
    let h = html(&[("x", js)]);
    assert!(!h.contains("href") && h.contains("link-off"), "{h}");
}

#[test]
fn one_anchor_across_runs_with_same_link() {
    let a = AttributeRun { link: Some("https://e.com".into()), ..run("a") };
    let b = AttributeRun { link: Some("https://e.com".into()), font_weight: Some(1), ..run("b") };
    assert_eq!(html(&[("a", a), ("b", b)]), "<p><a href=\"https://e.com\" rel=\"noopener noreferrer\">a<b>b</b></a></p>\n");
}

#[test]
fn nested_lists() {
    let h = html(&[
        ("a\n", para_run("a\n", 100, 0)),
        ("b\n", para_run("b\n", 100, 1)),
        ("c\n", para_run("c\n", 100, 1)),
        ("d\n", para_run("d\n", 100, 0)),
        ("e\n", para_run("e\n", 102, 0)),
        ("f\n", para_run("f\n", -1, 0)),
    ]);
    assert_eq!(h, "<ul><li>a<ul><li>b</li>\n<li>c</li></ul>\n</li>\n<li>d</li></ul>\n<ol><li>e</li></ol>\n<p>f</p>\n");
}

#[test]
fn list_starting_deep_gets_wrapper_li() {
    let h = html(&[("a\n", para_run("a\n", 101, 2))]);
    assert_eq!(h, "<ul class=\"dash\"><li class=\"nest\"><ul class=\"dash\"><li class=\"nest\"><ul class=\"dash\"><li>a</li></ul>\n</li></ul>\n</li></ul>\n");
}

#[test]
fn checklists() {
    let mut done = para_run("x\n", 103, 0);
    done.paragraph_style.as_mut().unwrap().checklist = Some(Checklist { uuid: vec![1; 16], done: 1 });
    let mut todo = para_run("y\n", 103, 0);
    todo.paragraph_style.as_mut().unwrap().checklist = Some(Checklist { uuid: vec![2; 16], done: 0 });
    assert_eq!(
        html(&[("x\n", done), ("y\n", todo)]),
        "<ul class=\"check\"><li class=\"done\"><span class=\"box\">\u{2611}</span> x</li>\n<li><span class=\"box\">\u{2610}</span> y</li></ul>\n"
    );
}

#[test]
fn headings_mono_and_quote() {
    let mut q = run("q\n");
    q.paragraph_style = Some(ParagraphStyle { block_quote: Some(1), ..Default::default() });
    let h = html(&[
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
    let h = html(&[
        ("a", AttributeRun { font_weight: Some(1), ..run("a") }),
        ("\u{1F95A}", AttributeRun { font_weight: Some(2), ..run("\u{1F95A}") }),
        ("b\n", run("b\n")),
    ]);
    assert_eq!(h, "<p><b>a</b><i>\u{1F95A}</i>b</p>\n");
    // A run ending mid-surrogate: still balanced, no panic.
    let n = Note {
        note_text: "a\u{1F95A}b\n".into(),
        attribute_run: vec![AttributeRun { length: 2, ..Default::default() }, AttributeRun { length: 3, ..Default::default() }],
    };
    let h = render_note(&n, &fake(AttachmentView::Text(String::new())));
    balanced(&h).unwrap();
    assert_eq!(h.matches('\u{FFFD}').count(), 2);
}

#[test]
fn attachment_ids_are_checked_before_resolving() {
    let r = fake(AttachmentView::Placeholder { kind: "table".into() });
    let h = html_with(
        &[("\u{FFFC}", att("ABCD-1234", "com.apple.notes.table")), ("\u{FFFC}", att("\"><script>", "public.png"))],
        &r,
    );
    assert_eq!(
        h,
        "<p><span class=\"att\" data-kind=\"table\">[table]</span><span class=\"att\" data-kind=\"image\">[image]</span></p>\n"
    );
    // Only the well-formed id reached the resolver.
    assert_eq!(*r.seen.borrow(), vec![("ABCD-1234".to_string(), "com.apple.notes.table".to_string())]);
    // A bad UTI is passed as empty.
    let r = fake(AttachmentView::Text("t".into()));
    html_with(&[("\u{FFFC}", att("A-1", "public.png\"x"))], &r);
    assert_eq!(r.seen.borrow()[0].1, "");
}

#[test]
fn bidi_and_controls_stripped() {
    let t = "a\u{202E}b\u{0007}c\r\n";
    assert_eq!(html(&[(t, run(t))]), "<p>abc</p>\n");
}

#[test]
fn colour_is_clamped() {
    let r = AttributeRun { color: Some(Color { red: 2.0, green: f32::NAN, blue: 0.5, alpha: -1.0 }), ..run("x") };
    assert_eq!(html(&[("x", r)]), "<p><span style=\"color:rgba(255,0,128,0.30)\">x</span></p>\n");
}

#[test]
fn near_default_colours_are_dropped() {
    let grey = |v: f32| AttributeRun { color: Some(Color { red: v, green: v, blue: v, alpha: 1.0 }), ..run("x") };
    // #333333 and near-white vanish in one of the themes: drop them.
    assert_eq!(html(&[("x", grey(0.2))]), "<p>x</p>\n");
    assert_eq!(html(&[("x", grey(0.95))]), "<p>x</p>\n");
    // Mid grey and real colours stay.
    assert!(html(&[("x", grey(0.5))]).contains("color:rgba(128,128,128,1.00)"));
    let red = AttributeRun { color: Some(Color { red: 0.8, green: 0.0, blue: 0.0, alpha: 1.0 }), ..run("x") };
    assert!(html(&[("x", red)]).contains("color:rgba(204,0,0,1.00)"));
}

#[test]
fn last_paragraph_without_newline() {
    let runs = vec![para_run("a", 1, 0), para_run("b", -1, 0)];
    let p = paragraphs("ab", &runs);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].attrs.style, -1);
}

#[test]
fn image_attachment() {
    let r = fake(AttachmentView::Image {
        src: "http://notes-asset.localhost/ABC-1/full".into(),
        w: Some(640),
        h: None,
        alt: "a \"pic\"".into(),
    });
    let h = html_with(&[("\u{FFFC}", att("ABC-1", "public.png")), ("\n", run("\n"))], &r);
    assert_eq!(
        h,
        "<p><img loading=\"lazy\" src=\"http://notes-asset.localhost/ABC-1/full\" alt=\"a &quot;pic&quot;\" width=\"640\"></p>\n"
    );
    // A src that isn't our scheme never reaches the page.
    let r = fake(AttachmentView::Image { src: "https://evil/x.png".into(), w: None, h: None, alt: String::new() });
    let h = html_with(&[("\u{FFFC}", att("ABC-1", "public.png"))], &r);
    assert!(!h.contains("evil") && h.contains("[image]"), "{h}");
}

#[test]
fn text_attachment_is_escaped() {
    let r = fake(AttachmentView::Text("#<b>tag".into()));
    let h = html_with(&[("see ", run("see ")), ("\u{FFFC}", att("H-1", "com.apple.notes.inlinetextattachment.hashtag"))], &r);
    assert_eq!(h, "<p>see <span class=\"inl\">#&lt;b&gt;tag</span></p>\n");
}

#[test]
fn table_cells_use_inline_renderer() {
    let bold = AttributeRun { font_weight: Some(1), ..run("hi") };
    let cell = |parts: &[(&str, AttributeRun)]| Some(note(parts));
    let t = Table {
        rows: vec![
            vec![cell(&[("hi", bold)]), None],
            vec![
                cell(&[("<x>\n", run("<x>\n")), ("y", run("y"))]),
                cell(&[("l", AttributeRun { link: Some("javascript:1".into()), ..run("l") })]),
            ],
        ],
    };
    let r = fake(AttachmentView::Table(t));
    let h = html_with(&[("\u{FFFC}", att("T-1", "com.apple.notes.table")), ("\n", run("\n")), ("after\n", run("after\n"))], &r);
    assert_eq!(
        h,
        "<div><table><tr><td><b>hi</b></td><td></td></tr>\
         <tr><td>&lt;x&gt;<br>y</td><td><span class=\"link-off\" title=\"link not opened (other)\">l</span></td></tr></table></div>\n\
         <p>after</p>\n"
    );
}

#[test]
fn nested_table_depth_is_capped() {
    // The resolver always returns a table whose single cell holds another table attachment.
    let inner = note(&[("\u{FFFC}", att("T-2", "com.apple.notes.table"))]);
    let r = fake(AttachmentView::Table(Table { rows: vec![vec![Some(inner)]] }));
    let mut a = att("T-1", "com.apple.notes.table");
    a.paragraph_style = Some(ParagraphStyle { style_type: Some(100), ..Default::default() });
    let h = html_with(&[("\u{FFFC}", a)], &r);
    assert_eq!(h.matches("<table>").count(), MAX_TABLE_DEPTH, "{h}");
    assert!(h.contains("[table]"), "{h}");
}

#[test]
fn page_wrapper() {
    let p = page("<p>x</p>", true);
    assert!(p.contains("Content-Security-Policy") && p.contains("default-src 'none'"));
    assert!(p.contains("<base target=\"_blank\">") && p.contains("class=\"dark\"") && p.contains("<p>x</p>"));
    assert!(!page("", false).contains("class=\"dark\""));
}
