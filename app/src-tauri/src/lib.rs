//! Tauri host (WP-H): windows, login, the §3.4 commands and the `notes-asset://` protocol.
//!
//! Commands delegate to the backend held in Tauri state: `notes_service::Service`, or with the `mock`
//! cargo feature `mock::MockService` (same method names; for UI development only). Extra Rust-side parameters (`State`, `AppHandle`,
//! `Window`) may be added freely: the JS contract is only the argument names and the JSON results.
//!
//! Privacy: nothing here logs cookie values or note content, only names and counts.

#[cfg(feature = "mock")]
mod mock;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use icloud_session::{CookieJar, StoredCookie};
use notes_model::{AssetVariant, CmdError, FolderView, NoteHtml, NoteSummary, RefreshResult, Status};
use tauri::http::{Request, Response, StatusCode, header};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

/// The backend behind the commands.
#[cfg(feature = "mock")]
type Backend = mock::MockService;
#[cfg(not(feature = "mock"))]
type Backend = notes_service::Service;
type Shared<'a> = State<'a, Arc<Backend>>;

const LOGIN_LABEL: &str = "login";
const MAIN_LABEL: &str = "main";
const ICLOUD_HOME: &str = "https://www.icloud.com";
/// Cookies that apply to this URL are the ones `/validate` needs (spike 1).
const VALIDATE_URL: &str = "https://setup.icloud.com/setup/ws/1/validate";
const SESSION_COOKIE: &str = "X-APPLE-WEBAUTH-TOKEN";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL: Duration = Duration::from_secs(2);
/// Let the page finish its post-login requests (token rotation) before capturing (spike 4).
const SETTLE: Duration = Duration::from_secs(8);

/// `status` → `{signedIn, syncing, lastSync?, error?}`.
#[tauri::command]
async fn status(svc: Shared<'_>) -> Result<Status, CmdError> {
    Ok(svc.status())
}

