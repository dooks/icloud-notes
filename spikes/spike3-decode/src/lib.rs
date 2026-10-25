//! Spike 3: CloudKit `TextDataEncrypted` -> base64 -> gzip/zlib -> NoteStoreProto -> text + runs.

use std::collections::BTreeMap;
use std::io::Read;

use base64::Engine;
use prost::Message;

pub mod proto {
    // notestore.proto has no `package`, so prost writes `_.rs`.
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

use proto::{AttributeRun, NoteStoreProto};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Zlib,
    None,
}

pub struct Decoded {
    pub compression: Compression,
    pub compressed_len: usize,
    pub inflated: Vec<u8>,
    pub store: NoteStoreProto,
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    let e = base64::engine::general_purpose::STANDARD;
    e.decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.trim_end_matches('=')))
        .map_err(|e| format!("base64: {e}"))
}

/// Sniff the magic bytes and inflate. Falls through to "no compression" for anything else.
pub fn inflate(raw: &[u8]) -> Result<(Compression, Vec<u8>), String> {
    let mut out = Vec::new();
    match raw {
        [0x1f, 0x8b, ..] => {
            // MultiGzDecoder: tolerate concatenated members.
            flate2::read::MultiGzDecoder::new(raw)
                .read_to_end(&mut out)
                .map_err(|e| format!("gunzip: {e}"))?;
            Ok((Compression::Gzip, out))
        }
        [0x78, b, ..] if (0x7800u16 | *b as u16) % 31 == 0 => {
            flate2::read::ZlibDecoder::new(raw)
                .read_to_end(&mut out)
                .map_err(|e| format!("zlib: {e}"))?;
            Ok((Compression::Zlib, out))
        }
        _ => Ok((Compression::None, raw.to_vec())),
    }
}

pub fn decode_bytes(raw: &[u8]) -> Result<Decoded, String> {
    let (compression, inflated) = inflate(raw)?;
    let store = NoteStoreProto::decode(inflated.as_slice()).map_err(|e| format!("protobuf: {e}"))?;
    Ok(Decoded { compression, compressed_len: raw.len(), inflated, store })
}

pub fn decode_b64(s: &str) -> Result<Decoded, String> {
    decode_bytes(&b64_decode(s)?)
}

/// Encode a NoteStoreProto the way we expect Apple to: protobuf -> gzip -> base64.
pub fn encode_b64(store: &NoteStoreProto) -> String {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&store.encode_to_vec()).unwrap();
    base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap())
}

// ---------- attribute runs ----------

/// Paragraph style_type values as documented by apple_cloud_notes_parser.
pub fn style_name(t: i32) -> String {
    match t {
        -1 => "body".into(),
        0 => "title".into(),
        1 => "heading".into(),
        2 => "subheading".into(),
        4 => "monospaced".into(),
        100 => "bullet-list".into(),
        101 => "dash-list".into(),
        102 => "numbered-list".into(),
        103 => "checklist".into(),
        other => format!("style#{other}"),
    }
}

/// One run with its position. Offsets are UTF-16 code units (NSString semantics).
pub struct Span<'a> {
    pub start: usize,
    pub run: &'a AttributeRun,
    pub text: String,
}

pub fn spans<'a>(text: &str, runs: &'a [AttributeRun]) -> (Vec<Span<'a>>, usize) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let len = run.length.max(0) as usize;
        let end = (pos + len).min(units.len());
        let slice = String::from_utf16_lossy(&units[pos.min(end)..end]);
        out.push(Span { start: pos, run, text: slice });
        pos += len;
    }
    (out, units.len())
}

