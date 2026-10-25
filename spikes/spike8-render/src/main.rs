//! spike8-render stats  [dir]                    content-free feature stats over all notes
//! spike8-render list   [dir]                    content-free list (index, date, size, feature codes) -> out\list.txt
//! spike8-render render [dir] <idx,idx..> [--debug]   write out\note-<idx>.html (shows note content)
//!
//! [dir] defaults to ..\spike2-cloudkit\out (saved changes/zone pages). No network.

#[allow(dead_code)]
mod decode;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use decode::{Decoded, b64_decode, decode_b64, run_extras, style_name};
use render::{Counters, ListKind, Opts, balanced, bump, document, link_bucket, paragraphs, render_body};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_dir = manifest.join("../spike2-cloudkit/out");
    let pos: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();
    let dir_arg = pos.first().filter(|p| Path::new(p.as_str()).is_dir());
    let dir = dir_arg.map(|p| PathBuf::from(p.as_str())).unwrap_or(default_dir);
    let out = manifest.join("out");
    match args.first().map(String::as_str) {
        Some("stats") => stats(&load(&dir)),
        Some("list") => list(&load(&dir), &out),
        Some("render") => {
            let picks: Vec<usize> = pos
                .iter()
                .skip(dir_arg.is_some() as usize)
                .flat_map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect::<Vec<_>>())
                .collect();
            render_notes(&load(&dir), &picks, args.iter().any(|a| a == "--debug"), &out);
        }
        _ => eprintln!("usage: spike8-render stats|list [dir] | render [dir] <idx,..> [--debug]"),
    }
}

struct NoteRec {
    idx: usize,
    modified_ms: Option<i64>,
    deleted: bool,
    title_b64: Option<String>,
    body_b64: String,
}

fn collect_records<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) if m.contains_key("recordType") && m.contains_key("fields") => out.push(v),
        Value::Object(m) => m.values().for_each(|x| collect_records(x, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_records(x, out)),
        _ => {}
    }
}

/// Note records with an inline body, in page order. A recordName seen again on a later page replaces
/// the earlier copy but keeps its index.
fn load(dir: &Path) -> Vec<NoteRec> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut notes: Vec<NoteRec> = Vec::new();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(f).expect("read")).expect("json");
        let mut recs = Vec::new();
        collect_records(&json, &mut recs);
        for r in recs {
            if r["recordType"] != "Note" {
                continue;
            }
            let Some(body) = r["fields"]["TextDataEncrypted"]["value"].as_str() else { continue };
            let name = r["recordName"].as_str().unwrap_or("?").to_string();
            let rec = NoteRec {
                idx: 0,
                modified_ms: r["fields"]["ModificationDate"]["value"].as_i64(),
                deleted: r["fields"]["Deleted"]["value"].as_i64().unwrap_or(0) != 0,
                title_b64: r["fields"]["TitleEncrypted"]["value"].as_str().map(str::to_string),
                body_b64: body.to_string(),
            };
            match by_name.get(&name) {
                Some(&i) => notes[i] = NoteRec { idx: notes[i].idx, ..rec },
                None => {
                    by_name.insert(name, notes.len());
                    notes.push(NoteRec { idx: notes.len() + 1, ..rec });
                }
            }
        }
    }
    eprintln!("{} page file(s), {} notes with inline bodies", files.len(), notes.len());
    notes
}

fn ps7_per_run(d: &Decoded) -> Vec<Option<u64>> {
    run_extras(&d.inflated)
        .iter()
        .map(|e| e.split(' ').find_map(|k| k.strip_prefix("ps7=")).and_then(|v| v.parse().ok()))
        .collect()
}

/// Everything we learn about one note without keeping any of its content.
struct Profile {
    /// feature -> number of runs or paragraphs carrying it
    feat: BTreeMap<String, usize>,
    counters: Counters,
    utf16_len: usize,
    paras: usize,
    html_len: usize,
    balanced: Result<(), String>,
}

