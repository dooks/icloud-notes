//! Spike 5: attachments. Shared helpers for the offline `analyze` bin and the online `fetch` bin.
//! The decode pipeline (base64 -> gzip/zlib -> protobuf) is copied from spike 3.
//!
//! Privacy: nothing here returns note text, filenames or cell contents to the caller as
//! strings meant for printing; table stats are counts and CRDT schema names only.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;

use base64::Engine;
use prost::Message;
use serde_json::Value;

pub mod proto {
    // notestore.proto has no `package`, so prost writes `_.rs`.
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

use proto::{MergableDataProto, NoteStoreProto};

// ---------- copied from spike 3 ----------

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    let e = base64::engine::general_purpose::STANDARD;
    e.decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.trim_end_matches('=')))
        .map_err(|e| format!("base64: {e}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Compression {
    Gzip,
    Zlib,
    None,
}

/// Sniff the magic bytes and inflate. Falls through to "no compression" for anything else.
pub fn inflate(raw: &[u8]) -> Result<(Compression, Vec<u8>), String> {
    let mut out = Vec::new();
    match raw {
        [0x1f, 0x8b, ..] => {
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

pub fn decode_note(raw: &[u8]) -> Result<NoteStoreProto, String> {
    let (_, inflated) = inflate(raw)?;
    NoteStoreProto::decode(inflated.as_slice()).map_err(|e| format!("protobuf: {e}"))
}

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

/// Generic tag census: walks length-delimited payloads recursively when they parse as
/// protobuf (heuristic), up to `depth`. Tag paths only, never values.
pub fn tag_tree(buf: &[u8], path: &str, depth: usize, census: &mut BTreeMap<String, usize>) {
    let Some(fields) = wire_fields(buf) else { return };
    for (tag, wt, payload) in fields {
        let p = format!("{path}.{tag}");
        *census.entry(format!("{p} (wt{wt})")).or_default() += 1;
        if let (Some(pl), true) = (payload, depth > 0) {
            if pl.len() > 1 && wire_fields(pl).is_some_and(|f| !f.is_empty()) {
                tag_tree(pl, &p, depth - 1, census);
            }
        }
    }
}

// ---------- records ----------

pub struct Rec {
    pub name: String,
    pub rtype: String,
    pub deleted: bool,
    pub raw: Value,
}

impl Rec {
    pub fn field(&self, f: &str) -> Option<&Value> {
        self.raw.get("fields")?.get(f)
    }
    pub fn fval(&self, f: &str) -> Option<&Value> {
        self.field(f)?.get("value")
    }
    pub fn fstr(&self, f: &str) -> Option<&str> {
        self.fval(f)?.as_str()
    }
    pub fn fref(&self, f: &str) -> Option<&str> {
        self.fval(f)?.get("recordName")?.as_str()
    }
    pub fn fbytes(&self, f: &str) -> Option<Vec<u8>> {
        b64_decode(self.fstr(f)?).ok()
    }
    pub fn field_names(&self) -> Vec<String> {
        match self.raw.get("fields").and_then(Value::as_object) {
            Some(o) => o.keys().cloned().collect(),
            None => vec![],
        }
    }
    /// All asset values for a field (ASSETID or ASSETID_LIST).
    pub fn assets(&self, f: &str) -> Vec<&Value> {
        match self.fval(f) {
            Some(Value::Array(a)) => a.iter().filter(|v| v.is_object()).collect(),
            Some(v @ Value::Object(_)) => vec![v],
            _ => vec![],
        }
    }
}

fn collect(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(o) if o.contains_key("recordName") && (o.contains_key("recordType") || o.contains_key("deleted")) => {
            out.push(v.clone())
        }
        Value::Object(o) => o.values().for_each(|c| collect(c, out)),
        Value::Array(a) => a.iter().for_each(|c| collect(c, out)),
        _ => {}
    }
}

/// Load every saved page (sorted by name); later pages win per recordName.
pub fn load_records(dir: &Path) -> Vec<Rec> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("read pages dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("changes-zone-page-")))
        .collect();
    files.sort();
    let mut order = Vec::new();
    let mut by: HashMap<String, Value> = HashMap::new();
    for f in files {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        let mut recs = Vec::new();
        collect(&v, &mut recs);
        for r in recs {
            let n = r["recordName"].as_str().unwrap_or_default().to_owned();
            if !by.contains_key(&n) {
                order.push(n.clone());
            }
            by.insert(n, r);
        }
    }
    order
        .into_iter()
        .map(|n| {
            let raw = by.remove(&n).unwrap();
            Rec {
                rtype: raw["recordType"].as_str().unwrap_or("<none>").to_owned(),
                deleted: raw["deleted"].as_bool().unwrap_or(false),
                name: n,
                raw,
            }
        })
        .collect()
}

// ---------- tables (MergableDataProto) ----------

#[derive(Default, Debug)]
pub struct TableStats {
    pub version: i32,
    pub entries: usize,
    pub key_items: Vec<String>,
    pub type_items: Vec<String>,
    pub uuid_items: usize,
    pub entry_kinds: BTreeMap<&'static str, usize>,
    pub root_found: bool,
    pub root_keys: Vec<String>,
    pub rows_attach: Option<usize>,
    pub rows_elems: Option<usize>,
    pub cols_attach: Option<usize>,
    pub cols_elems: Option<usize>,
    pub cell_cols: Option<usize>,
    pub cells: usize,
    pub cells_nonempty: usize,
    pub cell_utf16: usize,
    pub cell_styled_runs: usize,
    pub cell_attachments: usize,
}

pub fn table_stats(inflated: &[u8]) -> Result<TableStats, String> {
    let m = MergableDataProto::decode(inflated).map_err(|e| format!("protobuf: {e}"))?;
    let o = &m.mergable_data_object;
    let d = &o.mergeable_data_object_data;
    let e = &d.mergeable_data_object_entry;
    let mut s = TableStats {
        version: o.version,
        entries: e.len(),
        key_items: d.mergeable_data_object_key_item.clone(),
        type_items: d.mergeable_data_object_type_item.clone(),
        uuid_items: d.mergeable_data_object_uuid_item.len(),
        ..Default::default()
    };
    for x in e {
        let k = if x.custom_map.is_some() {
            "custom_map"
        } else if x.ordered_set.is_some() {
            "ordered_set"
        } else if x.dictionary.is_some() {
            "dictionary"
        } else if x.note.is_some() {
            "note"
        } else if x.list.is_some() {
            "list"
        } else if x.unknown_message.is_some() {
            "unknown_message"
        } else {
            "register_latest_only"
        };
        *s.entry_kinds.entry(k).or_default() += 1;
    }

    let ty = |i: i32| d.mergeable_data_object_type_item.get(i as usize).map(String::as_str);
    let key = |i: i32| d.mergeable_data_object_key_item.get(i as usize).map(String::as_str);
    let Some(root) = e.iter().find(|x| x.custom_map.as_ref().is_some_and(|m| ty(m.r#type) == Some("com.apple.notes.ICTable")))
    else {
        return Ok(s);
    };
    s.root_found = true;
    let map = root.custom_map.as_ref().unwrap();
    for me in &map.map_entry {
        let kname = key(me.key).unwrap_or("?").to_owned();
        s.root_keys.push(kname.clone());
        let Some(target) = e.get(me.value.object_index as usize) else { continue };
        match kname.as_str() {
            "crRows" | "crColumns" => {
                let os = target.ordered_set.as_ref();
                let a = os.map(|o| o.ordering.array.attachment.len());
                let el = os.map(|o| o.elements.element.len());
                if kname == "crRows" {
                    (s.rows_attach, s.rows_elems) = (a, el);
                } else {
                    (s.cols_attach, s.cols_elems) = (a, el);
                }
            }
            "cellColumns" => {
                let Some(dict) = target.dictionary.as_ref() else { continue };
                s.cell_cols = Some(dict.element.len());
                for col in &dict.element {
                    let Some(rows) = e.get(col.value.object_index as usize).and_then(|x| x.dictionary.as_ref()) else {
                        continue;
                    };
                    for cell in &rows.element {
                        s.cells += 1;
                        let Some(note) = e.get(cell.value.object_index as usize).and_then(|x| x.note.as_ref()) else {
                            continue;
                        };
                        let n = note.note_text.encode_utf16().count();
                        if note.note_text.trim().is_empty() {
                            continue;
                        }
                        s.cells_nonempty += 1;
                        s.cell_utf16 += n;
                        for r in &note.attribute_run {
                            if r.attachment_info.is_some() {
                                s.cell_attachments += 1;
                            }
                            if r.font_weight.is_some_and(|w| w != 0)
                                || r.underlined.is_some_and(|w| w != 0)
                                || r.strikethrough.is_some_and(|w| w != 0)
                                || r.link.is_some()
                                || r.color.is_some()
                                || r.paragraph_style.as_ref().is_some_and(|p| p.style_type.is_some_and(|t| t != -1))
                            {
                                s.cell_styled_runs += 1;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(s)
}

/// First bytes classified as a file type (no content shown).
pub fn sniff(b: &[u8]) -> &'static str {
    match b {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xff, 0xd8, 0xff, ..] => "jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        [_, _, _, _, b'f', b't', b'y', b'p', b'h', b'e', b'i', ..] => "heic",
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "isobmff(ftyp)",
        [b'%', b'P', b'D', b'F', ..] => "pdf",
        [b'G', b'I', b'F', ..] => "gif",
        [b'P', b'K', 3, 4, ..] => "zip",
        [b'b', b'p', b'l', b'i', b's', b't', ..] => "bplist",
        [b'S', b'Q', b'L', b'i', b't', b'e', ..] => "sqlite",
        [0x1f, 0x8b, ..] => "gzip",
        [0x78, b, ..] if (0x7800u16 | *b as u16) % 31 == 0 => "zlib",
        [] => "empty",
        _ => "unknown",
    }
}
