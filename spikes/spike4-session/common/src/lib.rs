// Spike 4 shared code: a cookie set saved with DPAPI, Set-Cookie merging, the two
// read-only calls (setup validate, CloudKit changes/zone) and a redacting log.
//
// Privacy: cookie values, dsid and error bodies are never logged. Cookies are shown
// as name, length and an 8-hex SHA-256 prefix, so changed values can be spotted.

use std::{
    fs,
    path::{Path, PathBuf},
};

use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub const ICLOUD: &str = "https://www.icloud.com/";
pub const ORIGIN: &str = "https://www.icloud.com";
pub const VALIDATE: &str = "https://setup.icloud.com/setup/ws/1/validate";
pub const DB: &str = "/database/1/com.apple.notes/production/private";
pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
pub const IDENTIFIER: &str = "com.ziggy.icloud-notes-spike4";
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out");

/// Outside the repo, next to the WebView2 profile: %LOCALAPPDATA%\<identifier>.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA not set");
    let dir = Path::new(&base).join(IDENTIFIER);
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn store_path(name: &str) -> PathBuf {
    data_dir().join(format!("{name}.dpapi"))
}

pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn stamp() -> String {
    OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// Short fingerprint of a secret: enough to tell values apart, useless to an attacker.
pub fn fp(value: &str) -> String {
    Sha256::digest(value.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn days(at: i64) -> String {
    format!("{:+.1}d", (at - now()) as f64 / 86400.0)
}

// ---------- log ----------

pub struct Log {
    path: PathBuf,
    buf: String,
}

impl Log {
    /// Appends to out/report-<name>.txt, rewritten after every line so a crash keeps it.
    pub fn new(name: &str) -> Log {
        let _ = fs::create_dir_all(OUT);
        let path = Path::new(OUT).join(format!("report-{name}.txt"));
        let buf = fs::read_to_string(&path).unwrap_or_default();
        let mut log = Log { path, buf };
        log.line(format!("\n######## {name} run at {} ########", stamp()));
        log
    }

    pub fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        self.buf.push_str(s.as_ref());
        self.buf.push('\n');
        let _ = fs::write(&self.path, &self.buf);
    }
}

// ---------- cookies ----------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Unix seconds; None = session cookie.
    pub expires: Option<i64>,
    pub http_only: Option<bool>,
    pub secure: Option<bool>,
    pub same_site: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Jar {
    pub saved_at: i64,
    pub source: String,
    pub cookies: Vec<StoredCookie>,
}

/// What one Set-Cookie header did to the jar. Holds no values.
pub struct SetCookieInfo {
    pub name: String,
    pub value_len: usize,
    pub fp: String,
    pub changed: bool,
    pub removed: bool,
    pub expires: Option<i64>,
    pub domain: Option<String>,
}

impl Jar {
    /// Every stored cookie is domain=icloud.com, path=/, so all of them go to every *.icloud.com call.
    pub fn header(&self) -> String {
        self.cookies.iter().map(|c| format!("{}={}", c.name, c.value)).collect::<Vec<_>>().join("; ")
    }

    pub fn get(&self, name: &str) -> Option<&StoredCookie> {
        self.cookies.iter().find(|c| c.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.cookies.iter().map(|c| c.name.clone()).collect()
    }

    pub fn without(&self, name: &str) -> Jar {
        let mut j = self.clone();
        j.cookies.retain(|c| c.name != name);
        j
    }

    pub fn only(&self, names: &[String]) -> Jar {
        let mut j = self.clone();
        j.cookies.retain(|c| names.contains(&c.name));
        j
    }

    /// Same length, one character changed in the middle.
    pub fn corrupted(&self, name: &str) -> Jar {
        let mut j = self.clone();
        if let Some(c) = j.cookies.iter_mut().find(|c| c.name == name) {
            let mut chars: Vec<char> = c.value.chars().collect();
            if !chars.is_empty() {
                let i = chars.len() / 2;
                chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
            }
            c.value = chars.into_iter().collect();
        }
        j
    }

    /// Applies Set-Cookie headers like a browser would (by name; all cookies here share domain and path).
    pub fn merge(&mut self, set_cookies: &[String]) -> Vec<SetCookieInfo> {
        let mut out = vec![];
        for raw in set_cookies {
            let Ok(c) = cookie::Cookie::parse(raw.as_str()) else { continue };
            let expires = match (c.max_age(), c.expires_datetime()) {
                (Some(ma), _) => Some(now() + ma.whole_seconds()),
                (None, Some(dt)) => Some(dt.unix_timestamp()),
                _ => None,
            };
            let removed = expires.is_some_and(|e| e <= now()) || c.value().is_empty();
            let old = self.cookies.iter().position(|s| s.name == c.name());
            let changed = old.map(|i| self.cookies[i].value != c.value()).unwrap_or(true);
            out.push(SetCookieInfo {
                name: c.name().to_owned(),
                value_len: c.value().len(),
                fp: fp(c.value()),
                changed,
                removed,
                expires,
                domain: c.domain().map(str::to_owned),
            });
            if let Some(i) = old {
                self.cookies.remove(i);
            }
            if !removed {
                self.cookies.push(StoredCookie {
                    name: c.name().to_owned(),
                    value: c.value().to_owned(),
                    domain: c.domain().map(str::to_owned).or_else(|| Some("icloud.com".into())),
                    path: c.path().map(str::to_owned).or_else(|| Some("/".into())),
                    expires,
                    http_only: c.http_only(),
                    secure: c.secure(),
                    same_site: c.same_site().map(|s| s.to_string()),
                });
            }
        }
        out
    }

    pub fn describe(&self, log: &mut Log, title: &str) {
        log.line(format!("== {title}: {} cookies (source={}, saved {}s ago) ==", self.cookies.len(), self.source, now() - self.saved_at));
        let mut cs = self.cookies.clone();
        cs.sort_by(|a, b| a.name.cmp(&b.name));
        for c in cs {
            log.line(format!(
                "  {:<34} len={:<5} fp={} expires={:<8} httpOnly={:?} secure={:?} sameSite={:?} domain={:?}",
                c.name,
                c.value.len(),
                fp(&c.value),
                c.expires.map(days).unwrap_or_else(|| "session".into()),
                c.http_only,
                c.secure,
                c.same_site,
                c.domain,
            ));
        }
    }
}

pub fn log_set_cookies(log: &mut Log, infos: &[SetCookieInfo]) {
    if infos.is_empty() {
        return log.line("  set-cookie: none");
    }
    log.line(format!("  set-cookie ({}):", infos.len()));
    for i in infos {
        log.line(format!(
            "    {:<34} len={:<5} fp={} {} expires={} domain={:?}",
            i.name,
            i.value_len,
            i.fp,
            if i.removed { "REMOVED" } else if i.changed { "changed" } else { "same" },
            i.expires.map(days).unwrap_or_else(|| "session".into()),
            i.domain,
        ));
    }
}

// ---------- DPAPI store ----------

mod dpapi {
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::Cryptography::{
                CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
            },
        },
        core::w,
    };

    // Extra entropy so another app running as the same user can't just call CryptUnprotectData blindly.
    const ENTROPY: &[u8] = b"icloud-notes-spike4-session";

    fn blob(d: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: d.len() as u32, pbData: d.as_ptr() as *mut u8 }
    }

    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(Some(HLOCAL(out.pbData.cast()))) };
        v
    }

    pub fn protect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, w!("spike4 session"), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
            Ok(take(out))
        }
    }
}