fn profile(d: &Decoded) -> Profile {
    let note = &d.store.document.note;
    let ps7 = ps7_per_run(d);
    let paras = paragraphs(&note.note_text, &note.attribute_run, &ps7);
    let mut f: BTreeMap<String, usize> = BTreeMap::new();
    let mut inc = |k: String| *f.entry(k).or_default() += 1;

    for r in &note.attribute_run {
        match r.font_weight.unwrap_or(0) {
            0 => {}
            1 => inc("run: bold".into()),
            2 => inc("run: italic".into()),
            3 => inc("run: bold+italic".into()),
            w => inc(format!("run: font_weight={w}")),
        }
        if r.underlined.unwrap_or(0) != 0 {
            inc("run: underline".into());
        }
        if r.strikethrough.unwrap_or(0) != 0 {
            inc("run: strikethrough".into());
        }
        match r.superscript.unwrap_or(0) {
            0 => {}
            s if s > 0 => inc("run: superscript".into()),
            _ => inc("run: subscript".into()),
        }
        if let Some(l) = &r.link {
            inc(format!("run: link {}", link_bucket(l)));
        }
        if let Some(c) = &r.color {
            let h = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            inc(format!("run: color #{:02x}{:02x}{:02x} a={:.2}", h(c.red), h(c.green), h(c.blue), c.alpha));
        }
        if let Some(e) = r.emphasis_style {
            inc(format!("run: emphasis_style={e}"));
        }
        if let Some(font) = &r.font {
            match (&font.font_name, font.point_size) {
                (Some(_), _) => inc("run: font name".into()),
                (None, Some(p)) => inc(format!("run: font size {p}")),
                (None, None) => inc("run: font (empty)".into()),
            }
            if let Some(h) = font.font_hints {
                inc(format!("run: font_hints={h}"));
            }
        }
        if let Some(a) = &r.attachment_info {
            inc(format!("attachment: {}", a.type_uti.as_deref().unwrap_or("?")));
        }
        if r.unknown_identifier.is_some() {
            inc("run: AttributeRun.13 set".into());
        }
    }

    // Per-level running number of numbered items, to test whether ps7 is the item number.
    let mut numbers: Vec<(ListKind, u64)> = Vec::new();
    let mut max_level = None;
    for (i, p) in paras.iter().enumerate() {
        let a = &p.attrs;
        let kind = ListKind::of(a);
        inc(format!("para: {}", style_name(a.style)));
        if let Some(k) = kind {
            let lvl = a.indent.max(0) as usize;
            inc(format!("list: {k:?} level {lvl}"));
            max_level = max_level.max(Some(lvl));
            numbers.truncate(lvl + 1);
            while numbers.len() < lvl + 1 {
                numbers.push((k, 0));
            }
            if numbers[lvl].0 != k {
                numbers[lvl] = (k, 0);
            }
            numbers[lvl].1 += 1;
            if k == ListKind::Numbered {
                let n = numbers[lvl].1;
                inc(match a.ps7 {
                    Some(v) if v == n => "ps7 test: numbered item, ps7 == running number".into(),
                    Some(_) => "ps7 test: numbered item, ps7 != running number".into(),
                    None => format!("ps7 test: numbered item #{} without ps7", n.min(3)),
                });
            }
        } else {
            numbers.clear();
            if a.indent > 0 {
                inc(format!("para: indent={} (not a list)", a.indent));
            }
        }
        if let Some(done) = a.check {
            inc(format!("checklist: {}", if done { "checked" } else { "unchecked" }));
        }
        if a.quote {
            inc("para: block quote".into());
        }
        if a.align != 0 {
            inc(format!("para: alignment={}", a.align));
        }
        if p.mixed {
            inc("para: runs disagree on paragraph attrs".into());
        }
        if p.segs.is_empty() {
            inc("para: empty".into());
        }
        if let Some(v) = a.ps7 {
            inc(format!("ps7={v} on {} level {}", style_name(a.style), a.indent));
        }
        if a.style == 3 {
            let nb = |j: Option<usize>| j.and_then(|j| paras.get(j)).map(|q| style_name(q.attrs.style)).unwrap_or("-".into());
            inc(format!("style3: prev={} next={} empty={}", nb(i.checked_sub(1)), nb(Some(i + 1)), p.segs.is_empty()));
        }
    }
    if let Some(l) = max_level {
        inc(format!("note: deepest list level {l}"));
    }

    let mut counters = Counters::new();
    let body = render_body(&paras, Opts::default(), &mut counters);
    let html = document(&body, "x", Opts::default());
    Profile {
        feat: f,
        counters,
        utf16_len: note.note_text.encode_utf16().count(),
        paras: paras.len(),
        html_len: html.len(),
        balanced: balanced(&html),
    }
}

fn stats(notes: &[NoteRec]) {
    // feature -> (runs/paragraphs, notes)
    let mut agg: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut misc = Counters::new();
    let (mut html_total, mut html_max) = (0usize, 0usize);
    for n in notes {
        if n.deleted {
            bump(&mut misc, "notes marked Deleted (still rendered)");
        }
        let d = match decode_b64(&n.body_b64) {
            Ok(d) => d,
            Err(e) => {
                bump(&mut misc, &format!("decode failed: {}", e.split(':').next().unwrap_or("?")));
                continue;
            }
        };
        let p = profile(&d);
        bump(&mut misc, "notes rendered");
        if let Err(e) = &p.balanced {
            bump(&mut misc, "HTML NOT BALANCED");
            eprintln!("note #{}: {e}", n.idx);
        }
        html_total += p.html_len;
        html_max = html_max.max(p.html_len);
        for (k, v) in p.feat {
            let e = agg.entry(k).or_default();
            e.0 += v;
            e.1 += 1;
        }
        for (k, v) in p.counters {
            *misc.entry(k).or_default() += v;
        }
    }
    println!("{:<64} {:>8} {:>6}", "feature", "runs/par", "notes");
    for (k, (n, notes)) in &agg {
        println!("{k:<64} {n:>8} {notes:>6}");
    }
    println!("\nmisc:");
    for (k, v) in &misc {
        println!("  {k}: {v}");
    }
    println!("  HTML bytes total {html_total}, max {html_max}");
}

