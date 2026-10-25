// Spike 2: reuse spike 1's WebView2 session (same identifier, same profile), call
// setup validate, then CloudKit changes/zone on com.apple.notes and save the raw pages.
//
// Privacy: out/ (gitignored) holds the raw note data. report.txt and stdout only get
// status codes, param names, counts, field names and type tags: never note text,
// recordNames, dsid, cookie values or error bodies (only their error/reason fields).

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
const DB: &str = "/database/1/com.apple.notes/production/private";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out");
const REPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out/report.txt");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PROBE_LIMIT: u32 = 5;
const PAGE_LIMIT: u32 = 200;
const MAX_PAGES: usize = 50;

type Params = Vec<(&'static str, String)>;

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
    notes: Option<String>,
    dsid: String,
    client_id: String,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(ICLOUD.parse()?))
                .title("spike2: iCloud session (sign in only if asked; closes by itself)")
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
        Some(s) => cloudkit(&mut log, &s),
        None => log.line("no session; stopping"),
    }
    log.line(format!("done; report is {REPORT}, raw pages are in {OUT}"));
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
        let mut names: Vec<_> = cookies.iter().map(|c| c.name().to_owned()).collect();
        names.sort();
        log.line(format!("{} cookies from the webview: {names:?}", cookies.len()));

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
    let dsid = match &v["dsInfo"]["dsid"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => {
            log.line("  no dsInfo.dsid");
            return None;
        }
    };
    let notes = ws["notes"]["url"].as_str().map(|u| u.trim_end_matches('/').to_owned());
    log.line(format!("  ckdatabasews={ckdb} notes={notes:?} dsid present"));
    Some(Session {
        client,
        ckdb: ckdb.trim_end_matches('/').to_owned(),
        notes,
        dsid,
        client_id: uuid::Uuid::new_v4().to_string().to_uppercase(),
    })
}

/// Param sets from smallest to largest: none, what hstratus-notes sends, and that plus
/// pyicloud's client/ckjs params and dsid. The build/version values are made up.
fn param_sets(s: &Session) -> Vec<(&'static str, Params)> {
    let hstratus: Params = vec![
        ("remapEnums", "true".into()),
        ("getCurrentSyncToken", "true".into()),
        ("clientId", s.client_id.clone()),
    ];
    let mut full: Params = vec![
        ("clientBuildNumber", "2534Project66".into()),
        ("clientMasteringNumber", "2534B22".into()),
        ("ckjsBuildVersion", "2310ProjectDev27".into()),
        ("ckjsVersion", "2.6.4".into()),
    ];
    full.extend(hstratus.iter().cloned());
    full.push(("dsid", s.dsid.clone()));
    vec![("bare", vec![]), ("hstratus", hstratus), ("full", full)]
}

fn zone_body(limit: u32, token: Option<&str>, zone_type: bool) -> Value {
    let mut zone_id = json!({ "zoneName": "Notes" });
    if zone_type {
        zone_id["zoneType"] = json!("REGULAR_CUSTOM_ZONE");
    }
    let mut zone = json!({ "zoneID": zone_id, "resultsLimit": limit });
    if let Some(t) = token {
        zone["syncToken"] = json!(t);
    }
    json!({ "zones": [zone] })
}

fn cloudkit(log: &mut Log, s: &Session) {
    let sets = param_sets(s);
    let mut working: Option<(&str, Params)> = None;
    let mut last_status = 0;
    for (name, params) in &sets {
        let body = zone_body(PROBE_LIMIT, None, true);
        if let Some((st, v, _)) = ck_post(log, s, &format!("changes/zone, params={name}"), "changes/zone", params, true, &body) {
            last_status = st;
            if zone_result(log, st, &v).is_some() && working.is_none() {
                working = Some((name, params.clone()));
            }
        }
    }

    let Some((name, params)) = working else {
        log.line("\nno param set worked on changes/zone");
        let full = &sets[2].1;
        if matches!(last_status, 400 | 404) {
            let body = zone_body(PROBE_LIMIT, None, true);
            if let Some((st, v, _)) = ck_post(log, s, "records/changes, params=full", "records/changes", full, true, &body) {
                if zone_result(log, st, &v).is_some() {
                    return full_pull(log, s, "records/changes", full);
                }
            }
        }
        return notes_fallback(log, s, full);
    };
    log.line(format!("\nsmallest working param set: {name}"));

    let body = zone_body(PROBE_LIMIT, None, true);
    if let Some((st, v, _)) = ck_post(log, s, &format!("changes/zone, params={name}, no Origin/Referer"), "changes/zone", &params, false, &body) {
        zone_result(log, st, &v);
    }
    let body = zone_body(PROBE_LIMIT, None, false);
    if let Some((st, v, _)) = ck_post(log, s, &format!("changes/zone, params={name}, zoneID without zoneType"), "changes/zone", &params, true, &body) {
        zone_result(log, st, &v);
    }

    full_pull(log, s, "changes/zone", &params);
}

