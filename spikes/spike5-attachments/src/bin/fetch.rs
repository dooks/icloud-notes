// Spike 5 online part: reuse spike 1's WebView2 session (same identifier, same profile),
// validate, then for the (at most 2) targets in out/targets.json:
//   - range-GET the downloadURL saved by spike 2 (age test), without cookies
//   - records/lookup (read-only) to get a fresh asset value, compare it to the saved one
//   - range-GET the fresh URL with/without cookies, Origin and ${f} substitution
//   - one full download, saved to out/, checked against size and fileChecksum
//
// Privacy: out/fetch-report.txt and stdout get status codes, header names (values only for
// content-type/length/range/cache headers), sizes, hashes-match booleans and URL parameter
// *names*. Never cookie values, recordNames, URL values or file contents.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use reqwest::{blocking::Client, cookie::Jar};
use serde_json::{Value, json};
use sha1::Digest as _;
use spike5::{b64_decode, sniff};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::Cookie};

const ICLOUD: &str = "https://www.icloud.com/";
const ORIGIN: &str = "https://www.icloud.com";
const VALIDATE: &str = "https://setup.icloud.com/setup/ws/1/validate";
const DB: &str = "/database/1/com.apple.notes/production/private";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/out");
const REPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/out/fetch-report.txt");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct Log(String);

impl Log {
    fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        self.0.push_str(s.as_ref());
        self.0.push('\n');
        let _ = fs::write(REPORT, &self.0);
    }
}

struct Session {
    client: Client,
    ckdb: String,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(ICLOUD.parse()?))
                .title("spike5: iCloud session (sign in only if asked; closes by itself)")
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
    let targets: Vec<Value> = fs::read_to_string(format!("{OUT}/targets.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if targets.is_empty() {
        log.line("no out/targets.json (run `cargo run --bin analyze` first)");
    } else {
        match session(&mut log, &win) {
            Some(s) => {
                for t in targets.iter().take(2) {
                    target(&mut log, &s, t);
                }
            }
            None => log.line("no session; stopping"),
        }
    }
    log.line(format!("done; report is {REPORT}"));
    app.exit(0);
}

// ---------- session (copied from spike 2, trimmed) ----------

/// Waits for an auth cookie in the webview and runs validate with it. If validate
/// rejects that cookie (stale profile), stops: re-authenticating needs the user's OK.
fn session(log: &mut Log, win: &WebviewWindow) -> Option<Session> {
    let setup_url: tauri::Url = VALIDATE.parse().unwrap();
    let start = Instant::now();
    log.line("waiting for X-APPLE-WEBAUTH-TOKEN from the reused profile...");
    loop {
        thread::sleep(Duration::from_secs(2));
        if start.elapsed() > TIMEOUT {
            log.line("TIMEOUT: no auth cookie within 15 min");
            return None;
        }
        let Ok(cookies) = win.cookies_for_url(setup_url.clone()) else { continue };
        if token_of(&cookies).is_none() {
            if start.elapsed() > Duration::from_secs(60) {
                log.line("no auth cookie after 60 s: the profile looks signed out. Stopping (ask before signing in).");
                return None;
            }
            continue;
        }
        log.line(format!("auth cookie seen after {}s; waiting 8s for the page to settle", start.elapsed().as_secs()));
        thread::sleep(Duration::from_secs(8));
        let cookies = win.cookies_for_url(setup_url.clone()).unwrap_or(cookies);
        log.line(format!("{} cookies from the webview", cookies.len()));
        let s = validate(log, &cookies);
        if s.is_none() {
            log.line("validate failed: stale session. Stopping (ask before signing in).");
        }
        return s;
    }
}

