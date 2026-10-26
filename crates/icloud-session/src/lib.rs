//! The iCloud web session (WP-D). Port of `spikes/spike4-session/common`.
//!
//! Our DPAPI-encrypted cookie jar is the source of truth. Every response's `Set-Cookie` is merged back
//! with [`Session::absorb`], and every request gets Origin/Referer/UA/Cookie from [`Session::apply_headers`].
//! Cookie values are never logged.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use notes_model::{ApiError, AuthError};
use serde::{Deserialize, Serialize};

/// `X-APPLE-WEBAUTH-TOKEN`: present when signed in (spike 4).
pub const SESSION_COOKIE: &str = "X-APPLE-WEBAUTH-TOKEN";
/// Required on every request to Apple (a 421 without it).
pub const ORIGIN: &str = "https://www.icloud.com";
/// Required on every request to Apple.
pub const REFERER: &str = "https://www.icloud.com/";
/// Default base for `/setup/ws/1/validate`.
pub const SETUP_BASE: &str = "https://setup.icloud.com";
/// Browser UA sent to Apple (not required by the server, but matches what the web app sends).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";

const APP_DIR: &str = "com.ziggy.starnotes";
const STORE_FILE: &str = "session.dpapi";

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Session-layer failures that are not Apple's verdict (local storage, DPAPI, JSON).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("dpapi: {0}")]
    Dpapi(String),
    #[error("io: {0}")]
    Io(String),
    #[error("json: {0}")]
    Json(String),
}

/// One cookie. `expires` is Unix seconds; `None` is a session cookie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
}

impl StoredCookie {
    fn is_expired(&self) -> bool {
        self.expires.is_some_and(|e| e <= now())
    }
}

/// The iCloud cookie jar. Cookies are keyed by name (all are for `.icloud.com`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJar {
    cookies: Vec<StoredCookie>,
}

/// The jar as shared between the session and the host.
pub type SharedJar = Arc<Mutex<CookieJar>>;

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a cookie by name (used for cookies captured from the login WebView).
    pub fn insert(&mut self, cookie: StoredCookie) {
        match self.cookies.iter_mut().find(|c| c.name == cookie.name) {
            Some(slot) => *slot = cookie,
            None => self.cookies.push(cookie),
        }
    }

    /// Merge raw `Set-Cookie` header values by name. Expired or empty-valued cookies are removed.
    /// Returns the number of cookies added or changed.
    pub fn merge_set_cookies(&mut self, set_cookies: &[String]) -> usize {
        let mut changed = 0;
        for raw in set_cookies {
            let Ok(c) = cookie::Cookie::parse(raw.as_str()) else { continue };
            let expires = match (c.max_age(), c.expires_datetime()) {
                (Some(ma), _) => Some(now().saturating_add(ma.whole_seconds())),
                (None, Some(dt)) => Some(dt.unix_timestamp()),
                _ => None,
            };
            let remove = expires.is_some_and(|e| e <= now()) || c.value().is_empty();
            let old = self.cookies.iter().position(|s| s.name == c.name());
            if remove {
                if let Some(i) = old {
                    self.cookies.remove(i);
                    changed += 1;
                }
                continue;
            }
            let new = StoredCookie {
                name: c.name().to_owned(),
                value: c.value().to_owned(),
                domain: c.domain().map(|d| d.trim_start_matches('.').to_owned()).unwrap_or_else(|| "icloud.com".into()),
                path: c.path().map(str::to_owned).unwrap_or_else(|| "/".into()),
                expires,
                secure: c.secure().unwrap_or(false),
                http_only: c.http_only().unwrap_or(false),
            };
            match old {
                Some(i) if self.cookies[i] == new => {}
                Some(i) => {
                    self.cookies[i] = new;
                    changed += 1;
                }
                None => {
                    self.cookies.push(new);
                    changed += 1;
                }
            }
        }
        changed
    }

    /// The `Cookie:` request header value (`a=1; b=2`), skipping expired cookies.
    pub fn header(&self) -> String {
        self.cookies
            .iter()
            .filter(|c| !c.is_expired())
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn get(&self, name: &str) -> Option<&StoredCookie> {
        self.cookies.iter().find(|c| c.name == name)
    }

    /// Cookie names only (safe to log).
    pub fn names(&self) -> Vec<String> {
        self.cookies.iter().map(|c| c.name.clone()).collect()
    }

    /// True when [`SESSION_COOKIE`] is present and not expired.
    pub fn has_session(&self) -> bool {
        self.get(SESSION_COOKIE).is_some_and(|c| !c.is_expired())
    }

    pub fn to_json(&self) -> Result<Vec<u8>, SessionError> {
        serde_json::to_vec(self).map_err(|e| SessionError::Json(e.to_string()))
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, SessionError> {
        serde_json::from_slice(bytes).map_err(|e| SessionError::Json(e.to_string()))
    }
}

mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::w;

    // Extra entropy so another app running as the same user can't just call CryptUnprotectData blindly.
    const ENTROPY: &[u8] = b"starnotes-session";

    fn blob(d: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: d.len() as u32, pbData: d.as_ptr() as *mut u8 }
    }

    /// Copy out and free a DPAPI-allocated blob.
    ///
    /// # Safety
    /// `out` must have been filled by a successful DPAPI call.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(Some(HLOCAL(out.pbData.cast()))) };
        v
    }

    pub fn protect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, w!("starnotes session"), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
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