/// Short, content-free description of a run's attributes.
pub fn describe(run: &AttributeRun) -> String {
    let mut parts = Vec::new();
    if let Some(ps) = &run.paragraph_style {
        let st = ps.style_type.unwrap_or(-1);
        if st != -1 {
            parts.push(style_name(st));
        }
        if let Some(c) = &ps.checklist {
            parts.push(if c.done != 0 { "[x]".into() } else { "[_]".into() });
        }
        if let Some(i) = ps.indent_amount.filter(|i| *i != 0) {
            parts.push(format!("indent={i}"));
        }
        if let Some(a) = ps.alignment.filter(|a| *a != 0) {
            parts.push(format!("align={a}"));
        }
        if ps.block_quote.unwrap_or(0) != 0 {
            parts.push("quote".into());
        }
    }
    match run.font_weight {
        Some(1) => parts.push("bold".into()),
        Some(2) => parts.push("italic".into()),
        Some(3) => parts.push("bold+italic".into()),
        Some(w) if w != 0 => parts.push(format!("weight={w}")),
        _ => {}
    }
    if run.underlined.unwrap_or(0) != 0 {
        parts.push("underline".into());
    }
    if run.strikethrough.unwrap_or(0) != 0 {
        parts.push("strike".into());
    }
    if let Some(s) = run.superscript.filter(|s| *s != 0) {
        parts.push(if s > 0 { "super".into() } else { "sub".into() });
    }
    if run.link.is_some() {
        parts.push("link".into());
    }
    if run.color.is_some() {
        parts.push("color".into());
    }
    if let Some(f) = &run.font {
        parts.push(format!("font({})", f.font_name.as_deref().unwrap_or("?")));
    }
    if let Some(e) = run.emphasis_style {
        parts.push(format!("emphasis={e}"));
    }
    if let Some(a) = &run.attachment_info {
        parts.push(format!("attachment({})", a.type_uti.as_deref().unwrap_or("?")));
    }
    if parts.is_empty() {
        "plain".into()
    } else {
        parts.join(" ")
    }
}

// ---------- raw wire walk (to spot fields the schema doesn't know) ----------

fn varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*i)?;
        *i += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Top-level fields of one message: (tag, wire type, payload if length-delimited).
pub fn wire_fields(buf: &[u8]) -> Option<Vec<(u32, u8, Option<&[u8]>)>> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < buf.len() {
        let key = varint(buf, &mut i)?;
        let (tag, wt) = ((key >> 3) as u32, (key & 7) as u8);
        match wt {
            0 => {
                varint(buf, &mut i)?;
                out.push((tag, wt, None));
            }
            1 => {
                i += 8;
                out.push((tag, wt, None));
            }
            2 => {
                let n = varint(buf, &mut i)? as usize;
                let p = buf.get(i..i.checked_add(n)?)?;
                i += n;
                out.push((tag, wt, Some(p)));
            }
            5 => {
                i += 4;
                out.push((tag, wt, None));
            }
            _ => return None,
        }
    }
    (i == buf.len()).then_some(out)
}

/// Scan forward from `*i` to the next varint field with `tag`; return its value.
fn nth_varint(buf: &[u8], i: &mut usize, tag: u32) -> Option<u64> {
    while *i < buf.len() {
        let key = varint(buf, i)?;
        let (t, wt) = ((key >> 3) as u32, key & 7);
        match wt {
            0 => {
                let v = varint(buf, i)?;
                if t == tag {
                    return Some(v);
                }
            }
            1 => *i += 8,
            2 => *i += varint(buf, i)? as usize,
            5 => *i += 4,
            _ => return None,
        }
    }
    None
}

/// Per-run varints that prost drops: ParagraphStyle fields 3 and 7 (not in the schema).
/// Walks NoteStoreProto.2 -> Document.3 -> Note.5[] -> AttributeRun.2 on the raw bytes.
pub fn run_extras(inflated: &[u8]) -> Vec<String> {
    let sub = |buf: &[u8], tag: u32| -> Option<Vec<u8>> {
        wire_fields(buf)?.into_iter().find(|f| f.0 == tag).and_then(|f| f.2.map(<[u8]>::to_vec))
    };
    let Some(note) = sub(inflated, 2).and_then(|d| sub(&d, 3)) else { return vec![] };
    let Some(fields) = wire_fields(&note) else { return vec![] };
    fields
        .into_iter()
        .filter(|f| f.0 == 5)
        .map(|(_, _, run)| {
            let Some(ps) = run.and_then(|r| sub(r, 2)) else { return String::new() };
            let mut parts = Vec::new();
            for tag in [3u32, 7] {
                if let Some(v) = nth_varint(&ps, &mut 0, tag) {
                    parts.push(format!("ps{tag}={v}"));
                }
            }
            parts.join(" ")
        })
        .collect()
}

