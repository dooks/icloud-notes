//! spike3-decode synth
//! spike3-decode records <changes-zone.json> [--show-text N]
//!
//! By default only content-free stats are printed. Note text, titles and link targets
//! are printed for the first N notes only with --show-text N.

use std::collections::BTreeMap;

use serde_json::Value;
use spike3_decode::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("synth") if args.get(1).is_some_and(|a| a == "--emit") => {
            println!("{}", encode_b64(&synthetic::note()))
        }
        Some("synth") => synth(),
        Some("records") if args.len() >= 2 => {
            let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
            let show = opt("--show-text").and_then(|n| n.parse().ok()).unwrap_or(0);
            let pick: Vec<usize> = opt("--pick")
                .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
                .unwrap_or_default();
            MASK.store(args.iter().any(|a| a == "--mask"), std::sync::atomic::Ordering::Relaxed);
            records(&args[1], show, &pick);
        }
        _ => eprintln!("usage: spike3-decode synth | records <json> [--show-text N]"),
    }
}

static MASK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// With --mask: letters -> x/X, digits -> 9, everything else (punctuation, bullets, whitespace, emoji, U+FFFC) kept.
fn m(s: &str) -> String {
    if !MASK.load(std::sync::atomic::Ordering::Relaxed) {
        return s.to_string();
    }
    s.chars()
        .map(|c| match c {
            c if c.is_numeric() => '9',
            c if c.is_uppercase() => 'X',
            c if c.is_alphabetic() => 'x',
            c => c,
        })
        .collect()
}

fn synth() {
    let b64 = encode_b64(&synthetic::note());
    println!("synthetic: {} base64 chars", b64.len());
    match decode_b64(&b64) {
        Ok(d) => print_note(&d, true, &mut BTreeMap::new()),
        Err(e) => println!("DECODE FAILED: {e}"),
    }
}

fn print_note(d: &Decoded, show_text: bool, census: &mut BTreeMap<String, usize>) {
    tag_census(&d.inflated, "NoteStoreProto", census);
    // prost 0.14 makes proto2 `required` message fields plain structs: a missing one decodes as Default.
    let doc = &d.store.document;
    let note = &doc.note;
    let (sp, utf16_len) = spans(&note.note_text, &note.attribute_run);
    let covered: i64 = note.attribute_run.iter().map(|r| r.length as i64).sum();
    println!(
        "  {:?} {}B -> {}B, doc.version={}, text={} chars / {} utf16, runs={}, run coverage={} ({})",
        d.compression,
        d.compressed_len,
        d.inflated.len(),
        doc.version,
        note.note_text.chars().count(),
        utf16_len,
        note.attribute_run.len(),
        covered,
        if covered as usize == utf16_len { "matches utf16" } else { "MISMATCH" },
    );

    let extras = run_extras(&d.inflated);
    let extra = |i: usize| extras.get(i).map(String::as_str).unwrap_or("");
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for (i, s) in sp.iter().enumerate() {
        for k in describe(s.run).split(' ').chain(extra(i).split(' ').filter(|k| k.starts_with("ps7"))) {
            // collapse attachment(uti) / font(name) only by kind for the stats line
            *kinds.entry(k.to_string()).or_default() += 1;
        }
        // Content-free correlations for the unknown ps7 / style 3.
        let ps = s.run.paragraph_style.as_ref();
        let st = ps.and_then(|p| p.style_type).unwrap_or(-1);
        let ind = ps.and_then(|p| p.indent_amount).unwrap_or(0);
        if let Some(v) = extra(i).split(' ').find(|k| k.starts_with("ps7")) {
            *kinds.entry(format!("~{v}@st{st}/ind{ind}")).or_default() += 1;
        }
        if st == 3 {
            let al = ps.and_then(|p| p.alignment).unwrap_or(0);
            *kinds.entry(format!("~st3@len{}/align{al}/{}", s.run.length, describe(s.run))).or_default() += 1;
        }
    }
    println!("  run kinds: {kinds:?}");

    if show_text {
        println!("  --- text ---");
        for line in note.note_text.lines() {
            println!("  | {}", m(line));
        }
        println!("  --- runs (utf16 offset+len: attrs: text) ---");
        for (i, s) in sp.iter().enumerate() {
            let link = s.run.link.as_deref().map(|l| format!(" -> {}", m(l))).unwrap_or_default();
            let st = s.run.paragraph_style.as_ref().and_then(|p| p.style_type);
            println!(
                "  {:>5}+{:<4} {} [st={st:?} {}]{}: {:?}",
                s.start, s.run.length, describe(s.run), extra(i), link, m(&s.text)
            );
        }
    }
}