pub fn save(path: &Path, jar: &Jar) -> Result<usize, String> {
    let plain = serde_json::to_vec(jar).map_err(|e| e.to_string())?;
    let enc = dpapi::protect(&plain).map_err(|e| format!("CryptProtectData: {e}"))?;
    fs::write(path, &enc).map_err(|e| e.to_string())?;
    Ok(enc.len())
}

pub fn load(path: &Path) -> Result<Jar, String> {
    let enc = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let plain = dpapi::unprotect(&enc).map_err(|e| format!("CryptUnprotectData: {e}"))?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

// ---------- the two read-only calls ----------

pub fn client(ua: bool) -> Client {
    let b = Client::builder();
    let b = if ua { b.user_agent(UA) } else { b };
    b.build().unwrap()
}

pub struct Outcome {
    pub status: u16,
    pub ok: bool,
    /// Redacted one-line summary: flags, keys, error fields. Safe to log.
    pub summary: String,
    pub set_cookies: Vec<String>,
    pub ckdb: Option<String>,
}

fn set_cookies(resp: &Response) -> Vec<String> {
    resp.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok().map(str::to_owned)).collect()
}

/// Only error/serverErrorCode/reason/success: error bodies can carry trust tokens.
fn err_fields(v: &Value) -> String {
    let parts: Vec<String> = ["success", "error", "serverErrorCode", "reason"]
        .iter()
        .filter_map(|k| v.get(*k).map(|x| format!("{k}={x}")))
        .collect();
    if parts.is_empty() { "(no error fields)".into() } else { parts.join(" ") }
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default()
}