/// Known tags per message, from notestore.proto.
const KNOWN: &[(&str, &[u32])] = &[
    ("NoteStoreProto", &[2]),
    ("Document", &[2, 3]),
    ("Note", &[2, 5]),
    ("AttributeRun", &[1, 2, 3, 5, 6, 7, 8, 9, 10, 12, 13, 14]),
    ("ParagraphStyle", &[1, 2, 4, 5, 8]),
    ("Checklist", &[1, 2]),
    ("Font", &[1, 2, 3]),
    ("AttachmentInfo", &[1, 2]),
    ("Color", &[1, 2, 3, 4]),
];

fn child(msg: &str, tag: u32) -> Option<&'static str> {
    Some(match (msg, tag) {
        ("NoteStoreProto", 2) => "Document",
        ("Document", 3) => "Note",
        ("Note", 5) => "AttributeRun",
        ("AttributeRun", 2) => "ParagraphStyle",
        ("AttributeRun", 3) => "Font",
        ("AttributeRun", 10) => "Color",
        ("AttributeRun", 12) => "AttachmentInfo",
        ("ParagraphStyle", 5) => "Checklist",
        // Not in the schema: walk them for tag numbers only.
        // ParagraphStyle.9 is always 16 bytes (a UUID, not a message), so it isn't walked.
        ("Note", 3) => "Note.3?",
        ("Note", 4) => "Note.4?",
        _ => return None,
    })
}

/// Count every (message, tag, wiretype) seen, flagging tags not in the schema.
/// Records only tag numbers, never values.
pub fn tag_census(buf: &[u8], msg: &str, census: &mut BTreeMap<String, usize>) {
    let Some(fields) = wire_fields(buf) else {
        *census.entry(format!("{msg}: <unparseable>")).or_default() += 1;
        return;
    };
    let known = KNOWN.iter().find(|(m, _)| *m == msg).map(|(_, t)| *t).unwrap_or(&[]);
    // Small-int values are recorded only for these header/style messages (never inside text or CRDT data).
    let want_values = matches!(msg, "NoteStoreProto" | "Document" | "ParagraphStyle");
    let mut i = 0;
    for (tag, wt, payload) in fields {
        let flag = if known.contains(&tag) { "" } else { "  <-- NOT IN SCHEMA" };
        *census.entry(format!("{msg}.{tag} (wt{wt}){flag}")).or_default() += 1;
        if want_values && wt == 0 && (!known.contains(&tag) || msg == "Document") {
            // re-walk to fetch the varint value for this field
            if let Some(v) = nth_varint(buf, &mut i, tag) {
                *census.entry(format!("{msg}.{tag} value={v}")).or_default() += 1;
            }
        }
        if let (Some(p), true) = (payload, !known.contains(&tag)) {
            *census.entry(format!("{msg}.{tag} len={}", p.len())).or_default() += 1;
        }
        if let (Some(c), Some(p)) = (child(msg, tag), payload) {
            tag_census(p, c, census);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_round_trip() {
        let store = synthetic::note();
        let b64 = encode_b64(&store);
        let d = decode_b64(&b64).unwrap();
        assert_eq!(d.compression, Compression::Gzip);
        assert_eq!(d.store, store);

        let note = &d.store.document.note;
        let (sp, total) = spans(&note.note_text, &note.attribute_run);
        let covered: usize = note.attribute_run.iter().map(|r| r.length as usize).sum();
        assert_eq!(covered, total, "run lengths cover the text in UTF-16 units");
        assert_eq!(sp[2].text, "Eggs \u{1F95A}\n");
        assert!(describe(sp[0].run).contains("title"));
        assert!(describe(sp[1].run).contains("[x]"));
        assert!(describe(sp[4].run).contains("link"));
        assert!(describe(sp[6].run).contains("attachment(public.jpeg)"));

        let mut census = BTreeMap::new();
        tag_census(&d.inflated, "NoteStoreProto", &mut census);
        assert!(census.keys().all(|k| !k.contains("NOT IN SCHEMA")));
    }

    #[test]
    fn zlib_and_raw_also_decode() {
        use std::io::Write;
        let pb = synthetic::note().encode_to_vec();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&pb).unwrap();
        assert_eq!(decode_bytes(&z.finish().unwrap()).unwrap().compression, Compression::Zlib);
        assert_eq!(decode_bytes(&pb).unwrap().compression, Compression::None);
    }

    #[test]
    fn unknown_tag_is_flagged() {
        // Append Note field 99 (varint) inside the synthetic note.
        let mut note_bytes = synthetic::note().document.note.encode_to_vec();
        note_bytes.extend_from_slice(&[0x98, 0x06, 0x01]); // key = (99<<3)|0
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0x10, 0x01]); // version = 1
        doc.push(0x1a);
        prost::encoding::encode_varint(note_bytes.len() as u64, &mut doc);
        doc.extend_from_slice(&note_bytes);
        let mut top = vec![0x12];
        prost::encoding::encode_varint(doc.len() as u64, &mut top);
        top.extend_from_slice(&doc);

        // prost silently skips it...
        assert!(NoteStoreProto::decode(top.as_slice()).is_ok());
        // ...but the census sees it.
        let mut census = BTreeMap::new();
        tag_census(&top, "NoteStoreProto", &mut census);
        assert!(census.contains_key("Note.99 (wt0)  <-- NOT IN SCHEMA"));
    }
}

