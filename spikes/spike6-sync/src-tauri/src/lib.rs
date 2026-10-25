// Spike 6: incremental sync on com.apple.notes changes/zone.
//
// `offline`: analyses spike 2's saved pages (token layout, paging order, Recently Deleted).
// default: reuses spike 1's WebView2 session like spike 2, runs validate, then read-only probes:
// fresh full pull, resume from a mid-run token, spike 2's old token, bad tokens, cheap
// "anything changed?" checks, and records/lookup compared with changes/zone.
//
// Privacy: out/ (gitignored) holds raw note data. report-*.txt and stdout only get status codes,
// counts, field names, type tags, token counters and redacted folder names: never note text,
// note recordNames, dsid, cookie values or error bodies beyond their error/reason fields.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime},
};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use reqwest::{blocking::Client, cookie::Jar};
use serde_json::{Value, json};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::Cookie};

const ICLOUD: &str = "https://www.icloud.com/";
const ORIGIN: &str = "https://www.icloud.com";
const VALIDATE: &str = "https://setup.icloud.com/setup/ws/1/validate";
const DB: &str = "/database/1/com.apple.notes/production/private";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out");
const SPIKE2_OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spike2-cloudkit/out");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_PAGES: usize = 60;

struct Log {
    text: String,
    path: String,
}

impl Log {
    fn new(name: &str) -> Self {
        let _ = fs::create_dir_all(OUT);
        Log { text: String::new(), path: format!("{OUT}/{name}") }
    }

    fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        self.text.push_str(s.as_ref());
        self.text.push('\n');
        // Rewritten after every line, so closing the window early keeps what was logged.
        let _ = fs::write(&self.path, &self.text);
    }
}

struct Session {
    client: Client,
    ckdb: String,
}

// ---------------------------------------------------------------- offline (phase A)

pub fn offline() {
    let mut log = Log::new("report-offline.txt");
    let pages = spike2_pages();
    log.line(format!("{} saved pages from spike 2", pages.len()));

    log.line("\n== token layout per page ==");
    for (i, z) in pages.iter().enumerate() {
        let t = z["syncToken"].as_str().map(token_desc).unwrap_or_else(|| "none".into());
        log.line(format!("  page {:>2}: {:>3} records, moreComing={}, token {t}", i + 1, recs(z).len(), z["moreComing"]));
    }
    if let Ok(t) = fs::read_to_string(format!("{SPIKE2_OUT}/synctoken.txt")) {
        log.line(format!("  synctoken.txt: {}", token_desc(t.trim())));
    }

    let by_page: Vec<Vec<Value>> = pages.iter().map(|z| recs(z).to_vec()).collect();
    page_order(&mut log, &by_page);
    let all: Vec<Value> = by_page.into_iter().flatten().collect();
    analyze(&mut log, &all);
    log.line("done");
}

fn spike2_pages() -> Vec<Value> {
    let mut pages = Vec::new();
    for i in 1.. {
        let Ok(t) = fs::read_to_string(format!("{SPIKE2_OUT}/changes-zone-page-{i:03}.json")) else { break };
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
        pages.push(v["zones"][0].clone());
    }
    pages
}