/// `CryptProtectData`, CurrentUser scope, `CRYPTPROTECT_UI_FORBIDDEN`, with the app's entropy.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, SessionError> {
    dpapi::protect(data).map_err(|e| SessionError::Dpapi(format!("CryptProtectData: {e}")))
}

/// Inverse of [`protect`].
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, SessionError> {
    dpapi::unprotect(data).map_err(|e| SessionError::Dpapi(format!("CryptUnprotectData: {e}")))
}

/// The jar on disk: `%LOCALAPPDATA%\com.ziggy.starnotes\session.dpapi`, DPAPI-encrypted JSON.
#[derive(Debug, Clone)]
pub struct DpapiStore {
    path: PathBuf,
}

impl DpapiStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DpapiStore { path: path.into() }
    }

    /// `%LOCALAPPDATA%\com.ziggy.starnotes\session.dpapi`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join(APP_DIR).join(STORE_FILE)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Ok(None)` if the file doesn't exist.
    pub fn load(&self) -> Result<Option<CookieJar>, SessionError> {
        let enc = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SessionError::Io(e.to_string())),
        };
        let plain = unprotect(&enc)?;
        CookieJar::from_json(&plain).map(Some)
    }

    /// Atomic write: encrypt to `<path>.tmp`, then rename over `<path>`.
    pub fn save(&self, jar: &CookieJar) -> Result<(), SessionError> {
        let enc = protect(&jar.to_json()?)?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| SessionError::Io(e.to_string()))?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, &enc).map_err(|e| SessionError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            SessionError::Io(e.to_string())
        })
    }

    /// Remove the file (sign-out). Missing is not an error.
    pub fn delete(&self) -> Result<(), SessionError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SessionError::Io(e.to_string())),
        }
    }
}

/// What a successful `/validate` tells us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoints {
    /// `webservices.ckdatabasews.url`, e.g. `https://p123-ckdatabasews.icloud.com:443`.
    pub ckdatabasews: String,
    /// `dsInfo.dsid`.
    pub dsid: String,
}

/// The live session: an HTTP client, the shared jar, and (optionally) where to persist it.
pub struct Session {
    client: reqwest::Client,
    jar: SharedJar,
    store: Option<DpapiStore>,
    setup_base: String,
}