/// `sign_in`: open the login window; resolves once `X-APPLE-WEBAUTH-TOKEN` is captured into the jar.
#[tauri::command]
async fn sign_in(app: AppHandle, svc: Shared<'_>) -> Result<(), CmdError> {
    if let Some(w) = app.get_webview_window(LOGIN_LABEL) {
        let _ = w.set_focus();
        return Err(CmdError::internal("login window already open"));
    }
    // No capabilities are granted to this label (see capabilities/default.json), so the page gets no IPC.
    let win = WebviewWindowBuilder::new(&app, LOGIN_LABEL, WebviewUrl::External(ICLOUD_HOME.parse().expect("url")))
        .title("Sign in to iCloud")
        .inner_size(1000.0, 800.0)
        .build()
        .map_err(|e| CmdError::internal(format!("login window: {e}")))?;

    // Cookie calls block on WebView2, so poll off the async runtime's threads.
    let captured = tauri::async_runtime::spawn_blocking(move || {
        let start = Instant::now();
        let grab = || win.cookies_for_url(VALIDATE_URL.parse().expect("url")).ok();
        let has_token = |c: &[tauri::webview::Cookie<'static>]| c.iter().any(|c| c.name() == SESSION_COOKIE);
        loop {
            std::thread::sleep(POLL);
            if win.app_handle().get_webview_window(LOGIN_LABEL).is_none() {
                return Err(CmdError::internal("login window closed"));
            }
            if start.elapsed() > SIGN_IN_TIMEOUT {
                let _ = win.close();
                return Err(CmdError::internal("sign-in timed out"));
            }
            if grab().is_some_and(|c| has_token(&c)) {
                break;
            }
        }
        std::thread::sleep(SETTLE);
        let cookies = grab().filter(|c| has_token(c)).ok_or_else(|| CmdError::internal("cookies vanished"))?;
        let _ = win.close();
        Ok(cookies)
    })
    .await
    .map_err(|e| CmdError::internal(format!("login task: {e}")))??;

    let mut jar = CookieJar::new();
    for c in &captured {
        jar.insert(to_stored(c));
    }
    tracing::info!(cookies = captured.len(), "sign-in captured");
    svc.signed_in(jar).await?;
    let _ = app.emit("sign-in-done", ());
    Ok(())
}

fn to_stored(c: &tauri::webview::Cookie<'static>) -> StoredCookie {
    StoredCookie {
        name: c.name().to_owned(),
        value: c.value().to_owned(),
        domain: c.domain().unwrap_or("icloud.com").to_owned(),
        path: c.path().unwrap_or("/").to_owned(),
        expires: c.expires_datetime().map(|d| d.unix_timestamp()),
        secure: c.secure().unwrap_or(false),
        http_only: c.http_only().unwrap_or(false),
    }
}

/// `sign_out`: delete the jar and clear the WebView2 profile's browsing data.
#[tauri::command]
async fn sign_out(app: AppHandle, svc: Shared<'_>) -> Result<(), CmdError> {
    svc.sign_out().await?;
    // All windows share the default profile, so any webview can clear it.
    let w = app
        .get_webview_window(LOGIN_LABEL)
        .or_else(|| app.get_webview_window(MAIN_LABEL))
        .ok_or_else(|| CmdError::internal("no webview to clear"))?;
    w.clear_all_browsing_data().map_err(|e| CmdError::internal(format!("clear browsing data: {e}")))?;
    Ok(())
}

/// `refresh` → `{changed}`.
#[tauri::command]
async fn refresh(svc: Shared<'_>) -> Result<RefreshResult, CmdError> {
    svc.refresh().await
}

/// `folders` → `FolderView[]`.
#[tauri::command]
async fn folders(svc: Shared<'_>) -> Result<Vec<FolderView>, CmdError> {
    svc.folders()
}

/// `notes {folderId}` → `NoteSummary[]`, newest first. `folderId` may be `"quick-notes"`.
#[tauri::command]
async fn notes(svc: Shared<'_>, folder_id: String) -> Result<Vec<NoteSummary>, CmdError> {
    svc.notes(&folder_id)
}

/// `search {q}` → `NoteSummary[]`.
#[tauri::command]
async fn search(svc: Shared<'_>, q: String) -> Result<Vec<NoteSummary>, CmdError> {
    svc.search(&q)
}

/// `note_html {noteId, dark}` → `{title, html}` (a full page for `srcdoc`).
#[tauri::command]
async fn note_html(svc: Shared<'_>, note_id: String, dark: bool) -> Result<NoteHtml, CmdError> {
    svc.note_html(&note_id, dark)
}

/// `open_link {url}`: re-check the scheme (http/https/mailto only), then open in the OS browser.
#[tauri::command]
async fn open_link(app: AppHandle, url: String) -> Result<(), CmdError> {
    open_checked(&app, &url)
}

/// Parse `url` and accept only http, https and mailto.
fn check_link(url: &str) -> Option<Url> {
    let u = Url::parse(url.trim()).ok()?;
    matches!(u.scheme(), "http" | "https" | "mailto").then_some(u)
}

fn open_checked(app: &AppHandle, url: &str) -> Result<(), CmdError> {
    let u = check_link(url).ok_or_else(|| CmdError::internal("link scheme not allowed"))?;
    tracing::info!(scheme = u.scheme(), "open_link");
    app.opener().open_url(u.as_str(), None::<&str>).map_err(|e| CmdError::internal(format!("open: {e}")))
}

/// Is `url` the app's own page? (`http://tauri.localhost` on Windows, `tauri://localhost` elsewhere,
/// the dev server in `tauri dev`.) `about:srcdoc` frames are the note iframes.
fn is_app_origin(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => matches!(url.host_str(), Some("tauri.localhost" | "localhost" | "127.0.0.1")),
        "about" => url.as_str() == "about:srcdoc",
        _ => false,
    }
}

/// Content type from magic bytes; the type a service reports is only a fallback.
fn sniff_mime(b: &[u8], fallback: &str) -> String {
    let m = if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if b.starts_with(b"%PDF") {
        "application/pdf"
    } else if b.len() > 12 && &b[4..8] == b"ftyp" && matches!(&b[8..12], b"heic" | b"heix" | b"mif1") {
        "image/heic"
    } else if b.starts_with(b"<svg") || b.starts_with(b"<?xml") {
        "image/svg+xml"
    } else if fallback.is_empty() {
        "application/octet-stream"
    } else {
        return fallback.to_owned();
    };
    m.to_owned()
}

/// Split `/<attachment-id>/<variant>`. The id is restricted to a safe charset; nothing is taken from note content.
fn parse_asset_path(path: &str) -> Option<(String, AssetVariant)> {
    let mut parts = path.trim_start_matches('/').split('/');
    let (id, variant) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || id.is_empty() || id.len() > 128 {
        return None;
    }
    if !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) || id.starts_with('.') {
        return None;
    }
    Some((id.to_owned(), AssetVariant::parse(variant)?))
}

fn plain(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(status).header(header::CONTENT_TYPE, "text/plain").body(Vec::new()).expect("response")
}