fn recs(z: &Value) -> &[Value] {
    z["records"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn is_tomb(r: &Value) -> bool {
    r["deleted"].as_bool() == Some(true)
}

/// Per page: range of `modified` dates and whether pages overlap, to see how the server orders changes.
fn page_order(log: &mut Log, pages: &[Vec<Value>]) {
    log.line("\n== modified timestamps per page (records that have one) ==");
    let mut prev_max: Option<i64> = None;
    let mut overlaps = 0;
    for (i, p) in pages.iter().enumerate() {
        let ts: Vec<i64> = p.iter().filter_map(|r| r["modified"]["timestamp"].as_i64()).collect();
        let tomb_with_ts = p.iter().filter(|r| is_tomb(r) && r["modified"].is_object()).count();
        let (Some(&lo), Some(&hi)) = (ts.iter().min(), ts.iter().max()) else {
            log.line(format!("  page {:>2}: no timestamps ({} records)", i + 1, p.len()));
            continue;
        };
        if prev_max.is_some_and(|m| lo < m) {
            overlaps += 1;
        }
        prev_max = Some(prev_max.map_or(hi, |m| m.max(hi)));
        log.line(format!(
            "  page {:>2}: {} with modified, {} tombstones with modified, {} .. {}",
            i + 1,
            ts.len(),
            tomb_with_ts,
            ymd(lo),
            ymd(hi)
        ));
    }
    log.line(format!("  pages whose earliest modified is before an earlier page's latest: {overlaps}"));
}

fn analyze(log: &mut Log, all: &[Value]) {
    let tombs: Vec<&Value> = all.iter().filter(|r| is_tomb(r)).collect();
    let mut tkeys = BTreeSet::new();
    let mut ttypes: BTreeMap<String, usize> = BTreeMap::new();
    for r in &tombs {
        tkeys.extend(keys(r).into_iter().map(str::to_owned));
        *ttypes.entry(r["recordType"].as_str().unwrap_or("<none>").to_owned()).or_default() += 1;
    }
    log.line(format!(
        "\n== tombstones: {} of {} records ==\n  keys: {tkeys:?}\n  recordType: {ttypes:?}",
        tombs.len(),
        all.len()
    ));

    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut latest: HashMap<&str, &Value> = HashMap::new();
    for r in all {
        if let Some(n) = r["recordName"].as_str() {
            *seen.entry(n).or_default() += 1;
            latest.insert(n, r);
        }
    }
    let dups: Vec<&str> = seen.iter().filter(|(_, c)| **c > 1).map(|(n, _)| *n).collect();
    let dups_dead = dups.iter().filter(|n| is_tomb(latest[*n])).count();
    log.line(format!(
        "  unique recordNames {}; seen more than once {} (of which last copy is a tombstone: {dups_dead})",
        seen.len(),
        dups.len()
    ));
    let live: Vec<&Value> = latest.values().copied().filter(|r| !is_tomb(r)).collect();
    log.line(format!("  live after applying all pages in order: {}", live.len()));

    // Folders and where notes point.
    let mut red = Redactor::default();
    let folders: BTreeMap<&str, &Value> = live
        .iter()
        .filter(|r| r["recordType"] == "Folder")
        .filter_map(|r| Some((r["recordName"].as_str()?, *r)))
        .collect();
    log.line(format!("\n== Folder records (live): {} ==", folders.len()));
    for (name, r) in &folders {
        let fields: Vec<&str> = keys(&r["fields"]);
        let parent = r["parent"].is_object() || r["fields"]["ParentFolder"].is_object();
        log.line(format!("  {:<28} parent={parent} fields={fields:?}", red.name(name)));
    }

    for ty in ["Note", "PasswordProtectedNote", "Note_UserSpecific", "PasswordProtectedNote_UserSpecific"] {
        // folder target -> Deleted value -> count
        let mut by_target: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        let mut folders_len: BTreeMap<String, usize> = BTreeMap::new();
        let mut folders_mismatch = 0;
        let mut n = 0;
        for r in live.iter().filter(|r| r["recordType"] == ty) {
            n += 1;
            let target = r["fields"]["Folder"]["value"]["recordName"].as_str();
            let label = match target {
                Some(t) if folders.contains_key(t) => red.name(t),
                Some(t) => format!("{} (no Folder record)", red.name(t)),
                None => "<no Folder field>".into(),
            };
            let del = match &r["fields"]["Deleted"]["value"] {
                Value::Null => "absent".to_owned(),
                v => v.to_string(),
            };
            *by_target.entry(label).or_default().entry(format!("Deleted={del}")).or_default() += 1;
            if let Some(list) = r["fields"]["Folders"]["value"].as_array() {
                *folders_len.entry(format!("len {}", list.len())).or_default() += 1;
                let names: Vec<&str> = list.iter().filter_map(|x| x["recordName"].as_str()).collect();
                if target.is_some_and(|t| !names.contains(&t)) {
                    folders_mismatch += 1;
                }
            }
        }
        if n == 0 {
            continue;
        }
        log.line(format!("\n== {ty} (live: {n}) by Folder target ==  Folders list: {folders_len:?}, Folder not in Folders: {folders_mismatch}"));
        for (label, dels) in &by_target {
            log.line(format!("  {label:<40} {dels:?}"));
        }
    }

    // *_UserSpecific -> the note it belongs to: same zone? which folder does that note point at?
    for ty in ["Note_UserSpecific", "PasswordProtectedNote_UserSpecific"] {
        let mut rows: BTreeMap<String, usize> = BTreeMap::new();
        for r in live.iter().filter(|r| r["recordType"] == ty) {
            let own = r["fields"]["Folder"]["value"]["recordName"].as_str().map(|t| red.name(t)).unwrap_or_else(|| "-".into());
            let note = r["fields"]["Note"]["value"]["recordName"].as_str().and_then(|n| latest.get(n));
            let other = match note {
                None => "note not in this zone".to_owned(),
                Some(n) if is_tomb(n) => "note is a tombstone".to_owned(),
                Some(n) => format!(
                    "{} in {}",
                    n["recordType"].as_str().unwrap_or("?"),
                    n["fields"]["Folder"]["value"]["recordName"].as_str().map(|t| red.name(t)).unwrap_or_else(|| "-".into())
                ),
            };
            *rows.entry(format!("own Folder {own:<24} -> {other}")).or_default() += 1;
        }
        if !rows.is_empty() {
            log.line(format!("\n== {ty}: own Folder vs the note's Folder =="));
            for (k, n) in &rows {
                log.line(format!("  {k:<70} {n}"));
            }
        }
    }
    let ref_keys: BTreeSet<String> = live
        .iter()
        .filter(|r| r["recordType"] == "Note_UserSpecific")
        .filter_map(|r| r["fields"]["Note"]["value"].as_object())
        .flat_map(|o| o.keys().cloned())
        .collect();
    log.line(format!("  Note_UserSpecific.Note reference keys: {ref_keys:?}"));

    let mut deleted_vals: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in &live {
        if let Some(v) = r["fields"].get("Deleted") {
            let ty = r["recordType"].as_str().unwrap_or("?").to_owned();
            *deleted_vals.entry(ty).or_default().entry(v["value"].to_string()).or_default() += 1;
        }
    }
    log.line(format!("\n== Deleted field values per type (live records) ==\n  {deleted_vals:?}"));

    // Bodies that are not inline.
    let notes: Vec<&&Value> = live.iter().filter(|r| r["recordType"] == "Note").collect();
    let asset_body = notes.iter().filter(|r| r["fields"]["TextDataAsset"].is_object()).count();
    let inline_body = notes.iter().filter(|r| r["fields"]["TextDataEncrypted"].is_object()).count();
    let neither = notes.iter().filter(|r| !r["fields"]["TextDataAsset"].is_object() && !r["fields"]["TextDataEncrypted"].is_object()).count();
    let max_inline = notes
        .iter()
        .filter_map(|r| r["fields"]["TextDataEncrypted"]["value"].as_str().map(str::len))
        .max()
        .unwrap_or(0);
    log.line(format!(
        "\n== Note bodies ==\n  inline TextDataEncrypted {inline_body}, TextDataAsset {asset_body}, neither {neither}, largest inline base64 {max_inline} chars"
    ));
}

/// Folder recordNames: fixed system names are printed, UUID-like ones become <uuid#k>.
#[derive(Default)]
struct Redactor(HashMap<String, String>);

impl Redactor {
    fn name(&mut self, n: &str) -> String {
        let k = self.0.len() + 1;
        self.0
            .entry(n.to_owned())
            .or_insert_with(|| {
                let uuidish = n.len() == 36 && n.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
                let system = n.len() <= 40 && n.contains("Folder") && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if uuidish {
                    format!("<uuid#{k}>")
                } else if system {
                    n.to_owned()
                } else {
                    format!("<other#{k},{} chars>", n.len())
                }
            })
            .clone()
    }
}

// ---------------------------------------------------------------- sync tokens

fn b64_decode(t: &str) -> Option<(Vec<u8>, bool)> {
    STANDARD.decode(t).map(|b| (b, false)).or_else(|_| URL_SAFE.decode(t).map(|b| (b, true))).ok()
}

fn b64_encode(b: &[u8], url: bool) -> String {
    if url { URL_SAFE.encode(b) } else { STANDARD.encode(b) }
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn write_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Generic protobuf layout: field numbers, small varints, lengths of byte fields.
fn pb_desc(b: &[u8], depth: usize) -> Option<String> {
    let mut i = 0;
    let mut parts = vec![];
    while i < b.len() {
        let key = read_varint(b, &mut i)?;
        let (f, wt) = (key >> 3, key & 7);
        if f == 0 {
            return None;
        }
        match wt {
            0 => {
                let v = read_varint(b, &mut i)?;
                parts.push(if v < 1 << 40 { format!("{f}={v}") } else { format!("{f}=<big>") });
            }
            1 | 5 => {
                i += if wt == 1 { 8 } else { 4 };
                if i > b.len() {
                    return None;
                }
                parts.push(format!("{f}=fixed{}", if wt == 1 { 64 } else { 32 }));
            }
            2 => {
                let n = read_varint(b, &mut i)? as usize;
                let end = i.checked_add(n)?;
                let sub = b.get(i..end)?;
                i = end;
                let inner = if depth < 3 && !sub.is_empty() { pb_desc(sub, depth + 1) } else { None };
                parts.push(match inner {
                    Some(s) => format!("{f}:{{{s}}}"),
                    None => format!("{f}:bytes[{n}]"),
                });
            }
            _ => return None,
        }
    }
    Some(parts.join(" "))
}

fn token_desc(t: &str) -> String {
    let Some((b, url)) = b64_decode(t) else { return format!("{} chars, not base64", t.len()) };
    let alphabet = if url { "url-safe" } else { "standard" };
    match b.split_first() {
        Some((&first, rest)) if first == 0x1f => {
            format!("{} B ({alphabet}): 0x1f + {}", b.len(), pb_desc(rest, 0).unwrap_or_else(|| "unparsed".into()))
        }
        _ => format!("{} B ({alphabet}): {}", b.len(), pb_desc(&b, 0).unwrap_or_else(|| "unparsed".into())),
    }
}

/// The change counter: 0x1f, field 1 { field 1 = varint }.
fn token_counter(t: &str) -> Option<u64> {
    let (b, _) = b64_decode(t)?;
    if b.first() != Some(&0x1f) || b.get(1) != Some(&0x0a) {
        return None;
    }
    let mut i = 2;
    read_varint(&b, &mut i)?;
    if b.get(i) != Some(&0x08) {
        return None;
    }
    i += 1;
    read_varint(&b, &mut i)
}

fn token_short(t: &str) -> String {
    match token_counter(t) {
        Some(c) => format!("ctr={c} len={}", t.len()),
        None => format!("(no counter) len={}", t.len()),
    }
}

/// Same token with the change counter replaced.
fn token_with_counter(t: &str, f: impl Fn(u64) -> u64) -> Option<String> {
    let (b, url) = b64_decode(t)?;
    if b.first() != Some(&0x1f) || b.get(1) != Some(&0x0a) {
        return None;
    }
    let mut i = 2;
    let len = read_varint(&b, &mut i)? as usize;
    let inner = b.get(i..i + len)?;
    let rest = &b[i + len..];
    let mut j = 1;
    if inner.first() != Some(&0x08) {
        return None;
    }
    let v = read_varint(inner, &mut j)?;
    let mut new_inner = vec![0x08];
    write_varint(f(v), &mut new_inner);
    new_inner.extend_from_slice(&inner[j..]);
    let mut out = vec![0x1f, 0x0a];
    write_varint(new_inner.len() as u64, &mut out);
    out.extend(new_inner);
    out.extend_from_slice(rest);
    Some(b64_encode(&out, url))
}

fn token_flip_byte(t: &str) -> Option<String> {
    let (mut b, url) = b64_decode(t)?;
    let k = b.len() / 2;
    b[k] ^= 0xff;
    Some(b64_encode(&b, url))
}

fn ymd(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------- network (phase B)

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(ICLOUD.parse()?))
                .title("spike6: iCloud session (sign in only if asked; closes by itself)")
                .inner_size(1000.0, 800.0)
                .build()?;
            let handle = app.handle().clone();
            // Off the main thread: WebView2 cookie calls deadlock if made from it synchronously.
            thread::spawn(move || watch(handle, win));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn watch(app: AppHandle, win: WebviewWindow) {
    // `delta <label>`: one phase C round. `collapse`: everything since the R0 token, against the R0 cache.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    let label = args.get(1).cloned().unwrap_or_else(|| "x".into());
    let mut log = match mode {
        "delta" => Log::new(&format!("report-delta-{label}.txt")),
        "collapse" => Log::new("report-collapse.txt"),
        _ => Log::new("report-network.txt"),
    };
    match (session(&mut log, &win), mode) {
        (Some(s), "delta") => delta_round(&mut log, &s, &label),
        (Some(s), "collapse") => collapse(&mut log, &s),
        (Some(s), _) => cloudkit(&mut log, &s),
        (None, _) => log.line("no session; stopping"),
    }
    log.line(format!("done; report is {}", log.path));
    app.exit(0);
}

/// Waits for an auth cookie in the webview and runs validate with it. If validate
/// rejects that cookie (stale profile), waits for a new one from a fresh sign-in.
fn session(log: &mut Log, win: &WebviewWindow) -> Option<Session> {
    let setup_url: tauri::Url = VALIDATE.parse().unwrap();
    let start = Instant::now();
    let mut rejected: Option<String> = None;
    log.line("waiting for X-APPLE-WEBAUTH-TOKEN (reused profile, or sign in in the window)...");

    loop {
        thread::sleep(Duration::from_secs(2));
        if start.elapsed() > TIMEOUT {
            log.line("TIMEOUT: no usable auth cookie within 15 min");
            return None;
        }
        let Ok(cookies) = win.cookies_for_url(setup_url.clone()) else { continue };
        let Some(token) = token_of(&cookies) else { continue };
        if rejected.as_deref() == Some(token.as_str()) {
            continue;
        }

        log.line(format!("auth cookie seen after {}s; waiting 8s for the page to settle", start.elapsed().as_secs()));
        thread::sleep(Duration::from_secs(8));
        let cookies = win.cookies_for_url(setup_url.clone()).unwrap_or(cookies);
        log.line(format!("{} cookies from the webview", cookies.len()));

        match validate(log, &cookies) {
            Some(s) => return Some(s),
            None => {
                log.line("validate failed; waiting for a new auth cookie (sign in in the window if it asks)");
                rejected = token_of(&cookies);
            }
        }
    }
}

fn token_of(cookies: &[Cookie<'static>]) -> Option<String> {
    cookies.iter().find(|c| c.name() == "X-APPLE-WEBAUTH-TOKEN").map(|c| c.value().to_owned())
}

fn validate(log: &mut Log, cookies: &[Cookie<'static>]) -> Option<Session> {
    // A jar instead of a fixed Cookie header, so the cookies validate sets again are kept.
    let jar = Arc::new(Jar::default());
    let base: reqwest::Url = ICLOUD.parse().unwrap();
    for c in cookies {
        jar.add_cookie_str(&format!("{}={}; Domain=icloud.com; Path=/; Secure", c.name(), c.value()), &base);
    }
    let client = Client::builder().user_agent(UA).cookie_provider(jar).build().unwrap();

    log.line(format!("\n== validate ==\n  POST {VALIDATE}"));
    let resp = client
        .post(VALIDATE)
        .header("Origin", ORIGIN)
        .header("Referer", ICLOUD)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "*/*")
        .body("")
        .send();
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            log.line(format!("  send error: {e}"));
            return None;
        }
    };
    let status = resp.status();
    let v: Value = resp.json().unwrap_or(Value::Null);
    log.line(format!("  status: {status}"));
    if !status.is_success() {
        log.line(format!("  error: {}", err_fields(&v)));
        return None;
    }
    let Some(ckdb) = v["webservices"]["ckdatabasews"]["url"].as_str() else {
        log.line("  no webservices.ckdatabasews.url");
        return None;
    };
    Some(Session { client, ckdb: ckdb.trim_end_matches('/').to_owned() })
}

/// {ckdatabasews}{DB}/{path}, no query params (spike 2: none are needed).
fn ck(log: &mut Log, s: &Session, method: &str, path: &str, body: Option<&Value>, quiet: bool) -> Option<(u16, Value, String)> {
    let url = format!("{}{DB}/{path}", s.ckdb);
    let mut req = if method == "GET" { s.client.get(&url) } else { s.client.post(&url) };
    req = req
        .header("Origin", ORIGIN)
        .header("Referer", ICLOUD)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "application/json");
    if let Some(b) = body {
        req = req.body(b.to_string());
    }
    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => {
            log.line(format!("  {method} {path}: send error: {e}"));
            return None;
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !quiet {
        log.line(format!("  {method} {path}: HTTP {status}, {} bytes, keys {:?}", text.len(), keys(&v)));
    }
    Some((status, v, text))
}

fn zone_body(opts: &Value, token: Option<&str>) -> Value {
    let mut z = json!({ "zoneID": { "zoneName": "Notes", "zoneType": "REGULAR_CUSTOM_ZONE" } });
    for (k, v) in opts.as_object().into_iter().flatten() {
        z[k] = v.clone();
    }
    if let Some(t) = token {
        z["syncToken"] = json!(t);
    }
    json!({ "zones": [z] })
}

#[derive(Default)]
struct Pull {
    pages: Vec<Vec<Value>>,
    tokens: Vec<String>,
    error: Option<String>,
}

impl Pull {
    fn final_token(&self) -> Option<&str> {
        self.tokens.last().map(String::as_str)
    }

    fn records(&self) -> impl Iterator<Item = &Value> {
        self.pages.iter().flatten()
    }
}

fn pull(log: &mut Log, s: &Session, label: &str, start: Option<&str>, opts: &Value, max_pages: usize, save: Option<&str>) -> Pull {
    log.line(format!("\n== {label} =="));
    let mut p = Pull::default();
    let mut token = start.map(str::to_owned);
    for page in 1..=max_pages {
        let body = zone_body(opts, token.as_deref());
        let Some((st, v, text)) = ck(log, s, "POST", "changes/zone", Some(&body), true) else {
            p.error = Some("send error".into());
            break;
        };
        let z = &v["zones"][0];
        if st != 200 || z.get("serverErrorCode").is_some() || !z["records"].is_array() {
            let e = format!("HTTP {st}, {} bytes; top: {}; zone: {}", text.len(), err_fields(&v), err_fields(z));
            log.line(format!("  page {page}: ERROR {e}"));
            p.error = Some(e);
            break;
        }
        if let Some(prefix) = save {
            let _ = fs::write(format!("{OUT}/{prefix}-{page:03}.json"), &text);
        }
        let rs = recs(z).to_vec();
        let del = rs.iter().filter(|r| is_tomb(r)).count();
        let more = z["moreComing"].as_bool().unwrap_or(false);
        let t = z["syncToken"].as_str().map(str::to_owned);
        log.line(format!(
            "  page {page}: {} records ({del} deleted), {} bytes, moreComing={more}, token {}",
            rs.len(),
            text.len(),
            t.as_deref().map(token_short).unwrap_or_else(|| "none".into())
        ));
        p.pages.push(rs);
        if let Some(t) = &t {
            p.tokens.push(t.clone());
        }
        token = t;
        if !more || token.is_none() {
            break;
        }
        if page == max_pages {
            log.line("  (page cap reached, moreComing still true)");
        }
    }
    p
}

fn key_of(r: &Value) -> (String, String) {
    let tag = if is_tomb(r) { "<deleted>" } else { r["recordChangeTag"].as_str().unwrap_or("") };
    (r["recordName"].as_str().unwrap_or("").to_owned(), tag.to_owned())
}

fn latest_map(records: impl IntoIterator<Item = Value>) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    for r in records {
        if let Some(n) = r["recordName"].as_str() {
            m.insert(n.to_owned(), r);
        }
    }
    m
}

/// Drops downloadURL, which is re-signed on every fetch, so values can be compared.
fn strip_urls(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().filter(|(k, _)| k.as_str() != "downloadURL").map(|(k, v)| (k.clone(), strip_urls(v))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(strip_urls).collect()),
        x => x.clone(),
    }
}