impl Session {
    /// A session over `jar`. When `store` is set, the jar is saved after every change made by [`Session::absorb`].
    pub fn new(jar: CookieJar, store: Option<DpapiStore>) -> Self {
        Session {
            client: reqwest::Client::builder().build().unwrap_or_default(),
            jar: Arc::new(Mutex::new(jar)),
            store,
            setup_base: SETUP_BASE.to_owned(),
        }
    }

    /// Override [`SETUP_BASE`] (tests point this at httpmock).
    pub fn with_setup_base(mut self, base: impl Into<String>) -> Self {
        self.setup_base = base.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn jar(&self) -> SharedJar {
        self.jar.clone()
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn persist(&self, jar: &CookieJar) {
        if let Some(store) = &self.store
            && let Err(e) = store.save(jar)
        {
            tracing::warn!(error = %e, "could not persist session jar");
        }
    }

    /// Replace the whole jar (after a re-login) and persist it.
    pub fn replace_jar(&self, jar: CookieJar) -> Result<(), SessionError> {
        let mut guard = lock(&self.jar);
        *guard = jar;
        if let Some(store) = &self.store {
            store.save(&guard)?;
        }
        Ok(())
    }

    /// `POST {setup}/setup/ws/1/validate`. Auth failures are classified with [`classify`];
    /// transport failures are `ApiError::Http`.
    pub async fn validate(&self) -> Result<Endpoints, ApiError> {
        if lock(&self.jar).header().is_empty() {
            return Err(AuthError::NotSignedIn.into());
        }
        let url = format!("{}/setup/ws/1/validate", self.setup_base);
        let rb = self
            .client
            .post(url)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .header("Accept", "*/*")
            .body("");
        let resp = self.apply_headers(rb).send().await.map_err(|e| ApiError::Http(e.without_url().to_string()))?;
        self.absorb(&resp);
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ApiError::Http(e.without_url().to_string()))?;
        if let Some(auth) = classify(status, &body) {
            return Err(auth.into());
        }
        if status != 200 {
            return Err(ApiError::Http(format!("validate: unexpected HTTP {status}")));
        }
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| ApiError::Http("validate: unparseable body".into()))?;
        let ck = v["webservices"]["ckdatabasews"]["url"].as_str().map(|u| u.trim_end_matches('/').to_owned());
        let dsid = match &v["dsInfo"]["dsid"] {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        match (ck, dsid) {
            (Some(ckdatabasews), Some(dsid)) => Ok(Endpoints { ckdatabasews, dsid }),
            _ => Err(ApiError::Http("validate: response lacks ckdatabasews url or dsid".into())),
        }
    }

    /// Add `Origin`, `Referer`, `User-Agent` and `Cookie` to a request to Apple.
    pub fn apply_headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.header("Origin", ORIGIN).header("Referer", REFERER).header("User-Agent", USER_AGENT);
        let cookie = lock(&self.jar).header();
        if cookie.is_empty() { rb } else { rb.header("Cookie", cookie) }
    }

    /// Merge the response's `Set-Cookie` headers into the jar (and persist if changed).
    pub fn absorb(&self, resp: &reqwest::Response) {
        let raw: Vec<String> =
            resp.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok().map(str::to_owned)).collect();
        if raw.is_empty() {
            return;
        }
        let mut guard = lock(&self.jar);
        let changed = guard.merge_set_cookies(&raw);
        if changed > 0 {
            tracing::debug!(changed, "merged Set-Cookie into session jar");
            self.persist(&guard);
        }
    }
}

