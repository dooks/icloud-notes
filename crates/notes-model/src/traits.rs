//! The seams between the sync engine and its I/O crates.
//!
//! `CloudKitApi` methods return `impl Future + Send` (RPITIT) rather than being `async fn`, so callers can
//! `tokio::spawn` a sync. Implementations may still write `async fn` bodies.

use std::future::Future;

use crate::error::{ApiError, StoreError};
use crate::record::{AssetRef, ChangeItem, ChangesPage, CkRecord, RecordName};
use crate::view::{FolderView, NoteSummary};

/// Read-only CloudKit access to the private `Notes` zone. Implemented by `cloudkit::CloudKitClient`.
pub trait CloudKitApi {
    /// `zones/lookup`: the zone's current syncToken (the cheap "anything changed?" check).
    fn zone_token(&self) -> impl Future<Output = Result<String, ApiError>> + Send;

    /// `changes/zone`: one page of changes after `token` (`None` = from the beginning), at most `limit` records.
    fn changes(&self, token: Option<&str>, limit: u32) -> impl Future<Output = Result<ChangesPage, ApiError>> + Send;

    /// `records/lookup`: fetch records by name, optionally only `desired_keys`. Also how fresh asset URLs are obtained.
    fn lookup(
        &self,
        names: &[RecordName],
        desired_keys: Option<&[&str]>,
    ) -> impl Future<Output = Result<Vec<ChangeItem>, ApiError>> + Send;

    /// Plain GET of an asset URL with **no cookies**; the result length is checked against `asset.size`.
    fn download(&self, asset: &AssetRef) -> impl Future<Output = Result<Vec<u8>, ApiError>> + Send;
}

/// The local cache. Implemented by `notes_store::SqliteStore`.
pub trait NotesStore {
    /// The stored `changes/zone` cursor, if any.
    fn sync_token(&self) -> Result<Option<String>, StoreError>;
    /// Apply one page in a single transaction, storing `page.sync_token` in the same transaction.
    fn apply_page(&mut self, page: &ChangesPage) -> Result<(), StoreError>;
    /// Start a mark-and-sweep resync: mark every row unseen and clear the stored token.
    fn begin_full_resync(&mut self) -> Result<(), StoreError>;
    /// Finish a resync: delete every row not seen since [`NotesStore::begin_full_resync`].
    fn finish_full_resync(&mut self) -> Result<(), StoreError>;
    /// One record by name.
    fn record(&self, name: &RecordName) -> Result<Option<CkRecord>, StoreError>;
    /// Folder list in display order: Quick Notes, Notes, user folders by title, Recently Deleted.
    fn folders(&self) -> Result<Vec<FolderView>, StoreError>;
    /// Visible notes whose `Folder` is `folder`, newest first.
    fn notes_in(&self, folder: &RecordName) -> Result<Vec<NoteSummary>, StoreError>;
    /// Visible notes with `Note_UserSpecific.IsSystemPaper == 1`, newest first.
    fn quick_notes(&self) -> Result<Vec<NoteSummary>, StoreError>;
    /// Full-text search over title and plain text.
    fn search(&self, q: &str) -> Result<Vec<NoteSummary>, StoreError>;
    /// The decompressed `NoteStoreProto` bytes, if hydrated.
    fn note_body(&self, note: &RecordName) -> Result<Option<Vec<u8>>, StoreError>;
    /// Store a decoded body and its plain text (for FTS).
    fn put_note_body(&mut self, note: &RecordName, proto: &[u8], plain_text: &str) -> Result<(), StoreError>;
    /// Notes whose body changed or whose `TextDataAsset` is still pending.
    fn notes_needing_body(&self) -> Result<Vec<RecordName>, StoreError>;
}