fn token_of(cookies: &[Cookie<'static>]) -> Option<String> {
    cookies.iter().find(|c| c.name() == "X-APPLE-WEBAUTH-TOKEN").map(|c| c.value().to_owned())
}

fn validate(log: &mut Log, cookies: &[Cookie<'static>]) -> Option<Session> {
    let jar = Arc::new(Jar::default());
    let base: reqwest::Url = ICLOUD.parse().unwrap();
    for c in cookies {
        jar.add_cookie_str(&format!("{}={}; Domain=icloud.com; Path=/; Secure", c.name(), c.value()), &base);
    }
    let client = Client::builder().user_agent(UA).cookie_provider(jar).build().unwrap();
    let resp = client
        .post(VALIDATE)
        .header("Origin", ORIGIN)
        .header("Referer", ICLOUD)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .body("")
        .send()
        .ok()?;
    let status = resp.status();
    log.line(format!("validate: {status}"));
    let v: Value = resp.json().unwrap_or(Value::Null);
    let ckdb = v["webservices"]["ckdatabasews"]["url"].as_str()?;
    Some(Session { client, ckdb: ckdb.trim_end_matches('/').to_owned() })
}

// ---------- attachment tests ----------

fn target(log: &mut Log, s: &Session, t: &Value) {
    let kind = t["kind"].as_str().unwrap_or("?");
    let field = t["field"].as_str().unwrap_or("?");
    let saved = &t["saved"];
    log.line(format!("\n== target {kind}: uti={} {field} size={} ==", t["uti"], t["size"]));
    let bare = Client::builder().user_agent(UA).build().unwrap();

    let old_url = saved["downloadURL"].as_str().unwrap_or("").to_owned();
    let pages_age = fs::metadata(concat!(env!("CARGO_MANIFEST_DIR"), "/../spike2-cloudkit/out/changes-zone-page-001.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map(|d| format!("{:.1} h", d.as_secs_f64() / 3600.0))
        .unwrap_or("?".into());
    log.line(format!("saved URL: {} (pages saved {pages_age} ago)", url_shape(&old_url)));
    probe(log, &bare, "saved URL, ${f}=file, no cookies", &old_url.replace("${f}", "file"), false);

    // records/lookup: read-only.
    let url = format!("{}{DB}/records/lookup", s.ckdb);
    let body = json!({"records": [{"recordName": t["record"]}], "zoneID": {"zoneName": "Notes"}});
    let resp = s
        .client
        .post(&url)
        .header("Origin", ORIGIN)
        .header("Referer", ICLOUD)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .body(body.to_string())
        .send();
    let v: Value = match resp {
        Ok(r) => {
            log.line(format!("records/lookup: {}", r.status()));
            r.json().unwrap_or(Value::Null)
        }
        Err(e) => {
            log.line(format!("records/lookup send error: {e}"));
            return;
        }
    };
    let rec = &v["records"][0];
    if rec.get("serverErrorCode").is_some() {
        log.line(format!("  record error: {} {}", rec["serverErrorCode"], rec["reason"]));
        return;
    }
    let fv = &rec["fields"][field]["value"];
    let fresh = match fv {
        Value::Array(a) => a.iter().find(|x| x["fileChecksum"] == saved["fileChecksum"]).or(a.first()).cloned(),
        Value::Object(_) => Some(fv.clone()),
        _ => None,
    };
    let Some(fresh) = fresh else {
        log.line("  no asset in lookup result");
        return;
    };
    for k in ["fileChecksum", "referenceChecksum", "wrappingKey", "size"] {
        log.line(format!("  {k}: same as saved = {}", fresh[k] == saved[k]));
    }
    let new_url = fresh["downloadURL"].as_str().unwrap_or("").to_owned();
    log.line(format!("  fresh URL: {}", url_shape(&new_url)));
    log.line(format!("  fresh URL identical to saved = {}; {}", new_url == old_url, param_diff(&old_url, &new_url)));

    let u = new_url.replace("${f}", "file");
    probe(log, &bare, "fresh URL, ${f}=file, no cookies, no Origin", &u, false);
    probe(log, &bare, "fresh URL, ${f} left literal, no cookies", &new_url, false);
    probe(log, &s.client, "fresh URL, ${f}=file, cookies + Origin", &u, true);

    // One full download.
    match bare.get(&u).send() {
        Ok(r) => {
            let status = r.status();
            let hdrs = safe_headers(r.headers());
            let bytes = r.bytes().map(|b| b.to_vec()).unwrap_or_default();
            log.line(format!("full GET (no cookies): {status}, {} bytes, type={}, headers: {hdrs}", bytes.len(), sniff(&bytes)));
            log.line(format!("  len == asset.size: {}", Some(bytes.len() as u64) == fresh["size"].as_u64()));
            let ck = fresh["fileChecksum"].as_str().and_then(|c| b64_decode(c).ok()).unwrap_or_default();
            let s1 = sha1::Sha1::digest(&bytes).to_vec();
            let s2 = sha2::Sha256::digest(&bytes).to_vec();
            let m = if ck.len() == 21 && ck[1..] == s1[..] {
                format!("fileChecksum = {:#04x} || SHA-1(file)", ck[0])
            } else if ck[..] == s1[..] {
                "fileChecksum = SHA-1(file)".into()
            } else if ck.len() >= 20 && s2.starts_with(&ck[ck.len() - 20..]) {
                "fileChecksum tail = SHA-256 prefix".into()
            } else {
                format!("fileChecksum ({} bytes, first {:#04x}) matches neither SHA-1 nor SHA-256 of the body", ck.len(), ck.first().copied().unwrap_or(0))
            };
            log.line(format!("  {m}"));
            if status.is_success() && !bytes.is_empty() {
                let ext = match sniff(&bytes) {
                    "unknown" => "bin",
                    e => e,
                };
                let p = format!("{OUT}/{kind}.{ext}");
                let _ = fs::write(&p, &bytes);
                log.line(format!("  saved {p}"));
            }
        }
        Err(e) => log.line(format!("full GET error: {e}")),
    }
    probe(log, &bare, "saved URL again, after lookup", &old_url.replace("${f}", "file"), false);
}

/// Range GET of the first 32 bytes; logs status, safe headers and the sniffed type.
fn probe(log: &mut Log, c: &Client, label: &str, url: &str, origin: bool) {
    let mut rq = c.get(url).header("Range", "bytes=0-31");
    if origin {
        rq = rq.header("Origin", ORIGIN).header("Referer", ICLOUD);
    }
    match rq.send() {
        Ok(r) => {
            let status = r.status();
            let hdrs = safe_headers(r.headers());
            let b = r.bytes().map(|b| b.to_vec()).unwrap_or_default();
            let what = if status.is_success() { sniff(&b).to_owned() } else { format!("{} body bytes", b.len()) };
            log.line(format!("  [{label}] {status} -> {what}; {hdrs}"));
        }
        Err(e) => log.line(format!("  [{label}] send error: {e}")),
    }
}

fn safe_headers(h: &reqwest::header::HeaderMap) -> String {
    let show = ["content-type", "content-length", "content-range", "cache-control", "expires", "accept-ranges", "access-control-allow-origin", "content-encoding"];
    let mut v: Vec<String> = h
        .iter()
        .map(|(k, val)| {
            let k = k.as_str();
            if show.contains(&k) { format!("{k}={}", val.to_str().unwrap_or("?")) } else { k.to_owned() }
        })
        .collect();
    v.sort();
    v.join(" ")
}

fn split(u: &str) -> (&str, Vec<(&str, &str)>) {
    let (base, q) = u.split_once('?').unwrap_or((u, ""));
    (base, q.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).collect())
}

fn url_shape(u: &str) -> String {
    let (base, q) = split(u);
    let host = base.split('/').nth(2).unwrap_or("?");
    let segs: Vec<usize> = base.split('/').skip(3).map(str::len).collect();
    let params: Vec<String> = q.iter().map(|(k, v)| format!("{k}(len {})", v.len())).collect();
    format!("host={host} path segment lengths={segs:?} params={params:?}")
}

fn param_diff(a: &str, b: &str) -> String {
    let ((ba, qa), (bb, qb)) = (split(a), split(b));
    let changed: Vec<&str> = qb.iter().filter(|(k, v)| qa.iter().find(|(ka, _)| ka == k).map(|(_, va)| va) != Some(v)).map(|(k, _)| *k).collect();
    format!("path same = {}, params that differ: {changed:?}", ba == bb)
}
