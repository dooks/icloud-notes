//! Offline analysis of spike 2's saved changes/zone pages: how attachments are stored.
//! Usage: cargo run --bin analyze [-- <pages dir>]
//!
//! Prints counts, field names, type tags, sizes, UTIs, URL *parameter names* and CRDT schema
//! names only. No note text, titles, filenames, cell text, recordNames or URL values.
//! Writes out/analysis.txt and out/targets.json (recordNames for the fetch bin; not printed).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Value, json};
use spike5::*;

const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/out");
const PAGES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../spike2-cloudkit/out");

macro_rules! out {
    ($r:expr, $($a:tt)*) => {{ let s = format!($($a)*); println!("{s}"); let _ = writeln!($r, "{s}"); }};
}

fn main() {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PAGES.into());
    let recs = load_records(&dir);
    let mut r = String::new();
    let by_name: HashMap<&str, &Rec> = recs.iter().map(|x| (x.name.as_str(), x)).collect();

    // ---- 1. counts
    out!(r, "== 1. records (deduped by recordName, later page wins) ==");
    let mut counts: BTreeMap<(&str, bool), usize> = BTreeMap::new();
    for x in &recs {
        *counts.entry((x.rtype.as_str(), x.deleted)).or_default() += 1;
    }
    for ((t, d), n) in &counts {
        out!(r, "  {t:<36} {}{n}", if *d { "deleted " } else { "" });
    }

    let atts: Vec<&Rec> = recs.iter().filter(|x| x.rtype == "Attachment").collect();
    let uti_of = |x: &Rec| x.fstr("UTI").unwrap_or("?").to_owned();

    // ---- 2. per-UTI field matrix
    out!(r, "\n== 2. Attachment fields per UTI (count of records carrying the field) ==");
    let mut by_uti: BTreeMap<String, Vec<&Rec>> = BTreeMap::new();
    for a in &atts {
        by_uti.entry(uti_of(a)).or_default().push(a);
    }
    for (uti, list) in &by_uti {
        let mut f: BTreeMap<String, usize> = BTreeMap::new();
        for a in list {
            for n in a.field_names() {
                *f.entry(n).or_default() += 1;
            }
        }
        let del: BTreeMap<String, usize> = list.iter().fold(BTreeMap::new(), |mut m, a| {
            *m.entry(a.fval("Deleted").map(|v| v.to_string()).unwrap_or("-".into())).or_default() += 1;
            m
        });
        out!(r, "  -- {uti}: {} records, Deleted field values {del:?}", list.len());
        let line = f.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", ");
        out!(r, "     {line}");
    }

    // ---- 3. record-level parent / references
    out!(r, "\n== 3. links ==");
    let mut parent_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut note_ref_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut media_ref_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for a in &atts {
        let p = a.raw.get("parent").and_then(|p| p.get("recordName")).and_then(Value::as_str);
        let k = match p {
            None => "none".to_owned(),
            Some(n) => by_name.get(n).map(|t| t.rtype.clone()).unwrap_or("not in pages".into()),
        };
        *parent_kinds.entry(k).or_default() += 1;
        let k = match a.fref("Note") {
            None => "none".to_owned(),
            Some(n) => by_name.get(n).map(|t| format!("{}{}", t.rtype, if t.deleted { " (deleted)" } else { "" })).unwrap_or("not in pages".into()),
        };
        *note_ref_kinds.entry(k).or_default() += 1;
        if let Some(n) = a.fref("Media") {
            let k = by_name.get(n).map(|t| format!("{}{}", t.rtype, if t.deleted { " (deleted)" } else { "" })).unwrap_or("not in pages".into());
            *media_ref_kinds.entry(format!("{} -> {k}", uti_of(a))).or_default() += 1;
        }
    }
    out!(r, "  Attachment record `parent` -> {parent_kinds:?}");
    out!(r, "  Attachment.Note ref -> {note_ref_kinds:?}");
    out!(r, "  Attachment.Media ref -> {media_ref_kinds:?}");
    let medias: Vec<&Rec> = recs.iter().filter(|x| x.rtype == "Media" && !x.deleted).collect();
    let media_referrers: BTreeSet<&str> = atts.iter().filter_map(|a| a.fref("Media")).collect();
    out!(r, "  Media records: {}, referenced by an Attachment: {}", medias.len(), medias.iter().filter(|m| media_referrers.contains(m.name.as_str())).count());
    let mut mparent: BTreeMap<String, usize> = BTreeMap::new();
    for m in &medias {
        let p = m.raw.get("parent").and_then(|p| p.get("recordName")).and_then(Value::as_str);
        *mparent.entry(p.map(|n| by_name.get(n).map(|t| t.rtype.clone()).unwrap_or("not in pages".into())).unwrap_or("none".into())).or_default() += 1;
    }
    out!(r, "  Media record `parent` -> {mparent:?}");
    let mut fext: BTreeMap<String, usize> = BTreeMap::new();
    for m in &medias {
        let k = match m.fbytes("FilenameEncrypted").map(String::from_utf8) {
            Some(Ok(s)) => format!("utf8, ext={}", s.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or("<none>".into())),
            Some(Err(_)) => "not utf8".into(),
            None => "absent".into(),
        };
        *fext.entry(k).or_default() += 1;
    }
    out!(r, "  Media.FilenameEncrypted -> {fext:?}");

    // ---- 4. note body join
    out!(r, "\n== 4. note body attachment runs vs records ==");
    let att_by_name: HashMap<&str, &Rec> = atts.iter().map(|a| (a.name.as_str(), *a)).collect();
    let inl_by_name: HashMap<&str, &Rec> = recs.iter().filter(|x| x.rtype == "InlineAttachment").map(|a| (a.name.as_str(), a)).collect();
    let mut join: BTreeMap<String, usize> = BTreeMap::new();
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut bodies = 0;
    for n in recs.iter().filter(|x| x.rtype == "Note" && !x.deleted) {
        let Some(raw) = n.fbytes("TextDataEncrypted") else { continue };
        let Ok(store) = decode_note(&raw) else {
            *join.entry("body decode failed".into()).or_default() += 1;
            continue;
        };
        bodies += 1;
        for run in &store.document.note.attribute_run {
            let Some(ai) = &run.attachment_info else { continue };
            let id = ai.attachment_identifier.as_deref().unwrap_or("");
            let uti = ai.type_uti.as_deref().unwrap_or("?");
            referenced.insert(id.to_owned());
            let k = if let Some(a) = att_by_name.get(id) {
                let back = if a.fref("Note") == Some(n.name.as_str()) { "Note ref matches" } else { "Note ref DIFFERS" };
                let um = if a.fstr("UTI") == Some(uti) { "uti matches" } else { "uti DIFFERS" };
                format!("{uti}: Attachment record, {back}, {um}")
            } else if let Some(a) = inl_by_name.get(id) {
                let back = if a.fref("Note") == Some(n.name.as_str()) { "Note ref matches" } else { "Note ref DIFFERS" };
                format!("{uti}: InlineAttachment record, {back}")
            } else {
                format!("{uti}: NO record with that id")
            };
            *join.entry(k).or_default() += 1;
        }
    }
    out!(r, "  decoded bodies: {bodies}");
    for (k, v) in &join {
        out!(r, "  {v:>4}  {k}");
    }
    let unref: BTreeMap<String, usize> = atts.iter().filter(|a| !referenced.contains(&a.name)).fold(BTreeMap::new(), |mut m, a| {
        let nk = a.fref("Note").and_then(|n| by_name.get(n)).map(|t| t.rtype.as_str()).unwrap_or("?");
        *m.entry(format!("{} (Deleted={}, note={nk})", uti_of(a), a.fval("Deleted").map(|v| v.to_string()).unwrap_or("-".into()))).or_default() += 1;
        m
    });
    out!(r, "  Attachment records not referenced by any decoded body: {unref:?}");

    // ---- 5. InlineAttachment
    out!(r, "\n== 5. InlineAttachment ==");
    for a in inl_by_name.values() {
        let uti = a.fbytes("UTIEncrypted").and_then(|b| String::from_utf8(b).ok()).unwrap_or("?".into());
        let tok = a.fbytes("TokenContentIdentifierEncrypted").map(|b| b.len());
        let alt = a.fbytes("AltTextEncrypted").map(|b| b.len());
        out!(r, "  uti={uti} token_len={tok:?} alt_len={alt:?} referenced_by_body={}", referenced.contains(&a.name));
    }

    // ---- 6. small decoded fields
    out!(r, "\n== 6. other Attachment fields (presence/shape only) ==");
    for (uti, list) in &by_uti {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for a in list {
            let ue = a.fbytes("UTIEncrypted").and_then(|b| String::from_utf8(b).ok());
            *m.entry(format!("UTIEncrypted==UTI:{}", ue.as_deref() == a.fstr("UTI"))).or_default() += 1;
            for f in ["TitleEncrypted", "SummaryEncrypted", "FallbackTitle", "FallbackSubtitleIOS", "FallbackSubtitleMac", "MetadataData", "OcrSummaryEncrypted", "HandwritingSummaryEncrypted", "ImageClassificationSummaryEncrypted"] {
                let Some(v) = a.fstr(f) else { continue };
                let k = if f.starts_with("Fallback") {
                    format!("{f}:string len>0={}", !v.is_empty())
                } else {
                    match b64_decode(v) {
                        Ok(b) => match inflate(&b) {
                            Ok((c, i)) if c != Compression::None => format!("{f}:{c:?}->{}", sniff(&i)),
                            _ => format!("{f}:{}", if std::str::from_utf8(&b).is_ok() { "utf8" } else { sniff(&b) }),
                        },
                        Err(_) => format!("{f}:bad b64"),
                    }
                };
                *m.entry(k).or_default() += 1;
            }
            for f in ["ImageFilterType", "Orientation", "HandwritingSummaryVersion", "OcrSummaryVersion", "MinimumSupportedNotesVersion"] {
                if let Some(v) = a.fval(f) {
                    *m.entry(format!("{f}={v}")).or_default() += 1;
                }
            }
        }
        out!(r, "  -- {uti}: {m:?}");
    }

    // ---- 7. previews
    out!(r, "\n== 7. previews / fallbacks per UTI ==");
    for (uti, list) in &by_uti {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for a in list {
            let n = a.assets("PreviewImages").len();
            *m.entry(format!("PreviewImages n={n}")).or_default() += 1;
            if let Some(v) = a.fval("PreviewAppearances") {
                *m.entry(format!("Appearances={v}")).or_default() += 1;
            }
            if let Some(v) = a.fval("PreviewScales") {
                *m.entry(format!("Scales={v}")).or_default() += 1;
            }
            if let Some(v) = a.fval("PreviewScaleWhenDrawings") {
                *m.entry(format!("ScaleWhenDrawings={v}")).or_default() += 1;
            }
            if a.fval("FallbackImage").is_some() {
                *m.entry("FallbackImage".into()).or_default() += 1;
            }
            if a.fval("PaperAssets").is_some() {
                *m.entry(format!("PaperAssets n={}", a.assets("PaperAssets").len())).or_default() += 1;
            }
        }
        out!(r, "  -- {uti}: {m:?}");
        let sizes: Vec<u64> = list.iter().flat_map(|a| a.assets("PreviewImages")).filter_map(|v| v["size"].as_u64()).collect();
        if !sizes.is_empty() {
            out!(r, "     preview sizes min/max {:?}/{:?} B", sizes.iter().min(), sizes.iter().max());
        }
        let dims: BTreeSet<String> = list.iter().filter_map(|a| Some(format!("{:.0}x{:.0}", a.fval("Width")?.as_f64()?, a.fval("Height")?.as_f64()?))).take(6).collect();
        if !dims.is_empty() {
            out!(r, "     sample Width x Height: {dims:?}");
        }
    }
    let notes: Vec<&Rec> = recs.iter().filter(|x| x.rtype == "Note" && !x.deleted).collect();
    let thumbs = notes.iter().filter(|n| n.fval("FirstAttachmentThumbnail").is_some()).count();
    let mut avt: BTreeMap<String, usize> = BTreeMap::new();
    for n in &notes {
        if let Some(v) = n.fval("AttachmentViewType") {
            *avt.entry(v.to_string()).or_default() += 1;
        }
    }
    let fau: BTreeMap<String, usize> = notes.iter().filter_map(|n| n.fbytes("FirstAttachmentUTIEncrypted")).fold(BTreeMap::new(), |mut m, b| {
        *m.entry(String::from_utf8(b).unwrap_or("not utf8".into())).or_default() += 1;
        m
    });
    out!(r, "  Note.FirstAttachmentThumbnail on {thumbs} notes; FirstAttachmentUTI {fau:?}; AttachmentViewType {avt:?}");

    // ---- 8. asset shape
    out!(r, "\n== 8. asset values ==");
    let mut fields: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for x in recs.iter().filter(|x| !x.deleted) {
        for f in x.field_names() {
            let t = x.field(&f).and_then(|v| v["type"].as_str()).unwrap_or("");
            if t.starts_with("ASSETID") {
                fields.entry(format!("{}.{f}", x.rtype)).or_default().extend(x.assets(&f));
            }
        }
    }
    for (k, vs) in &fields {
        let sizes: Vec<u64> = vs.iter().filter_map(|v| v["size"].as_u64()).collect();
        let mut shape: BTreeMap<String, usize> = BTreeMap::new();
        for v in vs {
            let keys: Vec<&str> = v.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
            *shape.entry(format!("keys={keys:?}")).or_default() += 1;
            for f in ["fileChecksum", "referenceChecksum", "wrappingKey"] {
                if let Some(b) = v[f].as_str().and_then(|s| b64_decode(s).ok()) {
                    *shape.entry(format!("{f}: {} bytes, first byte {:#04x}", b.len(), b.first().copied().unwrap_or(0))).or_default() += 1;
                }
            }
            if let Some(u) = v["downloadURL"].as_str() {
                let (base, q) = u.split_once('?').unwrap_or((u, ""));
                let params: Vec<&str> = q.split('&').filter(|p| !p.is_empty()).map(|p| p.split('=').next().unwrap_or("")).collect();
                let host = base.split('/').nth(2).unwrap_or("?");
                let segs = base.split('/').skip(3).count();
                *shape.entry(format!("url host={host} path_segments={segs} has_${{f}}={} params={params:?}", u.contains("${f}"))).or_default() += 1;
            }
        }
        out!(r, "  -- {k}: n={} size min/max {:?}/{:?}", vs.len(), sizes.iter().min(), sizes.iter().max());
        for (s, n) in &shape {
            out!(r, "       {n:>3}  {s}");
        }
    }

    // ---- 9. mergeable data
    out!(r, "\n== 9. MergeableData per UTI ==");
    for (uti, list) in &by_uti {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        let mut census: BTreeMap<String, usize> = BTreeMap::new();
        let mut tables: Vec<TableStats> = Vec::new();
        for a in list {
            let src = if a.fval("MergeableDataEncrypted").is_some() {
                "inline"
            } else if a.fval("MergeableDataAsset").is_some() {
                *m.entry("asset only (not downloaded)".into()).or_default() += 1;
                continue;
            } else {
                *m.entry("none".into()).or_default() += 1;
                continue;
            };
            let Some(raw) = a.fbytes("MergeableDataEncrypted") else { continue };
            match inflate(&raw) {
                Ok((c, i)) => {
                    *m.entry(format!("{src} {c:?} -> {}", if wire_fields(&i).is_some() { "protobuf" } else { sniff(&i) })).or_default() += 1;
                    if uti == "com.apple.notes.table" {
                        match table_stats(&i) {
                            Ok(s) => tables.push(s),
                            Err(e) => *m.entry(format!("table decode error {e}")).or_default() += 1,
                        }
                    } else {
                        tag_tree(&i, "root", 3, &mut census);
                    }
                }
                Err(e) => *m.entry(format!("inflate error {e}")).or_default() += 1,
            }
        }
        out!(r, "  -- {uti}: {m:?}");
        if !census.is_empty() {
            let top: Vec<String> = census.iter().filter(|(k, _)| k.matches('.').count() <= 3).map(|(k, v)| format!("{k}:{v}")).take(40).collect();
            out!(r, "     tag tree (depth<=3): {}", top.join(", "));
        }
        if !tables.is_empty() {
            let keys: BTreeSet<&String> = tables.iter().flat_map(|t| &t.key_items).collect();
            let types: BTreeSet<&String> = tables.iter().flat_map(|t| &t.type_items).collect();
            let roots: BTreeSet<String> = tables.iter().map(|t| t.root_keys.join("|")).collect();
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            for t in &tables {
                for (k, v) in &t.entry_kinds {
                    *kinds.entry(k).or_default() += v;
                }
            }
            out!(r, "     key items (union): {keys:?}");
            out!(r, "     type items (union): {types:?}");
            out!(r, "     root map keys: {roots:?}; ICTable root found in {}/{}", tables.iter().filter(|t| t.root_found).count(), tables.len());
            out!(r, "     entry kinds total: {kinds:?}; versions {:?}", tables.iter().map(|t| t.version).collect::<BTreeSet<_>>());
            for (i, t) in tables.iter().enumerate() {
                out!(
                    r,
                    "     table {i:>2}: entries={} uuids={} rows(attach/elems)={:?}/{:?} cols={:?}/{:?} cellColumns={:?} cells={} nonempty={} utf16={} styled_runs={} cell_attachments={}",
                    t.entries, t.uuid_items, t.rows_attach, t.rows_elems, t.cols_attach, t.cols_elems, t.cell_cols, t.cells, t.cells_nonempty, t.cell_utf16, t.cell_styled_runs, t.cell_attachments
                );
            }
        }
    }

    // ---- 10. targets for the fetch bin
    let live = |a: &&&Rec| a.fval("Deleted").and_then(Value::as_i64).unwrap_or(0) == 0;
    let media_target = atts
        .iter()
        .filter(live)
        .filter_map(|a| Some((a, by_name.get(a.fref("Media")?)?)))
        .filter_map(|(a, m)| Some((a, m, m.assets("Asset").first()?["size"].as_u64()?)))
        .min_by_key(|t| t.2);
    let preview_target = atts
        .iter()
        .filter(live)
        .filter(|a| a.fstr("UTI") != Some("public.png") && a.fstr("UTI") != Some("org.webmproject.webp"))
        .filter_map(|a| Some((a, a.assets("PreviewImages").iter().filter_map(|v| v["size"].as_u64()).min()?)))
        .min_by_key(|t| t.1);
    let mut targets = vec![];
    if let Some((a, m, size)) = media_target {
        out!(r, "\n== 10. fetch targets ==\n  media: uti={} size={size}", uti_of(a));
        targets.push(json!({"kind": "media", "uti": uti_of(a), "attachment": a.name, "record": m.name, "field": "Asset", "size": size,
            "saved": m.assets("Asset")[0]}));
    }
    if let Some((a, size)) = preview_target {
        out!(r, "  preview: uti={} size={size}", uti_of(a));
        let v = a.assets("PreviewImages").into_iter().min_by_key(|v| v["size"].as_u64()).unwrap().clone();
        targets.push(json!({"kind": "preview", "uti": uti_of(a), "attachment": a.name, "record": a.name, "field": "PreviewImages", "size": size, "saved": v}));
    }
    std::fs::create_dir_all(OUT).unwrap();
    std::fs::write(format!("{OUT}/targets.json"), serde_json::to_string_pretty(&targets).unwrap()).unwrap();
    std::fs::write(format!("{OUT}/analysis.txt"), &r).unwrap();
    println!("\nwrote {OUT}/analysis.txt and {OUT}/targets.json");
}