fn changed_fields(old: &Value, new: &Value) -> Vec<String> {
    let names: BTreeSet<&str> = keys(&old["fields"]).into_iter().chain(keys(&new["fields"])).collect();
    names
        .into_iter()
        .filter(|n| strip_urls(&old["fields"][*n]) != strip_urls(&new["fields"][*n]))
        .map(str::to_owned)
        .collect()
}

/// Delta against a cache: new / changed (which fields) / deleted, per recordType. Names only.
fn summarize_delta(log: &mut Log, cache: &HashMap<String, Value>, delta: &Pull) {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut fields: BTreeMap<String, usize> = BTreeMap::new();
    let mut unchanged_tag = 0;
    for r in delta.records() {
        let name = r["recordName"].as_str().unwrap_or("");
        let old = cache.get(name);
        let old_ty = old.and_then(|o| o["recordType"].as_str()).unwrap_or("?");
        let kind = match (old, is_tomb(r)) {
            (None, true) => "tombstone for unknown record".to_owned(),
            (Some(o), true) if is_tomb(o) => "tombstone again".to_owned(),
            (Some(_), true) => format!("tombstone for cached {old_ty}"),
            (None, false) => format!("new {}", r["recordType"].as_str().unwrap_or("?")),
            (Some(o), false) => {
                if o["recordChangeTag"] == r["recordChangeTag"] {
                    unchanged_tag += 1;
                }
                let ty = r["recordType"].as_str().unwrap_or("?");
                for f in changed_fields(o, r) {
                    *fields.entry(format!("{ty}.{f}")).or_default() += 1;
                }
                format!("changed {ty}")
            }
        };
        *kinds.entry(kind).or_default() += 1;
    }
    log.line(format!("  delta: {} records, error {:?}", delta.records().count(), delta.error));
    for (k, n) in &kinds {
        log.line(format!("    {k:<40} {n}"));
    }
    log.line(format!("    changed records with the same recordChangeTag: {unchanged_tag}"));
    log.line(format!("    changed fields: {fields:?}"));
}