/// Map an HTTP status + body from `/validate` or CloudKit to an [`AuthError`], using spike 4's
/// failure table. Returns `None` when it's not an auth problem. A 421 "Invalid or missing Origin header"
/// is a bug in our request, **not** a re-login, so it also returns `None`.
pub fn classify(status: u16, body: &str) -> Option<AuthError> {
    if !matches!(status, 401 | 403 | 421) {
        return None;
    }
    // Only pull `serverErrorCode`, `reason` and `error` out of the body: validate errors also carry trust tokens.
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let text = |k: &str| match &v[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let code = text("serverErrorCode");
    let reason = [text("reason"), text("error")].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let lower = reason.to_ascii_lowercase();

    if status == 421 && lower.contains("origin") {
        return None;
    }
    if lower.contains("pcs cookie") {
        return Some(AuthError::PcsRejected { reason });
    }
    if code == "ACCESS_DENIED" || status == 403 {
        return if code == "ACCESS_DENIED" { Some(AuthError::AccessDenied { reason }) } else { None };
    }
    Some(AuthError::SessionInvalid { status, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn cookie(name: &str, value: &str, expires: Option<i64>) -> StoredCookie {
        StoredCookie {
            name: name.into(),
            value: value.into(),
            domain: "icloud.com".into(),
            path: "/".into(),
            expires,
            secure: true,
            http_only: true,
        }
    }

    fn signed_in_jar() -> CookieJar {
        let mut j = CookieJar::new();
        j.insert(cookie(SESSION_COOKIE, "tok1", Some(now() + 86400)));
        j.insert(cookie("X-APPLE-WEBAUTH-USER", "user1", Some(now() + 86400)));
        j
    }

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("icloud-session-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("session.dpapi")
    }

    #[test]
    fn merge_adds_replaces_and_removes() {
        let mut jar = signed_in_jar();
        let n = jar.merge_set_cookies(&[
            "X-APPLE-WEBAUTH-TOKEN=tok2; Max-Age=2592000; Path=/; Domain=.icloud.com; Secure; HttpOnly".into(),
            "X-APPLE-WEBAUTH-VALIDATE=v1; Path=/; Domain=.icloud.com".into(),
            "garbage".into(),
        ]);
        assert_eq!(n, 2);
        assert_eq!(jar.get(SESSION_COOKIE).unwrap().value, "tok2");
        assert!(jar.get(SESSION_COOKIE).unwrap().http_only);
        assert_eq!(jar.get("X-APPLE-WEBAUTH-VALIDATE").unwrap().expires, None);
        assert_eq!(jar.names().len(), 3);

        // Same value and attributes again: not a change.
        let n = jar.merge_set_cookies(&["X-APPLE-WEBAUTH-VALIDATE=v1; Path=/; Domain=.icloud.com".into()]);
        assert_eq!(n, 0);

        // Empty value and past Expires both delete.
        let n = jar.merge_set_cookies(&[
            "X-APPLE-WEBAUTH-VALIDATE=; Path=/".into(),
            "X-APPLE-WEBAUTH-USER=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/".into(),
            "never-there=; Path=/".into(),
        ]);
        assert_eq!(n, 2);
        assert_eq!(jar.names(), vec![SESSION_COOKIE.to_string()]);
    }

    #[test]
    fn header_skips_expired_and_has_session_checks_expiry() {
        let mut jar = CookieJar::new();
        jar.insert(cookie("a", "1", None));
        jar.insert(cookie("b", "2", Some(now() - 10)));
        jar.insert(cookie("c", "3", Some(now() + 100)));
        assert_eq!(jar.header(), "a=1; c=3");
        assert!(!jar.has_session());
        jar.insert(cookie(SESSION_COOKIE, "t", Some(now() - 1)));
        assert!(!jar.has_session());
        jar.insert(cookie(SESSION_COOKIE, "t", Some(now() + 100)));
        assert!(jar.has_session());
    }

    #[test]
    fn json_round_trip() {
        let jar = signed_in_jar();
        let back = CookieJar::from_json(&jar.to_json().unwrap()).unwrap();
        assert_eq!(jar, back);
        assert!(matches!(CookieJar::from_json(b"nope"), Err(SessionError::Json(_))));
    }

    #[test]
    fn dpapi_round_trip_and_store() {
        let enc = protect(b"hello").unwrap();
        assert_ne!(enc, b"hello");
        assert_eq!(unprotect(&enc).unwrap(), b"hello");
        assert!(unprotect(b"not a blob").is_err());

        let path = temp_path("store");
        let store = DpapiStore::new(&path);
        assert!(store.load().unwrap().is_none());
        store.delete().unwrap();
        let jar = signed_in_jar();
        store.save(&jar).unwrap();
        store.save(&jar).unwrap(); // overwrite via rename
        assert!(!path.with_file_name("session.dpapi.tmp").exists());
        let raw = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("tok1"));
        assert_eq!(store.load().unwrap().unwrap(), jar);
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_path_shape() {
        let p = DpapiStore::default_path();
        assert!(p.ends_with(r"com.ziggy.starnotes\session.dpapi") || p.ends_with("com.ziggy.starnotes/session.dpapi"));
    }

    // Spike 4's failure table, row by row.
    #[test]
    fn classify_failure_table() {
        let invalid = |status, reason: &str| Some(AuthError::SessionInvalid { status, reason: reason.into() });
        // validate: no cookies / TOKEN missing
        assert_eq!(
            classify(421, r#"{"success":false,"error":"Missing X-APPLE-WEBAUTH-TOKEN cookie","trustTokens":["secret"]}"#),
            invalid(421, "Missing X-APPLE-WEBAUTH-TOKEN cookie")
        );
        // validate: TOKEN invalid
        assert_eq!(classify(421, r#"{"success":false,"error":1}"#), invalid(421, "1"));
        // validate: USER missing / invalid
        assert_eq!(
            classify(421, r#"{"error":"Missing/Invalid X-APPLE-WEBAUTH-USER cookie"}"#),
            invalid(421, "Missing/Invalid X-APPLE-WEBAUTH-USER cookie")
        );
        // validate: no Origin is our bug, not a re-login
        assert_eq!(classify(421, r#"{"success":false,"error":"Invalid or missing Origin header"}"#), None);
        // CloudKit: no cookies
        assert_eq!(
            classify(401, r#"{"serverErrorCode":"AUTHENTICATION_FAILED","reason":"no auth method found","uuid":"x"}"#),
            invalid(401, "no auth method found")
        );
        // CloudKit: TOKEN invalid / USER invalid
        assert_eq!(classify(421, r#"{"error":2,"reason":"Invalid global session"}"#), invalid(421, "Invalid global session"));
        assert_eq!(classify(421, r#"{"error":1,"reason":"Invalid global session"}"#), invalid(421, "Invalid global session"));
        // CloudKit: PCS cookie bad
        assert_eq!(
            classify(401, r#"{"serverErrorCode":"INTERNAL_ERROR","reason":"could not decrypt pcs cookie"}"#),
            Some(AuthError::PcsRejected { reason: "could not decrypt pcs cookie".into() })
        );
        // CloudKit: no usable PCS cookie
        assert_eq!(
            classify(403, r#"{"serverErrorCode":"ACCESS_DENIED","reason":"private db access disabled for this account"}"#),
            Some(AuthError::AccessDenied { reason: "private db access disabled for this account".into() })
        );
        // Not auth problems.
        assert_eq!(classify(200, "{}"), None);
        assert_eq!(classify(500, "oops"), None);
        assert_eq!(classify(403, r#"{"serverErrorCode":"OTHER"}"#), None);
        // Non-JSON auth status still means re-login.
        assert_eq!(classify(401, "<html>"), invalid(401, ""));
    }

    fn session(server: &MockServer, jar: CookieJar, store: Option<DpapiStore>) -> Session {
        Session::new(jar, store).with_setup_base(server.base_url())
    }

    #[tokio::test]
    async fn validate_success_sends_headers_and_absorbs_cookies() {
        let server = MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/setup/ws/1/validate")
                    .header("Origin", ORIGIN)
                    .header("Referer", REFERER)
                    .header("User-Agent", USER_AGENT)
                    .header_includes("Cookie", "X-APPLE-WEBAUTH-TOKEN=tok1");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("set-cookie", "X-APPLE-WEBAUTH-TOKEN=tok2; Max-Age=2592000; Path=/; Domain=.icloud.com; HttpOnly")
                    .body(r#"{"dsInfo":{"dsid":"12345"},"webservices":{"ckdatabasews":{"url":"https://p1-ckdatabasews.icloud.com:443/"}}}"#);
            })
            .await;
        let path = temp_path("validate");
        let store = DpapiStore::new(&path);
        let s = session(&server, signed_in_jar(), Some(store.clone()));
        let ep = s.validate().await.unwrap();
        m.assert_async().await;
        assert_eq!(ep, Endpoints { ckdatabasews: "https://p1-ckdatabasews.icloud.com:443".into(), dsid: "12345".into() });
        assert_eq!(lock(&s.jar()).get(SESSION_COOKIE).unwrap().value, "tok2");
        // Persisted.
        assert_eq!(store.load().unwrap().unwrap().get(SESSION_COOKIE).unwrap().value, "tok2");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn validate_empty_jar_is_not_signed_in() {
        let server = MockServer::start_async().await;
        let s = session(&server, CookieJar::new(), None);
        let err = s.validate().await.unwrap_err();
        assert!(matches!(err, ApiError::Auth(AuthError::NotSignedIn)));
    }

    #[tokio::test]
    async fn validate_classifies_failures() {
        let server = MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(POST).path("/setup/ws/1/validate");
                then.status(421).body(r#"{"success":false,"error":"Missing X-APPLE-WEBAUTH-TOKEN cookie"}"#);
            })
            .await;
        let s = session(&server, signed_in_jar(), None);
        match s.validate().await.unwrap_err() {
            ApiError::Auth(AuthError::SessionInvalid { status: 421, .. }) => {}
            e => panic!("unexpected {e:?}"),
        }
        m.delete_async().await;

        // Origin complaint is a bug in our request: surfaced as Http, not Auth.
        let m = server
            .mock_async(|when, then| {
                when.method(POST).path("/setup/ws/1/validate");
                then.status(421).body(r#"{"success":false,"error":"Invalid or missing Origin header"}"#);
            })
            .await;
        assert!(matches!(s.validate().await.unwrap_err(), ApiError::Http(_)));
        m.delete_async().await;

        // 200 without the endpoints.
        server
            .mock_async(|when, then| {
                when.method(POST).path("/setup/ws/1/validate");
                then.status(200).body(r#"{"dsInfo":{}}"#);
            })
            .await;
        assert!(matches!(s.validate().await.unwrap_err(), ApiError::Http(_)));
    }

    #[tokio::test]
    async fn validate_transport_failure_is_http() {
        let s = Session::new(signed_in_jar(), None).with_setup_base("http://127.0.0.1:1");
        assert!(matches!(s.validate().await.unwrap_err(), ApiError::Http(_)));
    }

    #[tokio::test]
    async fn absorb_and_apply_headers_for_other_requests() {
        let server = MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(GET).path("/x").header("Origin", ORIGIN).header_includes("Cookie", "X-APPLE-WEBAUTH-USER=user1");
                then.status(200).header("set-cookie", "extra=1; Path=/");
            })
            .await;
        let s = session(&server, signed_in_jar(), None);
        let resp = s.apply_headers(s.client().get(server.url("/x"))).send().await.unwrap();
        s.absorb(&resp);
        m.assert_async().await;
        assert_eq!(lock(&s.jar()).get("extra").unwrap().value, "1");
    }

    #[test]
    fn replace_jar_persists() {
        let path = temp_path("replace");
        let store = DpapiStore::new(&path);
        let s = Session::new(CookieJar::new(), Some(store.clone()));
        let j = signed_in_jar();
        s.replace_jar(j.clone()).unwrap();
        assert!(lock(&s.jar()).has_session());
        assert_eq!(store.load().unwrap().unwrap(), j);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