fn body_shape(ctype: &str, text: &str, v: &Value) -> String {
    if v.is_null() {
        format!("non-JSON body, {} bytes, content-type={ctype:?}", text.len())
    } else {
        format!("{} bytes, keys={:?}, {}", text.len(), keys(v), err_fields(v))
    }
}

pub fn validate(client: &Client, jar: &Jar, origin: bool) -> Result<Outcome, String> {
    let mut req = client
        .post(VALIDATE)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "*/*")
        .body("");
    if !jar.cookies.is_empty() {
        req = req.header("Cookie", jar.header());
    }
    if origin {
        req = req.header("Origin", ORIGIN).header("Referer", ICLOUD);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let sc = set_cookies(&resp);
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let text = resp.text().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let ckdb = v["webservices"]["ckdatabasews"]["url"].as_str().map(|u| u.trim_end_matches('/').to_owned());
    let ok = status == 200 && ckdb.is_some();
    let summary = if ok {
        format!(
            "dsid present={} webservices={} isExtendedLogin={} hsaTrustedBrowser={} hsaChallengeRequired={}",
            !v["dsInfo"]["dsid"].is_null(),
            v["webservices"].as_object().map(|o| o.len()).unwrap_or(0),
            v["isExtendedLogin"],
            v["hsaTrustedBrowser"],
            v["hsaChallengeRequired"],
        )
    } else {
        body_shape(&ctype, &text, &v)
    };
    Ok(Outcome { status, ok, summary, set_cookies: sc, ckdb })
}

/// Read-only CloudKit call (spike 2's bare-params changes/zone, 1 record). Logs no record data.
pub fn changes_zone(client: &Client, jar: &Jar, ckdb: &str) -> Result<Outcome, String> {
    let body = json!({ "zones": [{ "zoneID": { "zoneName": "Notes", "zoneType": "REGULAR_CUSTOM_ZONE" }, "resultsLimit": 1 }] });
    let mut req = client
        .post(format!("{ckdb}{DB}/changes/zone"))
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "application/json")
        .header("Origin", ORIGIN)
        .header("Referer", ICLOUD)
        .body(body.to_string());
    if !jar.cookies.is_empty() {
        req = req.header("Cookie", jar.header());
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let sc = set_cookies(&resp);
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let text = resp.text().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let zone = &v["zones"][0];
    let ok = status == 200 && zone["records"].is_array() && zone.get("serverErrorCode").is_none();
    let summary = if ok {
        format!("records={} moreComing={}", zone["records"].as_array().map(Vec::len).unwrap_or(0), zone["moreComing"])
    } else if status == 200 {
        format!("zone error: {}", err_fields(zone))
    } else {
        body_shape(&ctype, &text, &v)
    };
    Ok(Outcome { status, ok, summary, set_cookies: sc, ckdb: None })
}
