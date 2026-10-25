// Spike 7: reuse spike 1's WebView2 session (same identifier, same profile), call setup
// validate, then read-only CloudKit probes: zone listings in the private and shared DBs,
// changes/zone for every shared zone, and records/lookup for folder/share fields.
//
// Read-only: only validate, zones/list, changes/database, changes/zone and records/lookup.
// Privacy: out/ (gitignored) holds raw responses. report.txt and stdout only get status
// codes, param names, counts, field names, type tags, zone names and system recordNames:
// never note/folder titles, user recordNames, dsid, cookie values or error bodies.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use reqwest::{blocking::Client, cookie::Jar};
use serde_json::{Value, json};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::Cookie};

const ICLOUD: &str = "https://www.icloud.com/";
const ORIGIN: &str = "https://www.icloud.com";
const VALIDATE: &str = "https://setup.icloud.com/setup/ws/1/validate";
const DB: &str = "/database/1/com.apple.notes/production";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out");
const REPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out/report.txt");
/// Spike 2's saved pages, read (never written) to pick recordNames for records/lookup.
const SPIKE2_OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spike2-cloudkit/out");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PAGE_LIMIT: u32 = 200;
const MAX_PAGES: usize = 20;

struct Log(String);

impl Log {
    fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        self.0.push_str(s.as_ref());
        self.0.push('\n');
        // Rewritten after every line, so closing the window early keeps what was logged.
        let _ = fs::write(REPORT, &self.0);
    }
}

struct Session {
    client: Client,
    ckdb: String,
    ckshare: Option<String>,
}

/// Maps ownerRecordNames to "me" / "other#N" so the report never shows them.
#[derive(Default)]
struct Owners {
    me: Option<String>,
    others: Vec<String>,
}

impl Owners {
    fn label(&mut self, owner: Option<&str>) -> String {
        let Some(o) = owner else { return "<none>".into() };
        if self.me.as_deref() == Some(o) {
            return "me".into();
        }
        let i = self.others.iter().position(|x| x == o).unwrap_or_else(|| {
            self.others.push(o.to_owned());
            self.others.len() - 1
        });
        format!("other#{}", i + 1)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(ICLOUD.parse()?))
                .title("spike7: iCloud session (sign in only if asked; closes by itself)")
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
    let mut log = Log(String::new());
    let _ = fs::create_dir_all(OUT);
    match session(&mut log, &win) {
        Some(s) => probes(&mut log, &s),
        None => log.line("no session; stopping"),
    }
    log.line(format!("done; report is {REPORT}, raw responses are in {OUT}"));
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

    let ws = &v["webservices"];
    let Some(ckdb) = ws["ckdatabasews"]["url"].as_str() else {
        log.line("  no webservices.ckdatabasews.url");
        return None;
    };
    for (name, svc) in ws.as_object().into_iter().flatten() {
        if name.starts_with("ck") || name.contains("shar") || name == "notes" {
            log.line(format!("  webservice {name}: url={} status={}", svc["url"], svc["status"]));
        }
    }
    let ckshare = ws["cksharews"]["url"].as_str().map(|u| u.trim_end_matches('/').to_owned());
    Some(Session { client, ckdb: ckdb.trim_end_matches('/').to_owned(), ckshare })
}