fn cloudkit(log: &mut Log, s: &Session) {
    let lim = |n: u32| json!({ "resultsLimit": n });

    // B1: fresh full pull.
    let full = pull(log, s, "B1 fresh full pull, resultsLimit 200", None, &lim(200), MAX_PAGES, Some("full"));
    let Some(t0) = full.final_token().map(str::to_owned).filter(|_| full.error.is_none()) else {
        return log.line("full pull failed; stopping");
    };
    let all: Vec<Value> = full.records().cloned().collect();
    log.line(format!("  total {} records over {} pages; final token {}", all.len(), full.pages.len(), token_short(&t0)));
    let _ = fs::write(format!("{OUT}/token-r0.txt"), &t0);

    // Same paging as spike 2?
    let s2 = spike2_pages();
    let s2_sizes: Vec<usize> = s2.iter().map(|z| recs(z).len()).collect();
    let s2_ctrs: Vec<Option<u64>> = s2.iter().map(|z| z["syncToken"].as_str().and_then(token_counter)).collect();
    let sizes: Vec<usize> = full.pages.iter().map(Vec::len).collect();
    let ctrs: Vec<Option<u64>> = full.tokens.iter().map(|t| token_counter(t)).collect();
    log.line(format!("  page sizes now     {sizes:?}\n  page sizes spike 2 {s2_sizes:?}"));
    log.line(format!("  counters now     {ctrs:?}\n  counters spike 2 {s2_ctrs:?}"));
    let s2_cache = latest_map(s2.iter().flat_map(|z| recs(z).iter().cloned()));
    analyze(log, &all);

    // B1b: other resultsLimit values, first pages only.
    for (label, opts) in [("resultsLimit 50", lim(50)), ("no resultsLimit", json!({})), ("resultsLimit 1000", lim(1000)), ("resultsLimit 1", lim(1))] {
        pull(log, s, &format!("B1b paging probe: {label}, first 3 pages"), None, &opts, 3, None);
    }

    // B1c: incremental from the final token right away.
    let inc = pull(log, s, "B1c incremental from the final token (expect empty)", Some(&t0), &lim(200), 5, None);
    log.line(format!("  returned token identical to the one sent: {}", inc.final_token() == Some(t0.as_str())));

    // B2: resume from a mid-run token.
    if full.tokens.len() > 4 {
        let mid = full.tokens[2].clone();
        let r = pull(log, s, "B2 resume from page 3's token", Some(&mid), &lim(200), MAX_PAGES, None);
        let orig: BTreeSet<(String, String)> = full.pages[3..].iter().flatten().map(key_of).collect();
        let got: BTreeSet<(String, String)> = r.records().map(key_of).collect();
        log.line(format!(
            "  (name,tag) pairs: original pages 4.. {}, resumed {}, only original {}, only resumed {}, same final token: {}",
            orig.len(),
            got.len(),
            orig.difference(&got).count(),
            got.difference(&orig).count(),
            r.final_token() == full.final_token()
        ));
        let a: Vec<usize> = full.pages[3..].iter().map(Vec::len).collect();
        let b: Vec<usize> = r.pages.iter().map(Vec::len).collect();
        log.line(format!("  page sizes original {a:?}\n  page sizes resumed  {b:?}"));
        pull(log, s, "B2b resume from page 3's token with resultsLimit 50, 2 pages", Some(&mid), &lim(50), 2, None);
    }

    // B3: spike 2's token (old) and one of its mid-run tokens.
    let old_path = format!("{SPIKE2_OUT}/synctoken.txt");
    match fs::read_to_string(&old_path) {
        Ok(old) => {
            let old = old.trim();
            let age = fs::metadata(&old_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .map(|d| d.as_secs_f64() / 3600.0);
            log.line(format!("\nspike 2 final token: saved {age:.1?} h ago, {}", token_short(old)));
            let d = pull(log, s, "B3 delta from spike 2's final token", Some(old), &lim(200), MAX_PAGES, Some("old-delta"));
            summarize_delta(log, &s2_cache, &d);
            log.line(format!("  final token equals today's: {}", d.final_token() == Some(t0.as_str())));
            pull(
                log,
                s,
                "B3b same, desiredKeys=[ModificationDate] (payload size)",
                Some(old),
                &json!({ "resultsLimit": 200, "desiredKeys": ["ModificationDate"] }),
                MAX_PAGES,
                None,
            );
        }
        Err(e) => log.line(format!("\nno spike 2 token: {e}")),
    }
    if let Some(mid_old) = s2.get(4).and_then(|z| z["syncToken"].as_str()) {
        pull(log, s, "B3c spike 2's page 5 (mid-run) token, 1 page", Some(mid_old), &lim(200), 1, None);
    }

    // B4: bad tokens.
    let bad: Vec<(&str, Option<String>)> = vec![
        ("flipped middle byte", token_flip_byte(&t0)),
        ("truncated to half", Some(t0[..t0.len() / 2].to_owned())),
        ("garbage", Some("not-a-token".into())),
        ("empty string", Some(String::new())),
        ("counter +1,000,000", token_with_counter(&t0, |c| c + 1_000_000)),
        ("counter -1000", token_with_counter(&t0, |c| c.saturating_sub(1000))),
        ("counter 1", token_with_counter(&t0, |_| 1)),
    ];
    for (label, t) in bad {
        match t {
            Some(t) => {
                pull(log, s, &format!("B4 bad token: {label} ({})", token_short(&t)), Some(&t), &lim(200), 1, None);
            }
            None => log.line(format!("\n== B4 bad token: {label} == could not build")),
        }
    }

    // B5: cheap "anything changed?" checks.
    log.line("\n== B5 changes/database (no token) ==");
    if let Some((_, v, text)) = ck(log, s, "POST", "changes/database", Some(&json!({})), false) {
        let _ = fs::write(format!("{OUT}/changes-database.json"), &text);
        log.line(format!("  {}", err_fields(&v)));
        db_zones(log, &v);
        if let Some(t) = v["syncToken"].as_str() {
            log.line(format!("  database token: {}", token_desc(t)));
            log.line("== B5 changes/database with its token ==");
            if let Some((_, v2, _)) = ck(log, s, "POST", "changes/database", Some(&json!({ "syncToken": t })), false) {
                db_zones(log, &v2);
                log.line(format!("  token unchanged: {}", v2["syncToken"].as_str() == Some(t)));
            }
        }
    }
    log.line("\n== B5 zones/list ==");
    for method in ["GET", "POST"] {
        if let Some((st, v, _)) = ck(log, s, method, "zones/list", (method == "POST").then(|| json!({})).as_ref(), false) {
            log.line(format!("  {}", err_fields(&v)));
            db_zones(log, &v);
            if st == 200 {
                break;
            }
        }
    }
    log.line("\n== B5 zones/lookup ==");
    if let Some((_, v, text)) = ck(log, s, "POST", "zones/lookup", Some(&json!({ "zones": [{ "zoneName": "Notes" }] })), false) {
        let _ = fs::write(format!("{OUT}/zones-lookup.json"), &text);
        log.line(format!("  {}", err_fields(&v)));
        db_zones(log, &v);
        if let Some(t) = v["zones"][0]["syncToken"].as_str() {
            log.line(format!("  zone syncToken {}; equals changes/zone final token: {}", token_short(t), t == t0));
        }
    }
    let filtered = pull(
        log,
        s,
        "B5 changes/zone desiredRecordTypes=[Note] desiredKeys=[ModificationDate], 2 pages",
        None,
        &json!({ "resultsLimit": 200, "desiredRecordTypes": ["Note"], "desiredKeys": ["ModificationDate"] }),
        2,
        None,
    );
    let mut ftypes: BTreeMap<String, usize> = BTreeMap::new();
    let mut ffields = BTreeSet::new();
    for r in filtered.records() {
        *ftypes.entry(r["recordType"].as_str().unwrap_or("<none>").to_owned()).or_default() += 1;
        ffields.extend(keys(&r["fields"]).into_iter().map(str::to_owned));
    }
    log.line(format!("  types {ftypes:?}, fields {ffields:?}"));

    // B6: records/lookup compared with the changes/zone copy.
    let cache = latest_map(all.iter().cloned());
    let find = |f: &dyn Fn(&Value) -> bool| cache.values().find(|r| f(r)).and_then(|r| r["recordName"].as_str()).map(str::to_owned);
    let picks = [
        ("Note with TextDataAsset", find(&|r| r["recordType"] == "Note" && r["fields"]["TextDataAsset"].is_object())),
        ("Note with inline body", find(&|r| r["recordType"] == "Note" && r["fields"]["TextDataEncrypted"].is_object())),
        ("Attachment with Media", find(&|r| r["recordType"] == "Attachment" && r["fields"]["Media"].is_object())),
        ("tombstone", find(&|r| is_tomb(r))),
        ("never existed", Some("spike6-does-not-exist".to_owned())),
    ];
    for (label, name) in picks {
        let Some(name) = name else {
            log.line(format!("\n== B6 lookup {label}: none in cache =="));
            continue;
        };
        log.line(format!("\n== B6 records/lookup: {label} =="));
        let body = json!({ "records": [{ "recordName": name }], "zoneID": { "zoneName": "Notes", "zoneType": "REGULAR_CUSTOM_ZONE" } });
        let Some((_, v, _)) = ck(log, s, "POST", "records/lookup", Some(&body), false) else { continue };
        let r = &v["records"][0];
        if r.get("serverErrorCode").is_some() || !r.is_object() {
            log.line(format!("  record error: {} / top: {}", err_fields(r), err_fields(&v)));
            continue;
        }
        let Some(c) = cache.get(&name) else { continue };
        let lf: BTreeSet<&str> = keys(&r["fields"]).into_iter().collect();
        let cf: BTreeSet<&str> = keys(&c["fields"]).into_iter().collect();
        log.line(format!(
            "  record keys {:?}\n  same changeTag: {}; fields only in lookup {:?}; only in changes/zone {:?}; differing values (URLs ignored) {:?}",
            keys(r),
            r["recordChangeTag"] == c["recordChangeTag"],
            lf.difference(&cf).collect::<Vec<_>>(),
            cf.difference(&lf).collect::<Vec<_>>(),
            changed_fields(c, r)
        ));
        for (f, val) in r["fields"].as_object().into_iter().flatten() {
            if val["type"].as_str().is_some_and(|t| t.starts_with("ASSETID")) {
                let one = if val["value"].is_array() { &val["value"][0] } else { &val["value"] };
                log.line(format!(
                    "  asset {f}: value keys {:?}; downloadURL same as in changes/zone: {}",
                    keys(one),
                    {
                        let cv = &c["fields"][f]["value"];
                        let cone = if cv.is_array() { &cv[0] } else { cv };
                        cone["downloadURL"] == one["downloadURL"]
                    }
                ));
            }
        }
    }
    if let Some(name) = cache.values().find(|r| r["recordType"] == "Note" && !is_tomb(r)).and_then(|r| r["recordName"].as_str()) {
        log.line("\n== B6b records/lookup with desiredKeys=[ModificationDate] ==");
        let body = json!({ "records": [{ "recordName": name }], "zoneID": { "zoneName": "Notes" }, "desiredKeys": ["ModificationDate"] });
        if let Some((_, v, _)) = ck(log, s, "POST", "records/lookup", Some(&body), false) {
            log.line(format!("  fields {:?} {}", keys(&v["records"][0]["fields"]), err_fields(&v["records"][0])));
        }
    }
}

// ---------------------------------------------------------------- phone rounds (phase C)

/// R0 cache: the B1 full pull pages, plus its final token.
fn r0_cache() -> Option<(HashMap<String, Value>, String)> {
    let mut all = Vec::new();
    for i in 1.. {
        let Ok(t) = fs::read_to_string(format!("{OUT}/full-{i:03}.json")) else { break };
        let v: Value = serde_json::from_str(&t).ok()?;
        all.extend(recs(&v["zones"][0]).iter().cloned());
    }
    let token = fs::read_to_string(format!("{OUT}/token-r0.txt")).ok()?;
    (!all.is_empty()).then(|| (latest_map(all), token.trim().to_owned()))
}

/// The running cache from earlier rounds, or R0 if there is none yet.
fn load_cache() -> Option<(HashMap<String, Value>, String)> {
    let c = fs::read_to_string(format!("{OUT}/cache.json"));
    let t = fs::read_to_string(format!("{OUT}/token-current.txt"));
    match (c, t) {
        (Ok(c), Ok(t)) => Some((serde_json::from_str(&c).ok()?, t.trim().to_owned())),
        _ => r0_cache(),
    }
}

/// Short, non-reversible id so one record can be followed across rounds without printing its name.
fn tag(name: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("#{:06x}", h & 0xff_ffff)
}

/// Folder labels numbered like `analyze` does (sorted live Folder names), so reports line up.
fn folder_redactor(cache: &HashMap<String, Value>) -> Redactor {
    let mut red = Redactor::default();
    let mut names: Vec<&str> = cache
        .values()
        .filter(|r| r["recordType"] == "Folder" && !is_tomb(r))
        .filter_map(|r| r["recordName"].as_str())
        .collect();
    names.sort();
    for n in names {
        red.name(n);
    }
    red
}

fn describe(log: &mut Log, red: &mut Redactor, r: &Value, old: Option<&Value>) {
    let name = r["recordName"].as_str().unwrap_or("");
    let ty = r["recordType"].as_str().or_else(|| old.and_then(|o| o["recordType"].as_str())).unwrap_or("?");
    let folder = |v: Option<&Value>, red: &mut Redactor| {
        v.and_then(|v| v["fields"]["Folder"]["value"]["recordName"].as_str()).map(|t| red.name(t)).unwrap_or_else(|| "-".into())
    };
    let deleted = |v: Option<&Value>| v.map(|v| v["fields"]["Deleted"]["value"].to_string()).unwrap_or_else(|| "-".into());
    if is_tomb(r) {
        let was = match old {
            Some(o) if is_tomb(o) => "already a tombstone",
            Some(_) => "cached",
            None => "not in cache",
        };
        log.line(format!(
            "  {} {ty:<28} TOMBSTONE (was {was}); keys {:?}; last folder {}",
            tag(name),
            keys(r),
            folder(old, red)
        ));
        return;
    }
    let kind = match old {
        None => "NEW".to_owned(),
        Some(o) if is_tomb(o) => "REVIVED".to_owned(),
        Some(o) => format!("CHANGED (tag {})", if o["recordChangeTag"] == r["recordChangeTag"] { "same" } else { "new" }),
    };
    let fields = match old {
        Some(o) if !is_tomb(o) => changed_fields(o, r),
        _ => keys(&r["fields"]).into_iter().map(str::to_owned).collect(),
    };
    let mut extra = String::new();
    if r["fields"]["Folder"].is_object() || old.is_some_and(|o| o["fields"]["Folder"].is_object()) {
        extra += &format!(" Folder {} -> {}", folder(old, red), folder(Some(r), red));
    }
    if r["fields"]["Deleted"].is_object() || old.is_some_and(|o| o["fields"]["Deleted"].is_object()) {
        extra += &format!(" Deleted {} -> {}", deleted(old), deleted(Some(r)));
    }
    if let Some(n) = r["fields"]["Note"]["value"]["recordName"].as_str() {
        extra += &format!(" note {}", tag(n));
    }
    if ty == "Folder" {
        extra += &format!(" folder {}", red.name(name));
    }
    log.line(format!("  {} {ty:<28} {kind};{extra}\n      fields: {fields:?}", tag(name)));
}

fn print_delta(log: &mut Log, cache: &HashMap<String, Value>, d: &Pull) {
    let mut red = folder_redactor(cache);
    log.line(format!("  {} records in {} pages, error {:?}", d.records().count(), d.pages.len(), d.error));
    for r in d.records() {
        let old = r["recordName"].as_str().and_then(|n| cache.get(n));
        describe(log, &mut red, r, old);
    }
}

fn delta_round(log: &mut Log, s: &Session, label: &str) {
    let Some((mut cache, token)) = load_cache() else {
        return log.line("no cache: run the network mode first (needs out/full-*.json and token-r0.txt)");
    };
    log.line(format!("\n== round {label}: cache {} records, token {} ==", cache.len(), token_short(&token)));
    if let Some((_, v, _)) = ck(log, s, "POST", "zones/lookup", Some(&json!({ "zones": [{ "zoneName": "Notes" }] })), false) {
        let zt = v["zones"][0]["syncToken"].as_str().unwrap_or("");
        log.line(format!("  zones/lookup token {}; equals stored: {}", token_short(zt), zt == token));
    }
    let d = pull(log, s, &format!("changes/zone from the stored token ({label})"), Some(&token), &json!({ "resultsLimit": 200 }), MAX_PAGES, Some(&format!("delta-{label}")));
    print_delta(log, &cache, &d);
    let Some(t) = d.final_token().filter(|_| d.error.is_none()) else {
        return log.line("  delta failed; cache and token left as they were");
    };
    for r in d.records() {
        if let Some(n) = r["recordName"].as_str() {
            cache.insert(n.to_owned(), r.clone());
        }
    }
    let _ = fs::write(format!("{OUT}/cache.json"), serde_json::to_string(&cache).unwrap_or_default());
    let _ = fs::write(format!("{OUT}/token-current.txt"), t);
    let _ = fs::write(format!("{OUT}/token-{label}.txt"), t);
    log.line(format!("  cache now {} records; token saved ({})", cache.len(), token_short(t)));
}

fn collapse(log: &mut Log, s: &Session) {
    let Some((cache, token)) = r0_cache() else {
        return log.line("no R0 cache");
    };
    let d = pull(log, s, "everything since the R0 token, against the R0 cache", Some(&token), &json!({ "resultsLimit": 200 }), MAX_PAGES, Some("collapse"));
    print_delta(log, &cache, &d);
}

fn db_zones(log: &mut Log, v: &Value) {
    let Some(zones) = v["zones"].as_array() else { return };
    log.line(format!("  {} zones, moreComing={}, top-level syncToken: {}", zones.len(), v["moreComing"], v["syncToken"].is_string()));
    for z in zones {
        log.line(format!(
            "    zone {} keys {:?}{}",
            z["zoneID"]["zoneName"].as_str().unwrap_or("?"),
            keys(z),
            z["syncToken"].as_str().map(|t| format!(" token {}", token_short(t))).unwrap_or_default()
        ));
    }
}

/// Only the error/serverErrorCode/reason fields: error bodies can carry trust tokens.
fn err_fields(v: &Value) -> String {
    let parts: Vec<String> = ["error", "serverErrorCode", "reason", "errorCode"]
        .iter()
        .filter_map(|k| v.get(*k).map(|x| format!("{k}={x}")))
        .collect();
    if parts.is_empty() { "(no error fields)".into() } else { parts.join(" ") }
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default()
}