async fn serve_asset(svc: Arc<Backend>, req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    if req.method() != tauri::http::Method::GET {
        return plain(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some((id, variant)) = parse_asset_path(req.uri().path()) else {
        return plain(StatusCode::BAD_REQUEST);
    };
    match svc.asset(&id, variant).await {
        Ok((bytes, mime)) => Response::builder()
            .header(header::CONTENT_TYPE, sniff_mime(&bytes, &mime))
            .header(header::CACHE_CONTROL, "private, max-age=3600")
            .header("X-Content-Type-Options", "nosniff")
            .body(bytes)
            .expect("response"),
        Err(e) => {
            tracing::warn!(code = ?e.code, "asset failed");
            plain(StatusCode::NOT_FOUND)
        }
    }
}

/// Forwards service events to the UI as Tauri events.
#[cfg(not(feature = "mock"))]
struct TauriSink(AppHandle);

#[cfg(not(feature = "mock"))]
impl notes_service::EventSink for TauriSink {
    fn emit(&self, event: notes_service::ServiceEvent) {
        use notes_service::ServiceEvent as E;
        let _ = match event {
            E::SyncProgress(p) => self.0.emit("sync-progress", p),
            E::SyncDone => self.0.emit("sync-done", ()),
            E::AuthRequired => self.0.emit("auth-required", ()),
        };
    }
}

#[cfg(not(feature = "mock"))]
fn open_backend(app: &AppHandle) -> Result<Backend, Box<dyn std::error::Error>> {
    let dir = app.path().app_local_data_dir()?;
    notes_service::Service::open(dir, Arc::new(TauriSink(app.clone()))).map_err(|e| e.message.into())
}

#[cfg(feature = "mock")]
fn open_backend(_app: &AppHandle) -> Result<Backend, Box<dyn std::error::Error>> {
    Ok(Backend::new())
}

/// Validate the stored session and sync in the background, then refresh periodically while focused.
#[cfg(not(feature = "mock"))]
fn start_backend(backend: Arc<Backend>, focused: Arc<AtomicBool>) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = backend.start().await {
            tracing::info!(code = ?e.code, "start: not syncing");
        }
        // Detached for the life of the app.
        drop(backend.spawn_periodic_refresh(notes_service::REFRESH_EVERY, move || focused.load(Ordering::Relaxed)));
    });
}

#[cfg(feature = "mock")]
fn start_backend(_backend: Arc<Backend>, _focused: Arc<AtomicBool>) {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let focused = Arc::new(AtomicBool::new(true));
    let focus_flag = focused.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .on_window_event(move |w, ev| {
            if let (MAIN_LABEL, tauri::WindowEvent::Focused(f)) = (w.label(), ev) {
                focus_flag.store(*f, Ordering::Relaxed);
            }
        })
        .register_asynchronous_uri_scheme_protocol("notes-asset", move |ctx, req, responder| {
            let svc: Arc<Backend> = ctx.app_handle().state::<Arc<Backend>>().inner().clone();
            tauri::async_runtime::spawn(async move { responder.respond(serve_asset(svc, req).await) });
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let backend = Arc::new(open_backend(&handle)?);
            app.manage(backend.clone());
            start_backend(backend, focused);
            // Built in code (not tauri.conf.json) so the navigation and new-window hooks can be attached.
            WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
                .title("StarNotes")
                .inner_size(1200.0, 800.0)
                .on_navigation(is_app_origin)
                .on_new_window(move |url, _features| {
                    if let Err(e) = open_checked(&handle, url.as_str()) {
                        tracing::warn!(msg = %e.message, "new-window request denied");
                    }
                    NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status, sign_in, sign_out, refresh, folders, notes, search, note_html, open_link
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_scheme_checked() {
        assert!(check_link("https://example.com/a").is_some());
        assert!(check_link("http://example.com").is_some());
        assert!(check_link("mailto:a@example.com").is_some());
        for bad in [
            "javascript:alert(1)",
            "file:///C:/Windows/notepad.exe",
            "ms-msdt:/x",
            "data:text/html,x",
            "notes-asset://localhost/a/b",
            "not a url",
        ] {
            assert!(check_link(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn origin_check() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(is_app_origin(&u("http://tauri.localhost/index.html")));
        assert!(is_app_origin(&u("tauri://localhost/")));
        assert!(is_app_origin(&u("about:srcdoc")));
        assert!(!is_app_origin(&u("https://example.com/")));
        assert!(!is_app_origin(&u("https://tauri.localhost.evil.com/")));
        assert!(!is_app_origin(&u("about:blank")));
    }

    #[test]
    fn asset_paths() {
        assert_eq!(parse_asset_path("/att-1/thumb"), Some(("att-1".into(), AssetVariant::Thumb)));
        for bad in ["/", "/a", "/a/b", "/a/original/x", "/../original", "/a b/original", "//original", "/.hidden/original"] {
            assert!(parse_asset_path(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn mime_is_sniffed() {
        assert_eq!(sniff_mime(&[0xff, 0xd8, 0xff, 0xe0], "application/x"), "image/jpeg");
        assert_eq!(sniff_mime(b"%PDF-1.4", ""), "application/pdf");
        assert_eq!(sniff_mime(b"zzzz", "image/x-foo"), "image/x-foo");
        assert_eq!(sniff_mime(b"zzzz", ""), "application/octet-stream");
    }
}
