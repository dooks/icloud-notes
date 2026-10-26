//! The facade `app/src-tauri` calls (WP-J, Wave 2). Every method maps 1:1 to a §3.4 command
//! or to the `notes-asset` protocol. Errors are already [`CmdError`]s.
//!
//! Logging: record names, counts and error codes only, never note content or cookies.

mod assets;
mod view;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloudkit::CloudKitClient;
use icloud_session::{CookieJar, DpapiStore, Session};
use notes_model::{
    ApiError, AssetRef, AssetVariant, AuthError, ChangeItem, CloudKitApi, CmdError, CmdErrorCode, DecodeError,
    FolderView, NoteHtml, NoteSummary, NotesStore, QUICK_NOTES_VIEW_ID, RecordName, RecordType, RefreshResult, Status,
    SyncProgress, int,
};
use notes_store::SqliteStore;
use notes_sync::DecodedBody;

/// Events the service raises; the host forwards them to the UI as Tauri events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEvent {
    /// `sync-progress {pages, records}`.
    SyncProgress(SyncProgress),
    /// `sync-done`.
    SyncDone,
    /// `auth-required`.
    AuthRequired,
}

/// Where the service sends [`ServiceEvent`]s (the host implements this over `AppHandle::emit`).
pub trait EventSink: Send + Sync {
    fn emit(&self, event: ServiceEvent);
}

/// Bodies fetched per `hydrate_bodies` call.
const HYDRATE_BATCH: usize = 50;
/// How often the window-focused refresh should run (PLAN.md §5 WP-J).
pub const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// Turns a stored jar into a validated API handle.
pub type Connect<A> = Box<dyn Fn(CookieJar) -> BoxFuture<Result<A, ApiError>> + Send + Sync>;

#[derive(Default)]
struct State {
    signed_in: bool,
    syncing: bool,
    last_sync: Option<String>,
    error: Option<String>,
}

struct Inner<A> {
    events: Arc<dyn EventSink>,
    jars: DpapiStore,
    assets_dir: PathBuf,
    connect: Connect<A>,
    api: Mutex<Option<Arc<A>>>,
    /// Serializes connecting (`validate`) so concurrent callers share one.
    connecting: tokio::sync::Mutex<()>,
    /// Reads (folders, notes, note_html) never wait for a sync: they use their own connection (WAL).
    reader: Mutex<SqliteStore>,
    /// Held for the whole of a sync/hydrate run.
    writer: tokio::sync::Mutex<SqliteStore>,
    state: Mutex<State>,
    background: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The app's backend, generic over the CloudKit implementation so tests can use fakes. The app uses [`Service`].
/// `Send + Sync`, held in Tauri state.
pub struct Core<A: CloudKitApi + Send + Sync + 'static> {
    inner: Arc<Inner<A>>,
}

/// The real service: [`CloudKitClient`] over an iCloud [`Session`].
pub type Service = Core<CloudKitClient>;

impl Service {
    /// Open the cache and jar under `data_dir` (`%LOCALAPPDATA%\com.ziggy.starnotes`). No network.
    pub fn open(data_dir: PathBuf, events: Arc<dyn EventSink>) -> Result<Self, CmdError> {
        let jars = DpapiStore::new(data_dir.join("session.dpapi"));
        let persist = jars.clone();
        let connect: Connect<CloudKitClient> = Box::new(move |jar| {
            let persist = persist.clone();
            Box::pin(async move {
                let session = Arc::new(Session::new(jar, Some(persist)));
                let endpoints = session.validate().await?;
                Ok(CloudKitClient::new(session, &endpoints))
            })
        });
        Core::with_parts(data_dir, events, jars, connect)
    }
}