fn full_pull(log: &mut Log, s: &Session, path: &str, params: &Params) {
    log.line(format!("\n== full pull: {path}, resultsLimit {PAGE_LIMIT}, max {MAX_PAGES} pages =="));
    let mut token: Option<String> = None;
    let mut stats = Stats::default();
    for page in 1..=MAX_PAGES {
        let body = zone_body(PAGE_LIMIT, token.as_deref(), true);
        let Some((st, v, text)) = ck_post(log, s, &format!("page {page}"), path, params, true, &body) else { return };
        let Some(zone) = zone_result(log, st, &v) else { return };

        let file = format!("{OUT}/changes-zone-page-{page:03}.json");
        if let Err(e) = fs::write(&file, &text) {
            log.line(format!("  could not save page: {e}"));
        }
        for r in zone["records"].as_array().into_iter().flatten() {
            stats.add(r);
        }
        let more = zone["moreComing"].as_bool().unwrap_or(false);
        token = zone["syncToken"].as_str().map(str::to_owned);
        if !more {
            break;
        }
        if token.is_none() {
            log.line("  moreComing but no syncToken; stopping");
            break;
        }
        if page == MAX_PAGES {
            log.line("  page cap reached with moreComing=true");
        }
    }
    stats.report(log);

    let Some(t) = token else {
        return log.line("no final syncToken");
    };
    let _ = fs::write(format!("{OUT}/synctoken.txt"), &t);
    let body = zone_body(PAGE_LIMIT, Some(&t), true);
    if let Some((st, v, _)) = ck_post(log, s, "incremental call with the final syncToken", path, params, true, &body) {
        zone_result(log, st, &v);
    }
}

fn notes_fallback(log: &mut Log, s: &Session, params: &Params) {
    let Some(notes) = &s.notes else {
        return log.line("no notes webservice in validate");
    };
    let url = format!("{notes}/no/startup");
    for method in ["GET", "POST"] {
        log.line(format!("\n== notes fallback: {method} {{notes}}/no/startup params={:?} ==", names(params)));
        let req = if method == "GET" { s.client.get(&url) } else { s.client.post(&url).body("") };
        let resp = req.query(params).header("Origin", ORIGIN).header("Referer", ICLOUD).send();
        match resp {
            Ok(r) => {
                let status = r.status();
                let v: Value = r.json().unwrap_or(Value::Null);
                log.line(format!("  status: {status}, top-level keys: {:?}", keys(&v)));
                if !status.is_success() {
                    log.line(format!("  error: {}", err_fields(&v)));
                }
            }
            Err(e) => log.line(format!("  send error: {e}")),
        }
    }
}

/// POSTs to {ckdatabasews}{DB}/{path}. Logs param names only: the URL carries the dsid.
fn ck_post(
    log: &mut Log,
    s: &Session,
    label: &str,
    path: &str,
    params: &Params,
    origin: bool,
    body: &Value,
) -> Option<(u16, Value, String)> {
    log.line(format!("\n== {label} ==\n  POST {{ckdatabasews}}{DB}/{path} params={:?} origin={origin}", names(params)));
    let mut req = s
        .client
        .post(format!("{}{DB}/{path}", s.ckdb))
        .query(params)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "application/json")
        .body(body.to_string());
    if origin {
        req = req.header("Origin", ORIGIN).header("Referer", ICLOUD);
    }
    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => {
            log.line(format!("  send error: {e}"));
            return None;
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
    log.line(format!("  status: {status}, {} bytes, top-level keys: {:?}", text.len(), keys(&v)));
    Some((status, v, text))
}

/// Returns the zone dictionary if the call succeeded and it carries a records array.
fn zone_result<'a>(log: &mut Log, status: u16, v: &'a Value) -> Option<&'a Value> {
    if status != 200 {
        log.line(format!("  error: {}", err_fields(v)));
        return None;
    }
    let Some(zone) = v["zones"].get(0) else {
        log.line("  no zones[0]");
        return None;
    };
    if zone.get("serverErrorCode").is_some() {
        log.line(format!("  zone error: {}", err_fields(zone)));
        return None;
    }
    let n = zone["records"].as_array().map(Vec::len);
    log.line(format!(
        "  zone keys: {:?}, records: {n:?}, moreComing: {}, syncToken present: {}",
        keys(zone),
        zone["moreComing"],
        zone["syncToken"].is_string(),
    ));
    n.map(|_| zone)
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

fn names(params: &Params) -> Vec<&str> {
    params.iter().map(|p| p.0).collect()
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
            "\n== records: {} total, {} deleted, {} per-record errors ==\n  record keys: {:?}",
            self.records, self.deleted, self.errors, self.record_keys
        ));
        for (ty, t) in &self.types {
            log.line(format!("  {ty}: {}", t.count));
            for (name, (n, tags)) in &t.fields {
                log.line(format!("    {name:<32} {n:>5}x  {tags:?}"));
            }
        }
    }
}
