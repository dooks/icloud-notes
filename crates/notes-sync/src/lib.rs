//! Sync engine (WP-G), following spike 6's recommended strategy.

use std::future::Future;
use std::time::Duration;

use notes_model::{
    asset, bytes_b64, ApiError, ChangeItem, CkError, CloudKitApi, DecodeError, NotesStore, RecordName, SyncError,
    SyncProgress,
};

/// `resultsLimit` for `changes/zone`.
pub const PAGE_LIMIT: u32 = 200;

/// What a [`sync`] run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pages: u32,
    /// Live records applied.
    pub records: u32,
    pub tombstones: u32,
    /// The `zone_token` cheap check matched the stored token; nothing was pulled.
    pub unchanged: bool,
    /// A full mark-and-sweep resync was run.
    pub full_resync: bool,
}

/// A body ready for [`NotesStore::put_note_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBody {
    /// Decompressed `NoteStoreProto` bytes.
    pub proto: Vec<u8>,
    /// Plain text for FTS.
    pub plain_text: String,
}

/// What a [`hydrate_bodies`] run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HydrateReport {
    pub done: u32,
    pub failed: u32,
    /// Still needing a body after this run (beyond the `n` limit).
    pub remaining: u32,
}

/// Retryable-error backoff: one sleep per entry, then give up (and resync).
const BACKOFF: [Duration; 3] = [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(4)];

/// Why a CloudKit call (with backoff applied) did not produce a value.
enum Fail {
    /// The token or zone is unusable: a full resync may fix it.
    Resync(CkError),
    /// A retryable code kept coming back after backoff.
    Exhausted(CkError),
    Fatal(SyncError),
}

impl Fail {
    /// The error to return when no resync can follow.
    fn into_error(self) -> SyncError {
        match self {
            Fail::Resync(e) => SyncError::Api(ApiError::Ck(e)),
            Fail::Exhausted(e) => SyncError::GaveUp(e),
            Fail::Fatal(e) => e,
        }
    }
}

/// Call `f`, sleeping and retrying on retryable CloudKit codes (at most `BACKOFF.len()` times in a row).
async fn with_backoff<T, F, Fut>(mut f: F) -> Result<T, Fail>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let mut retries = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(ApiError::Ck(e)) if e.needs_resync() => return Err(Fail::Resync(e)),
            Err(ApiError::Ck(e)) if e.is_retry_later() => match BACKOFF.get(retries) {
                Some(d) => {
                    tracing::debug!(code = %e.code, retry = retries + 1, "backing off");
                    tokio::time::sleep(*d).await;
                    retries += 1;
                }
                None => return Err(Fail::Exhausted(e)),
            },
            // Auth errors, transport errors and other CloudKit codes bubble up unchanged.
            Err(e) => return Err(Fail::Fatal(e.into())),
        }
    }
}

/// Page `changes` from `token` until `more_coming == false`, applying and reporting each page.
async fn pull<A, S>(
    api: &A,
    store: &mut S,
    mut token: Option<String>,
    progress: &mut (impl FnMut(SyncProgress) + Send),
    report: &mut SyncReport,
) -> Result<(), Fail>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    loop {
        let page = with_backoff(|| api.changes(token.as_deref(), PAGE_LIMIT)).await?;
        store.apply_page(&page).map_err(|e| Fail::Fatal(e.into()))?;
        report.pages += 1;
        for item in &page.items {
            match item {
                ChangeItem::Live(_) => report.records += 1,
                ChangeItem::Tombstone(_) => report.tombstones += 1,
            }
        }
        progress(SyncProgress { pages: report.pages, records: report.records });
        // Loop on `moreComing`, never on an empty page.
        if !page.more_coming {
            return Ok(());
        }
        token = Some(page.sync_token);
    }
}

/// Mark-and-sweep resync: a full listing from the beginning, then delete what it did not contain.
async fn full_resync<A, S>(
    api: &A,
    store: &mut S,
    progress: &mut (impl FnMut(SyncProgress) + Send),
) -> Result<SyncReport, SyncError>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    store.begin_full_resync()?;
    let mut report = SyncReport { full_resync: true, ..Default::default() };
    pull(api, store, None, progress, &mut report).await.map_err(Fail::into_error)?;
    store.finish_full_resync()?;
    Ok(report)
}