fn probes(log: &mut Log, s: &Session) {
    let mut owners = Owners::default();

    // 1. Zones in the private DB. The owner of Notes is "me" for every later label.
    for (label, scope) in [("private", "private"), ("shared", "shared")] {
        let file = format!("{scope}-zones-list");
        if let Some((st, v)) = ck(log, s, &s.ckdb, &format!("{label}: zones/list (GET)"), scope, "zones/list", None, &file) {
            if st == 200 {
                if scope == "private" {
                    owners.me = v["zones"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|z| z["zoneID"]["zoneName"] == "Notes")
                        .and_then(|z| z["zoneID"]["ownerRecordName"].as_str())
                        .map(str::to_owned);
                }
                zones_summary(log, &mut owners, &v["zones"]);
            }
        }
        let file = format!("{scope}-changes-database");
        if let Some((st, v)) = ck(log, s, &s.ckdb, &format!("{label}: changes/database"), scope, "changes/database", Some(&json!({})), &file) {
            if st == 200 {
                log.line(format!("  keys: {:?}, moreComing: {}, syncToken present: {}", keys(&v), v["moreComing"], v["syncToken"].is_string()));
                zones_summary(log, &mut owners, &v["zones"]);
            }
        }
    }

    // 2. Every zone the shared DB knows about, from either listing.
    let mut shared_zones: Vec<Value> = Vec::new();
    for file in ["shared-zones-list", "shared-changes-database"] {
        let v: Value = fs::read_to_string(format!("{OUT}/{file}.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        for z in v["zones"].as_array().into_iter().flatten() {
            let id = &z["zoneID"];
            if id.is_object() && !shared_zones.contains(id) {
                shared_zones.push(id.clone());
            }
        }
    }
    log.line(format!("\n== shared DB: {} distinct zone(s) ==", shared_zones.len()));
    for (i, id) in shared_zones.iter().enumerate() {
        zone_pull(log, s, &mut owners, "shared", id, &format!("shared-zone-{:02}", i + 1));
    }

    // 3. Does the share service answer the same database paths? (read-only listing only)
    match &s.ckshare {
        Some(share) => {
            ck(log, s, share, "cksharews: shared zones/list (GET)", "shared", "zones/list", None, "cksharews-shared-zones-list");
            ck(log, s, share, "cksharews: private zones/list (GET)", "private", "zones/list", None, "cksharews-private-zones-list");
        }
        None => log.line("\nno cksharews in validate"),
    }

    // 4. records/lookup on the private Notes zone: are share / parent fields hidden from changes/zone?
    lookups(log, s);
}

fn zones_summary(log: &mut Log, owners: &mut Owners, zones: &Value) {
    let Some(zs) = zones.as_array() else {
        return log.line("  no zones array");
    };
    log.line(format!("  {} zone(s)", zs.len()));
    for z in zs {
        let id = &z["zoneID"];
        let extra: Vec<&str> = keys(z).into_iter().filter(|k| *k != "zoneID").collect();
        log.line(format!(
            "    zoneName={} zoneType={} owner={} zoneID keys={:?} other keys={extra:?}{}",
            id["zoneName"],
            id["zoneType"],
            owners.label(id["ownerRecordName"].as_str()),
            keys(id),
            z.get("serverErrorCode").map(|_| format!(" error: {}", err_fields(z))).unwrap_or_default(),
        ));
    }
}

/// Pages changes/zone for one zone, saving each page and logging per-type field stats.
fn zone_pull(log: &mut Log, s: &Session, owners: &mut Owners, scope: &str, zone_id: &Value, file: &str) {
    let mut token: Option<String> = None;
    let mut stats = Stats::default();
    for page in 1..=MAX_PAGES {
        let mut zone = json!({ "zoneID": zone_id, "resultsLimit": PAGE_LIMIT });
        if let Some(t) = &token {
            zone["syncToken"] = json!(t);
        }
        let label = format!("{scope}: changes/zone zone={} owner={} page {page}", zone_id["zoneName"], owners.label(zone_id["ownerRecordName"].as_str()));
        let Some((st, v)) = ck(log, s, &s.ckdb, &label, scope, "changes/zone", Some(&json!({ "zones": [zone] })), &format!("{file}-page-{page:03}")) else { break };
        if st != 200 {
            break;
        }
        let Some(z) = v["zones"].get(0) else { break };
        if z.get("serverErrorCode").is_some() {
            log.line(format!("  zone error: {}", err_fields(z)));
            break;
        }
        for r in z["records"].as_array().into_iter().flatten() {
            stats.add(r);
        }
        log.line(format!("  records: {:?}, moreComing: {}", z["records"].as_array().map(Vec::len), z["moreComing"]));
        token = z["syncToken"].as_str().map(str::to_owned);
        if z["moreComing"].as_bool() != Some(true) || token.is_none() {
            break;
        }
    }
    stats.report(log);
}

fn lookups(log: &mut Log, s: &Session) {
    let (folder, shared_note) = pick_from_spike2();
    let mut names = vec!["DefaultFolder-CloudKit".to_owned(), "TrashFolder-CloudKit".to_owned(), "SystemPaper-CloudKit".to_owned()];
    names.extend(folder.clone());
    names.extend(shared_note.clone());
    log.line(format!(
        "\nlookup set: 3 system folders, custom folder with ParentModificationDate: {}, note with shortGUID: {}",
        folder.is_some(),
        shared_note.is_some()
    ));
    let records: Vec<Value> = names.iter().map(|n| json!({ "recordName": n })).collect();
    let zone = json!({ "zoneName": "Notes" });

    let body = json!({ "records": records, "zoneID": zone });
    if let Some((200, v)) = ck(log, s, &s.ckdb, "private: records/lookup (all keys)", "private", "records/lookup", Some(&body), "private-lookup-all") {
        lookup_summary(log, &v, &names);
    }
    // Field names other Notes clients use for nesting and sharing; asked for explicitly.
    let desired = ["ParentFolder", "Parent", "ParentFolderID", "Folder", "SortOrder", "Share", "share", "ShareURL", "Participants", "TitleEncrypted"];
    let body = json!({ "records": records, "zoneID": zone, "desiredKeys": desired });
    if let Some((200, v)) = ck(log, s, &s.ckdb, "private: records/lookup (desiredKeys)", "private", "records/lookup", Some(&body), "private-lookup-desired") {
        lookup_summary(log, &v, &names);
    }
}

fn lookup_summary(log: &mut Log, v: &Value, names: &[String]) {
    for (i, r) in v["records"].as_array().into_iter().flatten().enumerate() {
        let who = match i {
            0..=2 => names[i].clone(),
            _ => format!("<looked-up #{}>", i + 1),
        };
        if r.get("serverErrorCode").is_some() {
            log.line(format!("  {who}: {}", err_fields(r)));
            continue;
        }
        let fields: Vec<String> = r["fields"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, f)| format!("{k}[{}]", f["type"].as_str().unwrap_or("?")))
            .collect();
        log.line(format!("  {who}: type={} record keys={:?}\n      fields={fields:?}", r["recordType"], keys(r)));
        if let Some(sh) = r.get("share") {
            log.line(format!("      share keys={:?}", keys(sh)));
        }
    }
}