impl<A: CloudKitApi + Send + Sync + 'static> Core<A> {
    /// The seam [`Service::open`] and the tests share: `connect` turns a stored jar into a validated API handle.
    pub fn with_parts(
        data_dir: PathBuf,
        events: Arc<dyn EventSink>,
        jars: DpapiStore,
        connect: Connect<A>,
    ) -> Result<Self, CmdError> {
        std::fs::create_dir_all(&data_dir).map_err(|e| CmdError::internal(format!("data dir: {e}")))?;
        let db = data_dir.join("cache.sqlite");
        let writer = SqliteStore::open(&db)?;
        let reader = SqliteStore::open(&db)?;
        let signed_in = matches!(jars.load(), Ok(Some(j)) if j.has_session());
        Ok(Core {
            inner: Arc::new(Inner {
                events,
                jars,
                assets_dir: data_dir.join("assets"),
                connect,
                api: Mutex::new(None),
                connecting: tokio::sync::Mutex::new(()),
                reader: Mutex::new(reader),
                writer: tokio::sync::Mutex::new(writer),
                state: Mutex::new(State { signed_in, ..State::default() }),
                background: Mutex::new(None),
            }),
        })
    }

    /// Validate the stored jar (emit `AuthRequired` on an auth error), then sync and hydrate in the background.
    pub async fn start(&self) -> Result<(), CmdError> {
        self.inner.ensure_api().await?;
        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            let _ = inner.run_sync().await;
        });
        *lock(&self.inner.background) = Some(handle);
        Ok(())
    }

    /// Wait for the background run started by [`Core::start`] to finish (tests and shutdown).
    pub async fn wait_idle(&self) {
        let handle = lock(&self.inner.background).take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    /// `status` command.
    pub fn status(&self) -> Status {
        let s = lock(&self.inner.state);
        Status { signed_in: s.signed_in, syncing: s.syncing, last_sync: s.last_sync.clone(), error: s.error.clone() }
    }

    /// Called by the host after the login window captured cookies: replace and persist the jar, then [`Core::start`].
    pub async fn signed_in(&self, jar: CookieJar) -> Result<(), CmdError> {
        self.inner.jars.save(&jar).map_err(|e| CmdError::internal(format!("save session: {e}")))?;
        *lock(&self.inner.api) = None;
        {
            let mut s = lock(&self.inner.state);
            s.signed_in = true;
            s.error = None;
        }
        self.start().await
    }

    /// `sign_out` command (service half): delete the jar. The host clears the WebView2 profile.
    pub async fn sign_out(&self) -> Result<(), CmdError> {
        self.inner.jars.delete().map_err(|e| CmdError::internal(format!("delete session: {e}")))?;
        *lock(&self.inner.api) = None;
        let mut s = lock(&self.inner.state);
        s.signed_in = false;
        s.error = None;
        Ok(())
    }

    /// `refresh` command: cheap zone check, then delta sync and hydration.
    pub async fn refresh(&self) -> Result<RefreshResult, CmdError> {
        let changed = self.inner.run_sync().await?;
        Ok(RefreshResult { changed })
    }

    /// Refresh every `every` ([`REFRESH_EVERY`]) while `focused()` is true and the user is signed in.
    /// Abort the returned handle to stop.
    pub fn spawn_periodic_refresh(
        &self,
        every: Duration,
        focused: impl Fn() -> bool + Send + Sync + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let this = Core { inner: self.inner.clone() };
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                if focused() && this.status().signed_in {
                    let _ = this.refresh().await;
                }
            }
        })
    }

    /// `folders` command.
    pub fn folders(&self) -> Result<Vec<FolderView>, CmdError> {
        Ok(self.inner.read().folders()?)
    }

    /// `notes` command. `folder_id` is a folder recordName or `notes_model::QUICK_NOTES_VIEW_ID`.
    pub fn notes(&self, folder_id: &str) -> Result<Vec<NoteSummary>, CmdError> {
        let store = self.inner.read();
        Ok(if folder_id == QUICK_NOTES_VIEW_ID {
            store.quick_notes()?
        } else {
            store.notes_in(&RecordName::from(folder_id))?
        })
    }

    /// `search` command.
    pub fn search(&self, q: &str) -> Result<Vec<NoteSummary>, CmdError> {
        Ok(self.inner.read().search(q)?)
    }

    /// `note_html` command: decode the body, resolve attachments, render, wrap in `page`.
    ///
    /// Locked notes are never decoded; a note whose body has not been hydrated yet gets a placeholder page.
    pub fn note_html(&self, note_id: &str, dark: bool) -> Result<NoteHtml, CmdError> {
        let id = RecordName::from(note_id);
        let store = self.inner.read();
        let rec = store
            .record(&id)?
            .filter(|r| view::is_note(r) && int(r, "Deleted") != Some(1) && !r.fields.is_empty())
            .ok_or_else(|| CmdError::internal("no such note"))?;
        let title = view::title_of(&rec);
        let fragment = if rec.record_type == RecordType::PasswordProtectedNote {
            "<p class=\"placeholder\">\u{1F512} This note is locked and cannot be shown here.</p>".to_owned()
        } else {
            match store.note_body(&id)? {
                None => "<p class=\"placeholder\">This note has not been downloaded yet.</p>".to_owned(),
                Some(bytes) => {
                    let note = notes_proto::decode_note(&bytes)?;
                    notes_render::render_note(&note, &view::StoreResolver { store: &*store, dark })
                }
            }
        };
        Ok(NoteHtml { title, html: notes_render::page(&fragment, dark) })
    }

    /// `http://notes-asset.localhost/<attachment_id>/<variant>`: `(bytes, mime)`, from the disk cache keyed by
    /// `fileChecksum`, else a fresh `lookup` URL + `download`.
    pub async fn asset(&self, attachment_id: &str, variant: AssetVariant) -> Result<(Vec<u8>, String), CmdError> {
        let inner = &self.inner;
        let target = {
            let store = inner.read();
            assets::locate(&*store, &RecordName::from(attachment_id), variant)?
        }
        .ok_or_else(|| CmdError::internal("no such asset"))?;
        let dir = &inner.assets_dir;
        if let Some(bytes) = assets::read_cached(dir, &target.stored.file_checksum) {
            let mime = assets::sniff_mime(&bytes).to_owned();
            return Ok((bytes, mime));
        }
        let api = inner.ensure_api().await?;
        let (fresh, bytes) = fetch_fresh(&*api, &target).await.map_err(|e| inner.failed(e.into()))?;
        if let Err(e) = assets::write_cached(dir, &fresh.file_checksum, &bytes) {
            tracing::warn!(error = %e, "could not write the asset cache");
        }
        let mime = assets::sniff_mime(&bytes).to_owned();
        Ok((bytes, mime))
    }
}