/// Bring the store up to date.
///
/// 1. With a stored token, `zone_token`; equal → return `unchanged`.
/// 2. Loop `changes(PAGE_LIMIT)` until `more_coming == false`, `apply_page` per page, `progress` per page.
/// 3. `BAD_REQUEST` / `CHANGE_TOKEN_EXPIRED` / `ZONE_NOT_FOUND` → `begin_full_resync`, full pull, `finish_full_resync`.
/// 4. `TRY_AGAIN_LATER` → back off 3 times (tokio sleep), then resync.
/// 5. Auth errors bubble up unchanged.
///
/// An interrupted run resumes from the last applied page's token (`apply_page` stores it). A resync
/// interrupted before `finish_full_resync` also resumes from that token and is not swept; the next
/// resync re-marks every row.
pub async fn sync<A, S>(
    api: &A,
    store: &mut S,
    mut progress: impl FnMut(SyncProgress) + Send,
) -> Result<SyncReport, SyncError>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    let stored = store.sync_token()?;
    if let Some(stored) = &stored {
        match with_backoff(|| api.zone_token()).await {
            Ok(current) if &current == stored => {
                return Ok(SyncReport { unchanged: true, ..Default::default() });
            }
            Ok(_) => {}
            Err(Fail::Resync(_) | Fail::Exhausted(_)) => return full_resync(api, store, &mut progress).await,
            Err(Fail::Fatal(e)) => return Err(e),
        }
    }

    let mut report = SyncReport::default();
    match pull(api, store, stored, &mut progress, &mut report).await {
        Ok(()) => Ok(report),
        Err(Fail::Resync(e) | Fail::Exhausted(e)) => {
            tracing::warn!(code = %e.code, "incremental sync failed; running a full resync");
            full_resync(api, store, &mut progress).await
        }
        Err(Fail::Fatal(e)) => Err(e),
    }
}

/// Fill in up to `n` bodies from [`NotesStore::notes_needing_body`].
///
/// For each note: take `TextDataEncrypted` (via `notes_model::bytes_b64`) or, if absent, `lookup` the note for a
/// fresh `TextDataAsset` URL and `download` it; pass the raw (still compressed) bytes to `decode`; store the result.
/// A decode failure counts as `failed` and doesn't stop the run. `decode` is injected so this crate doesn't depend
/// on `notes-proto` (the service passes `inflate_bytes` + `decode_note` + `plain_text`).
///
/// A note that is missing from the store, has no body field, or whose CloudKit lookup fails with a CloudKit
/// error also counts as `failed`. Auth and transport errors abort the run.
pub async fn hydrate_bodies<A, S>(
    api: &A,
    store: &mut S,
    n: usize,
    decode: impl Fn(&[u8]) -> Result<DecodedBody, DecodeError> + Send + Sync,
) -> Result<HydrateReport, SyncError>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    let pending = store.notes_needing_body()?;
    let mut report = HydrateReport { remaining: pending.len().saturating_sub(n) as u32, ..Default::default() };

    for name in pending.into_iter().take(n) {
        let Some(record) = store.record(&name)? else {
            report.failed += 1;
            continue;
        };
        let raw = match bytes_b64(&record, "TextDataEncrypted") {
            Some(bytes) => Some(bytes),
            None => match fetch_asset_body(api, &name).await {
                Ok(bytes) => bytes,
                Err(ApiError::Ck(e)) => {
                    tracing::warn!(code = %e.code, "body lookup failed");
                    None
                }
                Err(e) => return Err(e.into()),
            },
        };
        let Some(raw) = raw else {
            report.failed += 1;
            continue;
        };
        match decode(&raw) {
            Ok(body) => {
                store.put_note_body(&name, &body.proto, &body.plain_text)?;
                report.done += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, "body decode failed");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// `lookup` a note for a fresh `TextDataAsset` URL and download it. `None` when the note has no such asset.
async fn fetch_asset_body<A: CloudKitApi + Sync>(api: &A, name: &RecordName) -> Result<Option<Vec<u8>>, ApiError> {
    let items = api.lookup(std::slice::from_ref(name), Some(&["TextDataAsset"])).await?;
    let found = items.iter().find_map(|item| match item {
        ChangeItem::Live(r) if &r.name == name => asset(r, "TextDataAsset"),
        _ => None,
    });
    match found {
        Some(a) => Ok(Some(api.download(&a).await?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
