//! The iCloud web session (WP-D). Port of `spikes/spike4-session/common`.
//!
//! Our DPAPI-encrypted cookie jar is the source of truth. Every response's `Set-Cookie` is merged back
//! with [`Session::absorb`], and every request gets Origin/Referer/UA/Cookie from [`Session::apply_headers`].
//! Cookie values are never logged.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables, dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
        todo!("WP-D")
    }

    /// Merge raw `Set-Cookie` header values by name. Expired or empty-valued cookies are removed.
    /// Returns the number of cookies added or changed.
    pub fn merge_set_cookies(&mut self, set_cookies: &[String]) -> usize {
        todo!("WP-D")
    }

    /// The `Cookie:` request header value (`a=1; b=2`), skipping expired cookies.
    pub fn header(&self) -> String {
        todo!("WP-D")
    }

    pub fn get(&self, name: &str) -> Option<&StoredCookie> {
        todo!("WP-D")
    }

    /// Cookie names only (safe to log).
    pub fn names(&self) -> Vec<String> {
        todo!("WP-D")
    }

    /// True when [`SESSION_COOKIE`] is present and not expired.
    pub fn has_session(&self) -> bool {
        todo!("WP-D")
    }

    pub fn to_json(&self) -> Result<Vec<u8>, SessionError> {
        todo!("WP-D")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, SessionError> {
        todo!("WP-D")
    }
}

/// `CryptProtectData`, CurrentUser scope, `CRYPTPROTECT_UI_FORBIDDEN`, with the app's entropy.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, SessionError> {
    todo!("WP-D")
}

/// Inverse of [`protect`].
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, SessionError> {
    todo!("WP-D")
}

/// The jar on disk: `%LOCALAPPDATA%\com.ziggy.icloud-notes\session.dpapi`, DPAPI-encrypted JSON.
#[derive(Debug, Clone)]
pub struct DpapiStore {
    path: PathBuf,
}

impl DpapiStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DpapiStore { path: path.into() }
    }

    /// `%LOCALAPPDATA%\com.ziggy.icloud-notes\session.dpapi`.
    pub fn default_path() -> PathBuf {
        todo!("WP-D")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Ok(None)` if the file doesn't exist.
    pub fn load(&self) -> Result<Option<CookieJar>, SessionError> {
        todo!("WP-D")
    }

    /// Atomic write: encrypt to `<path>.tmp`, then rename over `<path>`.
    pub fn save(&self, jar: &CookieJar) -> Result<(), SessionError> {
        todo!("WP-D")
    }

    /// Remove the file (sign-out). Missing is not an error.
    pub fn delete(&self) -> Result<(), SessionError> {
        todo!("WP-D")
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
        todo!("WP-D")
    }

    /// Override [`SETUP_BASE`] (tests point this at httpmock).
    pub fn with_setup_base(self, base: impl Into<String>) -> Self {
        todo!("WP-D")
    }

    pub fn jar(&self) -> SharedJar {
        self.jar.clone()
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Replace the whole jar (after a re-login) and persist it.
    pub fn replace_jar(&self, jar: CookieJar) -> Result<(), SessionError> {
        todo!("WP-D")
    }

    /// `POST {setup}/setup/ws/1/validate`. Auth failures are classified with [`classify`];
    /// transport failures are `ApiError::Http`.
    pub async fn validate(&self) -> Result<Endpoints, ApiError> {
        todo!("WP-D")
    }

    /// Add `Origin`, `Referer`, `User-Agent` and `Cookie` to a request to Apple.
    pub fn apply_headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        todo!("WP-D")
    }

    /// Merge the response's `Set-Cookie` headers into the jar (and persist if changed).
    pub fn absorb(&self, resp: &reqwest::Response) {
        todo!("WP-D")
    }
}

/// Map an HTTP status + body from `/validate` or CloudKit to an [`AuthError`], using spike 4's
/// failure table. Returns `None` when it's not an auth problem. A 421 "Invalid or missing Origin header"
/// is a bug in our request, **not** a re-login, so it also returns `None`.
pub fn classify(status: u16, body: &str) -> Option<AuthError> {
    todo!("WP-D")
}