/// A fresh `records/lookup` URL for the asset, then the cookie-less download.
async fn fetch_fresh<A: CloudKitApi + Sync>(api: &A, t: &assets::Target) -> Result<(AssetRef, Vec<u8>), ApiError> {
    let items = api.lookup(std::slice::from_ref(&t.holder), Some(&[t.field])).await?;
    let fresh = items
        .iter()
        .find_map(|i| match i {
            ChangeItem::Live(r) if r.name == t.holder => assets::extract(r, t.field, t.index),
            _ => None,
        })
        .ok_or_else(|| ApiError::Http("asset is no longer available".into()))?;
    let bytes = api.download(&fresh).await?;
    Ok((fresh, bytes))
}

impl<A: CloudKitApi + Send + Sync + 'static> Inner<A> {
    fn read(&self) -> MutexGuard<'_, SqliteStore> {
        lock(&self.reader)
    }

    /// Record a failure in the status; an auth failure drops the connection and raises `auth-required`.
    fn failed(&self, e: CmdError) -> CmdError {
        {
            let mut s = lock(&self.state);
            s.error = Some(e.message.clone());
            if e.code == CmdErrorCode::Auth {
                s.signed_in = false;
            }
        }
        if e.code == CmdErrorCode::Auth {
            *lock(&self.api) = None;
            self.events.emit(ServiceEvent::AuthRequired);
        }
        e
    }

    /// The connected API handle: load the jar and `validate` if not connected yet.
    async fn ensure_api(&self) -> Result<Arc<A>, CmdError> {
        if let Some(a) = lock(&self.api).clone() {
            return Ok(a);
        }
        let _connecting = self.connecting.lock().await;
        if let Some(a) = lock(&self.api).clone() {
            return Ok(a);
        }
        let jar = match self.jars.load() {
            Ok(Some(j)) if j.has_session() => j,
            Ok(_) => return Err(self.failed(ApiError::Auth(AuthError::NotSignedIn).into())),
            Err(e) => return Err(self.failed(CmdError::internal(format!("load session: {e}")))),
        };
        match (self.connect)(jar).await {
            Ok(api) => {
                let api = Arc::new(api);
                *lock(&self.api) = Some(api.clone());
                let mut s = lock(&self.state);
                s.signed_in = true;
                s.error = None;
                Ok(api)
            }
            Err(e) => Err(self.failed(e.into())),
        }
    }

    /// Connect if needed, sync, then hydrate bodies. Returns the number of changed records.
    /// Emits `sync-progress`, then `sync-done` (or `auth-required` on an auth failure).
    async fn run_sync(&self) -> Result<u32, CmdError> {
        let api = self.ensure_api().await?;
        lock(&self.state).syncing = true;
        let result = self.sync_and_hydrate(&api).await;
        {
            let mut s = lock(&self.state);
            s.syncing = false;
            if result.is_ok() {
                s.last_sync = Some(rfc3339_now());
                s.error = None;
            }
        }
        let result = result.map_err(|e| self.failed(e));
        if !matches!(&result, Err(e) if e.code == CmdErrorCode::Auth) {
            self.events.emit(ServiceEvent::SyncDone);
        }
        result
    }

    async fn sync_and_hydrate(&self, api: &Arc<A>) -> Result<u32, CmdError> {
        let mut store = self.writer.lock().await;
        let events = self.events.clone();
        let report = notes_sync::sync(&**api, &mut *store, move |p| events.emit(ServiceEvent::SyncProgress(p))).await?;
        loop {
            let h = notes_sync::hydrate_bodies(&**api, &mut *store, HYDRATE_BATCH, decode_body).await?;
            tracing::debug!(done = h.done, failed = h.failed, remaining = h.remaining, "hydrated bodies");
            // `failed` bodies stay pending, so stop as soon as a batch makes no progress.
            if h.remaining == 0 || h.done == 0 {
                break;
            }
        }
        tracing::info!(pages = report.pages, records = report.records, tombstones = report.tombstones, "synced");
        Ok(report.records + report.tombstones)
    }
}

/// The `hydrate_bodies` decoder: inflate, check it parses, and extract the search text.
fn decode_body(raw: &[u8]) -> Result<DecodedBody, DecodeError> {
    let proto = notes_proto::inflate_bytes(raw)?;
    let note = notes_proto::decode_note(&proto)?;
    Ok(DecodedBody { plain_text: notes_proto::plain_text(&note), proto })
}

/// `YYYY-MM-DDTHH:MM:SSZ` for now (UTC), without a date-time dependency.
fn rfc3339_now() -> String {
    rfc3339(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64)
}

fn rfc3339(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests;