fn collect_records<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) if m.contains_key("recordType") && m.contains_key("fields") => out.push(v),
        Value::Object(m) => m.values().for_each(|x| collect_records(x, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_records(x, out)),
        _ => {}
    }
}

fn records(path: &str, show: usize, pick: &[usize]) {
    // A single JSON file, or a directory of them (spike 2 saves out/changes-zone-page-NNN.json).
    let files: Vec<std::path::PathBuf> = if std::path::Path::new(path).is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(path)
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        v.sort();
        v
    } else {
        vec![path.into()]
    };
    let json = Value::Array(
        files
            .iter()
            .map(|f| serde_json::from_str(&std::fs::read_to_string(f).expect("read input")).expect("parse json"))
            .collect(),
    );
    println!("{} input file(s)", files.len());
    let mut recs = Vec::new();
    collect_records(&json, &mut recs);

    let mut types: BTreeMap<&str, usize> = BTreeMap::new();
    let mut note_fields: BTreeMap<String, usize> = BTreeMap::new();
    let mut census = BTreeMap::new();
    let (mut ok, mut failed, mut missing, mut shown) = (0, 0, 0, 0);

    for r in &recs {
        let ty = r["recordType"].as_str().unwrap_or("?");
        *types.entry(ty).or_default() += 1;
        if ty != "Note" {
            continue;
        }
        if let Some(f) = r["fields"].as_object() {
            for (k, v) in f {
                let t = v["type"].as_str().unwrap_or("?");
                *note_fields.entry(format!("{k}:{t}")).or_default() += 1;
            }
        }
        for k in ["TitleEncrypted", "SnippetEncrypted"] {
            if let Some(v) = r["fields"][k]["value"].as_str() {
                let kind = match b64_decode(v) {
                    Ok(b) if std::str::from_utf8(&b).is_ok() => "utf8",
                    Ok(_) => "non-utf8",
                    Err(_) => "bad-base64",
                };
                *note_fields.entry(format!("{k} decodes as {kind}")).or_default() += 1;
            }
        }
        let name = r["recordName"].as_str().unwrap_or("?");
        let Some(b64) = r["fields"]["TextDataEncrypted"]["value"].as_str() else {
            missing += 1;
            continue;
        };
        // 1-based index among Note records that carry an inline body (the order the stats run prints them).
        let idx = ok + failed + 1;
        let show_this = if pick.is_empty() { shown < show } else { pick.contains(&idx) };
        if !pick.is_empty() && !show_this {
            // still decode for the summary, but stay quiet
            match decode_b64(b64) {
                Ok(d) => {
                    ok += 1;
                    tag_census(&d.inflated, "NoteStoreProto", &mut census);
                }
                Err(_) => failed += 1,
            }
            continue;
        }
        println!("Note #{idx} {name}:");
        match decode_b64(b64) {
            Ok(d) => {
                ok += 1;
                if show_this {
                    shown += 1;
                    if let Some(t) = r["fields"]["TitleEncrypted"]["value"].as_str() {
                        let title = b64_decode(t).map(|b| String::from_utf8_lossy(&b).into_owned());
                        println!("  title: {:?}", title.map(|t| m(&t)));
                    }
                }
                print_note(&d, show_this, &mut census);
            }
            Err(e) => {
                failed += 1;
                let head = b64_decode(b64).map(|b| format!("{:02x?}", &b[..b.len().min(4)]));
                println!("  DECODE FAILED: {e} (first bytes {head:?})");
            }
        }
    }

    println!("\nrecord types: {types:?}");
    println!("Note fields (name:type -> count):");
    for (k, n) in &note_fields {
        println!("  {k} {n}");
    }
    println!("TextDataEncrypted: decoded={ok} failed={failed} missing={missing}");
    println!("protobuf tag census (message.tag (wiretype) -> count):");
    for (k, n) in &census {
        println!("  {k} {n}");
    }
}