pub mod synthetic {
    use super::proto::*;

    fn run(len: &str, style: Option<i32>) -> AttributeRun {
        AttributeRun {
            length: len.encode_utf16().count() as i32,
            paragraph_style: style.map(|s| ParagraphStyle { style_type: Some(s), ..Default::default() }),
            ..Default::default()
        }
    }

    /// A fake note exercising title, checklist, non-BMP char, link and attachment.
    pub fn note() -> NoteStoreProto {
        let parts: Vec<(&str, AttributeRun)> = vec![
            ("Shopping\n", run("Shopping\n", Some(0))),
            ("Milk\n", {
                let mut r = run("Milk\n", Some(103));
                r.paragraph_style.as_mut().unwrap().checklist =
                    Some(Checklist { uuid: vec![1; 16], done: 1 });
                r
            }),
            ("Eggs \u{1F95A}\n", {
                let mut r = run("Eggs \u{1F95A}\n", Some(103));
                r.paragraph_style.as_mut().unwrap().checklist =
                    Some(Checklist { uuid: vec![2; 16], done: 0 });
                r
            }),
            ("See ", AttributeRun { font_weight: Some(1), ..run("See ", None) }),
            ("https://example.com", AttributeRun {
                link: Some("https://example.com".into()),
                underlined: Some(1),
                ..run("https://example.com", None)
            }),
            ("\n", run("\n", None)),
            ("\u{FFFC}", AttributeRun {
                attachment_info: Some(AttachmentInfo {
                    attachment_identifier: Some("00000000-0000-0000-0000-000000000001".into()),
                    type_uti: Some("public.jpeg".into()),
                }),
                ..run("\u{FFFC}", None)
            }),
            ("\n", run("\n", None)),
        ];
        let note_text = parts.iter().map(|(s, _)| *s).collect::<String>();
        let attribute_run = parts.into_iter().map(|(_, r)| r).collect();
        NoteStoreProto {
            document: Document { version: 1, note: Note { note_text, attribute_run } },
        }
    }
}