/// Short, content-free feature codes for picking notes.
fn codes(p: &Profile) -> BTreeSet<String> {
    let has = |pre: &str| p.feat.keys().any(|k| k.starts_with(pre));
    let mut s = BTreeSet::new();
    for (code, pre) in [
        ("title", "para: title"),
        ("head", "para: heading"),
        ("subhead", "para: subheading"),
        ("mono", "para: monospaced"),
        ("style3", "para: style#3"),
        ("bullet", "list: Bullet"),
        ("dash", "list: Dash"),
        ("num", "list: Numbered"),
        ("check", "list: Check"),
        ("quote", "para: block quote"),
        ("indent", "para: indent="),
        ("align", "para: alignment"),
        ("bold", "run: bold"),
        ("italic", "run: italic"),
        ("underline", "run: underline"),
        ("strike", "run: strikethrough"),
        ("link", "run: link"),
        ("color", "run: color"),
        ("emph", "run: emphasis"),
        ("supsub", "run: super"),
        ("fontsize", "run: font size"),
        ("ps7", "ps7="),
        ("mixed", "para: runs disagree"),
    ] {
        if has(pre) {
            s.insert(code.to_string());
        }
    }
    if p.feat.keys().any(|k| k.starts_with("list:") && !k.ends_with("level 0")) {
        s.insert("nested".into());
    }
    for k in p.feat.keys() {
        if let Some(uti) = k.strip_prefix("attachment: ") {
            s.insert(format!("att:{}", render::attachment_kind(uti)));
        }
    }
    s
}

fn ymd(ms: i64) -> String {
    // Days since 1970-01-01 -> civil date (Howard Hinnant's algorithm).
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{}-{m:02}-{d:02}", yoe + era * 400 + (m <= 2) as i64)
}

fn list(notes: &[NoteRec], out: &Path) {
    let mut rows = Vec::new();
    let mut text = String::from("idx  modified    utf16  paras  features\n");
    for n in notes {
        let Ok(d) = decode_b64(&n.body_b64) else { continue };
        let p = profile(&d);
        let c = codes(&p);
        let date = n.modified_ms.map(ymd).unwrap_or("?".into());
        let del = if n.deleted { " DELETED" } else { "" };
        text.push_str(&format!(
            "{:>3}  {date}  {:>6}  {:>5}  {}{del}\n",
            n.idx,
            p.utf16_len,
            p.paras,
            c.iter().cloned().collect::<Vec<_>>().join(" ")
        ));
        rows.push((n.idx, p.utf16_len, c, n.deleted, date));
    }
    std::fs::create_dir_all(out).unwrap();
    let path = out.join("list.txt");
    std::fs::write(&path, &text).unwrap();
    println!("full list ({} notes) written to {}", rows.len(), path.display());

    // Greedy cover: fewest non-deleted notes that show every feature; ties go to the shorter note.
    let all: BTreeSet<String> = rows.iter().filter(|r| !r.3).flat_map(|r| r.2.iter().cloned()).collect();
    let mut left = all.clone();
    println!("\nsuggested picks (cover all {} feature codes seen in non-deleted notes):", all.len());
    println!("idx  modified    utf16  new features covered");
    while !left.is_empty() {
        let best = rows
            .iter()
            .filter(|r| !r.3)
            .max_by_key(|r| (r.2.intersection(&left).count(), std::cmp::Reverse(r.1)))
            .unwrap();
        let new: Vec<String> = best.2.intersection(&left).cloned().collect();
        if new.is_empty() {
            break;
        }
        println!("{:>3}  {}  {:>6}  {}", best.0, best.4, best.1, new.join(" "));
        for k in &new {
            left.remove(k);
        }
    }
}

fn render_notes(notes: &[NoteRec], picks: &[usize], debug: bool, out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    for &i in picks {
        let Some(n) = notes.iter().find(|n| n.idx == i) else {
            eprintln!("no note #{i}");
            continue;
        };
        let d = decode_b64(&n.body_b64).expect("decode");
        let note = &d.store.document.note;
        let paras = paragraphs(&note.note_text, &note.attribute_run, &ps7_per_run(&d));
        let title = n
            .title_b64
            .as_deref()
            .and_then(|t| b64_decode(t).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|| format!("Note {i}"));
        for (dbg, suffix) in [(false, ""), (true, "-debug")] {
            if dbg && !debug {
                continue;
            }
            let opts = Opts { debug: dbg };
            let mut c = Counters::new();
            let html = document(&render_body(&paras, opts, &mut c), &title, opts);
            let path = out.join(format!("note-{i:03}{suffix}.html"));
            std::fs::write(&path, &html).unwrap();
            println!("{} ({} bytes, balanced: {:?}) {:?}", path.display(), html.len(), balanced(&html).is_ok(), c);
        }
    }
}