/// From spike 2's saved pages: a custom folder that has ParentModificationDate, and a note with shortGUID.
fn pick_from_spike2() -> (Option<String>, Option<String>) {
    let (mut folder, mut note) = (None, None);
    let mut files: Vec<_> = fs::read_dir(SPIKE2_OUT).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for p in files.iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("changes-zone-page-"))) {
        let Ok(t) = fs::read_to_string(p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
        for r in v["zones"][0]["records"].as_array().into_iter().flatten() {
            let name = r["recordName"].as_str().map(str::to_owned);
            if folder.is_none() && r["recordType"] == "Folder" && !r["fields"]["ParentModificationDate"].is_null() && !name.as_deref().unwrap_or("").ends_with("-CloudKit") {
                folder = name;
            } else if note.is_none() && r["recordType"] == "Note" && r["shortGUID"].is_string() {
                note = name;
            }
        }
    }
    (folder, note)
}

/// GET (body None) or POST to {base}{DB}/{scope}/{path}; saves the raw body to out/{file}.json.
/// Logs param names only: no URL query, which would carry nothing secret but keeps it uniform.
#[allow(clippy::too_many_arguments)]
fn ck(log: &mut Log, s: &Session, base: &str, label: &str, scope: &str, path: &str, body: Option<&Value>, file: &str) -> Option<(u16, Value)> {
    let host = if base == s.ckdb { "{ckdatabasews}" } else { "{cksharews}" };
    let method = if body.is_some() { "POST" } else { "GET" };
    log.line(format!("\n== {label} ==\n  {method} {host}{DB}/{scope}/{path}"));
    let url = format!("{base}{DB}/{scope}/{path}");
    let req = match body {
        Some(b) => s.client.post(&url).header("Content-Type", "text/plain;charset=UTF-8").body(b.to_string()),
        None => s.client.get(&url),
    };
    let resp = match req.header("Accept", "application/json").header("Origin", ORIGIN).header("Referer", ICLOUD).send() {
        Ok(r) => r,
        Err(e) => {
            log.line(format!("  send error: {e}"));
            return None;
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let _ = fs::write(format!("{OUT}/{file}.json"), &text);
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    log.line(format!("  status: {status}, {} bytes, top-level keys: {:?}", text.len(), keys(&v)));
    if status != 200 {
        log.line(format!("  error: {}", err_fields(&v)));
    }
    Some((status, v))
}

/// Only the error/serverErrorCode/reason fields: error bodies can carry trust tokens.
fn err_fields(v: &Value) -> String {
    let parts: Vec<String> = ["error", "serverErrorCode", "reason"]
        .iter()
        .filter_map(|k| v.get(*k).map(|x| format!("{k}={x}")))
        .collect();
    if parts.is_empty() { "(no error fields)".into() } else { parts.join(" ") }
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default()
}

#[derive(Default)]
struct Stats {
    records: usize,
    deleted: usize,
    errors: usize,
    record_keys: BTreeSet<String>,
    types: BTreeMap<String, TypeStats>,
}

#[derive(Default)]
struct TypeStats {
    count: usize,
    /// field name -> (records that have it, type tags seen)
    fields: BTreeMap<String, (usize, BTreeSet<String>)>,
}

impl Stats {
    fn add(&mut self, r: &Value) {
        self.records += 1;
        self.record_keys.extend(keys(r).into_iter().map(str::to_owned));
        if r.get("serverErrorCode").is_some() {
            self.errors += 1;
            return;
        }
        if r["deleted"].as_bool() == Some(true) {
            self.deleted += 1;
        }
        let ty = r["recordType"].as_str().unwrap_or("<none>").to_owned();
        let t = self.types.entry(ty).or_default();
        t.count += 1;
        for (name, f) in r["fields"].as_object().into_iter().flatten() {
            let e = t.fields.entry(name.clone()).or_default();
            e.0 += 1;
            e.1.insert(f["type"].as_str().unwrap_or("?").to_owned());
        }
    }

    fn report(&self, log: &mut Log) {
        log.line(format!(
            "  == records: {} total, {} deleted, {} per-record errors; record keys: {:?}",
            self.records, self.deleted, self.errors, self.record_keys
        ));
        for (ty, t) in &self.types {
            log.line(format!("    {ty}: {}", t.count));
            for (name, (n, tags)) in &t.fields {
                log.line(format!("      {name:<32} {n:>5}x  {tags:?}"));
            }
        }
    }
}
